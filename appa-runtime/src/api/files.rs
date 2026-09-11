//! Opt-in runtime-owned Read/Write/Edit mediation. The host owns trajectory identity.
//!
//! # Experimental: not a supported security boundary
//!
//! Native file tools are not supported. Runtime-owned tools check the pinned call before
//! searching Edit's match string or returning file bytes. The `claude-files` launcher removes
//! native tools and implicit project discovery; its private stdio server binds every call
//! to a host-assigned trajectory, including when Claude reconnects to that server.
//! Inference requests and final responses are not mediated by this launcher. These file
//! Labels therefore do not establish complete provenance or audience confinement.
//!
//! The trusted host owns the workspace exclusively and keeps the ledger, runtime state,
//! configuration, credentials and execution controls outside it. Relative tool paths
//! resolve from that workspace. The host classifies initial files and supplies the prompt;
//! neither source Labels nor trajectory identifiers are model arguments.
//!
//! For calls reaching the runtime, the ledger reserves the workspace and pins the current version,
//! digest and Label. Hashes verify bytes; they never classify them. The engine checks the
//! pinned call, including any narrowing acceptance, and persists its basis on the dispatch.
//! Read results combine the source Label with the tool delta. Write publishes the receiving
//! trajectory Label combined with delta. Edit additionally combines the predecessor Label.
//! Reported acknowledgements and errors include the predecessor even for Write.
//! The runtime does not let the model select a clean trajectory or supply a source Label.
//!
//! The runtime stages replacement bytes beside the target and atomically replaces it.
//! Success validates bytes and publishes immutable version metadata before admitting the
//! result. No file-derived result reaches MCP before admission. An unchanged failure admits
//! its error text but publishes no version. A changed failure, missing outcome,
//! or unmatched digest leaves the durable reservation in place, including across restarts.
//! Further file calls stop. Recovery requires operator reconciliation; there is no reset API.
//!
//! # Enabling the draft
//!
//! Start `appa runtime` with `--file-workspace /absolute/workspace` and
//! `--file-ledger /protected/files.db`. On the first start only, also supply
//! `--initialize-file-trust suspicious --initialize-file-audience public` (or the operator's
//! actual classification). This classifies every existing file; it does not inspect content.
//! Subsequent starts require that same ledger and policy and omit initialization flags.
//! Launch `appa claude-files --runtime-url http://127.0.0.1:8787 'prompt'`.
//! The policy must declare the three `mcp/plugin_appa-runtime_appa/appa_*_file` tools.
//! This option does not enforce OS isolation. Use disposable test fixtures only.
//!
//! # Limitations
//!
//! The constrained launcher exposes only file tools and the remedy control tool. Bash, other
//! MCP tools, subagents, rename/delete, links and known execution-control paths are unsupported.
//! Sanitizer/rewrite policies are unsupported. The two databases are not one atomic transaction:
//! crash gaps stop progress conservatively and have no automatic recovery. Historical bytes are not retained.
//! No subprocess, unmediated filesystem, metadata or timing-flow guarantee is made.

use appa_engine::value::{FileBasis, FileSource};
use appa_eventlog::files::{FileOperation, FilePin, FileStore};
use std::io::Write;
use std::path::{Path, PathBuf};

use super::{EventError, ProposedCall};

pub(super) struct FileTracking {
    pub store: FileStore,
    pub policy_key: String,
    pub workspace: PathBuf,
    pub ledger: PathBuf,
}

pub(crate) const TOOLS: [&str; 3] = ["appa_read_file", "appa_write_file", "appa_edit_file"];
const PREFIX: &str = "mcp/plugin_appa-runtime_appa/";

#[derive(Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadArgs {
    pub file_path: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct WriteArgs {
    pub file_path: String,
    pub content: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct EditArgs {
    pub file_path: String,
    pub old_string: String,
    pub new_string: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FileReply {
    Value(String),
    Failure(String),
}

pub(crate) fn owns(call: &ProposedCall) -> bool {
    call.tool.strip_prefix(PREFIX).is_some_and(|name| TOOLS.contains(&name))
}

pub(super) fn operation(call: &ProposedCall) -> Result<(FileOperation, String), EventError> {
    let operation = match call.tool.as_str() {
        "mcp/plugin_appa-runtime_appa/appa_read_file" => FileOperation::Read,
        "mcp/plugin_appa-runtime_appa/appa_write_file" => FileOperation::Replace,
        "mcp/plugin_appa-runtime_appa/appa_edit_file" => FileOperation::Edit,
        _ => {
            return Err(refused(
                "file tracking permits only runtime-owned appa_read_file, appa_write_file and appa_edit_file",
            ));
        }
    };
    // Validate only argument shape here. In particular, never search old_string before
    // the engine has accepted observation of the predecessor's Label.
    let path = match operation {
        FileOperation::Read => serde_json::from_str::<ReadArgs>(call.arguments.get()).map(|args| args.file_path),
        FileOperation::Replace => serde_json::from_str::<WriteArgs>(call.arguments.get()).map(|args| args.file_path),
        FileOperation::Edit => serde_json::from_str::<EditArgs>(call.arguments.get()).map(|args| args.file_path),
    }
    .map_err(refused)?;
    Ok((operation, path))
}

/// Called only after the exact dispatch has been released for the host-bound caller.
pub(super) fn perform(workspace: &Path, call: &ProposedCall) -> Result<String, String> {
    let (operation, path) = operation(call).map_err(|error| error.to_string())?;
    let path = workspace.join(path);
    match operation {
        FileOperation::Read => std::fs::read_to_string(path).map_err(|error| error.to_string()),
        FileOperation::Replace => {
            let args: WriteArgs = serde_json::from_str(call.arguments.get()).map_err(|error| error.to_string())?;
            replace(&path, &args.content)
                .map(|()| "file written".into())
                .map_err(|error| error.to_string())
        }
        FileOperation::Edit => {
            let args: EditArgs = serde_json::from_str(call.arguments.get()).map_err(|error| error.to_string())?;
            if args.old_string.is_empty() {
                return Err("old_string must not be empty".into());
            }
            let content = std::fs::read_to_string(&path).map_err(|error| error.to_string())?;
            if content.matches(&args.old_string).count() != 1 {
                return Err("old_string must match exactly once".into());
            }
            replace(&path, &content.replacen(&args.old_string, &args.new_string, 1))
                .map(|()| "file edited".into())
                .map_err(|error| error.to_string())
        }
    }
}

fn replace(path: &Path, content: &str) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("missing parent directory"))?;
    std::fs::create_dir_all(parent)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(content.as_bytes())?;
    staged.as_file().sync_all()?;
    staged.persist(path).map_err(|error| error.error)?;
    Ok(())
}

impl super::Runtime {
    pub(crate) fn file_tracking_enabled(&self) -> bool {
        self.inner.files.is_some()
    }

    pub(crate) fn file_deployment(&self, config: PathBuf) -> Option<crate::claude_files::Deployment> {
        let files = self.inner.files.as_ref()?;
        Some(crate::claude_files::Deployment {
            config: std::fs::canonicalize(config).ok()?,
            db: std::fs::canonicalize(self.inner.state_path.as_ref()?).ok()?,
            workspace: files.workspace.clone(),
            ledger: std::fs::canonicalize(&files.ledger).ok()?,
        })
    }

    pub(crate) async fn execute_bound_file(
        &self,
        actor: &appa_runtime_api::Actor,
        tool: &str,
        arguments: serde_json::Value,
    ) -> Result<FileReply, EventError> {
        let call = ProposedCall {
            tool: format!("{PREFIX}{tool}"),
            arguments: serde_json::value::to_raw_value(&arguments).map_err(refused)?,
        };
        let session = self.session(&actor.root, super::acting_trajectory(actor))?;
        match session.on_tool_call(call.clone(), false).await? {
            super::ToolCallDecision::Deny { feedback, .. } => Ok(FileReply::Failure(feedback)),
            super::ToolCallDecision::Allow { .. } => session.execute_file(call).await,
        }
    }

    pub(crate) async fn execute_file(&self, tool: &str, arguments: serde_json::Value) -> Result<FileReply, EventError> {
        let (actor, _) = self
            .take_vouched(&super::PermitKey::call(tool, &arguments))
            .map_err(|_| refused("no unique one-shot host vouch for this file call"))?;
        let call = ProposedCall {
            tool: format!("{PREFIX}{tool}"),
            arguments: serde_json::value::to_raw_value(&arguments).map_err(refused)?,
        };
        self.session(&actor.root, super::acting_trajectory(&actor))?
            .execute_file(call)
            .await
    }
}

pub(super) fn basis(pin: FilePin) -> Result<FileBasis, EventError> {
    let source = match (pin.predecessor_version, pin.predecessor_digest, pin.predecessor_label) {
        (Some(version), Some(digest), Some(label)) => Some(FileSource {
            version: version.to_string(),
            digest,
            label,
        }),
        (None, None, None) => None,
        _ => return Err(refused("incomplete file ledger pin")),
    };
    Ok(match pin.operation {
        FileOperation::Read => FileBasis::Read(source.ok_or_else(|| refused("missing read source"))?),
        FileOperation::Edit => FileBasis::Edit(source.ok_or_else(|| refused("missing edit source"))?),
        FileOperation::Replace => FileBasis::Replace(source),
    })
}

pub(super) fn refused(message: impl ToString) -> EventError {
    EventError::RemedyArguments {
        detail: format!("file tracking: {}", message.to_string()),
    }
}

pub(super) fn key(dispatch: &appa_engine::value::DispatchId) -> Result<String, EventError> {
    serde_json::to_string(dispatch).map_err(refused)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::api::{OutcomeBody, RemedyDecision, Runtime, ToolCallDecision, ToolOutcome, TrajectoryId};
    use crate::config::Config;
    use crate::engine::RemedyArguments;
    use appa_engine::label::{Audience, Label, Trust};
    use std::path::Path;

    fn open(dir: &Path, initialize: bool) -> Runtime {
        let config = dir.join("policy.toml");
        std::fs::write(
            &config,
            r#"
[policy]
version = 2
[[policy.tool]]
name = "mcp/plugin_appa-runtime_appa/appa_read_file"
delta = {}
[[policy.tool]]
name = "mcp/plugin_appa-runtime_appa/appa_write_file"
delta = {}
[[policy.tool]]
name = "mcp/plugin_appa-runtime_appa/appa_edit_file"
delta = {}
[externals]
timeout_ms = 2000
max_body_bytes = 65536
"#,
        )
        .unwrap();
        Runtime::open_served(
            Config::load(&config).unwrap(),
            dir.join("runtime.db"),
            None,
            appa_adapter_claude_code::adapter(),
        )
        .unwrap()
        .with_file_tracking(
            dir.join("work"),
            dir.join("files.db"),
            initialize.then(|| Label::new(Trust::new(0), Audience::public())),
        )
        .unwrap()
    }

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("work")).unwrap();
        std::fs::write(dir.path().join("work/source.txt"), "outside information").unwrap();
        dir
    }

    fn call(tool: &str, path: &str) -> ProposedCall {
        let (name, arguments) = match tool {
            "Read" => ("appa_read_file", serde_json::json!({"file_path": path})),
            "Write" => (
                "appa_write_file",
                serde_json::json!({"file_path": path, "content": "fixture"}),
            ),
            "Edit" => (
                "appa_edit_file",
                serde_json::json!({"file_path": path, "old_string": "outside", "new_string": "inside"}),
            ),
            _ => (tool, serde_json::json!({"file_path": path})),
        };
        ProposedCall {
            tool: format!("{PREFIX}{name}"),
            arguments: super::super::session::raw(arguments),
        }
    }

    async fn execute(runtime: &Runtime, id: &TrajectoryId, call: ProposedCall) -> FileReply {
        let actor = appa_runtime_api::Actor {
            root: id.clone(),
            child: None,
        };
        runtime.vouch(&super::super::call_key(&call).unwrap(), &actor, None);
        runtime
            .execute_file(
                call.tool.strip_prefix(PREFIX).unwrap(),
                serde_json::from_str(call.arguments.get()).unwrap(),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn managed_files_bound_caller_retains_failure_taint_after_reopen() {
        let dir = fixture();
        let runtime = open(dir.path(), true);
        let actor = appa_runtime_api::Actor {
            root: TrajectoryId("host-bound-stdio".into()),
            child: None,
        };
        runtime.create_session(actor.root.clone()).unwrap();
        let arguments = serde_json::json!({
            "file_path": "source.txt", "old_string": "absent", "new_string": "replacement"
        });
        let blocked = runtime
            .execute_bound_file(&actor, "appa_edit_file", arguments.clone())
            .await
            .unwrap();
        assert!(matches!(blocked, FileReply::Failure(message) if message.contains("trusted -> suspicious")));
        let proposal = ProposedCall {
            tool: format!("{PREFIX}appa_edit_file"),
            arguments: super::super::session::raw(arguments.clone()),
        };
        allow(&runtime, &actor.root, proposal.clone()).await;
        // The approval above releases the call; execute exactly that dispatch as MCP does.
        assert_eq!(
            execute(&runtime, &actor.root, proposal).await,
            FileReply::Failure("old_string must match exactly once".into())
        );
        drop(runtime);
        let runtime = open(dir.path(), false);
        assert!(matches!(
            runtime.create_session(actor.root.clone()),
            Err(EventError::TrajectoryExists)
        ));
        assert_eq!(
            runtime
                .execute_bound_file(
                    &actor,
                    "appa_write_file",
                    serde_json::json!({"file_path":"after-reconnect.txt", "content":"the string was absent"})
                )
                .await
                .unwrap(),
            FileReply::Value("file written".into())
        );
        assert_eq!(label(&runtime, "after-reconnect.txt").trust, Trust::new(0));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("work/after-reconnect.txt")).unwrap(),
            "the string was absent"
        );
        assert!(
            runtime
                .execute_bound_file(
                    &actor,
                    "appa_write_file",
                    serde_json::json!({"file_path":"forged.txt", "content":"x", "trajectory":"clean"})
                )
                .await
                .is_err()
        );
        assert!(!dir.path().join("work/forged.txt").exists());
    }

    #[tokio::test]
    async fn managed_files_owned_execution_checks_before_matching_and_admits_errors() {
        let dir = fixture();
        let runtime = open(dir.path(), true);
        for (index, old) in ["absent substring", "outside"].into_iter().enumerate() {
            let id = TrajectoryId(format!("host-probe-{index}"));
            let session = runtime.create_session(id.clone()).unwrap();
            let proposal = ProposedCall {
                tool: format!("{PREFIX}appa_edit_file"),
                arguments: super::super::session::raw(
                    serde_json::json!({"file_path":"source.txt", "old_string":old, "new_string":"replacement"}),
                ),
            };
            assert!(matches!(
                session.on_tool_call(proposal.clone(), false).await.unwrap(),
                ToolCallDecision::Deny { .. }
            ));
            assert!(
                runtime
                    .execute_file(
                        "appa_edit_file",
                        serde_json::from_str(proposal.arguments.get()).unwrap()
                    )
                    .await
                    .is_err()
            );
            assert_eq!(
                std::fs::read_to_string(dir.path().join("work/source.txt")).unwrap(),
                "outside information"
            );
            allow(&runtime, &id, proposal.clone()).await;
            let reply = execute(&runtime, &id, proposal).await;
            if index == 0 {
                assert_eq!(reply, FileReply::Failure("old_string must match exactly once".into()));
            } else {
                assert_eq!(reply, FileReply::Value("file edited".into()));
            }
            let output = call("Write", &format!("result-{index}.txt"));
            allow(&runtime, &id, output.clone()).await;
            assert_eq!(
                execute(&runtime, &id, output.clone()).await,
                FileReply::Value("file written".into())
            );
            assert_eq!(label(&runtime, &format!("result-{index}.txt")).trust, Trust::new(0));
            assert!(
                runtime
                    .execute_file("appa_write_file", serde_json::from_str(output.arguments.get()).unwrap())
                    .await
                    .is_err()
            );
        }
        assert_eq!(
            std::fs::read_to_string(dir.path().join("work/source.txt")).unwrap(),
            "replacement information"
        );
    }

    async fn allow(runtime: &Runtime, id: &TrajectoryId, call: ProposedCall) {
        let session = runtime.session(id, id).unwrap();
        let decision = session.on_tool_call(call.clone(), false).await.unwrap();
        if let ToolCallDecision::Deny { offers, .. } = decision {
            assert!(!offers.is_empty(), "narrowing must offer acceptance");
            let offer = runtime
                .resolve_in(id, &crate::api::OfferId(offers[0].id.clone()))
                .unwrap()
                .0;
            assert!(matches!(
                session
                    .on_remedy(offer, RemedyArguments::default(), None, None)
                    .await
                    .unwrap(),
                RemedyDecision::Authorized { .. }
            ));
            assert!(matches!(
                session.on_tool_call(call, false).await.unwrap(),
                ToolCallDecision::Allow { .. }
            ));
        } else {
            assert!(matches!(decision, ToolCallDecision::Allow { .. }));
        }
    }

    async fn success(runtime: &Runtime, id: &TrajectoryId, call: ProposedCall) {
        runtime
            .session(id, id)
            .unwrap()
            .on_tool_result(
                call,
                ToolOutcome::Success {
                    body: OutcomeBody::Available("native output".into()),
                },
            )
            .await
            .unwrap();
    }

    fn label(runtime: &Runtime, path: &str) -> Label {
        runtime
            .inner
            .files
            .as_ref()
            .unwrap()
            .store
            .current(path)
            .unwrap()
            .unwrap()
            .label
    }

    #[tokio::test]
    async fn managed_files_read_write_edit_and_restart_use_engine_labels() {
        let dir = fixture();
        let runtime = open(dir.path(), true);
        let id = TrajectoryId("host-owned-session".into());
        runtime.create_session(id.clone()).unwrap();
        allow(&runtime, &id, call("Write", "clean.txt")).await;
        std::fs::write(dir.path().join("work/clean.txt"), "independent text").unwrap();
        success(&runtime, &id, call("Write", "clean.txt")).await;
        assert_eq!(label(&runtime, "clean.txt").trust, Trust::new(1));

        allow(&runtime, &id, call("Read", "source.txt")).await;
        success(&runtime, &id, call("Read", "source.txt")).await;
        allow(&runtime, &id, call("Write", "derived.txt")).await;
        std::fs::write(dir.path().join("work/derived.txt"), "derived from outside information").unwrap();
        success(&runtime, &id, call("Write", "derived.txt")).await;
        assert_eq!(label(&runtime, "derived.txt").trust, Trust::new(0));
        drop(runtime);

        let runtime = open(dir.path(), false);
        allow(&runtime, &id, call("Edit", "clean.txt")).await;
        std::fs::write(
            dir.path().join("work/clean.txt"),
            "independent text plus outside information",
        )
        .unwrap();
        success(&runtime, &id, call("Edit", "clean.txt")).await;
        assert_eq!(label(&runtime, "clean.txt").trust, Trust::new(0));
        let version = runtime
            .inner
            .files
            .as_ref()
            .unwrap()
            .store
            .current("clean.txt")
            .unwrap()
            .unwrap();
        assert_eq!(version.previous, version.edit_dependency);
        assert!(version.edit_dependency.is_some());
    }

    #[tokio::test]
    async fn managed_files_failures_admit_observations_and_partial_writes_quarantine() {
        let dir = fixture();
        let runtime = open(dir.path(), true);
        let id = TrajectoryId("host-owned-session".into());
        runtime.create_session(id.clone()).unwrap();
        allow(&runtime, &id, call("Edit", "source.txt")).await;
        runtime
            .session(&id, &id)
            .unwrap()
            .on_tool_result(
                call("Edit", "source.txt"),
                ToolOutcome::Failure {
                    message: "could not match outside information".into(),
                },
            )
            .await
            .unwrap();
        allow(&runtime, &id, call("Write", "after-error.txt")).await;
        std::fs::write(dir.path().join("work/after-error.txt"), "error-derived text").unwrap();
        success(&runtime, &id, call("Write", "after-error.txt")).await;
        assert_eq!(label(&runtime, "after-error.txt").trust, Trust::new(0));
        allow(&runtime, &id, call("Edit", "source.txt")).await;
        std::fs::write(dir.path().join("work/source.txt"), "partial write").unwrap();
        assert!(
            runtime
                .session(&id, &id)
                .unwrap()
                .on_tool_result(
                    call("Edit", "source.txt"),
                    ToolOutcome::Failure {
                        message: "disk error".into(),
                    }
                )
                .await
                .unwrap_err()
                .to_string()
                .contains("quarantined")
        );
        drop(runtime);
        let runtime = open(dir.path(), false);
        assert!(
            runtime
                .session(&id, &id)
                .unwrap()
                .on_tool_call(call("Read", "after-error.txt"), false)
                .await
                .unwrap_err()
                .to_string()
                .contains("pending")
        );
    }

    #[tokio::test]
    async fn managed_files_bypasses_and_missing_outcomes_fail_closed() {
        let dir = fixture();
        let runtime = open(dir.path(), true);
        let id = TrajectoryId("host-owned-session".into());
        let session = runtime.create_session(id.clone()).unwrap();
        for proposal in [
            call("Bash", "source.txt"),
            call("Read", "../policy.toml"),
            call("Write", ".claude/settings.json"),
        ] {
            assert!(session.on_tool_call(proposal, false).await.is_err());
        }
        allow(&runtime, &id, call("Write", "unreported.txt")).await;
        std::fs::write(dir.path().join("work/unreported.txt"), "outcome lost").unwrap();
        session.on_turn_end().await.unwrap();
        assert!(
            session
                .on_tool_call(call("Read", "source.txt"), false)
                .await
                .unwrap_err()
                .to_string()
                .contains("pending")
        );
    }
}
