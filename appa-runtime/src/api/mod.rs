//! The runtime API: `Runtime` and `Session` — the harness-agnostic
//! event model this crate declares.

mod session;

/// The fixture-only `Value` → raw-bytes helper, shared with the other
/// modules' test suites.
#[cfg(test)]
pub(crate) use session::raw;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

pub use crate::engine::{AuditEntry, AuditEvent, AuditLabel, DispatchOutcome, TrajectoryStatus};
pub use appa_runtime_api::{Actor, OutcomeBody, ProposedCall, SpawnBinding, SpawnRef, ToolOutcome, TrajectoryId};
pub(crate) use session::{LateOpen, Session, is_control_tool};

use crate::config::Config;
use crate::elicit::Elicitation;
use crate::engine::{EngineRefusal, EngineSeam, Liveness, PolicyEngine, RuntimeEngine};
use crate::external::ExternalServices;
use appa_eventlog::{Backend, Log, LogStore};

/// Identity of one open dispatch: a released call the harness is
/// executing. Runtime-internal: outcomes correlate by the
/// call's canonical bytes, never by an id an adapter
/// carries.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct DispatchId(pub String);

/// One remedy offer as it is quoted and carried.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OfferId(pub String);

/// The exact call the harness must now propose: the engine's canonical
/// bytes, never re-rendered and never edited.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ExactCall {
    pub tool: String,
    pub bytes: Vec<u8>,
}

impl ExactCall {
    fn proposed(self) -> ProposedCall {
        let text = String::from_utf8(self.bytes).expect("canonical argument bytes are UTF-8 JSON");
        ProposedCall {
            tool: self.tool,
            arguments: serde_json::value::RawValue::from_string(text)
                .expect("canonical argument bytes are one JSON value"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ToolCallDecision {
    Allow { spawn: Option<SpawnBinding> },
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
    Authorized { call: ExactCall },
    Substituted { call: ExactCall },
    Returned { value: String },
    Declined { feedback: String },
    NoAnswer { feedback: String },
}

/// What one whole `execute_remedy_plan` act produced: the engine's
/// answer, or the control channel's own refusal. `Refused` covers a
/// quote this trajectory pursues no offer for, an offer already
/// executing, and a storage failure — never an engine decision, and it
/// never says which.
#[derive(Debug, Clone, PartialEq)]
pub enum RemedyOutcome {
    Authorized { call: ProposedCall },
    Substituted { call: ProposedCall },
    Returned { value: String },
    Declined { feedback: String },
    NoAnswer { feedback: String },
    Refused { detail: String },
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

/// What a spawn call's result produced: the child's return, when
/// this call branched and the child the harness names is the one bound to
/// its fork; or an ordinary tool result, when the deployment did not
/// branch on this call.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SpawnResultDecision {
    Return(ChildReturnDecision),
    Outcome(ToolResultDecision),
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
    #[error(
        "cast {0} declares a constant, which the engine answers from the policy — remove its [externals.casts] binding"
    )]
    BoundConstantCast(String),
    #[error("the database is damaged: {0}")]
    Damaged(String),
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
    #[error(
        "the substituted {tool} call did not run and is now closed; propose your call again (a substituted call needs a fresh offer)"
    )]
    SubstitutionAbandoned { tool: String },
    #[error("the trajectory has ended")]
    TrajectoryEnded,
    #[error("the child has a call still open; report its outcome before the child ends")]
    ChildDispatchOpen,
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
    #[error("the spawn did not take: no prepared fork to open this child")]
    SpawnNotTaken,
    #[error("the family has more than one spawn in flight; the child cannot be tied to one")]
    SpawnAmbiguous,
    #[error("the fork and the child are already bound elsewhere")]
    BindingMismatch,
    #[error("the family log stayed contended after {attempts} replays")]
    Contended { attempts: u32 },
    #[error("the engine returned a follow-up this event cannot deliver")]
    UnexpectedDecision,
    #[error("the persisted log is refused: {0}")]
    UntrustedLog(String),
    #[error("the opening policy is unavailable: {0}")]
    PolicyUnavailable(String),
    #[error("engine invariant breach: {0}")]
    EngineInvariant(String),
    #[error("dynamic resolver {resolver} gave no usable answer ({reason}); the call was not checked")]
    ResolverUnavailable { resolver: String, reason: String },
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
            | EventError::PolicyUnavailable(_)
            | EventError::EngineInvariant(_)
            | EventError::Contended { .. }
            | EventError::ResolverUnavailable { .. }
            | EventError::UnexpectedDecision => true,
            EventError::CallOutstanding
            | EventError::SubstitutionAbandoned { .. }
            | EventError::TrajectoryEnded
            | EventError::ChildDispatchOpen
            | EventError::UnknownTrajectory
            | EventError::TrajectoryExists
            | EventError::UnknownDispatch
            | EventError::OutcomeMismatch
            | EventError::UnknownOffer
            | EventError::NotAChild
            | EventError::SpawnNotTaken
            | EventError::SpawnAmbiguous
            | EventError::BindingMismatch => false,
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
            EngineRefusal::OpeningMismatch { detail } => EventError::PolicyUnavailable(detail),
            EngineRefusal::Invariant { detail } => EventError::EngineInvariant(detail),
            EngineRefusal::Ended => EventError::TrajectoryEnded,
            EngineRefusal::DispatchClosed => EventError::UnknownDispatch,
            EngineRefusal::UnknownOffer => EventError::UnknownOffer,
            EngineRefusal::Unbindable => EventError::BindingMismatch,
        }
    }
}

/// How many claude-code consults may run at once across this runtime — subprocesses are
/// the one external whose cost is a full model call, so the gate is fixed and small.
const CLAUDE_CONSULT_PERMITS: usize = 4;

/// Everything one policy file settles: the file itself, the engine
/// compiled from it, and the implementations its `[externals]` bind.
/// A reload replaces the whole value; no field ever changes alone.
pub(crate) struct Deployment {
    config: Config,
    resident: RuntimeEngine,
    externals: ExternalServices,
}

impl Deployment {
    fn load(
        config: Config,
        modules: &crate::builtins::ModuleRegistry,
        claude_permits: Arc<tokio::sync::Semaphore>,
    ) -> Result<Deployment, OpenError> {
        let policy = compile_policy(&config)?;
        validate_deployment(&policy, &config.externals)?;
        let dynamic_builtins = policy
            .dynamic_resolver_builtins()
            .map(|(name, builtin)| (name.as_str().to_string(), builtin.clone()))
            .collect();
        let externals = ExternalServices::new(config.externals.clone(), modules, dynamic_builtins, claude_permits)
            .map_err(|error| OpenError::Modules(error.to_string()))?;
        Ok(Deployment {
            config,
            resident: RuntimeEngine::new(policy.engine().clone()),
            externals,
        })
    }

    fn resident(&self) -> PolicyEngine<'_> {
        PolicyEngine::Resident(&self.resident)
    }

    fn root_opening(&self, trajectory: &TrajectoryId) -> Vec<appa_engine::fact::Fact> {
        self.resident
            .root_opening(trajectory, self.config.policy_file().bytes())
    }
}

/// What a reload installed. The key identifies the exact file bytes;
/// the identity is what a root's opening record names.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Reloaded {
    pub policy_key: String,
    pub policy_identity: String,
    /// `false` when the new file's bytes are the ones already serving:
    /// the reload still ran every gate, and swapped an equal deployment.
    pub changed: bool,
}

pub struct Runtime {
    inner: Arc<Inner>,
}

struct Inner {
    deployment: std::sync::RwLock<Arc<Deployment>>,
    retired: std::sync::Mutex<std::collections::BTreeMap<String, Arc<RuntimeEngine>>>,
    store: LogStore,
    engine: EngineSeam,
    modules: crate::builtins::ModuleRegistry,
    executing: std::sync::Mutex<std::collections::BTreeSet<String>>,
    permits: std::sync::Mutex<std::collections::BTreeMap<String, Vec<Actor>>>,
    /// One gate for every claude-code consult this runtime runs; deployment reloads clone
    /// it, so old and new snapshots contend on the same permits.
    claude_permits: Arc<tokio::sync::Semaphore>,
}

impl Inner {
    fn deployment(&self) -> Arc<Deployment> {
        Arc::clone(
            &self
                .deployment
                .read()
                .expect("the deployment lock is never poisoned: no panic runs while it is held"),
        )
    }

    pub(super) fn resolve_policy<'a>(
        &self,
        deployment: &'a Deployment,
        log: &Log,
    ) -> Result<PolicyEngine<'a>, EventError> {
        let opened = self.engine.opened_under(log).ok_or_else(|| {
            EventError::PolicyUnavailable(format!(
                "the log of {} does not open with its opening record",
                log.root().as_str()
            ))
        })?;
        if crate::engine::policy_file_key(log.policy_file()) != opened.policy_file_key {
            return Err(EventError::PolicyUnavailable(format!(
                "the stored policy file does not hash to the key {} its opening names",
                opened.policy_file_key
            )));
        }
        let policy =
            if crate::engine::policy_file_key(deployment.config.policy_file().bytes()) == opened.policy_file_key {
                deployment.resident()
            } else {
                PolicyEngine::Retired(self.retired_engine(&opened.policy_file_key, log.policy_file())?)
            };
        if policy.identity_hex() != opened.policy_identity {
            return Err(EventError::PolicyUnavailable(format!(
                "the stored policy file compiles to a different identity than the opening of {}",
                log.root().as_str()
            )));
        }
        Ok(policy)
    }

    fn retired_engine(&self, key: &str, bytes: &[u8]) -> Result<Arc<RuntimeEngine>, EventError> {
        if let Some(engine) = self
            .retired
            .lock()
            .expect("the retired-engine mutex is never poisoned: no panic runs while it is held")
            .get(key)
        {
            return Ok(Arc::clone(engine));
        }
        let compiled = compile_stored_policy(bytes).map_err(EventError::PolicyUnavailable)?;
        let engine = Arc::new(RuntimeEngine::new(compiled.engine().clone()));
        self.retired
            .lock()
            .expect("the retired-engine mutex is never poisoned: no panic runs while it is held")
            .insert(key.to_string(), Arc::clone(&engine));
        Ok(engine)
    }

    pub(super) fn log(&self, root: &TrajectoryId) -> Result<Log, EventError> {
        self.store
            .log(&crate::engine::engine_id(root))
            .map_err(|error| match error {
                appa_eventlog::ReadError::UnknownRoot { .. } => EventError::UnknownTrajectory,
                appa_eventlog::ReadError::Undecodable(detail) => EventError::UntrustedLog(detail),
                error @ appa_eventlog::ReadError::PolicyFileMissing { .. } => {
                    EventError::PolicyUnavailable(error.to_string())
                }
                error => EventError::Storage(error.to_string()),
            })
    }
}

impl Runtime {
    /// Opens the modules, the engine, and the store. The `[policy]`
    /// table compiles through the documented dialect into the engine's
    /// registry — every surface and algebraic load lint runs here, and
    /// a policy this deployment cannot honor is refused before
    /// anything opens.
    pub fn open(config: Config, db: PathBuf, modules: Option<PathBuf>) -> Result<Runtime, OpenError> {
        Runtime::with_engine(config, db, modules, EngineSeam::Real)
    }

    /// The tests' entry: the same runtime with decisions from the
    /// enqueued queue and no modules directory. Every gate `open` runs
    /// runs here too — only the decisions are fake.
    #[cfg(test)]
    pub(crate) fn open_with_engine(
        config: Config,
        db: PathBuf,
        seam: crate::engine::TestSeam,
    ) -> Result<Runtime, OpenError> {
        Runtime::with_engine(config, db, None, EngineSeam::Test(seam))
    }

    fn with_engine(
        config: Config,
        db: PathBuf,
        modules: Option<PathBuf>,
        engine: EngineSeam,
    ) -> Result<Runtime, OpenError> {
        let modules =
            crate::builtins::load(modules.as_deref()).map_err(|error| OpenError::Modules(error.to_string()))?;
        let claude_permits = Arc::new(tokio::sync::Semaphore::new(CLAUDE_CONSULT_PERMITS));
        let deployment = Deployment::load(config, &modules, Arc::clone(&claude_permits))?;
        let store = LogStore::open(Backend::Sqlite { path: db }).map_err(|error| match error {
            appa_eventlog::OpenError::Damaged { path, detail } => OpenError::Damaged(format!("{path}: {detail}")),
            error @ appa_eventlog::OpenError::ForeignSchema { .. } => OpenError::Damaged(error.to_string()),
            error => OpenError::Storage(error.to_string()),
        })?;
        Ok(Runtime {
            inner: Arc::new(Inner {
                deployment: std::sync::RwLock::new(Arc::new(deployment)),
                retired: std::sync::Mutex::new(std::collections::BTreeMap::new()),
                store,
                engine,
                modules,
                executing: std::sync::Mutex::new(std::collections::BTreeSet::new()),
                permits: std::sync::Mutex::new(std::collections::BTreeMap::new()),
                claude_permits,
            }),
        })
    }

    /// Replace the serving deployment with the one this configuration
    /// declares, without stopping the process (
    /// reloading a policy). The caller reads the file; the runtime never
    /// learns where a configuration came from, so an embedding host
    /// reloads a composed policy the same way.
    pub fn reload(&self, config: Config) -> Result<Reloaded, OpenError> {
        let deployment = Deployment::load(config, &self.inner.modules, Arc::clone(&self.inner.claude_permits))?;
        let identity = deployment.resident().identity_hex();
        let deployment = Arc::new(deployment);
        let previous = {
            let mut serving = self
                .inner
                .deployment
                .write()
                .expect("the deployment lock is never poisoned: no panic runs while it is held");
            std::mem::replace(&mut *serving, Arc::clone(&deployment))
        };
        let key = crate::engine::policy_file_key(deployment.config.policy_file().bytes());
        let changed = crate::engine::policy_file_key(previous.config.policy_file().bytes()) != key;
        tracing::info!(
            policy_key = %key,
            policy_identity = %identity,
            changed,
            "reloaded the serving deployment"
        );
        Ok(Reloaded {
            policy_key: key,
            policy_identity: identity,
            changed,
        })
    }

    /// Opens a fresh root. Refuses an id whose log already exists: a
    /// reused harness id MUST NOT continue another trajectory's history
    /// One transaction writes the opening
    /// record and stores the policy file it names, so the root is bound
    /// to that file durably or is not opened at all.
    pub(crate) fn create_session(&self, id: TrajectoryId) -> Result<Session, SessionError> {
        let deployment = self.inner.deployment();
        let opening = deployment.root_opening(&id);
        let root = self
            .inner
            .store
            .create_root(opening, deployment.config.policy_file().bytes())
            .map_err(|error| match error {
                appa_eventlog::CreateError::AlreadyExists { .. } => SessionError::AlreadyExists,
                error => SessionError::Storage(error.to_string()),
            })?;
        let root = TrajectoryId(root.as_str().to_string());
        Ok(Session::attach(Arc::clone(&self.inner), deployment, root.clone(), root))
    }

    /// Reopens a persisted trajectory. There is no stored view: the next
    /// event rebuilds the engine's picture from the log.
    pub(crate) fn session(&self, root: &TrajectoryId, trajectory: &TrajectoryId) -> Result<Session, SessionError> {
        let known = self
            .inner
            .store
            .has_root(&crate::engine::engine_id(root))
            .map_err(|error| SessionError::Storage(error.to_string()))?;
        if !known {
            return Err(SessionError::Unknown);
        }
        Ok(Session::attach(
            Arc::clone(&self.inner),
            self.inner.deployment(),
            trajectory.clone(),
            root.clone(),
        ))
    }

    /// Whether this trajectory still accepts events. One view
    /// rebuild, for the two callers that have no following engine event to
    /// carry the refusal: the session-start hook, and the start-after-lazy-open
    /// race. Every other path refuses inside the event it is already deciding.
    pub(crate) fn live(&self, root: &TrajectoryId, trajectory: &TrajectoryId) -> Result<(), SessionError> {
        let log = match self.inner.log(root) {
            Ok(log) => log,
            Err(EventError::UnknownTrajectory) => return Err(SessionError::Unknown),
            Err(error) => return Err(SessionError::Storage(error.to_string())),
        };
        let deployment = self.inner.deployment();
        let policy = self
            .inner
            .resolve_policy(&deployment, &log)
            .map_err(|error| SessionError::Storage(error.to_string()))?;
        let view = self
            .inner
            .engine
            .rebuild_view(&policy, &log)
            .map_err(|refusal| SessionError::Storage(refusal.to_string()))?;
        match self.inner.engine.liveness(&view, trajectory) {
            Liveness::Unopened => Err(SessionError::Unknown),
            Liveness::Ended => Err(SessionError::Ended),
            Liveness::Live => Ok(()),
        }
    }

    pub fn status(&self, id: &TrajectoryId) -> Option<TrajectoryStatus> {
        let deployment = self.inner.deployment();
        let (policy, log) = self.root_log(&deployment, id, "status")?;
        let view = match self.inner.engine.rebuild_view(&policy, &log) {
            Ok(view) => view,
            Err(refusal) => {
                tracing::warn!(trajectory = %id.0, %refusal, "status read refused the persisted log");
                return None;
            }
        };
        self.inner.engine.trajectory_status(&policy, &view, id)
    }

    /// Every decision this family's log recorded, in log order.
    /// A projection like
    /// [`Runtime::status`]: it gates nothing, appends nothing, and
    /// expires no offer, and it answers for an ended
    /// trajectory because an audit is read after the run.
    pub fn audit(&self, id: &TrajectoryId) -> Option<Vec<AuditEntry>> {
        let deployment = self.inner.deployment();
        let (policy, log) = self.root_log(&deployment, id, "audit")?;
        match self.inner.engine.audit(&policy, &log) {
            Ok(entries) => entries,
            Err(refusal) => {
                tracing::warn!(trajectory = %id.0, %refusal, "audit read refused the persisted log");
                None
            }
        }
    }

    fn root_log<'a>(
        &self,
        deployment: &'a Deployment,
        id: &TrajectoryId,
        read: &str,
    ) -> Option<(PolicyEngine<'a>, Log)> {
        let log = match self.inner.log(id) {
            Ok(log) => log,
            Err(error) => {
                tracing::debug!(trajectory = %id.0, read, %error, "read refused: no log for this root");
                return None;
            }
        };
        match self.inner.resolve_policy(deployment, &log) {
            Ok(policy) => Some((policy, log)),
            Err(error) => {
                tracing::warn!(trajectory = %id.0, read, %error, "read refused: the opening policy is unavailable");
                None
            }
        }
    }

    /// Execute one surfaced remedy offer by its id.
    pub async fn execute_remedy(&self, acting: &Actor, offer: OfferId) -> RemedyOutcome {
        self.remedy(acting, offer, None).await
    }

    /// The whole act: resolve the quoted id inside the acting trajectory's
    /// own family, claim the offer, and answer. `elicitation` is supplied
    /// rather than extracted, so the body is reachable without a live peer.
    pub(crate) async fn remedy(
        &self,
        acting: &Actor,
        quoted: OfferId,
        elicitation: Option<&Elicitation>,
    ) -> RemedyOutcome {
        let unknown = || RemedyOutcome::Refused {
            detail: "no live offer with this id exists".to_string(),
        };
        let root = acting.root.clone();
        let trajectory = acting.child.clone().unwrap_or_else(|| root.clone());
        let Some((offer, pursuer)) = self.resolve_in(&root, &quoted) else {
            return unknown();
        };
        if pursuer != trajectory {
            return unknown();
        }
        self.spend_vouch(&quoted, acting);
        let Some(_claim) = self.claim_offer(&offer) else {
            return RemedyOutcome::Refused {
                detail: "this offer is already being executed".to_string(),
            };
        };
        let mut session = match self.session(&root, &pursuer) {
            Ok(session) => session,
            Err(error) => {
                return RemedyOutcome::Refused {
                    detail: error.to_string(),
                };
            }
        };
        match session.on_remedy(offer, elicitation).await {
            Ok(RemedyDecision::Authorized { call }) => RemedyOutcome::Authorized { call: call.proposed() },
            Ok(RemedyDecision::Substituted { call }) => RemedyOutcome::Substituted { call: call.proposed() },
            Ok(RemedyDecision::Returned { value }) => RemedyOutcome::Returned { value },
            Ok(RemedyDecision::Declined { feedback }) => RemedyOutcome::Declined { feedback },
            Ok(RemedyDecision::NoAnswer { feedback }) => RemedyOutcome::NoAnswer { feedback },
            Err(error) => RemedyOutcome::Refused {
                detail: error.to_string(),
            },
        }
    }

    /// The canonical identity a quoted id names in this family, and the
    /// trajectory that may execute it.
    pub(crate) fn resolve_in(&self, root: &TrajectoryId, quoted: &OfferId) -> Option<(OfferId, TrajectoryId)> {
        let log = self.inner.log(root).ok()?;
        let offer = crate::engine::resolve_rendered(&log, quoted)?;
        let deployment = self.inner.deployment();
        let policy = self.inner.resolve_policy(&deployment, &log).ok()?;
        let view = self.inner.engine.rebuild_view(&policy, &log).ok()?;
        let pursuer = self.inner.engine.offer_pursuer(&view, &offer)?;
        Some((offer, pursuer))
    }

    /// Record that this trajectory quoted this offer id, for the request
    /// that runs it.
    pub(crate) fn vouch(&self, quoted: &OfferId, acting: &Actor) {
        let mut permits = self.inner.permits.lock().expect("the permit mutex is never poisoned");
        let holders = permits.entry(quoted.0.clone()).or_default();
        if !holders.contains(acting) {
            holders.push(acting.clone());
        }
    }

    /// The trajectory vouched for this quoted id, taken once.
    pub(crate) fn take_vouched(&self, quoted: &OfferId) -> Option<Actor> {
        let mut permits = self.inner.permits.lock().expect("the permit mutex is never poisoned");
        let mut holders = permits.remove(&quoted.0)?;
        (holders.len() == 1).then(|| holders.remove(0))
    }

    fn spend_vouch(&self, quoted: &OfferId, acting: &Actor) {
        let mut permits = self.inner.permits.lock().expect("the permit mutex is never poisoned");
        let Some(holders) = permits.get_mut(&quoted.0) else {
            return;
        };
        holders.retain(|holder| holder != acting);
        if holders.is_empty() {
            permits.remove(&quoted.0);
        }
    }

    /// Claim one offer for the length of its execution, so two calls
    /// naming the same offer cannot both reach its authorities. A human
    /// review holds its call open for minutes, and without this the
    /// second call raises a second dialog for one decision.
    pub(crate) fn claim_offer(&self, offer: &OfferId) -> Option<OfferClaim> {
        let claimed = self
            .inner
            .executing
            .lock()
            .expect("the executing-offer mutex is never poisoned")
            .insert(offer.0.clone());
        claimed.then(|| OfferClaim {
            inner: Arc::clone(&self.inner),
            offer: offer.0.clone(),
        })
    }

    #[cfg(test)]
    pub(crate) fn log_facts(&self, root: &TrajectoryId) -> Vec<appa_engine::fact::Fact> {
        self.inner.log(root).expect("the log reads").facts().to_vec()
    }

    #[cfg(test)]
    pub(crate) fn log_basis(&self, root: &TrajectoryId) -> u64 {
        self.inner.log(root).expect("the log reads").basis()
    }

    #[cfg(test)]
    pub(crate) fn open_dispatches(
        &self,
        root: &TrajectoryId,
        trajectory: &TrajectoryId,
    ) -> Vec<crate::engine::OpenDispatch> {
        let log = self.inner.log(root).expect("the log reads");
        let deployment = self.inner.deployment();
        let policy = self
            .inner
            .resolve_policy(&deployment, &log)
            .expect("the opening policy resolves");
        let view = self.inner.engine.rebuild_view(&policy, &log).expect("the log rebuilds");
        self.inner.engine.open_dispatches(&view, trajectory)
    }

    /// Does the root's log name this trajectory, for the tests that
    /// assert on whether a child opened.
    #[cfg(test)]
    pub(crate) fn names_trajectory(&self, root: &TrajectoryId, trajectory: &TrajectoryId) -> bool {
        let log = self.inner.log(root).expect("the log reads");
        let deployment = self.inner.deployment();
        let policy = self
            .inner
            .resolve_policy(&deployment, &log)
            .expect("the opening policy resolves");
        let view = self.inner.engine.rebuild_view(&policy, &log).expect("the log rebuilds");
        self.inner.engine.liveness(&view, trajectory) != Liveness::Unopened
    }

    /// The substituted call a trajectory has standing, for the tests
    /// that assert on it: the open dispatch no proposal released.
    #[cfg(test)]
    pub(crate) fn substituted_release(
        &self,
        root: &TrajectoryId,
        trajectory: &TrajectoryId,
    ) -> Option<crate::engine::OpenDispatch> {
        let log = self.inner.log(root).expect("the log reads");
        let deployment = self.inner.deployment();
        let policy = self
            .inner
            .resolve_policy(&deployment, &log)
            .expect("the opening policy resolves");
        let view = self.inner.engine.rebuild_view(&policy, &log).expect("the log rebuilds");
        self.inner.engine.substituted_release(&view, trajectory)
    }

    /// Rebuild one root's view, scoped to a trajectory in it, for the tests
    /// that read a branch through the seam the root-only public surface does
    /// not expose.
    #[cfg(test)]
    pub(crate) fn branch_status(&self, root: &TrajectoryId, trajectory: &TrajectoryId) -> Option<TrajectoryStatus> {
        let log = self.inner.log(root).expect("the log reads");
        let deployment = self.inner.deployment();
        let policy = self
            .inner
            .resolve_policy(&deployment, &log)
            .expect("the policy resolves");
        let view = self.inner.engine.rebuild_view(&policy, &log).expect("the log rebuilds");
        self.inner.engine.trajectory_status(&policy, &view, trajectory)
    }

    /// Drive one event straight at the engine and take its refusal, for the
    /// tests that pin how a raced lifecycle classifies.
    #[cfg(test)]
    pub(crate) fn refuse(
        &self,
        root: &TrajectoryId,
        trajectory: &TrajectoryId,
        event: crate::engine::EngineEvent,
    ) -> EventError {
        let log = self.inner.log(root).expect("the log reads");
        let deployment = self.inner.deployment();
        let policy = self
            .inner
            .resolve_policy(&deployment, &log)
            .expect("the policy resolves");
        let view = self.inner.engine.rebuild_view(&policy, &log).expect("the log rebuilds");
        EventError::from(
            self.inner
                .engine
                .handle(&policy, &view, trajectory, event)
                .expect_err("the moved subject refuses the event"),
        )
    }

    /// The deployment's own policy file bytes, for tests that shape a stored
    /// file relative to it.
    #[cfg(test)]
    pub(crate) fn config_bytes(&self) -> Vec<u8> {
        self.inner.deployment().config.policy_file().bytes().to_vec()
    }

    #[cfg(test)]
    pub(crate) fn store(&self) -> &LogStore {
        &self.inner.store
    }

    #[cfg(test)]
    pub(crate) fn minted_offers(&self, root: &TrajectoryId, trajectory: &TrajectoryId) -> Vec<OfferId> {
        crate::engine::minted_offers(&self.inner.log(root).expect("the log reads"), trajectory)
    }

    #[cfg(test)]
    pub(crate) fn enqueue(&self, decision: crate::engine::EngineDecision) {
        self.inner.engine.enqueue(decision);
    }

    #[cfg(test)]
    pub(crate) fn engine_seen(&self) -> Vec<crate::engine::EngineEvent> {
        self.inner.engine.seen()
    }

    /// How long a human review may stay open before the runtime treats
    /// it as no answer. Deliberately unrelated to
    /// `[externals] timeout_ms`, which bounds a machine consult: a
    /// person reads the arguments and thinks.
    pub(crate) fn review_timeout(&self) -> std::time::Duration {
        self.inner.deployment().config.externals.review_timeout
    }
}

/// One offer's execution, released when the call that took it ends —
/// including by panic or by a client that walked away.
pub(crate) struct OfferClaim {
    inner: Arc<Inner>,
    offer: String,
}

impl Drop for OfferClaim {
    fn drop(&mut self) {
        self.inner
            .executing
            .lock()
            .expect("the executing-offer mutex is never poisoned")
            .remove(&self.offer);
    }
}

fn validate_deployment(policy: &appa_policy::Config, externals: &crate::config::Externals) -> Result<(), OpenError> {
    let profile = policy.engine().profile();
    if profile.binding() == appa_engine::profile::BindingMode::Token {
        return Err(OpenError::UnsupportedPolicy(
            "[deployment] binding = \"token\" — this runtime binds trajectories by harness session ids".to_string(),
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
    let dynamic_builtins: BTreeMap<_, _> = policy
        .dynamic_resolver_builtins()
        .map(|(resolver, builtin)| (resolver.as_str(), builtin.as_str()))
        .collect();
    for tool in &rc.tools {
        let name = tool.name.as_str();
        if is_control_tool(name) {
            return Err(OpenError::ReservedTool(name.to_string()));
        }
    }
    // A resolver-backed cast classifies over the wire, so it needs an endpoint. A
    // constant is answered from the policy itself and binds nothing — an endpoint bound
    // to one would never be called, so the deployment is refused rather than left
    // believing a classifier runs.
    for cast in &rc.casts {
        let name = cast.name.as_str();
        let bound = externals.casts.contains_key(name);
        match (&cast.resolution, bound) {
            (appa_engine::authority::CastResolution::Resolver { .. }, false) => {
                return Err(OpenError::UnboundExternal {
                    kind: "cast",
                    name: name.to_string(),
                });
            }
            (appa_engine::authority::CastResolution::Constant(_), true) => {
                return Err(OpenError::BoundConstantCast(name.to_string()));
            }
            _ => {}
        }
    }
    for authority in &rc.authorities {
        let name = authority.name.as_str();
        if !externals.authorities.contains_key(name) {
            return Err(OpenError::UnboundExternal {
                kind: "authority",
                name: name.to_string(),
            });
        }
    }
    if externals
        .sanitizers
        .contains_key(appa_engine::names::SanitizerName::ATTEST_SCHEMA)
    {
        return Err(OpenError::UnsupportedPolicy(
            "[externals] binds sanitizer attest-schema — the reserved builtin is applied by the engine itself and takes no implementation"
                .to_string(),
        ));
    }
    for sanitizer in &rc.sanitizers {
        let name = sanitizer.name.as_str();
        if sanitizer.name.is_attest_schema() {
            continue;
        }
        if !externals.sanitizers.contains_key(name) {
            return Err(OpenError::UnboundExternal {
                kind: "sanitizer",
                name: name.to_string(),
            });
        }
    }
    for tool in &rc.tools {
        for binding in &tool.resolvers {
            let name = binding.resolver.as_str();
            // A resolver with a declared builtin never uses the shared endpoint; every other
            // bound name requires it.
            if !dynamic_builtins.contains_key(name) && externals.dynamic.is_none() {
                return Err(OpenError::UnboundExternal {
                    kind: "dynamic resolver",
                    name: name.to_string(),
                });
            }
        }
    }
    if let Some(resolver) = &rc.membership
        && externals.membership.is_none()
    {
        return Err(OpenError::UnboundExternal {
            kind: "membership resolver",
            name: resolver.as_str().to_string(),
        });
    }
    Ok(())
}

fn compile_policy(config: &Config) -> Result<appa_policy::Config, OpenError> {
    let text = toml::to_string(config.policy_file().value())
        .map_err(|error| OpenError::UnsupportedPolicy(format!("the policy table does not serialize: {error}")))?;
    appa_policy::Config::from_toml_str(&text).map_err(OpenError::Policy)
}

fn compile_stored_policy(bytes: &[u8]) -> Result<appa_policy::Config, String> {
    let text = std::str::from_utf8(bytes).map_err(|error| format!("the stored policy file is not UTF-8: {error}"))?;
    let value: toml::Value =
        toml::from_str(text).map_err(|error| format!("the stored policy file does not parse: {error}"))?;
    let policy = value
        .get("policy")
        .ok_or("the stored policy file has no [policy] table")?;
    let text =
        toml::to_string(policy).map_err(|error| format!("the stored policy table does not serialize: {error}"))?;
    appa_policy::Config::from_toml_str(&text).map_err(|error| format!("the stored policy does not load: {error}"))
}

/// Plain-data helpers for tests outside this module — the adapter and
/// MCP tests — so they can drive the test seam without naming the
/// boundary (the source-scan structural guard holds for test
/// code too).
#[cfg(test)]
pub(crate) mod testing {
    use crate::engine::{EngineDecision, Feedback, Next, ReleasedCall, TestSeam};

    use super::{Config, DispatchId, OfferId, ProposedCall, Runtime};

    pub(crate) fn runtime(config: Config, db: std::path::PathBuf) -> Runtime {
        Runtime::open_with_engine(config, db, TestSeam::new()).expect("a fresh test runtime opens")
    }

    pub(crate) fn fail_next_commit(runtime: &Runtime) {
        runtime.inner.store.fail_commit_after(0);
    }

    fn enqueue(runtime: &Runtime, then: Next) {
        runtime.inner.engine.enqueue(EngineDecision { append: None, then });
    }

    pub(crate) fn enqueue_done(runtime: &Runtime) {
        enqueue(runtime, Next::Done);
    }

    fn engine_dispatch(label: &str) -> appa_engine::value::DispatchId {
        let policy = appa_policy::Config::from_toml_str("version = 1\n[[tool]]\nname = \"Bash\"\n")
            .expect("the fixture policy compiles");
        let engine = policy.engine().clone();
        let call = engine
            .resolve_call(appa_engine::value::ToolName::new("Bash"), br#"{"command":"ls"}"#)
            .expect("the fixture call resolves through the engine");
        appa_engine::value::DispatchId::new(appa_engine::value::TrajectoryId::new(label), call.digest(), 0)
    }

    pub(crate) fn spawn_binding(label: &str) -> super::SpawnBinding {
        let fork = appa_engine::value::ForkId::of(&engine_dispatch(label));
        super::SpawnBinding(serde_json::to_string(&fork).expect("a fork id serializes"))
    }

    fn wire_dispatch(label: &str) -> DispatchId {
        DispatchId(serde_json::to_string(&engine_dispatch(label)).expect("an engine dispatch id serializes"))
    }

    pub(crate) fn enqueue_release(runtime: &Runtime, dispatch: &str, tool: &str, arguments: &serde_json::Value) {
        let call = ProposedCall {
            tool: tool.to_string(),
            arguments: super::raw(arguments.clone()),
        };
        enqueue(
            runtime,
            Next::ModelResponse {
                invocations: vec![ReleasedCall {
                    dispatch: wire_dispatch(dispatch),
                    tool: call.tool.clone(),
                    bytes: serde_json::to_vec(&call).expect("the test call serializes"),
                    fork: None,
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
}

#[cfg(test)]
mod deployment_tests {
    fn test_permits() -> std::sync::Arc<tokio::sync::Semaphore> {
        std::sync::Arc::new(tokio::sync::Semaphore::new(4))
    }

    use std::collections::BTreeMap;
    use std::time::Duration;

    use super::*;
    use crate::config::Externals;

    fn claude_config(policy: &str) -> Config {
        Config::embedded(
            policy.to_string(),
            Externals {
                timeout: Duration::from_secs(30),
                review_timeout: Duration::from_secs(600),
                max_body_bytes: 65_536,
                authorities: BTreeMap::new(),
                sanitizers: BTreeMap::new(),
                casts: BTreeMap::new(),
                dynamic: None,
                membership: None,
                claude_code: Default::default(),
            },
        )
        .expect("the embedded configuration parses")
    }

    #[test]
    fn a_claude_builtin_deployment_opens_without_an_endpoint() {
        let tool_level = claude_config(
            r#"
                version = 1
                [[dynamic_resolver]]
                name = "classifier"
                builtin = "claude-code"
                [[tool]]
                name = "lookup"
                resolvers = [{ resolver = "classifier", returns = { delta = ["trust", "audience"], requires = ["attention"] } }]
            "#,
        );
        assert!(Deployment::load(tool_level, &crate::builtins::ModuleRegistry::empty(), test_permits()).is_ok());
    }

    #[test]
    fn a_reload_keeps_the_one_claude_consult_gate() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let config = || {
            claude_config(
                r#"
                version = 1
                [[dynamic_resolver]]
                name = "classifier"
                builtin = "claude-code"
                [[tool]]
                name = "fetch"
                resolvers = [{ resolver = "classifier", returns = { delta = ["trust"] } }]
            "#,
            )
        };
        let runtime = Runtime::open(config(), dir.path().join("appa.db"), None).expect("the deployment opens");
        let before = Arc::as_ptr(runtime.inner.deployment().externals.claude_permits());
        runtime.reload(config()).expect("the reload installs");
        let after = Arc::as_ptr(runtime.inner.deployment().externals.claude_permits());
        assert_eq!(
            before, after,
            "old and new deployment snapshots contend on the same permits"
        );
    }

    #[test]
    fn a_stored_policy_in_the_retired_resolver_syntax_refuses_before_replay() {
        // A history from before the unified resolver family carries its own policy bytes;
        // recompiling them is the trust gate, and it runs before any fact replays.
        let legacy = br#"
[policy]
version = 1
[[policy.dynamic_resolver]]
name = "directory"
[[policy.tool]]
name = "lookup"
parameters = { type = "object", properties = { customer = { type = "string" } }, required = ["customer"] }
delta = { audience = { resolver = "directory", argument = "customer" } }
"#;
        let refusal = compile_stored_policy(legacy).expect_err("the retired syntax does not compile");
        assert!(
            refusal.contains("the stored policy does not load"),
            "the refusal is loud and syntactic: {refusal}"
        );
    }

    #[test]
    fn the_shared_http_endpoint_covers_every_non_builtin_resolver() {
        let mut config = claude_config(
            r#"
                version = 1
                [[dynamic_resolver]]
                name = "bash-classifier"
                builtin = "claude-code"
                [[dynamic_resolver]]
                name = "other-classifier"
                [[tool]]
                name = "Bash"
                delta = {}
                resolvers = [{ resolver = "bash-classifier", returns = { requires = ["attention"] } }]
                [[tool]]
                name = "Other"
                delta = {}
                resolvers = [{ resolver = "other-classifier", returns = { requires = ["attention"] } }]
            "#,
        );
        config.externals.dynamic = Some(crate::config::Endpoint {
            url: "https://resolver.example".to_string(),
            token: None,
        });
        assert!(Deployment::load(config, &crate::builtins::ModuleRegistry::empty(), test_permits()).is_ok());
    }
}
