//! Opt-in runtime-owned Read/Write/Edit/Copy/Move mediation. The host owns trajectory identity.
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
//! The trusted host owns the workspace exclusively and keeps runtime state, configuration,
//! credentials and execution controls outside it. Relative tool paths
//! resolve from that workspace. The host classifies initial files and supplies the prompt;
//! neither source Labels nor trajectory identifiers are model arguments.
//!
//! For calls reaching the runtime, the ledger reserves the workspace and pins the current version,
//! digest and Label. Hashes verify bytes; they never classify them. The engine checks the
//! pinned call, including any narrowing acceptance, and persists its basis on the dispatch.
//! Read results combine the source Label with the tool delta. Write publishes the receiving
//! trajectory Label combined with delta. Edit additionally combines the predecessor Label.
//! Read/Write/Edit acknowledgements and errors include the predecessor even for Write.
//! Copy/Move publish the source Label combined with the receiving trajectory and delta.
//! Their constant acknowledgements and generic errors combine only the trajectory and delta:
//! the source payload does not enter model context. Destination requirements still check the
//! copied content Label. Replaced content contributes history, not replacement content taint.
//! The runtime does not let the model select a clean trajectory or supply a source Label.
//!
//! The runtime stages replacement bytes beside the target and atomically replaces it.
//! Success validates bytes and publishes immutable version metadata before admitting the
//! result. No file-derived result reaches MCP before admission. An unchanged failure admits
//! its error text but publishes no version. A changed failure, missing outcome,
//! or unmatched digest leaves the workspace's durable reservation in place. Further file
//! calls in that workspace stop. A released call the harness never ran gives its reservation
//! back at the turn end, and only while the workspace still shows the pinned state.
//! Copy/Move pin both paths under one reservation. Copy stages raw bytes; Move uses same-filesystem
//! rename. Success verifies both paths and atomically publishes destination metadata and Move's
//! source absence in the ledger. Failure must leave both files unchanged or remain quarantined.
//! Every operation executes on the path its pin recorded, never on a second reading of the
//! path the call spelled.
//! Process pins every declared input and one destination. It publishes only after isolated
//! execution and descendant teardown. Its output, stdout, stderr and failures combine every
//! input Label with the receiving trajectory and delta; no acknowledgement exemption applies.
//! The isolated command runs under the runner's resource ceilings, and the runtime imports
//! one regular output file of at most 64 MiB.
//!
//! # Enabling the draft
//!
//! Add `[file_tracking]` with `initial_trust` and `initial_audience` to the APPA
//! configuration. The table's presence enables file tracking. Each root session binds to
//! its first file call's working directory and checks it for links without reading content.
//! Each file gets the initial Label when a call first touches it. Its subagents share that workspace
//! event stream; roots bound to the same canonical workspace share its one reservation.
//! A workspace containing a symlink or hard link is refused, so use a dedicated directory.
//! With the APPA plugin installed, launch Claude with `APPA_GATE=1` and
//! `APPA_RUNTIME_URL` pointing to this runtime. SessionStart describes the file tools.
//! The plugin's HTTP MCP calls consume exact one-shot hook approvals; their outcomes
//! are already admitted when the post-tool hook arrives. Native tools remain visible in Claude.
//! Before root binding, calls use ordinary policy admission. Binding refuses native filesystem
//! and shell tools. Runtime-owned file tools use the workspace event stream, while other
//! policy-named tools continue through ordinary admission.
//! `appa claude-files` is a separate constrained test launcher, not required by the plugin.
//! The policy must declare each enabled tool. File tools alone do not enforce OS isolation.
//! `--file-process-backend /host/backend` additionally enables `appa_process_files`; its
//! staged-input contract is in `process.rs`. Use disposable test fixtures only.
//! # Limitations
//!
//! The constrained launcher exposes only file tools and the remedy control tool. Bash, other
//! MCP tools, subagents, general rename/delete, links and known execution-control writes are unsupported.
//! Copy/Move support regular files only; same-path and cross-filesystem moves are refused.
//! Tool-input sanitizers and rewrite routes are unsupported. Output-only sanitizers remain
//! available unless `confined_results` names a runtime-owned file tool. Workspace events survive
//! a runtime restart; historical bytes are not retained.
//! Only Process calls use the isolated backend. No unmediated filesystem, metadata or
//! timing-flow guarantee is made. The Claude process and inference remain outside isolation.

#[cfg(feature = "daemon")]
use appa_eventlog::files::PinnedBasis;
#[cfg(feature = "daemon")]
use appa_eventlog::files::beneath::{self, Entry};
use appa_eventlog::files::{FileOperation, FileStore};
#[cfg(feature = "daemon")]
use std::path::Path;
use std::path::PathBuf;

use super::{EventError, ProposedCall};

#[cfg(feature = "daemon")]
#[path = "process.rs"]
mod process;

pub(super) struct FileTracking {
    pub(super) initial: appa_engine::label::Label,
    pub policy_key: String,
    pub protected_paths: Vec<PathBuf>,
    pub process_backend: Option<PathBuf>,
}

impl FileTracking {
    /// Bind one root trajectory to the harness working directory reported on its first file
    /// call. Child trajectories use their parent's root, so they resolve to this same store.
    /// A later call cannot move the root to another workspace.
    pub(super) fn bind(
        &self,
        authority: &std::sync::Arc<appa_eventlog::LogStore>,
        root: &super::TrajectoryId,
        workspace: &str,
    ) -> Result<std::sync::Arc<FileStore>, appa_eventlog::files::FileStoreError> {
        let workspace = std::fs::canonicalize(workspace)?;
        if self.protected_paths.iter().any(|path| path.starts_with(&workspace))
            || self
                .process_backend
                .as_ref()
                .is_some_and(|path| path.starts_with(&workspace))
        {
            return Err(appa_eventlog::files::FileStoreError::Configuration(
                "runtime state, configuration, and process backends must be outside the tracked workspace".into(),
            ));
        }
        let store = std::sync::Arc::new(FileStore::open(
            std::sync::Arc::clone(authority),
            &workspace,
            &self.policy_key,
            &self.initial,
        )?);
        store.bind_root(root)?;
        Ok(store)
    }

    pub(super) fn store(
        &self,
        authority: &std::sync::Arc<appa_eventlog::LogStore>,
        root: &super::TrajectoryId,
    ) -> Result<std::sync::Arc<FileStore>, appa_eventlog::files::FileStoreError> {
        Ok(std::sync::Arc::new(FileStore::for_root(
            std::sync::Arc::clone(authority),
            root,
            &self.policy_key,
            &self.initial,
        )?))
    }

    pub(super) fn root_is_bound(
        &self,
        authority: &appa_eventlog::LogStore,
        root: &super::TrajectoryId,
    ) -> Result<bool, appa_eventlog::files::FileStoreError> {
        FileStore::root_is_bound(authority, root)
    }
}

/// The runtime-owned file tools, each served under its own MCP name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileTool {
    Read,
    Write,
    Edit,
    Copy,
    Move,
    Process,
}

impl FileTool {
    pub(crate) const ALL: [FileTool; 6] = [
        FileTool::Read,
        FileTool::Write,
        FileTool::Edit,
        FileTool::Copy,
        FileTool::Move,
        FileTool::Process,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            FileTool::Read => "appa_read_file",
            FileTool::Write => "appa_write_file",
            FileTool::Edit => "appa_edit_file",
            FileTool::Copy => "appa_copy_file",
            FileTool::Move => "appa_move_file",
            FileTool::Process => "appa_process_files",
        }
    }

    fn operation(self) -> FileOperation {
        match self {
            FileTool::Read => FileOperation::Read,
            FileTool::Write => FileOperation::Replace,
            FileTool::Edit => FileOperation::Edit,
            FileTool::Copy => FileOperation::Copy,
            FileTool::Move => FileOperation::Move,
            FileTool::Process => FileOperation::Process,
        }
    }

    fn of(call: &ProposedCall) -> Option<FileTool> {
        let name = call.tool.strip_prefix(PREFIX)?;
        FileTool::ALL.into_iter().find(|tool| tool.name() == name)
    }

    #[cfg(feature = "daemon")]
    fn call(self, arguments: &serde_json::Value) -> Result<ProposedCall, EventError> {
        Ok(ProposedCall {
            tool: format!("{PREFIX}{}", self.name()),
            arguments: serde_json::value::to_raw_value(arguments).map_err(refused)?,
            cwd: None,
        })
    }
}

const PREFIX: &str = "mcp/appa/";

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

#[derive(Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileTransferArgs {
    pub source_path: String,
    pub destination_path: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProcessArgs {
    pub input_paths: Vec<String>,
    pub output_path: String,
    pub command: String,
}

#[cfg(feature = "daemon")]
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FileReply {
    Value(String),
    Failure(String),
}

pub(crate) fn owns(call: &ProposedCall) -> bool {
    FileTool::of(call).is_some()
}

/// Claude Code tools that can observe or mutate a bound workspace without using its event
/// stream. Other native and MCP tools continue through ordinary policy admission.
pub(super) fn bypasses_tracking(call: &ProposedCall) -> bool {
    matches!(
        call.tool.as_str(),
        "host/claude-code/Read"
            | "host/claude-code/Write"
            | "host/claude-code/Edit"
            | "host/claude-code/MultiEdit"
            | "host/claude-code/NotebookEdit"
            | "host/claude-code/Grep"
            | "host/claude-code/Glob"
            | "host/claude-code/Bash"
            | "host/claude-code/PowerShell"
            | "host/claude-code/Monitor"
    )
}

pub(super) fn operation(call: &ProposedCall) -> Result<(FileOperation, String), EventError> {
    let operation = FileTool::of(call)
        .ok_or_else(|| refused("file tracking permits only runtime-owned file tools"))?
        .operation();
    // Validate only argument shape here. In particular, never search old_string before
    // the engine has accepted observation of the predecessor's Label.
    let path = match operation {
        FileOperation::Read => serde_json::from_str::<ReadArgs>(call.arguments.get()).map(|args| args.file_path),
        FileOperation::Replace => serde_json::from_str::<WriteArgs>(call.arguments.get()).map(|args| args.file_path),
        FileOperation::Edit => serde_json::from_str::<EditArgs>(call.arguments.get()).map(|args| args.file_path),
        FileOperation::Copy | FileOperation::Move => {
            serde_json::from_str::<FileTransferArgs>(call.arguments.get()).map(|args| args.destination_path)
        }
        FileOperation::Process => {
            serde_json::from_str::<ProcessArgs>(call.arguments.get()).map(|args| args.output_path)
        }
    }
    .map_err(refused)?;
    Ok((operation, path))
}

/// Called only after the exact dispatch has been released for the host-bound caller, on the
/// path the ledger pinned for it. The call's own argument bytes are read for content and
/// commands; every path comes from `pin`, so the file this runs on is the file the ledger
/// validated, hashed and reserved — never a second reading of what the model spelled.
#[cfg(feature = "daemon")]
pub(super) fn perform(
    files: &FileTracking,
    workspace: &Path,
    call: &ProposedCall,
    pin: &appa_eventlog::files::FilePin,
) -> Result<String, String> {
    match &pin.basis {
        PinnedBasis::Process { .. } => process::perform(files, workspace, call, pin),
        PinnedBasis::Read(_) => read(workspace, &pin.path).map_err(|error| error.to_string()),
        PinnedBasis::Replace(_) => {
            let args: WriteArgs = serde_json::from_str(call.arguments.get()).map_err(|error| error.to_string())?;
            replace(workspace, &pin.path, &args.content)
                .map(|()| "file written".into())
                .map_err(|error| error.to_string())
        }
        PinnedBasis::Edit(_) => {
            let args: EditArgs = serde_json::from_str(call.arguments.get()).map_err(|error| error.to_string())?;
            if args.old_string.is_empty() {
                return Err("old_string must not be empty".into());
            }
            let content = read(workspace, &pin.path).map_err(|error| error.to_string())?;
            if content.matches(&args.old_string).count() != 1 {
                return Err("old_string must match exactly once".into());
            }
            replace(
                workspace,
                &pin.path,
                &content.replacen(&args.old_string, &args.new_string, 1),
            )
            .map(|()| "file edited".into())
            .map_err(|error| error.to_string())
        }
        PinnedBasis::Copy { source, .. } | PinnedBasis::Move { source, .. } => {
            let result = (|| -> std::io::Result<()> {
                let destination = Entry::create(workspace, &pin.path)?;
                if matches!(pin.basis, PinnedBasis::Move { .. }) {
                    Entry::locate(workspace, &source.path)?
                        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?
                        .rename_to(&destination)
                } else {
                    destination.publish(&mut existing(workspace, &source.path)?)
                }
            })();
            // No source-derived error body is admitted at the acknowledgement Label.
            result
                .map(|()| "file transfer completed".into())
                .map_err(|_| "file transfer failed".into())
        }
    }
}

#[cfg(feature = "daemon")]
fn replace(workspace: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    Entry::create(workspace, relative)?.publish(&mut content.as_bytes())
}

#[cfg(feature = "daemon")]
fn existing(workspace: &Path, relative: &str) -> std::io::Result<std::fs::File> {
    beneath::open(workspace, relative)?.ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))
}

#[cfg(feature = "daemon")]
fn read(workspace: &Path, relative: &str) -> std::io::Result<String> {
    std::io::read_to_string(existing(workspace, relative)?)
}

/// Where one constrained file launcher's runtime lives: the paths a private
/// stdio server is started against, and what the runtime answers `/file-tools`
/// with.
#[cfg(feature = "daemon")]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct Deployment {
    pub config: PathBuf,
    pub db: PathBuf,
    pub initial: appa_engine::label::Label,
    pub process_backend: Option<PathBuf>,
}

impl super::Runtime {
    pub(crate) fn file_tracking_enabled(&self) -> bool {
        self.inner.shared.files.is_some()
    }

    pub(crate) fn file_process_enabled(&self) -> bool {
        self.inner
            .shared
            .files
            .as_ref()
            .is_some_and(|files| files.process_backend.is_some())
    }

    #[cfg(feature = "daemon")]
    pub(crate) fn file_deployment(&self, config: PathBuf) -> Option<Deployment> {
        let files = self.inner.shared.files.as_ref()?;
        Some(Deployment {
            config: std::fs::canonicalize(config).ok()?,
            db: std::fs::canonicalize(self.inner.shared.state_path.as_ref()?).ok()?,
            initial: files.initial.clone(),
            process_backend: files.process_backend.clone(),
        })
    }

    #[cfg(feature = "daemon")]
    pub(crate) async fn execute_bound_file(
        &self,
        actor: &appa_runtime_api::Actor,
        tool: FileTool,
        arguments: serde_json::Value,
    ) -> Result<FileReply, EventError> {
        let call = tool.call(&arguments)?;
        let session = self.session(&actor.root, super::acting_trajectory(actor))?;
        match session.on_tool_call_identified(call.clone(), None, None, None).await? {
            super::ToolCallDecision::Deny { feedback, .. } => Ok(FileReply::Failure(feedback)),
            super::ToolCallDecision::Allow { .. } => session.execute_file(call).await,
        }
    }

    #[cfg(feature = "daemon")]
    pub(crate) async fn execute_file(
        &self,
        tool: FileTool,
        arguments: serde_json::Value,
    ) -> Result<FileReply, EventError> {
        let (actor, _) = self
            .take_vouched(&super::PermitKey::call(tool.name(), &arguments))
            .map_err(|_| refused("no unique one-shot host vouch for this file call"))?;
        let call = tool.call(&arguments)?;
        self.session(&actor.root, super::acting_trajectory(&actor))?
            .execute_file(call)
            .await
    }
}

pub(super) fn refused(message: impl ToString) -> EventError {
    EventError::RemedyArguments {
        detail: format!("file tracking: {}", message.to_string()),
    }
}

pub(super) fn key(dispatch: &appa_engine::value::DispatchId) -> Result<String, EventError> {
    serde_json::to_string(dispatch).map_err(refused)
}

#[cfg(all(test, unix, feature = "daemon"))]
mod tests {
    use super::*;
    use crate::api::{OutcomeBody, RemedyDecision, Runtime, ToolCallDecision, ToolOutcome, TrajectoryId};
    use crate::config::Config;
    use crate::engine::RemedyArguments;
    use appa_engine::label::{Audience, Label, Trust};
    use appa_engine::value::{FileBasis, FileSource};
    use appa_eventlog::files::{FilePin, PinnedVersion};
    use std::path::Path;

    fn open(dir: &Path) -> Runtime {
        let config = dir.join("policy.toml");
        std::fs::write(
            &config,
            r#"
[policy]
version = 2
[[policy.tool]]
name = "mcp/appa/appa_read_file"
delta = {}
[[policy.tool]]
name = "mcp/appa/appa_write_file"
delta = {}
[[policy.tool]]
name = "mcp/appa/appa_edit_file"
delta = {}
[[policy.tool]]
name = "mcp/appa/appa_copy_file"
delta = {}
[[policy.tool]]
name = "mcp/appa/appa_move_file"
delta = {}
[[policy.tool]]
name = "mcp/appa/appa_process_files"
delta = {}
[[policy.tool]]
name = "host/claude-code/Agent"
delta = {}
[[policy.tool]]
name = "host/claude-code/Read"
delta = {}
[[policy.tool]]
name = "mcp/github/get_issue"
delta = {}
[policy.deployment]
context_control = true
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
        .with_file_tracking(Label::new(Trust::new(0), Audience::public()), config)
        .unwrap()
    }

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("work")).unwrap();
        std::fs::write(dir.path().join("work/source.txt"), "outside information").unwrap();
        dir
    }

    fn enable_with_policy(dir: &Path, policy: &str) -> Result<Runtime, super::super::OpenError> {
        let config = dir.join("compatibility-policy.toml");
        std::fs::write(&config, policy).unwrap();
        Runtime::open_served(
            Config::load(&config).unwrap(),
            dir.join("compatibility.db"),
            None,
            appa_adapter_claude_code::adapter(),
        )?
        .with_file_tracking(Label::new(Trust::new(0), Audience::public()), config)
    }

    #[test]
    fn file_tracking_accepts_output_sanitizers_but_refuses_input_sanitizers_and_confined_file_results() {
        let output = tempfile::tempdir().unwrap();
        enable_with_policy(
            output.path(),
            r#"
[policy]
version = 2
[[policy.tool]]
name = "host/claude-code/Bash"
[[policy.sanitizer]]
name = "redact-secrets"
on = ["tool_output"]
[policy.sanitizer.permits]
audience = { from = ["self"], to = ["public"] }
[policy.deployment]
confined_results = ["host/claude-code/Bash"]
[externals]
timeout_ms = 2000
max_body_bytes = 65536
[externals.sanitizers.redact-secrets]
builtin = "redact-secrets"
"#,
        )
        .expect("an output-only sanitizer is compatible");

        let input = tempfile::tempdir().unwrap();
        let Err(input_error) = enable_with_policy(
            input.path(),
            r#"
[policy]
version = 2
[[policy.tool]]
name = "host/claude-code/Bash"
[[policy.sanitizer]]
name = "redact-secrets"
on = ["tool_input"]
[policy.sanitizer.permits]
audience = { from = ["self"], to = ["public"] }
[externals]
timeout_ms = 2000
max_body_bytes = 65536
[externals.sanitizers.redact-secrets]
builtin = "redact-secrets"
"#,
        ) else {
            panic!("an input sanitizer must be refused");
        };
        assert!(input_error.to_string().contains("tool-input sanitizer"));

        let confined = tempfile::tempdir().unwrap();
        let Err(confined_error) = enable_with_policy(
            confined.path(),
            r#"
[policy]
version = 2
[[policy.tool]]
name = "mcp/appa/appa_read_file"
[policy.deployment]
confined_results = ["mcp/appa/appa_read_file"]
[externals]
timeout_ms = 2000
max_body_bytes = 65536
"#,
        ) else {
            panic!("a confined file-tool result must be refused");
        };
        assert!(confined_error.to_string().contains("cannot confine"));
    }

    #[test]
    fn initialized_default_and_claude_code_battery_start_with_file_tracking() {
        let dir = tempfile::tempdir().unwrap();
        let battery = dir.path().join("batteries/claude-code");
        std::fs::create_dir_all(&battery).unwrap();
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let default =
            std::fs::read_to_string(repository.join("marketplace/plugins/claude-code/default.appa.toml")).unwrap();
        std::fs::copy(
            repository.join("marketplace/batteries/claude-code/appa.toml"),
            battery.join("appa.toml"),
        )
        .unwrap();
        let config = dir.path().join("appa.toml");
        std::fs::write(
            &config,
            format!(
                "include = [\"batteries/claude-code/appa.toml\"]\n\n{default}\n[file_tracking]\ninitial_trust = \"suspicious\"\ninitial_audience = \"public\"\n"
            ),
        )
        .unwrap();
        let runtime = Runtime::open_served(
            Config::load(&config).unwrap(),
            dir.path().join("appa.db"),
            None,
            appa_adapter_claude_code::adapter(),
        )
        .unwrap();

        runtime
            .with_file_tracking(Label::new(Trust::new(0), Audience::public()), config)
            .expect("the shipped output-only sanitizer remains available with file tracking");
    }

    #[test]
    fn file_event_streams_are_shared_by_workspace_and_isolated_across_workspaces() {
        let dir = fixture();
        let other_workspace = dir.path().join("other-work");
        std::fs::create_dir(&other_workspace).unwrap();
        std::fs::write(other_workspace.join("other.txt"), "other information").unwrap();
        let runtime = open(dir.path());
        let files = runtime.inner.shared.files.as_ref().unwrap();
        let root = TrajectoryId("cc:session".into());
        let same_root_for_child = TrajectoryId("cc:session".into());
        let same_workspace_root = TrajectoryId("cc:same-workspace".into());
        let other_root = TrajectoryId("cc:other-session".into());
        let authority = &runtime.inner.store;

        files
            .bind(authority, &root, dir.path().join("work").to_str().unwrap())
            .unwrap();
        files
            .bind(
                authority,
                &same_workspace_root,
                dir.path().join("work").to_str().unwrap(),
            )
            .unwrap();
        files
            .bind(authority, &other_root, other_workspace.to_str().unwrap())
            .unwrap();
        let parent = files.store(authority, &root).unwrap();
        let child = files.store(authority, &same_root_for_child).unwrap();
        let same_workspace = files.store(authority, &same_workspace_root).unwrap();
        let other = files.store(authority, &other_root).unwrap();
        assert_eq!(parent.workspace(), child.workspace());
        assert_eq!(parent.workspace(), same_workspace.workspace());
        assert_ne!(parent.workspace(), other.workspace());
        assert_eq!(
            parent.workspace(),
            std::fs::canonicalize(dir.path().join("work")).unwrap()
        );
        assert_eq!(other.workspace(), std::fs::canonicalize(&other_workspace).unwrap());
        assert!(parent.current("other.txt").unwrap().is_none());
        assert!(other.current("source.txt").unwrap().is_none());
        assert!(files.bind(authority, &root, other_workspace.to_str().unwrap()).is_err());
        assert!(
            files
                .bind(
                    authority,
                    &TrajectoryId("cc:unsafe".into()),
                    dir.path().to_str().unwrap()
                )
                .is_err(),
            "the runtime database and policy cannot be inside a root's workspace"
        );

        child
            .prepare("cc:session:child", "call", FileOperation::Read, "source.txt")
            .unwrap();
        assert!(parent.pin_for("cc:session:child", "call").unwrap().is_some());
        assert!(same_workspace.pin_for("cc:session:child", "call").unwrap().is_some());
        assert!(other.pin_for("cc:session:child", "call").unwrap().is_none());
    }

    #[test]
    fn root_workspace_binding_survives_restart() {
        let dir = fixture();
        let other = dir.path().join("other-work");
        std::fs::create_dir(&other).unwrap();
        let root = TrajectoryId("durably-bound-root".into());
        let runtime = open(dir.path());
        runtime
            .bind_file_workspace(&root, dir.path().join("work").to_str().unwrap())
            .unwrap();
        drop(runtime);

        let runtime = open(dir.path());
        assert!(runtime.bind_file_workspace(&root, other.to_str().unwrap()).is_err());
        runtime
            .bind_file_workspace(&root, dir.path().join("work").to_str().unwrap())
            .unwrap();
    }

    fn bind(runtime: &Runtime, root: &TrajectoryId, dir: &Path) {
        runtime
            .bind_file_workspace(root, dir.join("work").to_str().unwrap())
            .unwrap();
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
            cwd: None,
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
                FileTool::of(&call).unwrap(),
                serde_json::from_str(call.arguments.get()).unwrap(),
            )
            .await
            .unwrap()
    }

    async fn hook(runtime: &Runtime, event: serde_json::Value) -> appa_runtime_api::HookDecision {
        use appa_runtime_api::{AdapterName, WireDecision, WireEvent};
        let codec = appa_adapter_claude_code::codec();
        let event = (codec.parse)(&serde_json::to_vec(&event).unwrap()).unwrap().unwrap();
        let wire = WireEvent::from_event(AdapterName::ClaudeCode, &event).unwrap();
        let (status, answer) = crate::hooks::answer(
            runtime,
            &appa_adapter_claude_code::adapter(),
            &serde_json::to_vec(&wire).unwrap(),
        )
        .await;
        assert_eq!(status, 200, "{answer}");
        serde_json::from_value::<WireDecision>(answer)
            .unwrap()
            .into_decision()
            .unwrap()
    }

    #[tokio::test]
    async fn bound_workspaces_refuse_bypass_tools_but_keep_other_policy_tools() {
        use appa_runtime_api::HookDecision;
        let dir = fixture();
        let runtime = open(dir.path());
        assert!(matches!(
            hook(
                &runtime,
                serde_json::json!({
                    "hook_event_name":"SessionStart", "session_id":"spawn-test"
                }),
            )
            .await,
            HookDecision::Context { .. }
        ));

        let spawn = hook(
            &runtime,
            serde_json::json!({
                "hook_event_name":"PreToolUse", "session_id":"spawn-test",
                    "cwd":dir.path().join("work"),
                "tool_name":"Agent",
                "tool_input":{
                    "description":"read a managed file",
                    "subagent_type":"general-purpose",
                    "prompt":"Read the managed file."
                }
            }),
        )
        .await;
        assert!(
            matches!(spawn, HookDecision::DenyCall { ref offers, .. } if !offers.is_empty()),
            "the declared spawn should reach the engine's return contract: {spawn:?}"
        );

        let unbound_read = hook(
            &runtime,
            serde_json::json!({
                "hook_event_name":"PreToolUse", "session_id":"unbound-test",
                "cwd":dir.path().join("work"),
                "tool_name":"Read", "tool_input":{"file_path":"source.txt"}
            }),
        )
        .await;
        assert_eq!(unbound_read, HookDecision::AllowCall { spawn: None });
        let connection = rusqlite::Connection::open(dir.path().join("runtime.db")).unwrap();
        let file_tables: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('file_events','file_roots')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(file_tables, 0, "an ordinary unbound call must not install file tables");

        runtime
            .bind_file_workspace(
                &TrajectoryId("cc:spawn-test".into()),
                dir.path().join("work").to_str().unwrap(),
            )
            .unwrap();

        let native = hook(
            &runtime,
            serde_json::json!({
                "hook_event_name":"PreToolUse", "session_id":"spawn-test",
                    "cwd":dir.path().join("work"),
                "tool_name":"Read", "tool_input":{"file_path":"source.txt"}
            }),
        )
        .await;
        assert!(
            matches!(native, HookDecision::DenyCall { ref feedback, .. } if feedback.contains("bound workspace refuses native filesystem and shell tools")),
            "a native filesystem tool must not bypass the bound workspace stream: {native:?}"
        );

        let github = hook(
            &runtime,
            serde_json::json!({
                "hook_event_name":"PreToolUse", "session_id":"spawn-test",
                "cwd":dir.path().join("work"),
                "tool_name":"mcp__github__get_issue", "tool_input":{"owner":"o", "repo":"r", "issue_number":1}
            }),
        )
        .await;
        assert_eq!(github, HookDecision::AllowCall { spawn: None });
    }

    #[tokio::test]
    async fn managed_files_plugin_hooks_bind_exact_calls_and_absorb_duplicate_results() {
        use appa_runtime_api::HookDecision;
        let dir = fixture();
        let other_workspace = dir.path().join("other-work");
        std::fs::create_dir(&other_workspace).unwrap();
        let runtime = open(dir.path());
        let start = hook(
            &runtime,
            serde_json::json!({
                "hook_event_name":"SessionStart", "session_id":"plugin-test"
            }),
        )
        .await;
        assert!(matches!(start, HookDecision::Context { text } if text.contains("appa_read_file(file_path)")));
        let arguments = serde_json::json!({"file_path":"new.txt", "content":"trusted original"});
        assert!(runtime.execute_file(FileTool::Write, arguments.clone()).await.is_err());
        assert!(matches!(
            hook(
                &runtime,
                serde_json::json!({
                    "hook_event_name":"PreToolUse", "session_id":"plugin-test",
                    "cwd":dir.path().join("work"),
                    "tool_name":"mcp__appa__appa_write_file", "tool_input":arguments
                })
            )
            .await,
            HookDecision::AllowCall { .. }
        ));
        assert!(
            runtime
                .execute_file(
                    FileTool::Write,
                    serde_json::json!({"file_path":"new.txt", "content":"substituted"})
                )
                .await
                .is_err()
        );
        // Another workspace has an independent event stream and reservation.
        let competing = hook(
            &runtime,
            serde_json::json!({
                "hook_event_name":"PreToolUse", "session_id":"other-session",
                "cwd":other_workspace,
                "tool_name":"mcp__appa__appa_write_file",
                "tool_input":{"file_path":"other.txt", "content":"other"}
            }),
        )
        .await;
        assert!(matches!(competing, HookDecision::AllowCall { .. }));
        assert_eq!(
            runtime.execute_file(FileTool::Write, arguments.clone()).await.unwrap(),
            FileReply::Value("file written".into())
        );
        let version = runtime
            .inner
            .shared
            .files
            .as_ref()
            .unwrap()
            .store(&runtime.inner.store, &TrajectoryId("cc:plugin-test".into()))
            .unwrap()
            .current("new.txt")
            .unwrap()
            .unwrap();
        for _ in 0..2 {
            assert!(matches!(
                hook(
                    &runtime,
                    serde_json::json!({
                        "hook_event_name":"PostToolUse", "session_id":"plugin-test",
                        "tool_name":"mcp__appa__appa_write_file",
                        "tool_input":arguments, "tool_response":"file written"
                    })
                )
                .await,
                HookDecision::Ack
            ));
        }
        assert!(runtime.execute_file(FileTool::Write, arguments).await.is_err());
        assert_eq!(
            runtime
                .inner
                .shared
                .files
                .as_ref()
                .unwrap()
                .store(&runtime.inner.store, &TrajectoryId("cc:plugin-test".into()))
                .unwrap()
                .current("new.txt")
                .unwrap()
                .unwrap(),
            version
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("work/new.txt")).unwrap(),
            "trusted original"
        );
        assert!(!dir.path().join("work/other.txt").exists());
    }

    #[tokio::test]
    async fn managed_files_bound_caller_retains_failure_taint_after_reopen() {
        let dir = fixture();
        let runtime = open(dir.path());
        let actor = appa_runtime_api::Actor {
            root: TrajectoryId("host-bound-stdio".into()),
            child: None,
        };
        bind(&runtime, &actor.root, dir.path());
        runtime.create_session(actor.root.clone(), None).unwrap();
        let arguments = serde_json::json!({
            "file_path": "source.txt", "old_string": "absent", "new_string": "replacement"
        });
        let blocked = runtime
            .execute_bound_file(&actor, FileTool::Edit, arguments.clone())
            .await
            .unwrap();
        assert!(matches!(blocked, FileReply::Failure(message) if message.contains("trusted -> suspicious")));
        let proposal = ProposedCall {
            tool: format!("{PREFIX}appa_edit_file"),
            arguments: super::super::session::raw(arguments.clone()),
            cwd: None,
        };
        allow(&runtime, &actor.root, proposal.clone()).await;
        // The approval above releases the call; execute exactly that dispatch as MCP does.
        assert_eq!(
            execute(&runtime, &actor.root, proposal).await,
            FileReply::Failure("old_string must match exactly once".into())
        );
        drop(runtime);
        let runtime = open(dir.path());
        bind(&runtime, &actor.root, dir.path());
        assert!(matches!(
            runtime.create_session(actor.root.clone(), None),
            Err(EventError::TrajectoryExists)
        ));
        assert_eq!(
            runtime
                .execute_bound_file(
                    &actor,
                    FileTool::Write,
                    serde_json::json!({"file_path":"after-reconnect.txt", "content":"the string was absent"})
                )
                .await
                .unwrap(),
            FileReply::Value("file written".into())
        );
        assert_eq!(label(&runtime, &actor.root, "after-reconnect.txt").trust, Trust::new(0));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("work/after-reconnect.txt")).unwrap(),
            "the string was absent"
        );
        assert!(
            runtime
                .execute_bound_file(
                    &actor,
                    FileTool::Write,
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
        let runtime = open(dir.path());
        for (index, old) in ["absent substring", "outside"].into_iter().enumerate() {
            let id = TrajectoryId(format!("host-probe-{index}"));
            bind(&runtime, &id, dir.path());
            let session = runtime.create_session(id.clone(), None).unwrap();
            let proposal = ProposedCall {
                tool: format!("{PREFIX}appa_edit_file"),
                arguments: super::super::session::raw(
                    serde_json::json!({"file_path":"source.txt", "old_string":old, "new_string":"replacement"}),
                ),
                cwd: None,
            };
            assert!(matches!(
                session.on_tool_call(proposal.clone(), false).await.unwrap(),
                ToolCallDecision::Deny { .. }
            ));
            assert!(
                runtime
                    .execute_file(FileTool::Edit, serde_json::from_str(proposal.arguments.get()).unwrap())
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
            assert_eq!(
                label(&runtime, &id, &format!("result-{index}.txt")).trust,
                Trust::new(0)
            );
            assert!(
                runtime
                    .execute_file(FileTool::Write, serde_json::from_str(output.arguments.get()).unwrap())
                    .await
                    .is_err()
            );
        }
        assert_eq!(
            std::fs::read_to_string(dir.path().join("work/source.txt")).unwrap(),
            "replacement information"
        );
    }

    #[tokio::test]
    async fn managed_files_process_results_and_failures_keep_input_labels() {
        let dir = fixture();
        let backend = dir.path().join("backend");
        std::fs::create_dir(&backend).unwrap();
        for binary in ["agentsh", "agentsh-unixwrap"] {
            std::fs::write(backend.join(binary), "unit-test backend placeholder").unwrap();
        }
        // This trusted fixture tests runtime publication/admission, not OS confinement.
        std::fs::write(
            backend.join("run.py"),
            r#"
import json, pathlib, sys
job = pathlib.Path(sys.argv[2])
request = json.loads((job / 'request.json').read_text())
if request['command'] == 'fail':
    print(json.dumps({'result': {'exit_code': 7, 'stderr': 'outside information'}}))
else:
    (job / 'output/result').write_bytes((job / 'inputs/source.txt').read_bytes())
    print(json.dumps({'result': {'exit_code': 0, 'stdout': 'outside information'}}))
"#,
        )
        .unwrap();
        let runtime = open(dir.path()).with_file_process_backend(backend).unwrap();
        for command in ["success", "absolute", "fail"] {
            let id = TrajectoryId(format!("process-{command}"));
            bind(&runtime, &id, dir.path());
            runtime.create_session(id.clone(), None).unwrap();
            let input = if command == "absolute" {
                dir.path().join("work/source.txt").to_str().unwrap().to_string()
            } else {
                "source.txt".to_string()
            };
            let proposal = ProposedCall {
                tool: format!("{PREFIX}appa_process_files"),
                arguments: serde_json::value::to_raw_value(&serde_json::json!({
                    "input_paths": [input], "output_path": format!("{command}.txt"), "command": command
                }))
                .unwrap(),
                cwd: None,
            };
            let session = runtime.session(&id, &id).unwrap();
            assert!(matches!(
                session.on_tool_call(proposal.clone(), false).await.unwrap(),
                ToolCallDecision::Deny { .. }
            ));
            assert!(!dir.path().join(format!("work/{command}.txt")).exists());
            allow(&runtime, &id, proposal.clone()).await;
            let reply = execute(&runtime, &id, proposal).await;
            if command != "fail" {
                assert!(matches!(reply, FileReply::Value(body) if body.contains("outside information")));
                assert_eq!(label(&runtime, &id, &format!("{command}.txt")).trust, Trust::new(0));
            } else {
                assert!(matches!(reply, FileReply::Failure(body) if body.contains("outside information")));
                assert!(!dir.path().join("work/fail.txt").exists());
            }
            let report = call("Write", &format!("{command}-report.txt"));
            allow(&runtime, &id, report.clone()).await;
            execute(&runtime, &id, report).await;
            assert_eq!(
                label(&runtime, &id, &format!("{command}-report.txt")).trust,
                Trust::new(0)
            );
        }
        assert_eq!(
            std::fs::read(dir.path().join("work/source.txt")).unwrap(),
            b"outside information"
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

    fn label(runtime: &Runtime, root: &TrajectoryId, path: &str) -> Label {
        runtime
            .inner
            .shared
            .files
            .as_ref()
            .unwrap()
            .store(&runtime.inner.store, root)
            .unwrap()
            .current(path)
            .unwrap()
            .unwrap()
            .label
    }

    #[tokio::test]
    async fn managed_files_copy_move_bypass_payload_admission_but_preserve_labels() {
        let dir = fixture();
        std::fs::write(dir.path().join("work/CLAUDE.md"), "host instructions").unwrap();
        let runtime = open(dir.path());
        let actor = appa_runtime_api::Actor {
            root: TrajectoryId("file-transfer-test".into()),
            child: None,
        };
        bind(&runtime, &actor.root, dir.path());
        runtime.create_session(actor.root.clone(), None).unwrap();
        for (tool, source, destination) in [
            (FileTool::Copy, "source.txt", "copied.txt"),
            (FileTool::Move, "copied.txt", "moved.txt"),
        ] {
            assert_eq!(
                runtime
                    .execute_bound_file(
                        &actor,
                        tool,
                        serde_json::json!({
                            "source_path":source, "destination_path":destination
                        })
                    )
                    .await
                    .unwrap(),
                FileReply::Value("file transfer completed".into())
            );
            assert_eq!(label(&runtime, &actor.root, destination).trust, Trust::new(0));
            assert_eq!(
                std::fs::read(dir.path().join("work").join(destination)).unwrap(),
                b"outside information"
            );
        }
        assert!(!dir.path().join("work/copied.txt").exists());
        assert!(
            runtime
                .inner
                .shared
                .files
                .as_ref()
                .unwrap()
                .store(&runtime.inner.store, &actor.root)
                .unwrap()
                .current("copied.txt")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            runtime
                .execute_bound_file(
                    &actor,
                    FileTool::Write,
                    serde_json::json!({
                        "file_path":"ack-only.txt", "content":"only observed acknowledgements"
                    })
                )
                .await
                .unwrap(),
            FileReply::Value("file written".into())
        );
        assert_eq!(label(&runtime, &actor.root, "ack-only.txt").trust, Trust::new(1));
        drop(runtime);
        let runtime = open(dir.path());
        bind(&runtime, &actor.root, dir.path());
        assert!(
            matches!(runtime.execute_bound_file(&actor, FileTool::Read, serde_json::json!({
            "file_path":"moved.txt"
        })).await.unwrap(), FileReply::Failure(message) if message.contains("trusted -> suspicious"))
        );
        assert!(
            runtime
                .execute_bound_file(
                    &actor,
                    FileTool::Move,
                    serde_json::json!({
                        "source_path":"moved.txt", "destination_path":"CLAUDE.md"
                    })
                )
                .await
                .is_err()
        );
        assert!(dir.path().join("work/moved.txt").exists());
        assert!(
            runtime
                .execute_bound_file(
                    &actor,
                    FileTool::Move,
                    serde_json::json!({
                        "source_path":"CLAUDE.md", "destination_path":"stolen-instructions.txt"
                    })
                )
                .await
                .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("work/CLAUDE.md")).unwrap(),
            "host instructions"
        );
        assert!(!dir.path().join("work/stolen-instructions.txt").exists());
    }

    #[tokio::test]
    async fn managed_files_read_write_edit_and_restart_use_engine_labels() {
        let dir = fixture();
        let runtime = open(dir.path());
        let id = TrajectoryId("host-owned-session".into());
        bind(&runtime, &id, dir.path());
        runtime.create_session(id.clone(), None).unwrap();
        allow(&runtime, &id, call("Write", "clean.txt")).await;
        std::fs::write(dir.path().join("work/clean.txt"), "independent text").unwrap();
        success(&runtime, &id, call("Write", "clean.txt")).await;
        assert_eq!(label(&runtime, &id, "clean.txt").trust, Trust::new(1));

        allow(&runtime, &id, call("Read", "source.txt")).await;
        success(&runtime, &id, call("Read", "source.txt")).await;
        allow(&runtime, &id, call("Write", "derived.txt")).await;
        std::fs::write(dir.path().join("work/derived.txt"), "derived from outside information").unwrap();
        success(&runtime, &id, call("Write", "derived.txt")).await;
        assert_eq!(label(&runtime, &id, "derived.txt").trust, Trust::new(0));
        drop(runtime);

        let runtime = open(dir.path());
        bind(&runtime, &id, dir.path());
        assert_eq!(label(&runtime, &id, "clean.txt").trust, Trust::new(1));
        assert_eq!(label(&runtime, &id, "derived.txt").trust, Trust::new(0));
        allow(&runtime, &id, call("Edit", "clean.txt")).await;
        std::fs::write(
            dir.path().join("work/clean.txt"),
            "independent text plus outside information",
        )
        .unwrap();
        success(&runtime, &id, call("Edit", "clean.txt")).await;
        assert_eq!(label(&runtime, &id, "clean.txt").trust, Trust::new(0));
        let version = runtime
            .inner
            .shared
            .files
            .as_ref()
            .unwrap()
            .store(&runtime.inner.store, &id)
            .unwrap()
            .current("clean.txt")
            .unwrap()
            .unwrap();
        assert_eq!(
            version.previous.into_iter().collect::<Vec<_>>(),
            version.content_dependencies
        );
        assert!(!version.content_dependencies.is_empty());
    }

    #[tokio::test]
    async fn managed_file_quarantine_survives_restart() {
        let dir = fixture();
        let runtime = open(dir.path());
        let id = TrajectoryId("host-owned-session".into());
        bind(&runtime, &id, dir.path());
        runtime.create_session(id.clone(), None).unwrap();
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
        assert_eq!(label(&runtime, &id, "after-error.txt").trust, Trust::new(0));
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
        let runtime = open(dir.path());
        bind(&runtime, &id, dir.path());
        let error = runtime
            .session(&id, &id)
            .unwrap()
            .on_tool_call(call("Read", "after-error.txt"), false)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("pending"),
            "durable quarantine must refuse the workspace: {error}"
        );
    }

    #[tokio::test]
    async fn published_file_label_survives_an_outcome_append_failure_and_restart() {
        let dir = fixture();
        let id = TrajectoryId("append-failure-session".into());
        let write = call("Write", "published.txt");
        let runtime = open(dir.path());
        bind(&runtime, &id, dir.path());
        runtime.create_session(id.clone(), None).unwrap();
        allow(&runtime, &id, write.clone()).await;
        std::fs::write(dir.path().join("work/published.txt"), "durable publication").unwrap();

        runtime.store().fail_commit_after(0);
        let result = runtime
            .session(&id, &id)
            .unwrap()
            .on_tool_result(
                write.clone(),
                ToolOutcome::Success {
                    body: OutcomeBody::Available("native output".into()),
                },
            )
            .await;
        assert!(matches!(result, Err(EventError::Storage(_))));
        assert_eq!(label(&runtime, &id, "published.txt").trust, Trust::new(1));
        drop(runtime);

        let runtime = open(dir.path());
        bind(&runtime, &id, dir.path());
        assert_eq!(label(&runtime, &id, "published.txt").trust, Trust::new(1));
        success(&runtime, &id, write).await;
        assert!(runtime.open_dispatches(&id, &id).is_empty());
    }

    #[test]
    fn ledger_pins_derive_the_file_basis_the_engine_rules_on() {
        let dir = fixture();
        let work = dir.path().join("work");
        std::fs::write(work.join("second.txt"), "second").unwrap();
        std::fs::write(work.join("occupied.txt"), "occupied").unwrap();
        let store = FileStore::new(&work, &Label::top()).unwrap();
        let pinned = |path: &str| {
            let version = store.current(path).unwrap().unwrap();
            FileSource {
                version: version.id.to_string(),
                digest: version.digest,
                label: version.label,
            }
        };
        let derive = |key: &str, pin: FilePin| {
            store.cancel("a", key).unwrap();
            pin.file_basis()
        };

        let read = store.prepare("a", "read", FileOperation::Read, "source.txt").unwrap();
        assert_eq!(derive("read", read), FileBasis::Read(pinned("source.txt")));
        let edit = store.prepare("a", "edit", FileOperation::Edit, "source.txt").unwrap();
        assert_eq!(derive("edit", edit), FileBasis::Edit(pinned("source.txt")));
        let create = store
            .prepare("a", "create", FileOperation::Replace, "fresh.txt")
            .unwrap();
        assert_eq!(derive("create", create), FileBasis::Replace(None));
        let replace = store
            .prepare("a", "replace", FileOperation::Replace, "source.txt")
            .unwrap();
        assert_eq!(
            derive("replace", replace),
            FileBasis::Replace(Some(pinned("source.txt")))
        );
        let copy = store
            .prepare_transfer("a", "copy", FileOperation::Copy, "source.txt", "occupied.txt")
            .unwrap();
        assert_eq!(
            derive("copy", copy),
            FileBasis::Copy {
                source: pinned("source.txt"),
                replaced: Some(pinned("occupied.txt")),
            }
        );
        let moved = store
            .prepare_transfer("a", "move", FileOperation::Move, "source.txt", "moved.txt")
            .unwrap();
        assert_eq!(
            derive("move", moved),
            FileBasis::Move {
                source: pinned("source.txt"),
                replaced: None,
            }
        );
        let process = store
            .prepare_process(
                "a",
                "process",
                &["second.txt".into(), "source.txt".into()],
                "occupied.txt",
            )
            .unwrap();
        assert_eq!(
            derive("process", process),
            FileBasis::Process {
                inputs: vec![pinned("second.txt"), pinned("source.txt")],
                replaced: Some(pinned("occupied.txt")),
            }
        );
    }

    #[tokio::test]
    async fn managed_files_execute_the_pinned_path_not_the_argument_path() {
        let dir = fixture();
        let runtime = open(dir.path());
        // The pin names source.txt; the call's bytes name elsewhere.txt. The ledger validated,
        // hashed and reserved the pinned path, so that is the one that runs.
        let call = ProposedCall {
            tool: format!("{PREFIX}appa_write_file"),
            arguments: super::super::session::raw(
                serde_json::json!({"file_path": "elsewhere.txt", "content": "pinned content"}),
            ),
            cwd: None,
        };
        let files = runtime.inner.shared.files.as_ref().unwrap();
        let workspace = std::fs::canonicalize(dir.path().join("work")).unwrap();
        files
            .bind(
                &runtime.inner.store,
                &TrajectoryId("direct-perform".into()),
                workspace.to_str().unwrap(),
            )
            .unwrap();
        let pin = FilePin {
            path: "source.txt".into(),
            basis: PinnedBasis::Replace(None),
        };
        assert_eq!(
            perform(files, &workspace, &call, &pin).unwrap(),
            "file written".to_string()
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("work/source.txt")).unwrap(),
            "pinned content"
        );
        assert!(!dir.path().join("work/elsewhere.txt").exists());
    }

    #[tokio::test]
    async fn managed_files_never_follow_a_parent_swapped_for_a_symlink() {
        let dir = fixture();
        let runtime = open(dir.path());
        let files = runtime.inner.shared.files.as_ref().unwrap();
        let workspace = std::fs::canonicalize(dir.path().join("work")).unwrap();
        files
            .bind(
                &runtime.inner.store,
                &TrajectoryId("parent-swap".into()),
                workspace.to_str().unwrap(),
            )
            .unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("target.txt"), "outside bytes").unwrap();
        std::os::unix::fs::symlink(&outside, workspace.join("sub")).unwrap();
        let pin = |basis| FilePin {
            path: "sub/target.txt".into(),
            basis,
        };
        let write = ProposedCall {
            tool: format!("{PREFIX}appa_write_file"),
            arguments: super::super::session::raw(
                serde_json::json!({"file_path": "sub/target.txt", "content": "escaped"}),
            ),
            cwd: None,
        };
        let read = ProposedCall {
            tool: format!("{PREFIX}appa_read_file"),
            arguments: super::super::session::raw(serde_json::json!({"file_path": "sub/target.txt"})),
            cwd: None,
        };
        let outside_version = PinnedVersion {
            id: 1,
            digest: "unhashed".into(),
            label: Label::top(),
        };
        assert!(perform(files, &workspace, &write, &pin(PinnedBasis::Replace(None))).is_err());
        assert!(perform(files, &workspace, &read, &pin(PinnedBasis::Read(outside_version))).is_err());
        assert_eq!(
            std::fs::read_to_string(outside.join("target.txt")).unwrap(),
            "outside bytes"
        );
    }

    #[tokio::test]
    async fn managed_files_release_a_released_call_the_harness_never_ran() {
        let dir = fixture();
        let runtime = open(dir.path());
        let id = TrajectoryId("host-owned-session".into());
        bind(&runtime, &id, dir.path());
        let session = runtime.create_session(id.clone(), None).unwrap();
        // The policy released this call and the harness never ran it — a declined prompt, an
        // interrupted turn. The turn end gives the reservation back instead of wedging the
        // workspace for every later file call.
        allow(&runtime, &id, call("Write", "never-run.txt")).await;
        assert!(!dir.path().join("work/never-run.txt").exists());
        session.on_turn_end().await.unwrap();
        allow(&runtime, &id, call("Write", "later.txt")).await;
        assert_eq!(
            execute(&runtime, &id, call("Write", "later.txt")).await,
            FileReply::Value("file written".into())
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("work/later.txt")).unwrap(),
            "fixture"
        );
        assert!(
            runtime
                .inner
                .shared
                .files
                .as_ref()
                .unwrap()
                .store(&runtime.inner.store, &id)
                .unwrap()
                .current("later.txt")
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn managed_files_bypasses_and_missing_outcomes_fail_closed() {
        let dir = fixture();
        let runtime = open(dir.path());
        let id = TrajectoryId("host-owned-session".into());
        bind(&runtime, &id, dir.path());
        let session = runtime.create_session(id.clone(), None).unwrap();
        for proposal in [
            call("Bash", "source.txt"),
            call("Read", "../policy.toml"),
            call("Write", ".claude/settings.json"),
            call("Write", "CLAUDE.md"),
            call("Write", "nested/CLAUDE.local.md"),
        ] {
            assert!(session.on_tool_call(proposal, false).await.is_err());
        }
        assert!(!dir.path().join("work/CLAUDE.md").exists());
        assert!(!dir.path().join("work/nested").exists());
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
