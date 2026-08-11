//! The runtime API: `Runtime` and `Session` — the harness-agnostic
//! boundary declared in `docs/runtime.md`.

mod session;

use std::path::PathBuf;
use std::sync::Arc;

pub use session::Session;

use crate::config::Config;
use crate::external::ExternalServices;
use crate::mock_engine::MockEngine;
use crate::store::{Store, StoreError};

/// Identity of one trajectory (root or child). The adapter derives it
/// from the harness's own ids with a harness prefix; there is no
/// translation table.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TrajectoryId(pub String);

/// Identity of one open dispatch: a released call the harness is
/// executing.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DispatchId(pub String);

/// Identity of one remedy offer. Engine-derived and unguessable:
/// naming it proves the model read the offer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OfferId(pub String);

/// A tool call as the model proposed it. The engine canonicalizes;
/// the runtime and the adapter pass it through unchanged.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ProposedCall {
    pub tool: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChildTask(pub String);

/// What a dispatched tool produced, in the runtime's typing. The
/// adapter owns the mapping from its harness's wire to this type.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolOutcome {
    Success { body: OutcomeBody },
    Failure { message: String },
    Indeterminate,
}

/// The tool output body, where available. `Unavailable` records a
/// success whose body the runtime refused to carry (for example, over
/// the byte cap).
#[derive(Debug, Clone, PartialEq)]
pub enum OutcomeBody {
    Available(String),
    Unavailable,
}

/// An authorized call: the exact canonical bytes the harness must now
/// execute, never re-rendered and never edited.
#[derive(Debug, Clone, PartialEq)]
pub struct AuthorizedCall {
    pub tool: String,
    pub bytes: Vec<u8>,
}

/// Answer to a proposed tool call: run it, or do not — `feedback`
/// tells the model why not and what its options are. An allowed call
/// always runs with the arguments the model proposed; input
/// substitution is outside this runtime's coverage.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolCallDecision {
    Allow { dispatch: DispatchId },
    Deny { feedback: String },
}

/// What the adapter gives the harness as the tool output. `Keep`: use
/// the output as it is. `Replace`: use this text instead — a cleaned
/// version, or a short note saying the real output was not accepted.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolResultDecision {
    Keep,
    Replace { placeholder: String },
}

/// Outcome of one `execute_remedy_plan(offer_id)` call. `Declined` is
/// a denial: it ends every offer naming the denying authority for this
/// call. `NoAnswer` leaves the offer standing.
#[derive(Debug, Clone, PartialEq)]
pub enum RemedyDecision {
    Authorized { dispatch: DispatchId, call: AuthorizedCall },
    Returned { value: String },
    Staged { feedback: String },
    Declined { feedback: String },
    NoAnswer { feedback: String },
}

/// What happens to the child's final message: delivered to the parent,
/// nothing returned, or delivery stopped. The child
/// is finished, so `feedback` goes to the parent as the spawn call's
/// outcome and names the options by `OfferId`.
#[derive(Debug, Clone, PartialEq)]
pub enum ChildReturnDecision {
    Returned { value: String },
    NoValue,
    Blocked { feedback: String },
}

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("configuration refused: {0}")]
    Config(#[from] crate::config::ConfigError),
    #[error("the database is damaged: {0}")]
    Damaged(String),
    #[error("the database belongs to policy digest {stored}, not {supplied}")]
    PolicyMismatch { stored: String, supplied: String },
    #[error("storage failure: {0}")]
    Storage(String),
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
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
pub enum EventError {
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
    #[error("no live offer with this id exists")]
    UnknownOffer,
    #[error("only a child trajectory submits a return")]
    NotAChild,
    #[error("the family log stayed contended after {attempts} replays")]
    Contended { attempts: u32 },
    #[error("the engine returned a follow-up this event cannot deliver")]
    UnexpectedDecision,
    #[error("storage failure: {0}")]
    Storage(String),
}

pub struct Runtime {
    inner: Arc<Inner>,
}

struct Inner {
    store: Store,
    engine: MockEngine,
    externals: ExternalServices,
    config: Config,
}

impl Runtime {
    /// Opens the store and the engine. The policy would be validated
    /// against the coverage declaration here; the
    /// mock engine skips that and says so loudly.
    pub fn open(config: Config, db: PathBuf) -> Result<Runtime, OpenError> {
        Runtime::with_engine(config, db, MockEngine::permissive())
    }

    /// The tests' entry: the same runtime over an engine whose queue
    /// the test controls.
    #[cfg(test)]
    pub(crate) fn open_with_engine(config: Config, db: PathBuf, engine: MockEngine) -> Result<Runtime, OpenError> {
        Runtime::with_engine(config, db, engine)
    }

    fn with_engine(config: Config, db: PathBuf, engine: MockEngine) -> Result<Runtime, OpenError> {
        let store = Store::open(&db).map_err(|error| match error {
            StoreError::Damaged { path, detail } => OpenError::Damaged(format!("{path}: {detail}")),
            error => OpenError::Storage(error.to_string()),
        })?;
        let digest = policy_digest(&config.policy);
        store.bind_policy_digest(&digest).map_err(|error| match error {
            StoreError::PolicyMismatch { stored, supplied } => OpenError::PolicyMismatch { stored, supplied },
            error => OpenError::Storage(error.to_string()),
        })?;
        let externals = ExternalServices::new(config.externals.clone());
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
    pub fn create_session(&self, id: TrajectoryId) -> Result<Session, SessionError> {
        self.inner.store.create_root(&id).map_err(|error| match error {
            crate::store::CreateError::AlreadyExists => SessionError::AlreadyExists,
            crate::store::CreateError::Storage(error) => SessionError::Storage(error.to_string()),
        })?;
        Ok(Session::attach(Arc::clone(&self.inner), id.clone(), id))
    }

    /// Reopens a persisted trajectory. There is no stored view: the
    /// next event rebuilds the engine's picture from the log.
    /// Missing or ended state is refused.
    pub fn session(&self, id: &TrajectoryId) -> Result<Session, SessionError> {
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

    /// Which trajectory a surfaced offer routes to. The MCP endpoint
    /// uses this to find the session an `execute_remedy_plan` call
    /// belongs to; liveness stays the engine's judgment.
    pub(crate) fn offer_trajectory(&self, offer: &OfferId) -> Result<Option<TrajectoryId>, SessionError> {
        self.inner
            .store
            .offer_trajectory(offer)
            .map_err(|error| SessionError::Storage(error.to_string()))
    }

    /// The byte cap the deployment declares for carried bodies. The
    /// adapter applies it to tool outputs (Q14 mapping).
    pub(crate) fn max_body_bytes(&self) -> usize {
        self.inner.config.externals.max_body_bytes
    }
}

fn policy_digest(policy: &toml::Value) -> String {
    use sha2::{Digest, Sha256};
    let canonical = toml::to_string(policy).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Plain-data helpers for tests outside this module — the adapter and
/// MCP tests — so they can drive the test-mode engine without naming
/// the boundary (`IMP-1`; the source-scan test holds for test code
/// too).
#[cfg(test)]
pub(crate) mod testing {
    use crate::mock_engine::{EngineDecision, Feedback, MockEngine, Next, Presentation, ReleasedCall};

    use super::{Config, DispatchId, OfferId, ProposedCall, Runtime};

    pub(crate) fn runtime(config: Config, db: std::path::PathBuf) -> Runtime {
        Runtime::open_with_engine(config, db, MockEngine::test_mode()).expect("a fresh test runtime opens")
    }

    fn enqueue(runtime: &Runtime, then: Next) {
        runtime.inner.engine.enqueue(EngineDecision { append: None, then });
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
                offers: Vec::new(),
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
