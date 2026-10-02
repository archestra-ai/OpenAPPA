//! xmemory battery: internal reads that keep their trust, trusted writes, admin and
//! schema changes, reviewed schema migrations and instance deletion.
mod common;

use appa_runtime::api::{AuditEvent, RemedyOutcome, Runtime};
use appa_runtime_api::{HookDecision, ProposedCall};
use common::{actor, battery_runtime, members_source, offer_of, propose, ran, raw, root};
use std::sync::Arc;

fn call(tool: &str, args: serde_json::Value) -> ProposedCall {
    ProposedCall {
        tool: format!("mcp/xmemory/{tool}"),
        arguments: raw(args),
        cwd: None,
    }
}

fn structured_write(tool: &str) -> ProposedCall {
    call(
        tool,
        serde_json::json!({
            "text": "",
            "structured_mutations": [{
                "object_mutation": { "object_type": "Person", "delete": { "key": { "name": "Bob" } } }
            }],
        }),
    )
}

/// Runs `call`, accepting the label change it offers when it first narrows the trajectory.
/// The offer must not ask a reviewer: this helper runs only what needs no review.
async fn accept_and_run(runtime: &Arc<Runtime>, call: ProposedCall) {
    let decision = propose(runtime, call.clone()).await;
    if let HookDecision::DenyCall { feedback, .. } = &decision {
        assert!(!feedback.contains("approval"), "{}: {feedback}", call.tool);
        assert!(matches!(
            runtime.execute_remedy(&actor(), offer_of(&decision)).await,
            RemedyOutcome::Authorized { .. }
        ));
        assert_eq!(
            propose(runtime, call.clone()).await,
            HookDecision::AllowCall { spawn: None },
            "{}",
            call.tool
        );
    }
    ran(runtime, call).await;
}

async fn runtime(dir: &tempfile::TempDir) -> Arc<Runtime> {
    let source = members_source().await;
    battery_runtime(
        dir.path(),
        "xmemory",
        &format!(
            r#"[policy]
version = 2

[policy.audience]
internal = ["people:members"]

[[policy.tool]]
name = "mcp/web/fetch"
delta = {{ trust = "suspicious" }}

[[policy.authority]]
name = "xmemory-operator"
permits = {{ trust_below = "trusted", attention = ["xmemory-review"] }}

[externals]
timeout_ms = 30000
review_timeout_ms = 600000
max_body_bytes = 1048576

[externals.authorities.xmemory-operator]
builtin = "approve"

[externals.audience.people]
url = "{source}"
selectors = [{{ template = "members", feeds = "internal" }}]
"#
        ),
    )
    .await
}

/// A read keeps the trajectory's trust, since memory is written only from trusted data:
/// writes, schema decisions, and metadata changes run after it. Once outside text lowers
/// the trust, none of them runs.
#[tokio::test]
async fn a_read_keeps_trust_and_suspicious_data_cannot_enter_memory() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;

    accept_and_run(
        &runtime,
        call("read", serde_json::json!({ "query": "Who owns the Q3 plan?" })),
    )
    .await;

    let changes = || {
        [
            call("write", serde_json::json!({ "text": "Bob reviews the Q3 plan." })),
            call("write_async", serde_json::json!({ "text": "Bob reviews the Q3 plan." })),
            structured_write("write"),
            structured_write("write_async"),
            call(
                "decide_suggestions",
                serde_json::json!({ "proposal_version": "v1", "decisions": [] }),
            ),
            call(
                "admin_patch_instance_metadata_by_id",
                serde_json::json!({ "instance_id": "1", "agent_owner_instructions": "Review weekly." }),
            ),
        ]
    };
    for change in changes() {
        assert_eq!(
            propose(&runtime, change.clone()).await,
            HookDecision::AllowCall { spawn: None },
            "{}",
            change.tool
        );
        ran(&runtime, change).await;
    }

    accept_and_run(
        &runtime,
        ProposedCall {
            tool: "mcp/web/fetch".into(),
            arguments: raw(serde_json::json!({ "url": "https://example.com" })),
            cwd: None,
        },
    )
    .await;
    for change in changes() {
        assert!(
            !matches!(propose(&runtime, change.clone()).await, HookDecision::AllowCall { .. }),
            "{}",
            change.tool
        );
    }
}

/// From trusted data, a schema decision runs without review and keeps the trajectory
/// trusted; writes and creating an instance run and record their effects; a schema
/// migration and an instance deletion wait for the reviewer.
#[tokio::test]
async fn writes_record_their_effects_and_admin_and_schema_changes_ask_the_authority() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;

    accept_and_run(
        &runtime,
        call(
            "decide_suggestions",
            serde_json::json!({ "proposal_version": "v1", "decisions": [] }),
        ),
    )
    .await;
    accept_and_run(&runtime, structured_write("write")).await;
    accept_and_run(
        &runtime,
        call("write", serde_json::json!({ "text": "Alice owns the Q3 plan." })),
    )
    .await;
    accept_and_run(
        &runtime,
        call(
            "admin_create_instance",
            serde_json::json!({
                "cluster_id": "c1",
                "name": "team-memory",
                "schema_yaml": "xmd_version: v1\nobjects:\n  Person:\n    fields:\n      name:\n        type: str\n        required: true\n    primary_key:\n    - name\nrelations: {}\n",
            }),
        ),
    )
    .await;

    for reviewed in [
        call(
            "update_instance_schema",
            serde_json::json!({ "schema_yml": "objects: {}" }),
        ),
        call("admin_delete_instance_by_id", serde_json::json!({ "instance_id": "1" })),
    ] {
        let decision = propose(&runtime, reviewed.clone()).await;
        assert!(!matches!(decision, HookDecision::AllowCall { .. }), "{}", reviewed.tool);
        assert!(matches!(
            runtime.execute_remedy(&actor(), offer_of(&decision)).await,
            RemedyOutcome::Authorized { .. }
        ));
        assert_eq!(
            propose(&runtime, reviewed.clone()).await,
            HookDecision::AllowCall { spawn: None }
        );
        ran(&runtime, reviewed).await;
    }

    let effects: Vec<_> = runtime
        .audit(&root())
        .unwrap()
        .into_iter()
        .filter_map(|entry| match entry.event {
            AuditEvent::Released { tool, effects, .. } if tool.starts_with("mcp/xmemory/") => Some(effects),
            _ => None,
        })
        .collect();
    assert_eq!(
        effects,
        vec![
            vec!["xmemory.changed"],
            vec!["xmemory.changed"],
            vec!["xmemory.changed"],
            vec!["xmemory.admin"],
            vec!["xmemory.schema"],
            vec!["xmemory.deleted"]
        ]
    );
}
