//! The runtime API: `Runtime` and `Session` — the harness-agnostic
//! event model declared in `docs/runtime.md`.

mod session;

use std::path::PathBuf;
use std::sync::Arc;

pub use crate::engine::TrajectoryStatus;
pub use appa_runtime_api::{OutcomeBody, ProposedCall, ToolOutcome, TrajectoryId};
pub(crate) use session::{Session, is_control_tool};

use crate::config::Config;
use crate::engine::{EngineRefusal, EngineSeam, RuntimeEngine};
use crate::external::ExternalServices;
use crate::store::{Store, StoreError};

/// Identity of one open dispatch: a released call the harness is
/// executing. Runtime-internal: outcomes correlate by the
/// call's canonical bytes, never by an id an adapter
/// carries.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct DispatchId(pub String);

/// Identity of one remedy offer. Engine-derived and unguessable:
/// naming it proves the model read the offer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct OfferId(pub String);

/// An authorized call: the exact canonical bytes the harness must now
/// execute, never re-rendered and never edited.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AuthorizedCall {
    pub tool: String,
    pub bytes: Vec<u8>,
}

/// Answer to a proposed tool call: run it, do not run it — `feedback`
/// tells the model why not and what its options are — or pass it
/// through: `Control` is the runtime's own control tool, not a checked
/// flow; it passes unchecked and opens no dispatch. An
/// allowed call always runs with the arguments the model proposed;
/// input substitution is outside this runtime's coverage.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ToolCallDecision {
    Allow,
    Deny { feedback: String },
    Control,
}

/// What the adapter gives the harness as the tool output. `Keep`: use
/// the output as it is. `Replace`: use this text instead — a cleaned
/// version, or a short note saying the real output was not accepted.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ToolResultDecision {
    Keep,
    Replace { placeholder: String },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RemedyDecision {
    Authorized { call: AuthorizedCall },
    Returned { value: String },
    Declined { feedback: String },
    NoAnswer { feedback: String },
}

/// What happens to the child's final message: delivered to the parent,
/// nothing returned, or delivery stopped. The child
/// is finished, so `feedback` goes to the parent as the spawn call's
/// outcome and names the options by `OfferId`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ChildReturnDecision {
    Returned { value: String },
    NoValue,
    Blocked { feedback: String },
}

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("configuration refused: {0}")]
    Config(#[from] crate::config::ConfigError),
    #[error("builtin modules refused: {0}")]
    Modules(String),
    #[error("policy refused: {0}")]
    Policy(appa_policy::ConfigError),
    #[error("unsupported policy: {0}")]
    UnsupportedPolicy(String),
    #[error("policy declares reserved tool name {0}")]
    ReservedTool(String),
    #[error("policy names {kind} {name}, which has no [externals] binding")]
    UnboundExternal { kind: &'static str, name: String },
    #[error("the database is damaged: {0}")]
    Damaged(String),
    #[error("the database belongs to policy digest {stored}, not {supplied}")]
    PolicyMismatch { stored: String, supplied: String },
    #[error("storage failure: {0}")]
    Storage(String),
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SessionError {
    #[error("a trajectory with this id already exists")]
    AlreadyExists,
    #[error("no trajectory with this id exists")]
    Unknown,
    #[error("the trajectory has ended")]
    Ended,
    #[error("storage failure: {0}")]
    Storage(String),
}

/// Every lifecycle misuse is one typed error; the adapter renders it
/// as a deny.
#[derive(Debug, thiserror::Error)]
pub(crate) enum EventError {
    #[error("a call is already outstanding; propose one call at a time")]
    CallOutstanding,
    #[error("the trajectory has ended")]
    TrajectoryEnded,
    #[error("no trajectory with this id exists")]
    UnknownTrajectory,
    #[error("a trajectory with this id already exists")]
    TrajectoryExists,
    #[error("no open dispatch with this id exists")]
    UnknownDispatch,
    #[error("this outcome does not match the open dispatch; it is not reported")]
    OutcomeMismatch,
    #[error("no live offer with this id exists")]
    UnknownOffer,
    #[error("only a child trajectory submits a return")]
    NotAChild,
    #[error("the family log stayed contended after {attempts} replays")]
    Contended { attempts: u32 },
    #[error("the engine returned a follow-up this event cannot deliver")]
    UnexpectedDecision,
    #[error("the persisted log is refused: {0}")]
    UntrustedLog(String),
    #[error("engine invariant breach: {0}")]
    EngineInvariant(String),
    #[error("storage failure: {0}")]
    Storage(String),
}

impl EventError {
    /// Whether this failure is the deployment's problem rather than
    /// something the model or the harness can act on. An operational
    /// failure refuses wherever it happens, so the harness fails closed
    /// and an integration fault never reaches the model
    /// dressed as policy feedback. The match is exhaustive on purpose:
    /// a new variant has to pick a side.
    pub(crate) fn is_operational(&self) -> bool {
        match self {
            EventError::Storage(_)
            | EventError::UntrustedLog(_)
            | EventError::EngineInvariant(_)
            | EventError::Contended { .. }
            | EventError::UnexpectedDecision => true,
            EventError::CallOutstanding
            | EventError::TrajectoryEnded
            | EventError::UnknownTrajectory
            | EventError::TrajectoryExists
            | EventError::UnknownDispatch
            | EventError::OutcomeMismatch
            | EventError::UnknownOffer
            | EventError::NotAChild => false,
        }
    }
}

impl From<SessionError> for EventError {
    fn from(error: SessionError) -> EventError {
        match error {
            SessionError::AlreadyExists => EventError::TrajectoryExists,
            SessionError::Unknown => EventError::UnknownTrajectory,
            SessionError::Ended => EventError::TrajectoryEnded,
            SessionError::Storage(detail) => EventError::Storage(detail),
        }
    }
}

impl From<EngineRefusal> for EventError {
    fn from(refusal: EngineRefusal) -> EventError {
        match refusal {
            EngineRefusal::UntrustedLog { detail } => EventError::UntrustedLog(detail),
            EngineRefusal::Invariant { detail } => EventError::EngineInvariant(detail),
            EngineRefusal::ChildAlreadyForked => EventError::TrajectoryExists,
            EngineRefusal::Ended => EventError::TrajectoryEnded,
            EngineRefusal::DispatchClosed => EventError::UnknownDispatch,
        }
    }
}

pub struct Runtime {
    inner: Arc<Inner>,
}

struct Inner {
    store: Store,
    engine: EngineSeam,
    externals: ExternalServices,
    config: Config,
}

impl Runtime {
    /// Opens the modules, the engine, and the store. The `[policy]`
    /// table compiles through the documented dialect into the engine's
    /// registry — every surface and algebraic load lint runs here, and
    /// a policy this deployment cannot honor is refused before
    /// anything opens.
    pub fn open(config: Config, db: PathBuf, modules: Option<PathBuf>) -> Result<Runtime, OpenError> {
        let text = toml::to_string(&config.policy)
            .map_err(|error| OpenError::UnsupportedPolicy(format!("the policy table does not serialize: {error}")))?;
        let policy = appa_policy::Config::from_toml_str(&text).map_err(OpenError::Policy)?;
        validate_deployment(&policy, &config)?;
        let seam = EngineSeam::Real(RuntimeEngine::new(policy.engine().clone()));
        Runtime::with_engine(config, db, modules, seam)
    }

    /// The tests' entry: the same runtime over an engine seam whose
    /// queue the test controls, with no modules directory.
    #[cfg(test)]
    pub(crate) fn open_with_engine(config: Config, db: PathBuf, engine: EngineSeam) -> Result<Runtime, OpenError> {
        Runtime::with_engine(config, db, None, engine)
    }

    fn with_engine(
        config: Config,
        db: PathBuf,
        modules: Option<PathBuf>,
        engine: EngineSeam,
    ) -> Result<Runtime, OpenError> {
        let registry =
            crate::builtins::load(modules.as_deref()).map_err(|error| OpenError::Modules(error.to_string()))?;
        let externals = ExternalServices::new(config.externals.clone(), registry)
            .map_err(|error| OpenError::Modules(error.to_string()))?;
        let store = Store::open(&db).map_err(|error| match error {
            StoreError::Damaged { path, detail } => OpenError::Damaged(format!("{path}: {detail}")),
            error => OpenError::Storage(error.to_string()),
        })?;
        let digest = policy_digest(&config.policy);
        store.bind_policy_digest(&digest).map_err(|error| match error {
            StoreError::PolicyMismatch { stored, supplied } => OpenError::PolicyMismatch { stored, supplied },
            error => OpenError::Storage(error.to_string()),
        })?;
        Ok(Runtime {
            inner: Arc::new(Inner {
                store,
                engine,
                externals,
                config,
            }),
        })
    }

    /// Opens a fresh trajectory. Refuses an id that already exists: a
    /// reused harness id MUST NOT continue another trajectory's
    /// history (`POS-8`, harness binding).
    pub(crate) fn create_session(&self, id: TrajectoryId) -> Result<Session, SessionError> {
        self.inner.store.create_root(&id).map_err(|error| match error {
            crate::store::CreateError::AlreadyExists => SessionError::AlreadyExists,
            crate::store::CreateError::Storage(error) => SessionError::Storage(error.to_string()),
        })?;
        Ok(Session::attach(Arc::clone(&self.inner), id.clone(), id))
    }

    /// Reopens a persisted trajectory. There is no stored view: the
    /// next event rebuilds the engine's picture from the log.
    /// Missing or ended state is refused.
    pub(crate) fn session(&self, id: &TrajectoryId) -> Result<Session, SessionError> {
        let row = self
            .inner
            .store
            .trajectory(id)
            .map_err(|error| SessionError::Storage(error.to_string()))?
            .ok_or(SessionError::Unknown)?;
        if row.ended {
            return Err(SessionError::Ended);
        }
        Ok(Session::attach(Arc::clone(&self.inner), row.id, row.family))
    }

    pub fn status(&self, id: &TrajectoryId) -> Option<TrajectoryStatus> {
        let row = match self.inner.store.trajectory(id) {
            Ok(Some(row)) => row,
            Ok(None) => {
                tracing::debug!(trajectory = %id.0, "status read refused: unknown trajectory");
                return None;
            }
            Err(error) => {
                tracing::debug!(trajectory = %id.0, %error, "status read refused at the store");
                return None;
            }
        };
        if row.family != row.id {
            tracing::debug!(trajectory = %id.0, "status read refused: not a root trajectory");
            return None;
        }
        let (log, _revision) = match self.inner.store.load_log(&row.family) {
            Ok(log) => log,
            Err(error) => {
                tracing::debug!(trajectory = %id.0, %error, "status read refused at the store");
                return None;
            }
        };
        let view = match self.inner.engine.rebuild_view(&log, id) {
            Ok(view) => view,
            Err(refusal) => {
                tracing::warn!(trajectory = %id.0, %refusal, "status read refused the persisted log");
                return None;
            }
        };
        self.inner.engine.trajectory_status(&view)
    }

    /// Which trajectory a surfaced offer routes to. The MCP endpoint
    /// uses this to find the session an `execute_remedy_plan` call
    /// belongs to; liveness stays the engine's judgment.
    pub(crate) fn offer_trajectory(&self, offer: &OfferId) -> Result<Option<TrajectoryId>, SessionError> {
        self.inner
            .store
            .offer_trajectory(offer)
            .map_err(|error| SessionError::Storage(error.to_string()))
    }
}

fn validate_deployment(policy: &appa_policy::Config, config: &Config) -> Result<(), OpenError> {
    let profile = policy.engine().profile();
    if profile.starting_label() != &appa_engine::profile::neutral_starting_label(policy.registry().trust_chain()) {
        return Err(OpenError::UnsupportedPolicy(
            "[deployment] starting_label — the root fold is not yet initialized from the opening record (T17/T40), so a non-neutral starting label would silently not apply".to_string(),
        ));
    }
    if profile.binding() == appa_engine::profile::BindingMode::Token {
        return Err(OpenError::UnsupportedPolicy(
            "[deployment] binding = \"token\" — this runtime binds trajectories by harness session ids"
                .to_string(),
        ));
    }
    if profile.provider_surfaces().next().is_some() {
        return Err(OpenError::UnsupportedPolicy(
            "[deployment] provider_surfaces — this runtime never sees provider requests, so it can neither mediate a surface nor strip an undeclared one".to_string(),
        ));
    }
    if policy.registry().provider_run_contracts().next().is_some() {
        return Err(OpenError::UnsupportedPolicy(
            "[deployment] provider_run_tools — this runtime never sees inference responses, so it cannot admit a provider-run result".to_string(),
        ));
    }

    let rc = policy.registry_config();
    // Cast resolution is not wired in this runtime: an accepted
    // declaration would sit inert while unestablished blocks stay
    // terminal, so the declaration itself is refused.
    if !rc.casts.is_empty() {
        return Err(OpenError::UnsupportedPolicy(
            "[[cast]] declarations — cast resolution is not wired in this runtime".to_string(),
        ));
    }
    for tool in &rc.tools {
        let name = tool.name.as_str();
        if tool.pending_cast_dim().is_some() {
            return Err(OpenError::UnsupportedPolicy(format!(
                "tool {name} declares a pending-cast (\"unknown\") delta — cast resolution is not wired in this runtime"
            )));
        }
        if is_control_tool(name) {
            return Err(OpenError::ReservedTool(name.to_string()));
        }
    }
    for authority in &rc.authorities {
        let name = authority.name.as_str();
        if !config.externals.authorities.contains_key(name) {
            return Err(OpenError::UnboundExternal {
                kind: "authority",
                name: name.to_string(),
            });
        }
    }
    for sanitizer in &rc.sanitizers {
        let name = sanitizer.name.as_str();
        if !config.externals.sanitizers.contains_key(name) {
            return Err(OpenError::UnboundExternal {
                kind: "sanitizer",
                name: name.to_string(),
            });
        }
    }
    let dynamic_binding = rc.tools.iter().find_map(|tool| {
        crate::engine::dynamic_bindings(tool)
            .next()
            .map(|binding| binding.resolver.as_str().to_string())
    });
    if let Some(resolver) = dynamic_binding
        && config.externals.dynamic.is_none()
    {
        return Err(OpenError::UnboundExternal {
            kind: "dynamic resolver",
            name: resolver,
        });
    }
    Ok(())
}

fn policy_digest(policy: &toml::Value) -> String {
    use sha2::{Digest, Sha256};
    let canonical = toml::to_string(policy).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Plain-data helpers for tests outside this module — the adapter and
/// MCP tests — so they can drive the test seam without naming the
/// boundary (the source-scan structural guard holds for test
/// code too).
#[cfg(test)]
pub(crate) mod testing {
    use crate::engine::{
        EngineDecision, EngineSeam, Feedback, Next, OfferMutations, Presentation, ReleasedCall, TestSeam,
    };

    use super::{Config, DispatchId, OfferId, ProposedCall, Runtime};

    pub(crate) fn runtime(config: Config, db: std::path::PathBuf) -> Runtime {
        Runtime::open_with_engine(config, db, EngineSeam::Test(TestSeam::new())).expect("a fresh test runtime opens")
    }

    pub(crate) fn fail_next_commit(runtime: &Runtime) {
        runtime.inner.store.fail_next_commit();
    }

    fn enqueue(runtime: &Runtime, then: Next) {
        let EngineSeam::Test(seam) = &runtime.inner.engine else {
            panic!("testing helpers drive the test seam only");
        };
        seam.enqueue(EngineDecision {
            append: None,
            then,
            offers: OfferMutations::default(),
            ends_child: None,
        });
    }

    pub(crate) fn enqueue_done(runtime: &Runtime) {
        enqueue(runtime, Next::Done);
    }

    pub(crate) fn enqueue_release(runtime: &Runtime, dispatch: &str, tool: &str, arguments: &serde_json::Value) {
        let call = ProposedCall {
            tool: tool.to_string(),
            arguments: arguments.clone(),
        };
        enqueue(
            runtime,
            Next::ModelResponse {
                invocations: vec![ReleasedCall {
                    dispatch: DispatchId(dispatch.to_string()),
                    tool: call.tool.clone(),
                    bytes: serde_json::to_vec(&call).expect("the test call serializes"),
                }],
                feedback: Vec::new(),
            },
        );
    }

    pub(crate) fn enqueue_deny(runtime: &Runtime, feedback: &str, offers: &[&str]) {
        enqueue(
            runtime,
            Next::ModelResponse {
                invocations: Vec::new(),
                feedback: vec![Feedback {
                    text: feedback.to_string(),
                    offers: offers.iter().map(|id| OfferId(id.to_string())).collect(),
                }],
            },
        );
    }

    pub(crate) fn enqueue_keep_output(runtime: &Runtime) {
        enqueue(runtime, Next::PresentToModel(Presentation::KeepOutput));
    }

    pub(crate) fn enqueue_replace_output(runtime: &Runtime, placeholder: &str) {
        enqueue(
            runtime,
            Next::PresentToModel(Presentation::ReplaceOutput {
                placeholder: placeholder.to_string(),
            }),
        );
    }

    pub(crate) fn enqueue_value(runtime: &Runtime, value: &str) {
        enqueue(
            runtime,
            Next::PresentToModel(Presentation::Value {
                value: value.to_string(),
            }),
        );
    }
}
