//! The runtime API: `Runtime` and `Session` — the harness-agnostic
//! event model this crate declares.

mod session;

/// The fixture-only `Value` → raw-bytes helper, shared with the other
/// modules' test suites.
#[cfg(test)]
pub(crate) use session::raw;

use std::path::PathBuf;
use std::sync::Arc;

pub use crate::engine::{
    AuditEntry, AuditEvent, AuditLabel, DispatchOutcome, LabelSpelling, RemedyArguments, TrajectoryStatus,
};
pub use appa_runtime_api::{
    Actor, OfferedRemedy, OutcomeBody, ProposedCall, SpawnBinding, SpawnRef, ToolOutcome, TrajectoryId,
};
pub(crate) use session::{LateOpen, Session, is_control_tool};

use crate::config::Config;
use crate::elicit::Elicitation;
use crate::engine::{EngineRefusal, Liveness, PolicyEngine, RuntimeEngine};
use crate::external::{ConsultGates, ExternalServices};
use crate::yell;
use appa_eventlog::{Backend, Log, LogStore};

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

/// One trajectory's standing to run the offer it quoted at the control
/// tool's hook, with the person's ruling its harness attached, if any.
#[derive(Debug, Clone, PartialEq)]
struct Vouch {
    actor: Actor,
    ruling: Option<appa_runtime_api::Ruling>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ToolCallDecision {
    Allow {
        spawn: Option<SpawnBinding>,
        /// The dispatch this release opened. Never reaches an adapter — `HookDecision` is
        /// the wire type and carries no id — but the hook dispatcher needs it to tie a
        /// recorded event to the fact the same call produced.
        dispatch: appa_engine::value::DispatchId,
    },
    Deny {
        feedback: String,
        offers: Vec<OfferedRemedy>,
        review: Vec<appa_runtime_api::Review>,
    },
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

/// Whom taking an offer involves: nobody but the model (the plain narrowing acceptance), the
/// named authorities' rulings, or the named sanitizer's rewrite of the value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum OfferKind {
    Accept,
    Authority { names: Vec<String> },
    Sanitizer { name: String },
}

/// What happens to the child's final message: delivered to the parent,
/// nothing returned, or delivery stopped. The child
/// is finished, so `feedback` goes to the parent as the spawn call's
/// outcome and names the options by `OfferId`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ChildReturnDecision {
    Returned {
        value: String,
    },
    /// The fork's sanitizer derived `value`; it crosses when the child returns exactly it.
    Staged {
        value: String,
    },
    NoValue,
    Blocked {
        feedback: String,
    },
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
    Policy(Box<appa_policy::ConfigError>),
    #[error("unsupported policy: {0}")]
    UnsupportedPolicy(String),
    #[error("policy declares reserved tool name {0}")]
    ReservedTool(String),
    #[error("policy names {kind} {name}, which has no [externals] binding")]
    UnboundExternal { kind: &'static str, name: String },
    #[error("[externals] binds {kind} {name}, which the policy does not declare")]
    UndeclaredExternal { kind: &'static str, name: String },
    #[error(
        "annotator {0} names a builtin on its declaration and takes no [externals.annotators] binding — remove the binding"
    )]
    BoundBuiltinAnnotator(String),
    #[error("annotator {0} names the builtin \"llm\", but the deployment declares no [externals.llm]")]
    LlmNotConfigured(String),
    #[error(
        "annotator {0} names the builtin \"claude-code\", which runs a local process this platform does not support"
    )]
    UnsupportedClaudeCodePlatform(String),
    #[error("the database is damaged: {0}")]
    Damaged(String),
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
    #[error("external resolution did not settle after {rounds} rounds")]
    ResolutionDiverged { rounds: u32 },
    #[error("the engine returned a follow-up this event cannot deliver")]
    UnexpectedDecision,
    #[error("the persisted log is refused: {0}")]
    UntrustedLog(String),
    #[error("the opening policy is unavailable: {0}")]
    PolicyUnavailable(String),
    #[error("engine invariant breach: {0}")]
    EngineInvariant(String),
    #[error("annotator={annotator} error={reason}{next_action}")]
    AnnotationRefused {
        annotator: String,
        reason: String,
        next_action: &'static str,
    },
    #[error("tool {tool} is not declared in this policy and no wildcard covers it; the call is refused before it runs")]
    UndeclaredTool { tool: String },
    /// `execute_remedy_plan` came without what the offered plan needs, or with an
    /// argument the policy cannot read. Nothing is appended and the offer stands.
    #[error("{detail}")]
    RemedyArguments { detail: String },
    #[error(
        "delegation to {tool} is not declared by the policy: an agent runs as a child only under a contract that names it, and the wildcard covers no spawn"
    )]
    UndeclaredSpawn { tool: String },
    #[error("storage failure: {0}")]
    Storage(String),
}

impl EventError {
    fn annotation_refused(annotator: String, reason: String) -> Self {
        let next_action = if annotator == "claude-code.undeclared-tool" {
            "; this tool has no exact policy contract; run /appa-guide init to sync installed MCP tools"
        } else {
            ""
        };
        Self::AnnotationRefused {
            annotator,
            reason,
            next_action,
        }
    }

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
            | EventError::ResolutionDiverged { .. }
            | EventError::AnnotationRefused { .. }
            | EventError::UndeclaredTool { .. }
            | EventError::UnexpectedDecision => true,
            EventError::CallOutstanding
            | EventError::SubstitutionAbandoned { .. }
            | EventError::TrajectoryEnded
            | EventError::ChildDispatchOpen
            | EventError::RemedyArguments { .. }
            | EventError::UnknownTrajectory
            | EventError::TrajectoryExists
            | EventError::UnknownDispatch
            | EventError::OutcomeMismatch
            | EventError::UnknownOffer
            | EventError::NotAChild
            | EventError::SpawnNotTaken
            | EventError::SpawnAmbiguous
            | EventError::UndeclaredSpawn { .. }
            | EventError::BindingMismatch => false,
        }
    }
}

impl From<EngineRefusal> for EventError {
    fn from(refusal: EngineRefusal) -> EventError {
        match refusal {
            // The class is for a report; this conversion is the local error path, which
            // keeps the operator-facing detail.
            EngineRefusal::UntrustedLog { detail, .. } => EventError::UntrustedLog(detail),
            EngineRefusal::OpeningMismatch { detail, .. } => EventError::PolicyUnavailable(detail),
            EngineRefusal::Invariant { detail } => EventError::EngineInvariant(detail),
            EngineRefusal::Ended => EventError::TrajectoryEnded,
            EngineRefusal::DispatchClosed => EventError::UnknownDispatch,
            EngineRefusal::UnknownOffer => EventError::UnknownOffer,
            EngineRefusal::Unbindable => EventError::BindingMismatch,
            EngineRefusal::UndeclaredTool { tool } => EventError::UndeclaredTool { tool },
            EngineRefusal::Arguments { detail } => EventError::RemedyArguments { detail },
        }
    }
}

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
        gates: ConsultGates,
    ) -> Result<Deployment, OpenError> {
        let policy = compile_policy(&config)?;
        validate_deployment(&policy, &config.externals)?;
        let annotator_builtins = policy
            .annotators()
            .filter_map(|(name, binding)| binding.builtin.map(|builtin| (name.as_str().to_string(), builtin)))
            .collect();
        let externals = ExternalServices::new(config.externals.clone(), modules, annotator_builtins, gates)
            .map_err(|error| OpenError::Modules(error.to_string()))?;
        Ok(Deployment {
            config,
            resident: RuntimeEngine::from_policy(&policy),
            externals,
        })
    }

    /// Answer every authority and sanitizer the policy declares in process — approve, and
    /// the body unchanged — as if the bound party had. `appa replay`'s deployment only.
    fn stand_in_for_remedies(&mut self) {
        let registry = self.resident.registry();
        self.externals.stand_in_for_remedies(
            registry
                .authorities()
                .iter()
                .map(|authority| authority.name.as_str().to_string()),
            registry
                .sanitizers()
                .map(|sanitizer| sanitizer.name.as_str().to_string()),
        );
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

/// Which contract may release a spawn — a call that opens a child trajectory.
///
/// `Declared`: only a contract written for the tool's name; the wildcard, which covers
/// every ordinary call the policy does not write, covers no spawn. An agent the policy never
/// names is denied before it runs, with the reason as the model's feedback. `Wildcard`: the
/// wildcard covers a spawn as it covers any call. The harness adapter picks: kagent's spawns
/// are other agents called as tools, and a child trajectory is not something a per-call
/// annotation can stand for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpawnCoverage {
    Declared,
    Wildcard,
}

/// Everything `open` and `open_in_memory` share before the log is chosen: the modules, the
/// consult gates, and the deployment compiled from the configuration.
struct Prepared {
    modules: crate::builtins::ModuleRegistry,
    gates: ConsultGates,
    deployment: Deployment,
}

impl Prepared {
    fn new(config: Config, modules: Option<PathBuf>) -> Result<Prepared, OpenError> {
        let modules =
            crate::builtins::load(modules.as_deref()).map_err(|error| OpenError::Modules(error.to_string()))?;
        let gates = ConsultGates::per_runtime();
        let deployment = Deployment::load(config, &modules, gates.clone())?;
        gates.serve_llm(deployment.config.externals.llm_bound());
        Ok(Prepared {
            modules,
            gates,
            deployment,
        })
    }

    fn assemble(self, backend: Backend) -> Result<Runtime, OpenError> {
        let store = LogStore::open(backend).map_err(|error| match error {
            appa_eventlog::OpenError::Damaged { path, detail } => OpenError::Damaged(format!("{path}: {detail}")),
            error @ appa_eventlog::OpenError::ForeignSchema { .. } => OpenError::Damaged(error.to_string()),
            error => OpenError::Storage(error.to_string()),
        })?;
        Ok(Runtime {
            inner: Arc::new(Inner {
                deployment: std::sync::RwLock::new(Arc::new(self.deployment)),
                retired: std::sync::Mutex::new(std::collections::BTreeMap::new()),
                store,
                modules: self.modules,
                executing: std::sync::Mutex::new(std::collections::BTreeSet::new()),
                permits: std::sync::Mutex::new(std::collections::BTreeMap::new()),
                prompted: std::sync::Mutex::new(std::collections::BTreeSet::new()),
                events: std::sync::Mutex::new(crate::events::EventLog::default()),
                gates: self.gates,
                named_spawns: std::sync::atomic::AtomicBool::new(false),
            }),
        })
    }
}

struct Inner {
    deployment: std::sync::RwLock<Arc<Deployment>>,
    retired: std::sync::Mutex<std::collections::BTreeMap<String, Arc<RuntimeEngine>>>,
    store: LogStore,
    modules: crate::builtins::ModuleRegistry,
    executing: std::sync::Mutex<std::collections::BTreeSet<String>>,
    permits: std::sync::Mutex<std::collections::BTreeMap<String, Vec<Vouch>>>,
    /// Trajectories a prompt reached since their turn last settled. Claude Code sends no
    /// `Stop` hook for a turn the user interrupted, so the prompt is the only sign the
    /// previous turn is over; the next tool call or turn end settles what it left behind.
    prompted: std::sync::Mutex<std::collections::BTreeSet<String>>,
    /// What this runtime did, as opposed to what the engine decided: bounded, in this
    /// process, and gone on restart. A diagnostic only — see [`crate::events`].
    events: std::sync::Mutex<crate::events::EventLog>,
    /// The gates every process-costing consult of this runtime passes; deployment reloads
    /// clone them, so old and new snapshots contend on the same permits.
    gates: ConsultGates,
    /// `true` under [`SpawnCoverage::Declared`]: set once, before the runtime is shared.
    named_spawns: std::sync::atomic::AtomicBool,
}

/// The trajectory an actor's events belong to: the child when the harness names one.
pub(crate) fn acting_trajectory(actor: &Actor) -> &TrajectoryId {
    actor.child.as_ref().unwrap_or(&actor.root)
}

impl Runtime {
    /// Note one thing this runtime did. Infallible and best-effort by construction: a
    /// diagnostic must never fail a decision the engine has already made, and the lock is
    /// held only for the insert.
    ///
    /// `root` is the family's root, never the acting trajectory. The log is keyed by family
    /// because that is the unit a report is about and the unit the per-list bound applies to;
    /// filing a subagent's event under the subagent would put it outside its own family's
    /// account and leave [`crate::events::EventLog::recent_root`] naming an id no log reads
    /// for. `None` is for what happens before any family is known.
    pub(crate) fn record(&self, root: Option<&TrajectoryId>, event: crate::events::RuntimeEvent) {
        self.inner.record(root, event);
    }

    /// Settle which contracts may release a spawn. Called once by the binary, for the
    /// adapter it serves, before the runtime is shared; the default is `Wildcard`.
    pub fn with_spawn_coverage(self, coverage: SpawnCoverage) -> Runtime {
        self.inner.named_spawns.store(
            coverage == SpawnCoverage::Declared,
            std::sync::atomic::Ordering::Relaxed,
        );
        self
    }
}

impl Inner {
    pub(super) fn spawn_coverage(&self) -> SpawnCoverage {
        if self.named_spawns.load(std::sync::atomic::Ordering::Relaxed) {
            SpawnCoverage::Declared
        } else {
            SpawnCoverage::Wildcard
        }
    }

    /// Note a failed store operation as a closed class.
    ///
    /// Takes the *typed* error, deliberately. Every one of these errors carries free text —
    /// a root id, a path, a `rusqlite` message — and the call sites below convert them to
    /// strings a line later. Classifying after that conversion would mean reading prose, so
    /// the class is taken here, where the variant is still a variant.
    fn note_store_error<'a>(
        &self,
        root: Option<&TrajectoryId>,
        operation: crate::events::StoreOperation,
        error: impl Into<appa_eventlog::StoreErrorClass> + 'a,
    ) {
        self.record(
            root,
            crate::events::RuntimeEvent::StoreError {
                operation,
                class: error.into(),
            },
        );
    }

    /// See [`Runtime::record`]. Lives here because a `Session` holds the `Inner`, not the
    /// `Runtime`, and the consults worth timing happen inside a session.
    pub(crate) fn record(&self, root: Option<&TrajectoryId>, event: crate::events::RuntimeEvent) {
        self.events
            .lock()
            .expect("the event mutex is never poisoned: no panic runs while it is held")
            .record(root, event);
    }

    /// See [`crate::events::EventLog::events`].
    pub(crate) fn events(&self, root: &TrajectoryId) -> crate::events::Events {
        self.events
            .lock()
            .expect("the event mutex is never poisoned: no panic runs while it is held")
            .events(root)
    }

    /// See [`crate::events::EventLog::recent_root`].
    pub(crate) fn recent_root(&self, window: std::time::Duration) -> crate::events::Recent {
        self.events
            .lock()
            .expect("the event mutex is never poisoned: no panic runs while it is held")
            .recent_root(window)
    }

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
        let opened = crate::engine::opened_under(log).ok_or_else(|| {
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

    /// The engine for a policy this deployment no longer serves, compiled once. The
    /// compile stays outside the lock — it is the expensive step, and the mutex's
    /// "no panic runs while it is held" reading must keep holding — so a race can
    /// still compile twice, but only one result is ever cached and handed out.
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
        let engine = Arc::new(RuntimeEngine::from_policy(&compiled));
        Ok(Arc::clone(
            self.retired
                .lock()
                .expect("the retired-engine mutex is never poisoned: no panic runs while it is held")
                .entry(key.to_string())
                .or_insert(engine),
        ))
    }

    pub(super) fn log(&self, root: &TrajectoryId) -> Result<Log, EventError> {
        self.store
            .log(&crate::engine::engine_id(root))
            .inspect_err(|error| self.note_store_error(Some(root), crate::events::StoreOperation::Read, error))
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
        let prepared = Prepared::new(config, modules)?;
        prepared.assemble(Backend::Sqlite { path: db })
    }

    /// The deployment `appa replay` runs: the same session and engine over a log that lives
    /// only as long as this value, with every authority and sanitizer answered in process —
    /// approve, and the body unchanged — as if the bound party had. Annotators, audience
    /// sources, and identity stay bound as configured. Nothing of the run survives the process.
    pub fn open_in_memory(config: Config, modules: Option<PathBuf>) -> Result<Runtime, OpenError> {
        let mut prepared = Prepared::new(config, modules)?;
        prepared.deployment.stand_in_for_remedies();
        prepared.assemble(Backend::Memory)
    }

    /// The policy file key the serving deployment answers under. An install compares it
    /// against the key of the configuration it just validated: a process that kept
    /// running across the install serves the policy it loaded at startup, and only a
    /// difference here is worth reloading.
    pub(crate) fn serving_policy_key(&self) -> String {
        let serving = self
            .inner
            .deployment
            .read()
            .expect("the deployment lock is never poisoned: no panic runs while it is held");
        crate::engine::policy_file_key(serving.config.policy_file().bytes())
    }

    /// Replace the serving deployment with the one this configuration
    /// declares, without stopping the process (
    /// reloading a policy). The caller reads the file; the runtime never
    /// learns where a configuration came from, so an embedding host
    /// reloads a composed policy the same way.
    pub fn reload(&self, config: Config) -> Result<Reloaded, OpenError> {
        let deployment = Deployment::load(config, &self.inner.modules, self.inner.gates.clone())?;
        let identity = deployment.resident().identity_hex();
        let deployment = Arc::new(deployment);
        // The gate's bound and the serving snapshot change as one transition under the
        // deployment lock, so two reloads racing cannot leave the gate bound by the
        // deployment that lost.
        let previous = {
            let mut serving = self
                .inner
                .deployment
                .write()
                .expect("the deployment lock is never poisoned: no panic runs while it is held");
            self.inner.gates.serve_llm(deployment.config.externals.llm_bound());
            std::mem::replace(&mut *serving, Arc::clone(&deployment))
        };
        // Every reload retires at most one more policy, so clearing here bounds the
        // cache by the reloads since the last one instead of by the life of the
        // process. A trajectory still replaying under a dropped entry recompiles it.
        self.inner
            .retired
            .lock()
            .expect("the retired-engine mutex is never poisoned: no panic runs while it is held")
            .clear();
        // Every reload retires at most one more policy, so clearing here bounds the
        // cache by the reloads since the last one instead of by the life of the
        // process. A trajectory still replaying under a dropped entry recompiles it.

        let key = crate::engine::policy_file_key(deployment.config.policy_file().bytes());
        let changed = crate::engine::policy_file_key(previous.config.policy_file().bytes()) != key;
        tracing::info!(
            policy_key = %key,
            policy_identity = %identity,
            changed,
            "reloaded the serving deployment"
        );
        // Deployment-wide: a reload belongs to no trajectory, and every trajectory alive
        // across it needs to see that its policy moved under it.
        self.record(
            None,
            crate::events::RuntimeEvent::Reload {
                policy_key: key.clone(),
                changed,
            },
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
    pub(crate) fn create_session(&self, id: TrajectoryId) -> Result<Session, EventError> {
        let deployment = self.inner.deployment();
        let opening = deployment.root_opening(&id);
        let root = self
            .inner
            .store
            .create_root(opening, deployment.config.policy_file().bytes())
            .inspect_err(|error| {
                self.inner
                    .note_store_error(Some(&id), crate::events::StoreOperation::Open, error)
            })
            .map_err(|error| match error {
                appa_eventlog::CreateError::AlreadyExists { .. } => EventError::TrajectoryExists,
                error => EventError::Storage(error.to_string()),
            })?;
        let root = TrajectoryId(root.as_str().to_string());
        Ok(Session::attach(Arc::clone(&self.inner), deployment, root.clone(), root))
    }

    /// Reopens a persisted trajectory. There is no stored view: the next
    /// event rebuilds the engine's picture from the log.
    pub(crate) fn session(&self, root: &TrajectoryId, trajectory: &TrajectoryId) -> Result<Session, EventError> {
        let known = self
            .inner
            .store
            .has_root(&crate::engine::engine_id(root))
            .inspect_err(|error| {
                self.inner
                    .note_store_error(Some(root), crate::events::StoreOperation::Read, error)
            })
            .map_err(|error| EventError::Storage(error.to_string()))?;
        if !known {
            return Err(EventError::UnknownTrajectory);
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
    pub(crate) fn live(&self, root: &TrajectoryId, trajectory: &TrajectoryId) -> Result<(), EventError> {
        let log = self.inner.log(root)?;
        let deployment = self.inner.deployment();
        let policy = self.inner.resolve_policy(&deployment, &log)?;
        let view = policy.engine().rebuild_view(&log).map_err(EventError::from)?;
        match policy.engine().liveness(&view, trajectory) {
            Liveness::Unopened => Err(EventError::UnknownTrajectory),
            Liveness::Ended => Err(EventError::TrajectoryEnded),
            Liveness::Live => Ok(()),
        }
    }

    /// The first of `candidates` this family has opened, live or ended, from
    /// one view rebuild; `None` when it opened none of them.
    pub(crate) fn opened_among(
        &self,
        root: &TrajectoryId,
        candidates: &[TrajectoryId],
    ) -> Result<Option<TrajectoryId>, EventError> {
        if candidates.is_empty() {
            return Ok(None);
        }
        let log = self.inner.log(root)?;
        let deployment = self.inner.deployment();
        let policy = self.inner.resolve_policy(&deployment, &log)?;
        let view = policy.engine().rebuild_view(&log).map_err(EventError::from)?;
        Ok(candidates
            .iter()
            .find(|candidate| policy.engine().liveness(&view, candidate) != Liveness::Unopened)
            .cloned())
    }

    pub fn status(&self, id: &TrajectoryId) -> Option<TrajectoryStatus> {
        let deployment = self.inner.deployment();
        let (policy, log) = self.root_log(&deployment, id, "status")?;
        let view = match policy.engine().rebuild_view(&log) {
            Ok(view) => view,
            Err(refusal) => {
                tracing::warn!(trajectory = %id.0, %refusal, "status read refused the persisted log");
                return None;
            }
        };
        policy.engine().trajectory_status(&view, id)
    }

    /// Every decision this family's log recorded, in log order.
    /// A projection like
    /// [`Runtime::status`]: it gates nothing, appends nothing, and
    /// expires no offer, and it answers for an ended
    /// trajectory because an audit is read after the run.
    pub fn audit(&self, id: &TrajectoryId) -> Option<Vec<AuditEntry>> {
        let deployment = self.inner.deployment();
        let (policy, log) = self.root_log(&deployment, id, "audit")?;
        match policy.engine().audit(&log) {
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

    /// One trajectory's decisions, stripped for a report that leaves this machine.
    ///
    /// A read like [`Runtime::audit`]: it gates nothing and appends nothing. Unlike an audit
    /// it survives a log the engine refuses — a refused log is the very thing worth reporting
    /// — and carries the refusal as a closed class instead of the facts a view would have
    /// given. What may leave is decided in [`crate::yell::tables`], never here.
    pub(crate) fn diagnostic(&self, selection: yell::Selection, mode: yell::Mode) -> yell::Diagnostic {
        let (root, yelling) = match selection {
            yell::Selection::Root { root, yelling } => {
                let yelling = yelling.unwrap_or_else(|| root.clone());
                (root, Some(yelling))
            }
            yell::Selection::Recent => match yell::resolve(self.inner.recent_root(yell::RECENT_WINDOW)) {
                Ok(root) => {
                    let yelling = root.clone();
                    (root, Some(yelling))
                }
                Err(omitted_reason) => return yell::Diagnostic::Omitted { omitted_reason },
            },
        };
        let deployment = self.inner.deployment();
        let Ok(log) = self.inner.log(&root) else {
            // The store error is already recorded as a runtime event by `Inner::log`.
            return yell::Diagnostic::Omitted {
                omitted_reason: yell::OmittedReason::LogUnavailable,
            };
        };
        // The policy is what names the trust ranks, and the view is what names the parents.
        // Neither is required: without them the facts still export, with the fields a reader
        // cannot be given left empty and the refusal said out loud.
        let policy = self.inner.resolve_policy(&deployment, &log).ok();
        let rebuilt = policy.as_ref().map(|policy| policy.engine().rebuild_view(&log));
        let replay_refused = match &rebuilt {
            Some(Err(refusal)) => Some(refusal.class()),
            _ => None,
        };
        let view = rebuilt.and_then(Result::ok);
        let trust_chain = policy
            .as_ref()
            .map(|policy| {
                policy
                    .engine()
                    .registry()
                    .trust_chain()
                    .names()
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        // Which tool spellings the deployment itself chose. Without a policy nothing is
        // vouched, which is the safe end: every tool name is then a token.
        let vouched = policy
            .as_ref()
            .map(|policy| policy.engine().vouched_tools())
            .unwrap_or_default();
        // The rules as *this trajectory* ran under them, never as the deployment holds them
        // now: the log pins its own policy file, so a reload since the session opened does not
        // rewrite the rules a report explains. Its key comes from the same bytes, so the
        // document and its fingerprint are one snapshot.
        let pinned = String::from_utf8_lossy(log.policy_file()).into_owned();
        let policy_section = toml::from_str::<toml::Value>(&pinned)
            .ok()
            .and_then(|composed| composed.get("policy").cloned())
            .map(|document| (document, crate::engine::policy_file_key(log.policy_file())));
        let source = yell::Source {
            facts: log.facts(),
            events: self.inner.events(&root),
            trust_chain,
            policy: policy_section,
            vouched,
            parents: yell::branches(log.facts(), view.as_ref(), policy.as_ref()),
            replay_refused,
            yelling,
        };
        yell::Diagnostic::Present(Box::new(yell::build(source, mode)))
    }

    /// Execute one surfaced remedy offer by its id.
    pub async fn execute_remedy(&self, acting: &Actor, offer: OfferId) -> RemedyOutcome {
        self.remedy(acting, offer, RemedyArguments::default(), None, None).await
    }

    /// Execute one surfaced remedy offer with the arguments a plan declaring a subagent's
    /// return takes: the floor, and the schema where the plan attests.
    pub async fn execute_remedy_with(
        &self,
        acting: &Actor,
        offer: OfferId,
        arguments: RemedyArguments,
    ) -> RemedyOutcome {
        self.remedy(acting, offer, arguments, None, None).await
    }
}

/// The control call's arguments as a model spells them — `offer_id`, and for a plan
/// declaring a subagent's return `label` and `return_schema` — for a harness that routes the
/// control tool itself.
pub fn parse_control_arguments(arguments: &str) -> Result<(OfferId, RemedyArguments), String> {
    let args: crate::mcp::ExecuteRemedyPlanArgs =
        serde_json::from_str(arguments).map_err(|error| format!("execute_remedy_plan arguments: {error}"))?;
    Ok((OfferId(args.offer_id.clone()), RemedyArguments::from(args)))
}

impl Runtime {
    /// The whole act: resolve the quoted id inside the acting trajectory's
    /// own family, claim the offer, and answer. `elicitation` is supplied
    /// rather than extracted, so the body is reachable without a live peer.
    pub(crate) async fn remedy(
        &self,
        acting: &Actor,
        quoted: OfferId,
        arguments: RemedyArguments,
        elicitation: Option<&Elicitation>,
        ruling: Option<appa_runtime_api::Ruling>,
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
        let session = match self.session(&root, &pursuer) {
            Ok(session) => session,
            Err(error) => {
                return RemedyOutcome::Refused {
                    detail: error.to_string(),
                };
            }
        };
        match session.on_remedy(offer, arguments, elicitation, ruling).await {
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

    /// What taking a quoted offer in this root's family would consult, or `None` for an
    /// offer that no longer stands.
    pub(crate) fn offer_kind(&self, root: &TrajectoryId, quoted: &OfferId) -> Option<OfferKind> {
        let log = self.inner.log(root).ok()?;
        let offer = crate::engine::resolve_rendered(&log, quoted)?;
        let deployment = self.inner.deployment();
        let policy = self.inner.resolve_policy(&deployment, &log).ok()?;
        let view = policy.engine().rebuild_view(&log).ok()?;
        let pursuer = policy.engine().offer_pursuer(&view, &offer)?;
        policy.engine().offer_kind(&view, &pursuer, &offer)
    }

    /// The canonical identity a quoted id names in this family, and the
    /// trajectory that may execute it.
    pub(crate) fn resolve_in(&self, root: &TrajectoryId, quoted: &OfferId) -> Option<(OfferId, TrajectoryId)> {
        let log = self.inner.log(root).ok()?;
        let offer = crate::engine::resolve_rendered(&log, quoted)?;
        let deployment = self.inner.deployment();
        let policy = self.inner.resolve_policy(&deployment, &log).ok()?;
        let view = policy.engine().rebuild_view(&log).ok()?;
        let pursuer = policy.engine().offer_pursuer(&view, &offer)?;
        Some((offer, pursuer))
    }

    /// Record that this trajectory quoted this offer id, for the request
    /// that runs it. `ruling` is a person's answer the harness obtained
    /// through its own review channel; it rides the vouch and is spent
    /// with it, so it can answer exactly the execution it was given for.
    pub(crate) fn vouch(&self, quoted: &OfferId, acting: &Actor, ruling: Option<appa_runtime_api::Ruling>) {
        let mut permits = self.inner.permits.lock().expect("the permit mutex is never poisoned");
        let holders = permits.entry(quoted.0.clone()).or_default();
        match holders.iter_mut().find(|holder| holder.actor == *acting) {
            Some(holder) => holder.ruling = ruling,
            None => holders.push(Vouch {
                actor: acting.clone(),
                ruling,
            }),
        }
    }

    /// The trajectory vouched for this quoted id, taken once, with the
    /// ruling its harness attached.
    pub(crate) fn take_vouched(&self, quoted: &OfferId) -> Option<(Actor, Option<appa_runtime_api::Ruling>)> {
        let mut permits = self.inner.permits.lock().expect("the permit mutex is never poisoned");
        let mut holders = permits.remove(&quoted.0)?;
        (holders.len() == 1).then(|| {
            let vouch = holders.remove(0);
            (vouch.actor, vouch.ruling)
        })
    }

    /// Drop every vouch this actor still holds. A vouch is recorded when the actor
    /// quotes an offer at the control tool's hook and taken when the tool itself
    /// runs, both inside one turn. One still standing at the turn's end was never
    /// spent — the harness declined the call, or the tool never ran — and nothing
    /// later can spend it.
    pub(crate) fn release_vouches(&self, acting: &Actor) {
        let mut permits = self.inner.permits.lock().expect("the permit mutex is never poisoned");
        permits.retain(|_, holders| {
            holders.retain(|holder| holder.actor != *acting);
            !holders.is_empty()
        });
    }

    /// A prompt reached this actor. Nothing is recorded: the mark lives in memory and is
    /// consumed by the actor's next tool call or turn end, whichever comes first. A
    /// restarted runtime forgets it and the next proposal refuses until a turn end closes
    /// the abandoned call.
    pub(crate) fn note_prompt(&self, acting: &Actor) {
        let mut prompted = self
            .inner
            .prompted
            .lock()
            .expect("the prompted mutex is never poisoned");
        prompted.insert(acting_trajectory(acting).0.clone());
    }

    /// Whether a prompt reached this actor since its turn last settled. Consumed once.
    pub(crate) fn take_prompted(&self, acting: &Actor) -> bool {
        let mut prompted = self
            .inner
            .prompted
            .lock()
            .expect("the prompted mutex is never poisoned");
        prompted.remove(acting_trajectory(acting).0.as_str())
    }

    fn spend_vouch(&self, quoted: &OfferId, acting: &Actor) {
        let mut permits = self.inner.permits.lock().expect("the permit mutex is never poisoned");
        let Some(holders) = permits.get_mut(&quoted.0) else {
            return;
        };
        holders.retain(|holder| holder.actor != *acting);
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

    /// One root's rebuilt view and the engine that decides for it, for
    /// the test accessors that read a family the public surface does not
    /// expose. Panics where production would refuse: a test that reaches
    /// an unreadable log has already failed.
    #[cfg(test)]
    fn rebuilt<'a>(
        &self,
        deployment: &'a Deployment,
        root: &TrajectoryId,
    ) -> (PolicyEngine<'a>, crate::engine::EngineView) {
        let log = self.inner.log(root).expect("the log reads");
        let policy = self
            .inner
            .resolve_policy(deployment, &log)
            .expect("the opening policy resolves");
        let view = policy.engine().rebuild_view(&log).expect("the log rebuilds");
        (policy, view)
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
        let deployment = self.inner.deployment();
        let (policy, view) = self.rebuilt(&deployment, root);
        policy.engine().open_dispatches(&view, trajectory)
    }

    /// Does the root's log name this trajectory, for the tests that
    /// assert on whether a child opened.
    #[cfg(test)]
    pub(crate) fn names_trajectory(&self, root: &TrajectoryId, trajectory: &TrajectoryId) -> bool {
        let deployment = self.inner.deployment();
        let (policy, view) = self.rebuilt(&deployment, root);
        policy.engine().liveness(&view, trajectory) != Liveness::Unopened
    }

    /// The substituted call a trajectory has standing, for the tests
    /// that assert on it: the open dispatch no proposal released.
    #[cfg(test)]
    pub(crate) fn substituted_release(
        &self,
        root: &TrajectoryId,
        trajectory: &TrajectoryId,
    ) -> Option<crate::engine::OpenDispatch> {
        let deployment = self.inner.deployment();
        let (policy, view) = self.rebuilt(&deployment, root);
        policy.engine().substituted_release(&view, trajectory)
    }

    /// Rebuild one root's view, scoped to a trajectory in it, for the tests
    /// that read a branch the root-only public surface does not expose.
    #[cfg(test)]
    pub(crate) fn branch_status(&self, root: &TrajectoryId, trajectory: &TrajectoryId) -> Option<TrajectoryStatus> {
        let deployment = self.inner.deployment();
        let (policy, view) = self.rebuilt(&deployment, root);
        policy.engine().trajectory_status(&view, trajectory)
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
        let deployment = self.inner.deployment();
        let (policy, view) = self.rebuilt(&deployment, root);
        EventError::from(
            policy
                .engine()
                .handle(&view, trajectory, event)
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
    if policy.registry().provider_run_annotations().next().is_some() {
        return Err(OpenError::UnsupportedPolicy(
            "[deployment] provider_run_tools — this runtime never sees inference responses, so it cannot admit a provider-run result".to_string(),
        ));
    }

    let rc = policy.registry_config();
    for tool in &rc.tools {
        let name = tool.name().as_str();
        if is_control_tool(name) {
            return Err(OpenError::ReservedTool(name.to_string()));
        }
    }

    // Each binding names a registered component: a binding nothing registers would never
    // be consulted, so the deployment is refused rather than left believing an
    // implementation runs. Each registered name is bound, with two exceptions: an
    // authority may stay unbound and then returns no answer, and an Annotator that
    // names a stock builtin on its declaration is complete as written.
    no_undeclared(
        "authority",
        rc.authorities.iter().map(|authority| authority.name.as_str()),
        &externals.authorities,
    )?;
    if externals
        .sanitizers
        .contains_key(appa_engine::names::SanitizerName::ATTEST_SCHEMA)
    {
        return Err(OpenError::UnsupportedPolicy(
            "[externals] binds sanitizer attest-schema — the reserved builtin is applied by the engine itself and takes no implementation"
                .to_string(),
        ));
    }
    bound_exactly(
        "sanitizer",
        rc.sanitizers
            .iter()
            .filter(|sanitizer| !sanitizer.name.is_attest_schema())
            .map(|sanitizer| sanitizer.name.as_str()),
        &externals.sanitizers,
    )?;
    // A declared builtin is served by the runtime itself, so it is refused when it is also
    // bound, and when this deployment cannot serve it: a consult that can never answer is
    // a misconfiguration to refuse at open, not a no-answer to discover under an agent.
    // Every other Annotator is bound exactly once.
    let mut bound_by_deployment = Vec::new();
    for (name, binding) in policy.annotators() {
        let name = name.as_str();
        let Some(builtin) = binding.builtin else {
            bound_by_deployment.push(name);
            continue;
        };
        if externals.annotators.contains_key(name) {
            return Err(OpenError::BoundBuiltinAnnotator(name.to_string()));
        }
        match builtin {
            appa_policy::AnnotatorBuiltin::Llm if externals.llm.is_none() => {
                return Err(OpenError::LlmNotConfigured(name.to_string()));
            }
            appa_policy::AnnotatorBuiltin::ClaudeCode if !cfg!(unix) => {
                return Err(OpenError::UnsupportedClaudeCodePlatform(name.to_string()));
            }
            appa_policy::AnnotatorBuiltin::Llm | appa_policy::AnnotatorBuiltin::ClaudeCode => {}
        }
    }
    bound_exactly("annotator", bound_by_deployment.into_iter(), &externals.annotators)?;
    bound_exactly(
        "audience source",
        rc.audience.sources.iter().map(|source| source.provider.as_str()),
        &externals.audience,
    )?;
    // The shipped `verified-email` implementation is engine-computed and takes no binding;
    // only a policy-selected custom implementation binds, exactly once.
    let custom_identity = match &rc.audience.identity {
        Some(appa_engine::audience::IdentityImplementation::Custom(name)) => Some(name.as_str()),
        Some(appa_engine::audience::IdentityImplementation::VerifiedEmail) | None => None,
    };
    bound_exactly(
        "identity implementation",
        custom_identity.into_iter(),
        &externals.identity,
    )?;
    Ok(())
}

fn bound_exactly<'a, Implementation>(
    kind: &'static str,
    registered: impl Iterator<Item = &'a str>,
    bound: &std::collections::BTreeMap<String, Implementation>,
) -> Result<(), OpenError> {
    let registered: std::collections::BTreeSet<&str> = registered.collect();
    if let Some(name) = registered.iter().find(|name| !bound.contains_key(**name)) {
        return Err(OpenError::UnboundExternal {
            kind,
            name: (*name).to_string(),
        });
    }
    no_undeclared(kind, registered.into_iter(), bound)
}

fn no_undeclared<'a, Implementation>(
    kind: &'static str,
    registered: impl Iterator<Item = &'a str>,
    bound: &std::collections::BTreeMap<String, Implementation>,
) -> Result<(), OpenError> {
    let registered: std::collections::BTreeSet<&str> = registered.collect();
    if let Some(name) = bound.keys().find(|name| !registered.contains(name.as_str())) {
        return Err(OpenError::UndeclaredExternal {
            kind,
            name: name.clone(),
        });
    }
    Ok(())
}

fn compile_policy(config: &Config) -> Result<appa_policy::Config, OpenError> {
    let text = toml::to_string(config.policy_file().value())
        .map_err(|error| OpenError::UnsupportedPolicy(format!("the policy table does not serialize: {error}")))?;
    appa_policy::Config::from_toml_str(&text).map_err(|error| OpenError::Policy(Box::new(error)))
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

/// Plain-data fixtures for tests outside this module, so they can name
/// a fork the way the harness carries it without naming the engine
/// boundary (the source-scan structural guard holds for test code
/// too).
#[cfg(test)]
pub(crate) mod testing {
    fn engine_dispatch(label: &str) -> appa_engine::value::DispatchId {
        let policy = appa_policy::Config::from_toml_str(
            "version = 2
[[tool]]
name = \"Bash\"
",
        )
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
}

#[cfg(test)]
mod deployment_tests {
    fn test_permits() -> ConsultGates {
        ConsultGates::per_runtime()
    }

    use std::time::Duration;

    use super::*;
    use crate::config::{AnnotatorImplementation, Endpoint, ExternalBindings, LlmBinding, LlmProvider};

    #[test]
    fn the_undeclared_tool_fallback_refusal_names_the_recovery_action() {
        let fallback = EventError::annotation_refused(
            "claude-code.undeclared-tool".to_string(),
            "non_success status=1".to_string(),
        )
        .to_string();
        assert!(fallback.contains("run /appa-guide init"), "{fallback}");

        let exact = EventError::annotation_refused("bash-classifier".to_string(), "timeout".to_string()).to_string();
        assert!(!exact.contains("/appa-guide init"), "{exact}");
    }

    /// A deployment with no `[externals.annotators]` bindings: the policy under test names
    /// `builtin = "claude-code"` on the declarations it wants answered by Claude Code.
    fn claude_config(policy: &str) -> Config {
        let bindings = ExternalBindings::new(Duration::from_secs(30), 65_536);
        Config::embedded(policy.to_string(), bindings).expect("the embedded configuration parses")
    }

    fn endpoint() -> AnnotatorImplementation {
        AnnotatorImplementation::Resolver(Endpoint::new("https://resolver.example".to_string(), None))
    }

    fn load(config: Config) -> Result<Deployment, OpenError> {
        Deployment::load(config, &crate::builtins::ModuleRegistry::empty(), test_permits())
    }

    #[test]
    fn a_claude_builtin_deployment_opens_without_an_endpoint() {
        let tool_level = claude_config(
            r#"
                version = 2
                [[annotator]]
                name = "classifier"
                builtin = "claude-code"
                [[tool]]
                name = "lookup"
                description = "Looks one record up."
                annotator = "classifier"
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
                version = 2
                [[annotator]]
                name = "classifier"
                builtin = "claude-code"
                [[tool]]
                name = "fetch"
                description = "Fetches one URL."
                annotator = "classifier"
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
version = 2
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
    fn every_annotator_has_its_own_implementation() {
        let mut config = claude_config(
            r#"
                version = 2
                [[annotator]]
                name = "bash-classifier"
                builtin = "claude-code"
                [[annotator]]
                name = "other-classifier"
                [[tool]]
                name = "Bash"
                description = "Runs one shell command."
                annotator = "bash-classifier"
                [[tool]]
                name = "Other"
                description = "Does something else."
                annotator = "other-classifier"
            "#,
        );
        // The builtin Annotator is complete as declared; only the other one is bound here.
        config
            .externals
            .annotators
            .insert("other-classifier".to_string(), endpoint());
        assert!(load(config).is_ok());
    }

    #[test]
    fn an_authority_may_stay_unbound_but_its_binding_must_be_declared() {
        let policy = r#"
            version = 2
            [[authority]]
            name = "reviewer"
            [authority.permits]
            attention = ["irreversible"]
        "#;
        assert!(
            load(claude_config(policy)).is_ok(),
            "an unbound authority answers nothing"
        );

        let mut extra = claude_config(policy);
        extra.externals.authorities.insert(
            "auditor".to_string(),
            crate::config::Implementation::Resolver(Endpoint::new("https://auditor.example".to_string(), None)),
        );
        assert!(matches!(
            load(extra),
            Err(OpenError::UndeclaredExternal { kind: "authority", .. })
        ));
    }

    #[test]
    fn missing_and_undeclared_annotator_implementations_are_refused() {
        let policy = r#"
            version = 2
            [[annotator]]
            name = "classifier"
            [[tool]]
            name = "lookup"
            description = "Looks one record up."
            annotator = "classifier"
        "#;
        let missing = claude_config(policy);
        assert!(matches!(
            load(missing),
            Err(OpenError::UnboundExternal { kind: "annotator", .. })
        ));

        let mut extra = claude_config(policy);
        extra.externals.annotators.insert("classifier".to_string(), endpoint());
        extra.externals.annotators.insert("undeclared".to_string(), endpoint());
        assert!(matches!(
            load(extra),
            Err(OpenError::UndeclaredExternal { kind: "annotator", .. })
        ));
    }

    #[test]
    fn a_builtin_annotator_takes_no_deployment_binding() {
        let mut bound = claude_config(
            r#"
                version = 2
                [[annotator]]
                name = "classifier"
                builtin = "claude-code"
                [[tool]]
                name = "lookup"
                description = "Looks one record up."
                annotator = "classifier"
            "#,
        );
        bound.externals.annotators.insert("classifier".to_string(), endpoint());
        assert!(matches!(
            load(bound),
            Err(OpenError::BoundBuiltinAnnotator(name)) if name == "classifier"
        ));
    }

    /// A declared `llm` Annotator opens only over a deployment that declares the profile it
    /// consults — at open and at every reload.
    #[test]
    fn a_declared_llm_annotator_needs_the_llm_table_at_open_and_reload() {
        let policy = r#"
            version = 2
            [[annotator]]
            name = "classifier"
            builtin = "llm"
            [[tool]]
            name = "lookup"
            description = "Looks one record up."
            annotator = "classifier"
        "#;
        let with_profile = || {
            let mut bindings = ExternalBindings::new(Duration::from_secs(30), 65_536);
            bindings.llm = Some(LlmBinding {
                provider: LlmProvider::Ollama,
                model: "llama".to_string(),
                url: None,
                token_env: None,
                timeout_ms: None,
                max_concurrent: None,
            });
            Config::embedded(policy.to_string(), bindings).expect("the embedded configuration parses")
        };
        assert!(matches!(
            load(claude_config(policy)),
            Err(OpenError::LlmNotConfigured(name)) if name == "classifier"
        ));
        assert!(load(with_profile()).is_ok());

        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(with_profile(), dir.path().join("appa.db"), None).expect("the deployment opens");
        assert!(matches!(
            runtime.reload(claude_config(policy)),
            Err(OpenError::LlmNotConfigured(name)) if name == "classifier"
        ));
    }

    #[test]
    fn the_llm_gate_follows_the_serving_deployment_and_never_a_refused_one() {
        let policy = r#"
            version = 2
            [[annotator]]
            name = "classifier"
            builtin = "llm"
            [[tool]]
            name = "lookup"
            description = "Looks one record up."
            annotator = "classifier"
            [[authority]]
            name = "auditor"
            [authority.permits]
            attention = ["irreversible"]
        "#;
        let with_pool = |max_concurrent: u32| {
            let mut bindings = ExternalBindings::new(Duration::from_secs(30), 65_536);
            bindings.llm = Some(LlmBinding {
                provider: LlmProvider::Ollama,
                model: "llama".to_string(),
                url: None,
                token_env: None,
                timeout_ms: None,
                max_concurrent: Some(max_concurrent),
            });
            Config::embedded(policy.to_string(), bindings).expect("the embedded configuration parses")
        };
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(with_pool(2), dir.path().join("appa.db"), None).expect("the deployment opens");
        assert_eq!(runtime.inner.gates.llm_permits(), 2);

        // A candidate that validates but cannot build its externals declares a wider pool
        // and never serves: the gate stays as the serving deployment declared it.
        let mut refused = with_pool(5);
        refused.externals.authorities.insert(
            "auditor".to_string(),
            crate::config::Implementation::Builtin("no-such".to_string()),
        );
        assert!(matches!(runtime.reload(refused), Err(OpenError::Modules(_))));
        assert_eq!(runtime.inner.gates.llm_permits(), 2);

        assert!(runtime.reload(with_pool(3)).is_ok());
        assert_eq!(runtime.inner.gates.llm_permits(), 3);
        assert!(
            runtime.reload(claude_config(policy)).is_err(),
            "no profile, no declared llm"
        );
        assert_eq!(runtime.inner.gates.llm_permits(), 3);

        // Reloads racing from several threads: whichever deployment ends up serving, the
        // gate is bound as that deployment declares.
        std::thread::scope(|scope| {
            for round in 0..8u32 {
                let runtime = &runtime;
                let with_pool = &with_pool;
                scope.spawn(move || {
                    runtime
                        .reload(with_pool(2 + round % 4))
                        .expect("every candidate is a complete deployment");
                });
            }
        });
        let serving = runtime
            .inner
            .deployment
            .read()
            .expect("the deployment lock is never poisoned")
            .config
            .externals
            .llm_bound();
        assert_eq!(runtime.inner.gates.llm_permits(), serving);
    }

    /// Two policies that differ only in a tool's description, so a root opened
    /// under one replays against the other through the retired branch.
    fn versioned_policy(description: &str) -> Config {
        claude_config(&format!(
            r#"
            version = 2
            [[tool]]
            name = "fetch"
            description = "{description}"
            "#
        ))
    }

    #[tokio::test]
    async fn a_reload_drops_the_retired_engines_compiled_before_it() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = std::sync::Arc::new(
            Runtime::open(versioned_policy("first"), dir.path().join("appa.db"), None).expect("the deployment opens"),
        );
        let root = TrajectoryId("retired-cache".to_string());
        assert_eq!(
            crate::hooks::handle(
                &runtime,
                appa_runtime_api::HookEvent::SessionStart { root: root.clone() }
            )
            .await,
            appa_runtime_api::HookDecision::Ack
        );

        // The root's policy is no longer the serving one, so reading it compiles the
        // retired engine and caches it.
        runtime
            .reload(versioned_policy("second"))
            .expect("the second deployment loads");
        assert!(
            runtime.audit(&root).is_some(),
            "the root still reads under its own policy"
        );
        assert_eq!(retired_len(&runtime), 1);

        runtime
            .reload(versioned_policy("third"))
            .expect("the third deployment loads");
        assert_eq!(
            retired_len(&runtime),
            0,
            "the cache does not carry compiled engines across a reload"
        );
    }

    #[test]
    fn the_serving_policy_key_names_the_deployment_answering_now() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime =
            Runtime::open(versioned_policy("first"), dir.path().join("appa.db"), None).expect("the deployment opens");
        let before = runtime.serving_policy_key();

        let reloaded = runtime
            .reload(versioned_policy("second"))
            .expect("the second deployment loads");
        assert_eq!(
            runtime.serving_policy_key(),
            reloaded.policy_key,
            "the key answers for the deployment the reload installed"
        );
        assert_ne!(
            runtime.serving_policy_key(),
            before,
            "a different policy answers under a different key, which is what makes the key a divergence signal"
        );
    }

    fn retired_len(runtime: &Runtime) -> usize {
        runtime
            .inner
            .retired
            .lock()
            .expect("the retired-engine mutex is never poisoned")
            .len()
    }

    #[tokio::test]
    async fn a_vouch_the_turn_never_spent_does_not_outlive_it() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = std::sync::Arc::new(
            Runtime::open(versioned_policy("first"), dir.path().join("appa.db"), None).expect("the deployment opens"),
        );
        let root = TrajectoryId("vouch-release".to_string());
        assert_eq!(
            crate::hooks::handle(
                &runtime,
                appa_runtime_api::HookEvent::SessionStart { root: root.clone() }
            )
            .await,
            appa_runtime_api::HookDecision::Ack
        );
        let actor = Actor {
            root: root.clone(),
            child: None,
        };
        let quoted = OfferId("offer-1".to_string());

        runtime.vouch(&quoted, &actor, None);
        assert_eq!(
            runtime.take_vouched(&quoted),
            Some((actor.clone(), None)),
            "a standing vouch is what the tool takes"
        );

        runtime.vouch(&quoted, &actor, None);
        crate::hooks::handle(&runtime, appa_runtime_api::HookEvent::TurnEnd { actor: actor.clone() }).await;
        assert_eq!(
            runtime.take_vouched(&quoted),
            None,
            "the turn ended without spending it, so nothing later can"
        );
    }

    /// The session-start check refuses on any fault, so the variant it refuses with is
    /// what an operator reads. A missing policy file is not a storage failure, and
    /// flattening it to one named the wrong incident.
    #[tokio::test]
    async fn a_liveness_check_refuses_with_the_fault_it_met() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = std::sync::Arc::new(
            Runtime::open(versioned_policy("first"), dir.path().join("appa.db"), None).expect("the deployment opens"),
        );
        let root = TrajectoryId("liveness-fault".to_string());
        assert_eq!(
            crate::hooks::handle(
                &runtime,
                appa_runtime_api::HookEvent::SessionStart { root: root.clone() }
            )
            .await,
            appa_runtime_api::HookDecision::Ack
        );
        assert!(runtime.live(&root, &root).is_ok());

        runtime.inner.store.forget_policy_files();
        assert!(
            matches!(runtime.live(&root, &root), Err(EventError::PolicyUnavailable(_))),
            "the root's policy file is gone, which is not a storage failure"
        );
    }
}
