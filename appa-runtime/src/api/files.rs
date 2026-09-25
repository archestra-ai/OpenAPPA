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
//! or unmatched digest leaves the session's in-memory reservation in place. Further file
//! calls in that session stop. A released call the harness never ran gives its reservation
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
//! its first file call's working directory,
//! then classifies and hashes the files that exist there. Its subagents share that workspace
//! and ledger; another root session can bind to a different workspace.
//! A workspace containing a symlink or hard link is refused, so use a dedicated directory.
//! With the APPA plugin installed, launch Claude with `APPA_GATE=1` and
//! `APPA_RUNTIME_URL` pointing to this runtime. SessionStart describes the file tools.
//! The plugin's HTTP MCP calls consume exact one-shot hook approvals; their outcomes
//! are already admitted when the post-tool hook arrives. Native tools remain available
//! in Claude, but calls reaching APPA are refused in this mode — including APPA's own
//! management tools, which an operator runs from the `appa` command line instead.
//! `appa claude-files` is a separate constrained test launcher, not required by the plugin.
//! The policy must declare each enabled tool. File tools alone do not enforce OS isolation.
//! `--file-process-backend /host/backend` additionally enables `appa_process_files`; its
//! staged-input contract is in `process.rs`. Use disposable test fixtures only.
//! # Limitations
//!
//! The constrained launcher exposes only file tools and the remedy control tool. Bash, other
//! MCP tools, subagents, general rename/delete, links and known execution-control writes are unsupported.
//! Copy/Move support regular files only; same-path and cross-filesystem moves are refused.
//! Sanitizer/rewrite policies are unsupported. File ledger state does not survive a runtime
//! restart. Historical bytes are not retained.
//! Only Process calls use the isolated backend. No unmediated filesystem, metadata or
//! timing-flow guarantee is made. The Claude process and inference remain outside isolation.

use appa_engine::value::{FileBasis, FileSource};
#[cfg(feature = "daemon")]
use appa_eventlog::files::beneath::{self, Entry};
use appa_eventlog::files::{FileOperation, FilePin, FileStore};
use std::collections::HashMap;
#[cfg(feature = "daemon")]
use std::path::Path;
use std::path::PathBuf;

use super::{EventError, ProposedCall};

#[cfg(feature = "daemon")]
#[path = "process.rs"]
mod process;

pub(super) struct FileTracking {
    pub(super) stores: std::sync::Mutex<HashMap<String, std::sync::Arc<FileStore>>>,
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
        root: &super::TrajectoryId,
        workspace: &str,
    ) -> Result<std::sync::Arc<FileStore>, appa_eventlog::files::FileStoreError> {
        let workspace = std::fs::canonicalize(workspace)?;
        let mut stores = self
            .stores
            .lock()
            .map_err(|_| appa_eventlog::files::FileStoreError::Corrupt("file store map lock poisoned".into()))?;
        if let Some(store) = stores.get(&root.0) {
            if store.workspace() != workspace {
                return Err(appa_eventlog::files::FileStoreError::Configuration(
                    "a session cannot change its tracked workspace".into(),
                ));
            }
            return Ok(std::sync::Arc::clone(store));
        }
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
        let store = std::sync::Arc::new(FileStore::new(&workspace, &self.initial)?);
        stores.insert(root.0.clone(), std::sync::Arc::clone(&store));
        Ok(store)
    }

    pub(super) fn store(
        &self,
        root: &super::TrajectoryId,
    ) -> Result<std::sync::Arc<FileStore>, appa_eventlog::files::FileStoreError> {
        let stores = self
            .stores
            .lock()
            .map_err(|_| appa_eventlog::files::FileStoreError::Corrupt("file store map lock poisoned".into()))?;
        stores.get(&root.0).cloned().ok_or_else(|| {
            appa_eventlog::files::FileStoreError::Configuration(
                "the session has not supplied a working directory for file tracking".into(),
            )
        })
    }
}

pub(crate) const TOOLS: [&str; 6] = [
    "appa_read_file",
    "appa_write_file",
    "appa_edit_file",
    "appa_copy_file",
    "appa_move_file",
    "appa_process_files",
];
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
    call.tool.strip_prefix(PREFIX).is_some_and(|name| TOOLS.contains(&name))
}

pub(super) fn operation(call: &ProposedCall) -> Result<(FileOperation, String), EventError> {
    let operation = match call.tool.as_str() {
        "mcp/appa/appa_read_file" => FileOperation::Read,
        "mcp/appa/appa_write_file" => FileOperation::Replace,
        "mcp/appa/appa_edit_file" => FileOperation::Edit,
        "mcp/appa/appa_copy_file" => FileOperation::Copy,
        "mcp/appa/appa_move_file" => FileOperation::Move,
        "mcp/appa/appa_process_files" => FileOperation::Process,
        _ => {
            return Err(refused("file tracking permits only runtime-owned file tools"));
        }
    };
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
    let (operation, _) = operation(call).map_err(|error| error.to_string())?;
    match operation {
        FileOperation::Process => process::perform(files, workspace, call, pin),
        FileOperation::Read => read(workspace, &pin.path).map_err(|error| error.to_string()),
        FileOperation::Replace => {
            let args: WriteArgs = serde_json::from_str(call.arguments.get()).map_err(|error| error.to_string())?;
            replace(workspace, &pin.path, &args.content)
                .map(|()| "file written".into())
                .map_err(|error| error.to_string())
        }
        FileOperation::Edit => {
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
        FileOperation::Copy | FileOperation::Move => {
            let source = pin.source.as_ref().ok_or("the transfer pin carries no source")?;
            let result = (|| -> std::io::Result<()> {
                let destination = Entry::create(workspace, &pin.path)?;
                if operation == FileOperation::Move {
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
        tool: &str,
        arguments: serde_json::Value,
    ) -> Result<FileReply, EventError> {
        let call = ProposedCall {
            tool: format!("{PREFIX}{tool}"),
            arguments: serde_json::value::to_raw_value(&arguments).map_err(refused)?,
            cwd: None,
        };
        let session = self.session(&actor.root, super::acting_trajectory(actor))?;
        match session.on_tool_call_identified(call.clone(), None, false).await? {
            super::ToolCallDecision::Deny { feedback, .. } => Ok(FileReply::Failure(feedback)),
            super::ToolCallDecision::Allow { .. } => session.execute_file(call).await,
        }
    }

    #[cfg(feature = "daemon")]
    pub(crate) async fn execute_file(&self, tool: &str, arguments: serde_json::Value) -> Result<FileReply, EventError> {
        let (actor, _) = self
            .take_vouched(&super::PermitKey::call(tool, &arguments))
            .map_err(|_| refused("no unique one-shot host vouch for this file call"))?;
        let call = ProposedCall {
            tool: format!("{PREFIX}{tool}"),
            arguments: serde_json::value::to_raw_value(&arguments).map_err(refused)?,
            cwd: None,
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
        FileOperation::Process => FileBasis::Process {
            inputs: pin
                .inputs
                .into_iter()
                .map(|input| FileSource {
                    version: input.version.to_string(),
                    digest: input.digest,
                    label: input.label,
                })
                .collect(),
            replaced: source,
        },
        FileOperation::Read => FileBasis::Read(source.ok_or_else(|| refused("missing read source"))?),
        FileOperation::Edit => FileBasis::Edit(source.ok_or_else(|| refused("missing edit source"))?),
        FileOperation::Replace => FileBasis::Replace(source),
        FileOperation::Copy | FileOperation::Move => {
            let input = pin.source.ok_or_else(|| refused("missing transfer source"))?;
            let input = FileSource {
                version: input.version.to_string(),
                digest: input.digest,
                label: input.label,
            };
            match pin.operation {
                FileOperation::Copy => FileBasis::Copy {
                    source: input,
                    replaced: source,
                },
                _ => FileBasis::Move {
                    source: input,
                    replaced: source,
                },
            }
        }
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

#[cfg(all(test, unix, feature = "daemon"))]
mod tests {
    use super::*;
    use crate::api::{OutcomeBody, RemedyDecision, Runtime, ToolCallDecision, ToolOutcome, TrajectoryId};
    use crate::config::Config;
    use crate::engine::RemedyArguments;
    use appa_engine::label::{Audience, Label, Trust};
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

    #[test]
    fn file_ledgers_are_shared_by_subagents_and_isolated_across_root_workspaces() {
        let dir = fixture();
        let other_workspace = dir.path().join("other-work");
        std::fs::create_dir(&other_workspace).unwrap();
        std::fs::write(other_workspace.join("other.txt"), "other information").unwrap();
        let runtime = open(dir.path());
        let files = runtime.inner.shared.files.as_ref().unwrap();
        let root = TrajectoryId("cc:session".into());
        let same_root_for_child = TrajectoryId("cc:session".into());
        let other_root = TrajectoryId("cc:other-session".into());

        files.bind(&root, dir.path().join("work").to_str().unwrap()).unwrap();
        files.bind(&other_root, other_workspace.to_str().unwrap()).unwrap();
        let parent = files.store(&root).unwrap();
        let child = files.store(&same_root_for_child).unwrap();
        let other = files.store(&other_root).unwrap();
        assert!(std::sync::Arc::ptr_eq(&parent, &child));
        assert!(!std::sync::Arc::ptr_eq(&parent, &other));
        assert_eq!(
            parent.workspace(),
            std::fs::canonicalize(dir.path().join("work")).unwrap()
        );
        assert_eq!(other.workspace(), std::fs::canonicalize(&other_workspace).unwrap());
        assert!(parent.current("other.txt").unwrap().is_none());
        assert!(other.current("source.txt").unwrap().is_none());
        assert!(files.bind(&root, other_workspace.to_str().unwrap()).is_err());
        assert!(
            files
                .bind(&TrajectoryId("cc:unsafe".into()), dir.path().to_str().unwrap())
                .is_err(),
            "the runtime database and policy cannot be inside a root's workspace"
        );

        child
            .prepare("cc:session:child", "call", FileOperation::Read, "source.txt")
            .unwrap();
        assert_eq!(parent.reservation().unwrap().unwrap().actor, "cc:session:child");
        assert!(other.reservation().unwrap().is_none());
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
                call.tool.strip_prefix(PREFIX).unwrap(),
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
    async fn managed_files_allow_declared_subagent_spawns_but_refuse_other_native_tools() {
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
            matches!(native, HookDecision::DenyCall { ref feedback, .. } if feedback.contains("only runtime-owned file tools and declared subagent spawns")),
            "an unrelated native tool should remain refused: {native:?}"
        );
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
        assert!(
            runtime
                .execute_file("appa_write_file", arguments.clone())
                .await
                .is_err()
        );
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
                    "appa_write_file",
                    serde_json::json!({"file_path":"new.txt", "content":"substituted"})
                )
                .await
                .is_err()
        );
        // Another root has an independent session-local ledger and reservation.
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
            runtime
                .execute_file("appa_write_file", arguments.clone())
                .await
                .unwrap(),
            FileReply::Value("file written".into())
        );
        let version = runtime
            .inner
            .shared
            .files
            .as_ref()
            .unwrap()
            .store(&TrajectoryId("cc:plugin-test".into()))
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
        assert!(runtime.execute_file("appa_write_file", arguments).await.is_err());
        assert_eq!(
            runtime
                .inner
                .shared
                .files
                .as_ref()
                .unwrap()
                .store(&TrajectoryId("cc:plugin-test".into()))
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
            .execute_bound_file(&actor, "appa_edit_file", arguments.clone())
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
                    "appa_write_file",
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
            assert_eq!(
                label(&runtime, &id, &format!("result-{index}.txt")).trust,
                Trust::new(0)
            );
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
            .store(root)
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
            ("appa_copy_file", "source.txt", "copied.txt"),
            ("appa_move_file", "copied.txt", "moved.txt"),
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
                .store(&actor.root)
                .unwrap()
                .current("copied.txt")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            runtime
                .execute_bound_file(
                    &actor,
                    "appa_write_file",
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
            matches!(runtime.execute_bound_file(&actor, "appa_read_file", serde_json::json!({
            "file_path":"moved.txt"
        })).await.unwrap(), FileReply::Failure(message) if message.contains("trusted -> suspicious"))
        );
        assert!(
            runtime
                .execute_bound_file(
                    &actor,
                    "appa_move_file",
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
                    "appa_move_file",
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
            .store(&id)
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
    async fn managed_files_failures_quarantine_only_the_live_session_ledger() {
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
        assert!(
            runtime
                .session(&id, &id)
                .unwrap()
                .on_tool_call(call("Read", "after-error.txt"), false)
                .await
                .is_ok(),
            "a process restart creates a fresh session-local ledger"
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
            .bind(&TrajectoryId("direct-perform".into()), workspace.to_str().unwrap())
            .unwrap();
        let pin = FilePin {
            path: "source.txt".into(),
            operation: FileOperation::Replace,
            predecessor_version: None,
            predecessor_label: None,
            predecessor_digest: None,
            source: None,
            inputs: vec![],
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
            .bind(&TrajectoryId("parent-swap".into()), workspace.to_str().unwrap())
            .unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("target.txt"), "outside bytes").unwrap();
        std::os::unix::fs::symlink(&outside, workspace.join("sub")).unwrap();
        let pin = |operation| FilePin {
            path: "sub/target.txt".into(),
            operation,
            predecessor_version: None,
            predecessor_label: None,
            predecessor_digest: None,
            source: None,
            inputs: vec![],
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
        assert!(perform(files, &workspace, &write, &pin(FileOperation::Replace)).is_err());
        assert!(perform(files, &workspace, &read, &pin(FileOperation::Read)).is_err());
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
                .store(&id)
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
