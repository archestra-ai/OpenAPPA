//! Opt-in native Claude Code Read/Write/Edit mediation. The host owns trajectory identity.
//!
//! # Experimental: not a supported security boundary
//!
//! A live Claude Code 2.1.268 probe showed native Edit returning a content-dependent error
//! before PreToolUse. APPA receives no proposal or dispatch on that path. The observation
//! can influence later output without narrowing the trajectory Label. Read/Write
//! prevalidation coverage is unverified. Use disposable test fixtures only; these Labels
//! are not complete provenance. Rejecting Edit at PreToolUse does not close this gap.
//!
//! The trusted host starts the agent in the configured, exclusively owned workspace. It
//! installs hooks and keeps the ledger, runtime state, configuration, plugins,
//! credentials and other execution controls outside that workspace. Relative tool paths
//! resolve from that workspace. Implicit harness reads (instructions, memory, attachments)
//! are trusted ingress supplied by the host, not observations intercepted by this API.
//!
//! For calls reaching the runtime, the ledger reserves the workspace and pins the current version,
//! digest and Label. Hashes verify bytes; they never classify them. The engine checks the
//! pinned call, including any narrowing acceptance, and persists its basis on the dispatch.
//! Read results combine the source Label with the tool delta. Write publishes the receiving
//! trajectory Label combined with delta. Edit additionally combines the predecessor Label.
//! Reported acknowledgements and errors include the predecessor even for Write.
//! The runtime does not let the model select a clean trajectory or supply a source Label.
//!
//! The host executes the native tool, then reports success or failure. Success validates the
//! bytes and publishes immutable version metadata before admitting the result. An unchanged
//! failure admits its error text but publishes no version. A changed failure, missing outcome,
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
//! Point the installed Claude Code plugin at this runtime with `APPA_RUNTIME_URL`, and launch
//! the experimental session with `APPA_GATE=1`. This option does not enforce OS isolation.
//!
//! # Limitations
//!
//! At the hook boundary, only Read/Write/Edit and the remedy control tool are allowed. Bash, other
//! MCP tools, subagents, rename/delete, links and writes to known execution-control paths are
//! refused. Sanitizer/rewrite policies are unsupported. The native host, not this module,
//! owns write atomicity. The two databases are not one atomic transaction: crash gaps stop
//! progress conservatively and have no automatic recovery. Historical bytes are not retained.
//! No subprocess, unmediated filesystem, metadata or timing-flow guarantee is made.

use appa_engine::value::{FileBasis, FileSource};
use appa_eventlog::files::{FileOperation, FilePin, FileStore};

use super::{EventError, ProposedCall};

pub(super) struct FileTracking {
    pub store: FileStore,
    pub policy_key: String,
}

pub(super) fn operation(call: &ProposedCall) -> Result<(FileOperation, String), EventError> {
    let operation = match call.tool.as_str() {
        "host/claude-code/Read" => FileOperation::Read,
        "host/claude-code/Write" => FileOperation::Replace,
        "host/claude-code/Edit" => FileOperation::Edit,
        _ => {
            return Err(refused(
                "file tracking permits only Claude Code Read, Write and Edit; other tools are not mediated",
            ));
        }
    };
    let arguments: serde_json::Value =
        serde_json::from_str(call.arguments.get()).map_err(|_| refused("invalid file arguments"))?;
    let path = arguments
        .get("file_path")
        .and_then(|value| value.as_str())
        .ok_or_else(|| refused("file_path must be a string"))?;
    Ok((operation, path.to_owned()))
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
name = "Read"
delta = {}
[[policy.tool]]
name = "Write"
delta = {}
[[policy.tool]]
name = "Edit"
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
        ProposedCall {
            tool: format!("host/claude-code/{tool}"),
            arguments: super::super::session::raw(serde_json::json!({"file_path": path})),
        }
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
