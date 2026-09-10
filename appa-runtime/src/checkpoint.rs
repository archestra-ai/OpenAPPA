//! Loopback control API for durable detached checkpoints.

use appa_runtime_api::{Adapter, AdapterName, PROTOCOL};

use crate::api::Runtime;

#[derive(serde::Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Create {
        protocol: u32,
        adapter: AdapterName,
        root_id: String,
    },
    Fork {
        protocol: u32,
        adapter: AdapterName,
        checkpoint_id: String,
        root_id: String,
    },
}

/// Decode and admit one checkpoint control request. The served adapter derives
/// both root identities; callers never submit internal trajectory ids.
pub(crate) fn answer(runtime: &Runtime, served: Adapter, body: &[u8]) -> (u16, serde_json::Value) {
    let request: Request = match serde_json::from_slice(body) {
        Ok(request) => request,
        Err(error) => return refuse(400, error.to_string()),
    };
    let (protocol, adapter, root_id) = match &request {
        Request::Create {
            protocol,
            adapter,
            root_id,
        }
        | Request::Fork {
            protocol,
            adapter,
            root_id,
            ..
        } => (*protocol, *adapter, root_id),
    };
    if protocol != PROTOCOL || adapter != served.name {
        return refuse(409, "checkpoint request does not match this deployment".to_string());
    }
    if !crate::tool_validation::valid_host_trajectory_id(root_id) {
        return refuse(400, "root_id must be a nonempty host trajectory ID".to_string());
    }
    match request {
        Request::Create { root_id, .. } => match runtime.checkpoint(&served.name.root(&root_id)) {
            Ok(issue) => (
                200,
                serde_json::json!({
                    "checkpoint_id": issue.id.as_str(),
                    "source_scope": { "adapter": served.name, "root_id": root_id },
                    "position": issue.position,
                    "digest": issue.digest,
                }),
            ),
            Err(error) => refuse(409, error.to_string()),
        },
        Request::Fork {
            checkpoint_id, root_id, ..
        } => match runtime.fork_checkpoint(
            served.name,
            appa_engine::fact::CheckpointId::new(checkpoint_id),
            served.name.root(&root_id),
        ) {
            Ok(()) => (200, serde_json::json!({ "root_id": root_id })),
            Err(error) => refuse(409, error.to_string()),
        },
    }
}

fn refuse(status: u16, error: String) -> (u16, serde_json::Value) {
    (status, serde_json::json!({ "error": error }))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::config::{Config, ExternalBindings};

    fn runtime(dir: &tempfile::TempDir) -> Runtime {
        let config = Config::embedded(
            "version = 2\n[[tool]]\nname = \"host/claude-code/Bash\"\n".to_string(),
            ExternalBindings::new(Duration::from_secs(30), 65_536),
        )
        .unwrap();
        Runtime::open(config, dir.path().join("appa.db"), None).unwrap()
    }

    fn pending_runtime(dir: &tempfile::TempDir) -> Runtime {
        let config = Config::embedded(
            "version = 2\n[[tool]]\nname = \"host/claude-code/Agent\"\n[deployment]\ncontext_control = true\n"
                .to_string(),
            ExternalBindings::new(Duration::from_secs(30), 65_536),
        )
        .unwrap();
        Runtime::open(config, dir.path().join("pending.db"), None).unwrap()
    }

    fn progressing_runtime(dir: &tempfile::TempDir) -> Runtime {
        let config = Config::embedded(
            "version = 2\n[[tool]]\nname = \"mark\"\nparameters = { type = \"object\", properties = { a = { type = \"integer\" } } }\ndelta = {}\n".to_string(),
            ExternalBindings::new(Duration::from_secs(30), 65_536),
        )
        .unwrap();
        Runtime::open(config, dir.path().join("progressing.db"), None).unwrap()
    }

    #[test]
    fn a_trusted_adapter_can_checkpoint_and_detach_a_clean_root_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(&dir);
        let adapter = appa_adapter_claude_code::adapter();
        runtime.create_session(adapter.name.root("source")).unwrap();
        let before = runtime.audit(&adapter.name.root("source")).unwrap();
        let create = serde_json::json!({
            "protocol": PROTOCOL,
            "adapter": "claude-code",
            "operation": "create",
            "root_id": "source"
        });
        let (status, response) = answer(&runtime, adapter, &serde_json::to_vec(&create).unwrap());
        assert_eq!(status, 200, "{response}");
        let checkpoint = response["checkpoint_id"].as_str().unwrap();
        assert_eq!(response["source_scope"]["adapter"], "claude-code");
        assert_eq!(response["source_scope"]["root_id"], "source");
        assert_eq!(response["position"], 1);
        assert!(response["digest"].as_str().unwrap().starts_with("sha256:"));
        assert_eq!(runtime.audit(&adapter.name.root("source")).unwrap(), before);

        let fork = serde_json::json!({
            "protocol": PROTOCOL,
            "adapter": "claude-code",
            "operation": "fork",
            "checkpoint_id": checkpoint,
            "root_id": "detached"
        });
        for _ in 0..2 {
            let (status, response) = answer(&runtime, adapter, &serde_json::to_vec(&fork).unwrap());
            assert_eq!((status, response), (200, serde_json::json!({ "root_id": "detached" })));
        }
        runtime
            .live(&adapter.name.root("detached"), &adapter.name.root("detached"))
            .unwrap();
        assert_eq!(runtime.audit(&adapter.name.root("source")).unwrap(), before);

        let mismatched = serde_json::json!({
            "protocol": PROTOCOL,
            "adapter": "kagent",
            "operation": "create",
            "root_id": "source"
        });
        assert_eq!(
            answer(&runtime, adapter, &serde_json::to_vec(&mismatched).unwrap()).0,
            409
        );
    }

    #[test]
    fn a_checkpoint_cannot_cross_the_served_adapter_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(&dir);
        let claude = appa_adapter_claude_code::adapter();
        let kagent = appa_adapter_kagent::adapter();

        for (source_adapter, served_adapter, source_id, target_id) in [
            (kagent, claude, "kagent-source", "claude-target"),
            (claude, kagent, "claude-source", "kagent-target"),
        ] {
            runtime.create_session(source_adapter.name.root(source_id)).unwrap();
            let create = serde_json::json!({
                "protocol": PROTOCOL,
                "adapter": source_adapter.name,
                "operation": "create",
                "root_id": source_id,
            });
            let (status, created) = answer(&runtime, source_adapter, &serde_json::to_vec(&create).unwrap());
            assert_eq!(status, 200, "{created}");
            let checkpoint_id = created["checkpoint_id"].as_str().unwrap();
            let fork = serde_json::json!({
                "protocol": PROTOCOL,
                "adapter": served_adapter.name,
                "operation": "fork",
                "checkpoint_id": checkpoint_id,
                "root_id": target_id,
            });
            let (status, response) = answer(&runtime, served_adapter, &serde_json::to_vec(&fork).unwrap());
            assert_eq!(status, 409, "{response}");
            assert!(
                runtime.create_session(served_adapter.name.root(target_id)).is_ok(),
                "a rejected cross-adapter fork must not create its target"
            );
        }
    }

    #[tokio::test]
    async fn exact_checkpoint_replay_preserves_target_progress() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = progressing_runtime(&dir);
        let adapter = appa_adapter_claude_code::adapter();
        let source = adapter.name.root("source");
        let target = adapter.name.root("detached");
        runtime.create_session(source).unwrap();
        let create = serde_json::json!({
            "protocol": PROTOCOL,
            "adapter": adapter.name,
            "operation": "create",
            "root_id": "source",
        });
        let (_, created) = answer(&runtime, adapter, &serde_json::to_vec(&create).unwrap());
        let fork = serde_json::json!({
            "protocol": PROTOCOL,
            "adapter": adapter.name,
            "operation": "fork",
            "checkpoint_id": created["checkpoint_id"],
            "root_id": "detached",
        });
        assert_eq!(answer(&runtime, adapter, &serde_json::to_vec(&fork).unwrap()).0, 200);

        let session = runtime.session(&target, &target).unwrap();
        let call = appa_runtime_api::ProposedCall {
            tool: "mark".to_string(),
            arguments: crate::api::raw(serde_json::json!({"a": 1})),
        };
        session.on_tool_call(call.clone(), false).await.unwrap();
        session
            .on_tool_result(
                call,
                appa_runtime_api::ToolOutcome::Success {
                    body: appa_runtime_api::OutcomeBody::Available("marked".to_string()),
                },
            )
            .await
            .unwrap();
        let before_status = runtime.status(&target).unwrap();
        let before_audit = runtime.audit(&target).unwrap();
        let before_label = (before_status.trust.clone(), before_status.audience.clone());
        assert!(before_audit.len() > 1, "the detached root has progressed");

        assert_eq!(answer(&runtime, adapter, &serde_json::to_vec(&fork).unwrap()).0, 200);
        let after_status = runtime.status(&target).unwrap();
        assert_eq!((after_status.trust, after_status.audience), before_label);
        assert_eq!(runtime.audit(&target).unwrap(), before_audit);
    }

    #[tokio::test]
    async fn a_pending_offer_cannot_be_checkpointed_or_borrowed_by_a_detached_root() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = pending_runtime(&dir);
        let adapter = appa_adapter_claude_code::adapter();
        let root = adapter.name.root("pending");
        assert_eq!(
            crate::hooks::handle(
                &runtime,
                appa_runtime_api::HookEvent::SessionStart { root: root.clone() }
            )
            .await,
            appa_runtime_api::HookDecision::Ack
        );
        let held = crate::hooks::handle(
            &runtime,
            appa_runtime_api::HookEvent::ToolCall {
                actor: appa_runtime_api::Actor {
                    root: root.clone(),
                    child: None,
                },
                call: appa_runtime_api::ProposedCall {
                    tool: "host/claude-code/Agent".to_string(),
                    arguments: crate::api::raw(serde_json::json!({"prompt":"inspect"})),
                },
                spawn: true,
                ruling: None,
            },
        )
        .await;
        assert!(matches!(held, appa_runtime_api::HookDecision::DenyCall { .. }));
        let request = serde_json::json!({
            "protocol": PROTOCOL,
            "adapter": "claude-code",
            "operation": "create",
            "root_id": "pending"
        });
        let (status, response) = answer(&runtime, adapter, &serde_json::to_vec(&request).unwrap());
        assert_eq!(status, 409);
        assert!(response["error"].as_str().unwrap().contains("not quiescent"));
    }

    #[test]
    fn checkpoint_requests_refuse_invalid_host_root_ids_without_creating_roots() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(&dir);
        let adapter = appa_adapter_claude_code::adapter();
        runtime.create_session(adapter.name.root("source")).unwrap();
        let create_source = serde_json::json!({
            "protocol": PROTOCOL,
            "adapter": adapter.name,
            "operation": "create",
            "root_id": "source",
        });
        let (_, created) = answer(&runtime, adapter, &serde_json::to_vec(&create_source).unwrap());
        let checkpoint_id = created["checkpoint_id"].as_str().unwrap();

        for root_id in ["", "unsafe\nroot"] {
            let create = serde_json::json!({
                "protocol": PROTOCOL,
                "adapter": adapter.name,
                "operation": "create",
                "root_id": root_id,
            });
            let (status, response) = answer(&runtime, adapter, &serde_json::to_vec(&create).unwrap());
            assert_eq!(status, 400, "{response}");
            assert!(
                runtime
                    .live(&adapter.name.root(root_id), &adapter.name.root(root_id))
                    .is_err(),
                "an invalid create root must not be persisted"
            );

            let fork = serde_json::json!({
                "protocol": PROTOCOL,
                "adapter": adapter.name,
                "operation": "fork",
                "checkpoint_id": checkpoint_id,
                "root_id": root_id,
            });
            let (status, response) = answer(&runtime, adapter, &serde_json::to_vec(&fork).unwrap());
            assert_eq!(status, 400, "{response}");
            assert!(
                runtime
                    .live(&adapter.name.root(root_id), &adapter.name.root(root_id))
                    .is_err(),
                "an invalid fork target must not be persisted"
            );
        }
    }

    #[tokio::test]
    async fn a_checkpoint_fork_refuses_an_existing_target_after_it_progresses() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = progressing_runtime(&dir);
        let adapter = appa_adapter_claude_code::adapter();
        runtime.create_session(adapter.name.root("source")).unwrap();
        let create = serde_json::json!({
            "protocol": PROTOCOL,
            "adapter": adapter.name,
            "operation": "create",
            "root_id": "source",
        });
        let (_, created) = answer(&runtime, adapter, &serde_json::to_vec(&create).unwrap());
        let target = adapter.name.root("ordinary-target");
        let session = runtime.create_session(target.clone()).unwrap();
        let call = appa_runtime_api::ProposedCall {
            tool: "mark".to_string(),
            arguments: crate::api::raw(serde_json::json!({"a": 1})),
        };
        session.on_tool_call(call.clone(), false).await.unwrap();
        session
            .on_tool_result(
                call,
                appa_runtime_api::ToolOutcome::Success {
                    body: appa_runtime_api::OutcomeBody::Available("marked".to_string()),
                },
            )
            .await
            .unwrap();
        let before = runtime.audit(&target).unwrap();
        assert!(before.len() > 1, "the ordinary target progressed");

        let fork = serde_json::json!({
            "protocol": PROTOCOL,
            "adapter": adapter.name,
            "operation": "fork",
            "checkpoint_id": created["checkpoint_id"],
            "root_id": "ordinary-target",
        });
        let (status, response) = answer(&runtime, adapter, &serde_json::to_vec(&fork).unwrap());
        assert_eq!(status, 409, "{response}");
        assert_eq!(
            runtime.audit(&target).unwrap(),
            before,
            "the ordinary target is unchanged"
        );
    }
}
