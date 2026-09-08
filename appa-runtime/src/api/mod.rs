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
use appa_runtime_api::{Adapter, AdapterName};

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

/// What a vouch is *about*, and the reason a runtime-provided tool can trust the trajectory
/// it is told it belongs to.
///
/// An MCP request carries no session, so a tool this runtime serves cannot know which
/// trajectory called it. The hook that preceded the call does know — it is the one place the
/// harness names the actor — so it records the standing here and the tool spends it. The two
/// variants are the two things a hook can key that record by, and they are separate variants
/// because they can never mean each other: an offer id is a name the engine minted and the
/// model quotes back, and a call key is the call itself.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum PermitKey {
    /// The offer id `execute_remedy_plan` quotes.
    Offer(String),
    /// One call to a tool this runtime serves, by the tool's bare name and its arguments:
    /// the tool takes no id, and the arguments are the only thing both the hook and the
    /// tool see. A digest, so nothing a person wrote is a map key.
    Call(String),
}

impl PermitKey {
    pub(crate) fn offer(quoted: &OfferId) -> Self {
        Self::Offer(quoted.0.clone())
    }

    /// RFC 8785 over the parsed arguments, not over the bytes either side received: the
    /// harness and the MCP client serialize the same call differently, and the digest has
    /// to survive that.
    pub(crate) fn call(tool: &str, arguments: &serde_json::Value) -> Self {
        use sha2::Digest as _;
        let mut hasher = sha2::Sha256::new();
        hasher.update(tool.as_bytes());
        hasher.update([0]);
        hasher.update(appa_engine::params::canonical_bytes(arguments));
        Self::Call(format!("{:x}", hasher.finalize()))
    }
}

/// The key a proposed call to a tool this runtime serves is vouched under, or `None` when
/// the call is not one: a tool on another server under a matching name, or a shape the tool
/// itself would not read as its call.
pub(crate) fn call_key(call: &ProposedCall) -> Option<PermitKey> {
    const MANAGEMENT_TOOLS: [&str; 6] = [
        "appa_get_runtime_state",
        "appa_include_battery",
        "appa_match_batteries",
        "appa_reload_policy",
        "appa_refresh_batteries",
        "appa_update_policy",
    ];
    let bare = bare_runtime_tool(&call.tool);
    if bare == "yell" {
        return crate::yell::YellArgs::parse(&call.arguments).map(|args| args.ticket());
    }
    if !MANAGEMENT_TOOLS.contains(&bare) {
        return None;
    }
    let mut arguments = serde_json::from_str::<serde_json::Value>(call.arguments.get()).ok()?;
    if arguments.is_null() {
        arguments = serde_json::json!({});
    }
    Some(PermitKey::call(bare, &arguments))
}

/// The tool's own name, under whichever prefix a harness's MCP client spells this runtime's
/// server as.
fn bare_runtime_tool(tool: &str) -> &str {
    tool.strip_prefix("mcp__appa__")
        .or_else(|| tool.strip_prefix("mcp__plugin_appa-runtime_appa__"))
        .or_else(|| tool.strip_prefix("mcp/appa/"))
        .or_else(|| tool.strip_prefix("mcp/plugin_appa-runtime_appa/"))
        .or_else(|| tool.strip_prefix("mcp/appa-guide/"))
        .unwrap_or(tool)
}

/// Why a runtime-provided tool has no trajectory to act for. The two are different things to
/// tell a caller: one says no hook saw this call, the other says the call does not identify
/// which of two sessions made it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unvouched {
    Nobody,
    Ambiguous,
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
/// the output as it is. `Deliver`: use this admitted value — a confined
/// result the check let through, or a sanitizer's derivation — as it
/// crossed. `Replace`: use the runtime's own words instead, a short note
/// saying the real output was not accepted or the narrowing it causes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ToolResultDecision {
    Keep,
    Deliver { value: String },
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
    #[error("the policy names tool {name} in {field}, which a served deployment cannot name: {detail}")]
    NonCanonicalTool {
        field: &'static str,
        name: String,
        detail: String,
    },
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
    #[error("host tool inventory is invalid: {0}")]
    InventoryRefused(String),
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
            | EventError::InventoryRefused(_)
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

/// Which deployment this is, and with it every rule the harness fixes rather than the
/// policy: how the policy names tools, how a recorded name is spelled back when the runtime
/// addresses the model, and which contracts may release a spawn.
///
/// A served deployment answers exactly one host, and carries that host's adapter: the
/// adapter derives a canonical identity for every call, so the policy names tools that way,
/// and its inverse gives the host spelling the model can dispatch. A host that embeds the
/// runtime, and `appa replay`, name tools their own way: what the runtime records is already
/// the name their model calls.
#[derive(Clone, Copy)]
pub(crate) enum ToolNaming {
    Canonical { adapter: Adapter },
    AsAuthored,
}

impl ToolNaming {
    /// The spelling of one recorded or authored tool name the model can act on. A
    /// contract's `(selector)` is the policy's own discriminator and no part of what the
    /// model calls, so the host spells the bare name. A canonical id outside the served
    /// adapter's range — one no call under this host can name — stays as recorded.
    pub(crate) fn model_spelling(self, recorded: &str) -> String {
        match self {
            ToolNaming::AsAuthored => recorded.to_string(),
            ToolNaming::Canonical { adapter } => appa_runtime_api::CanonicalTool::parse(bare_tool_name(recorded))
                .ok()
                .and_then(|tool| (adapter.spell)(&tool))
                .unwrap_or_else(|| recorded.to_string()),
        }
    }

    /// Which contracts may release a spawn here. The served adapter settles it, so a
    /// deployment cannot serve one host under another's rule: kagent's spawns are other
    /// agents called as tools, and a child trajectory is not something a per-call
    /// annotation can stand for. Claude Code's `Task` keeps the wildcard's cover, and so
    /// does a host that embeds the runtime and delegates under contracts it writes itself.
    pub(crate) fn spawn_coverage(self) -> SpawnCoverage {
        match self {
            ToolNaming::Canonical { adapter } => match adapter.name {
                AdapterName::ClaudeCode => SpawnCoverage::Wildcard,
                AdapterName::Kagent => SpawnCoverage::Declared,
            },
            ToolNaming::AsAuthored => SpawnCoverage::Wildcard,
        }
    }
}

impl Deployment {
    fn load(
        config: Config,
        modules: &crate::builtins::ModuleRegistry,
        gates: ConsultGates,
        naming: ToolNaming,
    ) -> Result<Deployment, OpenError> {
        let policy = compile_policy(&config, naming)?;
        validate_deployment(&policy, &config.externals)?;
        let annotator_builtins = policy
            .annotators()
            .filter_map(|(name, binding)| binding.builtin.map(|builtin| (name.as_str().to_string(), builtin)))
            .collect();
        let externals = ExternalServices::new(config.externals.clone(), modules, annotator_builtins, gates)
            .map_err(|error| OpenError::Modules(error.to_string()))?;
        Ok(Deployment {
            config,
            resident: RuntimeEngine::from_policy(&policy, naming),
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
/// wildcard covers a spawn as it covers any call. No caller chooses: the deployment reads it
/// off [`ToolNaming`], which carries the adapter a served runtime answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpawnCoverage {
    Declared,
    Wildcard,
}

/// Everything `open` and `open_in_memory` share before the log is chosen: the modules, the
/// consult gates, and the deployment compiled from the configuration.
struct Prepared {
    modules: crate::builtins::ModuleRegistry,
    gates: ConsultGates,
    deployment: Deployment,
    naming: ToolNaming,
}

impl Prepared {
    fn new(config: Config, modules: Option<PathBuf>, naming: ToolNaming) -> Result<Prepared, OpenError> {
        let modules =
            crate::builtins::load(modules.as_deref()).map_err(|error| OpenError::Modules(error.to_string()))?;
        let gates = ConsultGates::per_runtime();
        let deployment = Deployment::load(config, &modules, gates.clone(), naming)?;
        gates.serve_llm(deployment.config.externals.llm_bound());
        Ok(Prepared {
            modules,
            gates,
            deployment,
            naming,
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
                naming: self.naming,
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
    permits: std::sync::Mutex<std::collections::BTreeMap<PermitKey, Vec<Vouch>>>,
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
    /// Which deployment this is: how its policy names tools, the spelling the model can
    /// dispatch, and which contracts release a spawn. Settled at open and unchanged by a
    /// reload — it is the deployment kind, not the policy.
    naming: ToolNaming,
}

/// The trajectory an actor's events belong to: the child when the harness names one.
pub(crate) fn acting_trajectory(actor: &Actor) -> &TrajectoryId {
    actor.child.as_ref().unwrap_or(&actor.root)
}

fn inventory_refused(error: appa_runtime_api::ParseRefusal) -> EventError {
    let (appa_runtime_api::ParseRefusal::Malformed { detail } | appa_runtime_api::ParseRefusal::Unreadable { detail }) =
        error;
    EventError::InventoryRefused(detail)
}

fn inventory_at(
    log: &Log,
    actor: &Actor,
    adapter: Adapter,
) -> Result<appa_runtime_api::inventory::ToolInventory, EventError> {
    use appa_runtime_api::inventory::ToolInventory;
    let mut previous = ToolInventory::default();
    if actor.child.is_none() {
        #[derive(serde::Deserialize)]
        struct OpeningInventory {
            #[serde(default)]
            appa_inventory: ToolInventory,
        }
        let source = std::str::from_utf8(log.policy_file())
            .map_err(|_| EventError::PolicyUnavailable("stored policy is not UTF-8".into()))?;
        let opening: OpeningInventory = toml::from_str(source)
            .map_err(|_| EventError::PolicyUnavailable("stored inventory does not decode".into()))?;
        previous = opening.appa_inventory;
    }
    let scope = crate::engine::engine_id(acting_trajectory(actor));
    for observation in log
        .inventories()
        .iter()
        .filter(|observation| observation.actor == scope)
    {
        if observation.adapter != adapter.name {
            return Err(EventError::InventoryRefused(
                "an actor cannot change its plugin adapter".into(),
            ));
        }
        previous = previous
            .extending(&observation.inventory, adapter)
            .map_err(inventory_refused)?;
    }
    Ok(previous)
}

impl Runtime {
    /// Run the serving load checks without opening a store, making network requests,
    /// or activating a deployment. Unknown inventory is reported, not rejected.
    pub(crate) fn validate_served(
        config: Config,
        adapter: Adapter,
    ) -> Result<crate::tool_validation::ValidationReport, OpenError> {
        let report = crate::tool_validation::resolve(
            config.policy_file().value(),
            adapter,
            &config.inventory,
            &config.server_aliases,
        )
        .report;
        Prepared::new(config, None, ToolNaming::Canonical { adapter })?;
        Ok(report)
    }

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
}

impl Inner {
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
    ///
    /// A retired policy decides under the identities this deployment derives now, so it
    /// meets the naming rule this deployment serves under or the trajectory does not
    /// reopen: a stored policy naming a tool the served host's raw way confines and
    /// excepts nothing, while a wildcard contract still permits the call.
    fn retired_engine(&self, key: &str, bytes: &[u8]) -> Result<Arc<RuntimeEngine>, EventError> {
        if let Some(engine) = self
            .retired
            .lock()
            .expect("the retired-engine mutex is never poisoned: no panic runs while it is held")
            .get(key)
        {
            return Ok(Arc::clone(engine));
        }
        let compiled = compile_stored_for_host(bytes, self.naming).map_err(EventError::PolicyUnavailable)?;
        let engine = Arc::new(RuntimeEngine::from_policy(&compiled, self.naming));
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
        let prepared = Prepared::new(config, modules, ToolNaming::AsAuthored)?;
        prepared.assemble(Backend::Sqlite { path: db })
    }

    /// The deployment `appa runtime` serves: [`Runtime::open`], plus the served-deployment
    /// normalization of native policy names to internal identities. One compile answers both. The
    /// served adapter comes in because a served deployment answers exactly one host: its
    /// spelling of a tool is what the runtime says where it addresses that host's model,
    /// and its rule is which contracts release a spawn.
    pub(crate) fn open_served(
        config: Config,
        db: PathBuf,
        modules: Option<PathBuf>,
        adapter: Adapter,
    ) -> Result<Runtime, OpenError> {
        let prepared = Prepared::new(config, modules, ToolNaming::Canonical { adapter })?;
        prepared.assemble(Backend::Sqlite { path: db })
    }

    /// The spelling of a recorded tool name this deployment's model can dispatch: the
    /// served host's own, or — where the host embeds the runtime and names its own tools —
    /// the recorded name itself. Every text the runtime addresses to the model names a
    /// tool this way.
    pub(crate) fn model_spelling(&self, recorded: &str) -> String {
        self.inner.naming.model_spelling(recorded)
    }

    /// The deployment `appa replay` runs: the same session and engine over a log that lives
    /// only as long as this value, with every authority and sanitizer answered in process —
    /// approve, and the body unchanged — as if the bound party had. Annotators, audience
    /// sources, and identity stay bound as configured. Nothing of the run survives the process.
    pub fn open_in_memory(config: Config, modules: Option<PathBuf>) -> Result<Runtime, OpenError> {
        let mut prepared = Prepared::new(config, modules, ToolNaming::AsAuthored)?;
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
    ///
    /// How this deployment names tools is its own state and no caller's choice, so the
    /// reload holds the naming it opened with: a served deployment's rule that the policy
    /// names every tool canonically survives the reload, and a refused candidate changes
    /// nothing — the deployment that was serving keeps serving.
    pub fn reload(&self, config: Config) -> Result<Reloaded, OpenError> {
        let deployment = Deployment::load(config, &self.inner.modules, self.inner.gates.clone(), self.inner.naming)?;
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
        self.create_session_under(id, deployment)
    }

    pub(crate) fn create_session_with_inventory(
        &self,
        id: TrajectoryId,
        inventory: appa_runtime_api::inventory::ToolInventory,
    ) -> Result<Session, EventError> {
        let config = self
            .inner
            .deployment()
            .config
            .with_inventory(inventory)
            .map_err(EventError::PolicyUnavailable)?;
        let deployment = Deployment::load(config, &self.inner.modules, self.inner.gates.clone(), self.inner.naming)
            .map_err(|error| EventError::PolicyUnavailable(error.to_string()))?;
        self.create_session_under(id, Arc::new(deployment))
    }

    /// Reserve identities in the actor's own scope, independently of the immutable
    /// policy registry. The same log CAS as tool admission makes a concurrent
    /// discovery retry before it can authorize a call under stale evidence.
    pub(crate) fn observe_inventory(
        &self,
        actor: &appa_runtime_api::Actor,
        adapter: appa_runtime_api::Adapter,
        candidate: &appa_runtime_api::inventory::ToolInventory,
    ) -> Result<(), EventError> {
        use appa_runtime_api::inventory::ToolInventory;
        candidate.validate(adapter).map_err(inventory_refused)?;
        let scope = crate::engine::engine_id(actor.child.as_ref().unwrap_or(&actor.root));
        const ATTEMPTS: u32 = 8;
        for _ in 0..ATTEMPTS {
            let log = self.inner.log(&actor.root)?;
            let previous = inventory_at(&log, actor, adapter)?;
            let combined = previous.extending(candidate, adapter).map_err(inventory_refused)?;
            let previous_tools: std::collections::BTreeMap<_, _> =
                previous.tools.iter().map(|tool| (&tool.name, &tool.tool)).collect();
            let previous_sources: std::collections::BTreeMap<_, _> =
                previous.sources.iter().map(|source| (&source.server, source)).collect();
            let delta = ToolInventory {
                tools: combined
                    .tools
                    .iter()
                    .filter(|tool| previous_tools.get(&tool.name).copied() != Some(&tool.tool))
                    .cloned()
                    .collect(),
                sources: combined
                    .sources
                    .iter()
                    .filter(|source| previous_sources.get(&source.server).copied() != Some(*source))
                    .cloned()
                    .collect(),
            };
            if delta.tools.is_empty() && delta.sources.is_empty() {
                return Ok(());
            }
            let observation = appa_eventlog::InventoryObservation {
                actor: scope.clone(),
                adapter: adapter.name,
                // Persist only the new evidence, not another full copy of history.
                inventory: delta,
            };
            match self.inner.store.append_inventory(&log, &observation) {
                Ok(()) => return Ok(()),
                Err(appa_eventlog::AppendError::Conflict { .. }) => continue,
                Err(error) => {
                    self.inner
                        .note_store_error(Some(&actor.root), crate::events::StoreOperation::Append, &error);
                    return Err(EventError::Storage(error.to_string()));
                }
            }
        }
        Err(EventError::Contended { attempts: ATTEMPTS })
    }

    /// Read-only preflight against the policy this family opened under. Another
    /// actor's complete inventory does not make absent policy tools invalid.
    pub(crate) fn preflight_inventory(
        &self,
        actor: Option<&Actor>,
        adapter: Adapter,
        inventory: &appa_runtime_api::inventory::ToolInventory,
    ) -> Result<crate::tool_validation::ValidationReport, EventError> {
        match actor {
            Some(actor) => {
                let mut report = self.check_inventory(&actor.root, adapter, inventory)?;
                let previous = inventory_at(&self.inner.log(&actor.root)?, actor, adapter)?;
                let accepted = previous.identities(adapter).map_err(inventory_refused)?;
                let names: std::collections::BTreeMap<_, _> =
                    accepted.iter().map(|(name, id, _)| (name.as_str(), id)).collect();
                let identities: std::collections::BTreeMap<_, _> =
                    accepted.iter().map(|(name, id, _)| (id, name.as_str())).collect();
                let mut conflicts = std::collections::BTreeSet::new();
                for observed in &inventory.tools {
                    if let Ok(id) = (adapter.derive)(&observed.tool)
                        && (names
                            .get(observed.name.as_str())
                            .is_some_and(|previous| **previous != id.canonical)
                            || identities
                                .get(&id.canonical)
                                .is_some_and(|previous| *previous != observed.name))
                    {
                        conflicts.insert(observed.name.as_str());
                    }
                }
                for check in &mut report.tools {
                    if conflicts.contains(check.tool.as_str()) {
                        check.status = crate::tool_validation::ToolStatus::Invalid {
                            reason: "tool identity conflicts with an earlier observation in this actor".into(),
                        };
                    }
                }
                report.accepted_tools = previous.tools;
                Ok(report)
            }
            None => {
                let serving = self.inner.deployment();
                Ok(crate::tool_validation::resolve(
                    serving.config.policy_file().value(),
                    adapter,
                    inventory,
                    &serving.config.server_aliases,
                )
                .report)
            }
        }
    }

    /// Read-only preflight under a pinned family policy, including child startup.
    pub(crate) fn check_inventory(
        &self,
        root: &TrajectoryId,
        adapter: Adapter,
        inventory: &appa_runtime_api::inventory::ToolInventory,
    ) -> Result<crate::tool_validation::ValidationReport, EventError> {
        let log = self.inner.log(root)?;
        self.inner.resolve_policy(&self.inner.deployment(), &log)?;
        #[derive(serde::Deserialize)]
        struct Rules {
            policy: toml::Value,
            #[serde(default)]
            server_aliases: std::collections::BTreeMap<String, String>,
        }
        let source = std::str::from_utf8(log.policy_file())
            .map_err(|_| EventError::PolicyUnavailable("stored policy is not UTF-8".into()))?;
        let rules: Rules = toml::from_str(source)
            .map_err(|_| EventError::PolicyUnavailable("stored validation rules do not decode".into()))?;
        Ok(crate::tool_validation::resolve(&rules.policy, adapter, inventory, &rules.server_aliases).report)
    }

    fn create_session_under(&self, id: TrajectoryId, deployment: Arc<Deployment>) -> Result<Session, EventError> {
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

    /// [`Runtime::report`], off the async workers.
    ///
    /// Stripping, serializing and gzipping a long trajectory is seconds of CPU over as much
    /// as [`crate::yell::report::MAX_PLAIN_BYTES`], and the same runtime serves the hooks that gate
    /// an agent's every tool call. A report is never worth stalling the sessions it is about,
    /// so every async caller goes through here and the synchronous builder stays synchronous.
    pub(crate) async fn report_off_thread(
        self: &Arc<Self>,
        request: yell::ReportRequest,
    ) -> Result<yell::Finished, yell::Oversize> {
        let runtime = Arc::clone(self);
        tokio::task::spawn_blocking(move || runtime.report(request))
            .await
            .expect("building a report does not panic")
    }

    /// One finished `openappa.yell.v1` document, ready to write and to send.
    ///
    /// Assembling here rather than in the CLI is what keeps the size loop honest: only a
    /// finished, gzipped document can be measured against the receiver's limits, and only this
    /// process can rebuild a smaller export. So an oversized report is built again from the
    /// source under half the counts — never trimmed as a document, which would leave its token
    /// numbering full of holes — until it fits.
    pub(crate) fn report(&self, request: yell::ReportRequest) -> Result<yell::Finished, yell::Oversize> {
        let report_id = yell::ReportId::generate();
        let origin = yell::Origin::new(request.author, request.mode);
        let mut budget = yell::Budget::default();
        loop {
            let projection = self.projection(request.selection.clone(), request.mode, budget);
            let (facts, events) = projection.counts();
            let report = yell::Report::serving(
                report_id.clone(),
                origin,
                request.message.clone(),
                request.harness,
                projection,
            );
            match report.finalize() {
                Ok(finished) => return Ok(finished),
                // Nothing left to drop: the message, the build and the policy are the whole
                // document, and they are over the limit on their own.
                Err(oversize) if facts + events == 0 => return Err(oversize),
                Err(_) => {
                    budget = yell::Budget {
                        facts: Some(facts / 2),
                        events: Some(events / 2),
                    }
                }
            }
        }
    }

    /// One trajectory's decisions, stripped for a report that leaves this machine.
    ///
    /// A read like [`Runtime::audit`]: it gates nothing and appends nothing. Unlike an audit
    /// it survives a log the engine refuses — a refused log is the very thing worth reporting
    /// — and carries the refusal as a closed class instead of the facts a view would have
    /// given. What may leave is decided in [`crate::yell::tables`], never here.
    pub(crate) fn projection(
        &self,
        selection: yell::Selection,
        mode: yell::Mode,
        budget: yell::Budget,
    ) -> yell::Projection {
        let deployment = self.inner.deployment();
        // Every path below that has no trajectory to show still says what the rules are, from
        // the policy this deployment serves now. A report with no facts is still a report about
        // a policy, and "the runtime would not give me my session" is a thing worth yelling.
        let serving = || policy_section(deployment.config.policy_file().bytes());
        let root = match selection {
            yell::Selection::RulesOnly => {
                return yell::Projection::rules_only(serving(), mode, yell::OmittedReason::NotRequested);
            }
            yell::Selection::Vouched(root) => root,
            yell::Selection::Recent => match yell::resolve(self.inner.recent_root(yell::RECENT_WINDOW)) {
                Ok(root) => root,
                Err(omitted_reason) => return yell::Projection::rules_only(serving(), mode, omitted_reason),
            },
        };
        let yelling = Some(root.clone());
        let Ok(log) = self.inner.log(&root) else {
            // The store error is already recorded as a runtime event by `Inner::log`.
            return yell::Projection::rules_only(serving(), mode, yell::OmittedReason::LogUnavailable);
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
        let source = yell::Source {
            facts: log.facts(),
            events: self.inner.events(&root),
            trust_chain,
            policy: policy_section(log.policy_file()),
            vouched,
            parents: yell::branches(log.facts(), view.as_ref(), policy.as_ref()),
            replay_refused,
            yelling,
        };
        yell::build(source, mode, budget)
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
        self.spend_vouch(&PermitKey::offer(&quoted), acting);
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

    /// Record that this trajectory stands behind this key, for the request
    /// that runs it. `ruling` is a person's answer the harness obtained
    /// through its own review channel; it rides the vouch and is spent
    /// with it, so it can answer exactly the execution it was given for.
    pub(crate) fn vouch(&self, key: &PermitKey, acting: &Actor, ruling: Option<appa_runtime_api::Ruling>) {
        let mut permits = self.inner.permits.lock().expect("the permit mutex is never poisoned");
        let holders = permits.entry(key.clone()).or_default();
        // An offer has one current pursuer: its new vouch replaces the former
        // pursuer's. A yell ticket may be quoted by independent sessions, so
        // retain those holders and refuse ambiguity when it is consumed.
        if matches!(key, PermitKey::Offer(_)) {
            holders.clear();
        }
        match holders.iter_mut().find(|holder| holder.actor == *acting) {
            Some(holder) => holder.ruling = ruling,
            None => holders.push(Vouch {
                actor: acting.clone(),
                ruling,
            }),
        }
    }

    /// The trajectory vouched for this key, taken once, with the ruling its harness
    /// attached.
    ///
    /// Two trajectories standing behind one key is not a tie to break: it means the key does
    /// not identify a caller, and answering either one would put one session's standing
    /// behind another session's call. That case keeps the record rather than consuming it —
    /// destroying it would make the *next* identical call look like one nothing vouched for,
    /// and the two need different answers. The turn's end releases it either way.
    pub(crate) fn take_vouched(&self, key: &PermitKey) -> Result<(Actor, Option<appa_runtime_api::Ruling>), Unvouched> {
        let mut permits = self.inner.permits.lock().expect("the permit mutex is never poisoned");
        let holders = permits.get_mut(key).ok_or(Unvouched::Nobody)?;
        if holders.len() != 1 {
            return Err(Unvouched::Ambiguous);
        }
        let vouch = holders.remove(0);
        permits.remove(key);
        Ok((vouch.actor, vouch.ruling))
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

    fn spend_vouch(&self, key: &PermitKey, acting: &Actor) {
        let mut permits = self.inner.permits.lock().expect("the permit mutex is never poisoned");
        let Some(holders) = permits.get_mut(key) else {
            return;
        };
        holders.retain(|holder| holder.actor != *acting);
        if holders.is_empty() {
            permits.remove(key);
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

    /// Whether this deployment lets an agent report on its own. Read from the deployment
    /// this runtime serves *now*, so a `/reload` that flips the knob decides the next MCP
    /// session rather than the next restart.
    pub(crate) fn agent_yell(&self) -> bool {
        self.inner.deployment().config.reporting.agent_yell
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

    // The control tool is the runtime's own: the hooks answer that identity before the
    // executor classifies a call or confines its result, so a policy naming it declares
    // something the deployment can never apply. A contract naming it and a `[deployment]`
    // field naming it are the same claim, and both are refused rather than accepted as a
    // silent no-op. Both of its spellings make that claim: a harness translates the
    // advertised alias into the canonical id before the hooks see it, so an authored
    // deployment naming the alias reaches remedy handling the same way. A served
    // deployment refuses the alias earlier, for not being canonical at all.
    let rc = policy.registry_config();
    let contracts = rc.tools.iter().map(|tool| tool.name().as_str());
    for name in contracts.chain(policy.deployment_tool_names().map(|(_, name)| name)) {
        if appa_runtime_api::is_reserved_tool_name(bare_tool_name(name)) {
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

/// The tool an authored `[[policy.tool]]` name names, before any `(selector)`.
fn bare_tool_name(authored: &str) -> &str {
    authored.split('(').next().unwrap_or(authored)
}

fn compile_policy(config: &Config, naming: ToolNaming) -> Result<appa_policy::Config, OpenError> {
    let policy = resolve_served_policy(
        config.policy_file().value(),
        naming,
        &config.inventory,
        &config.server_aliases,
    )
    .map_err(OpenError::UnsupportedPolicy)?;
    let text = toml::to_string(&policy)
        .map_err(|error| OpenError::UnsupportedPolicy(format!("the policy table does not serialize: {error}")))?;
    appa_policy::Config::from_toml_str(&text).map_err(|error| OpenError::Policy(Box::new(error)))
}

/// The `[policy]` table of a stored policy file, with the key of the bytes it came from.
///
/// A trajectory's own bytes rather than the deployment's current ones wherever there is a
/// trajectory: the log pins its policy file, so a reload since the session opened does not
/// rewrite the rules a report explains. The key comes from the same bytes, so the document and
/// its fingerprint are one snapshot.
fn policy_section(bytes: &[u8]) -> Option<(toml::Value, String)> {
    let text = std::str::from_utf8(bytes).ok()?;
    let composed: toml::Value = toml::from_str(text).ok()?;
    let document = composed.get("policy")?.clone();
    Some((document, crate::engine::policy_file_key(bytes)))
}

#[cfg(test)]
fn compile_stored_policy(bytes: &[u8]) -> Result<appa_policy::Config, String> {
    compile_stored_for_host(bytes, ToolNaming::AsAuthored)
}

fn resolve_served_policy(
    policy: &toml::Value,
    naming: ToolNaming,
    inventory: &appa_runtime_api::inventory::ToolInventory,
    aliases: &std::collections::BTreeMap<String, String>,
) -> Result<toml::Value, String> {
    match naming {
        ToolNaming::AsAuthored => Ok(policy.clone()),
        ToolNaming::Canonical { adapter } => {
            let resolved = crate::tool_validation::resolve(policy, adapter, inventory, aliases);
            if !resolved.report.is_valid() {
                let mut errors = resolved.report.errors;
                errors.extend(resolved.report.tools.into_iter().filter_map(|tool| match tool.status {
                    crate::tool_validation::ToolStatus::Invalid { reason } => Some(format!("{}: {reason}", tool.tool)),
                    _ => None,
                }));
                return Err(errors.join("; "));
            }
            Ok(resolved.policy)
        }
    }
}

fn compile_stored_for_host(bytes: &[u8], naming: ToolNaming) -> Result<appa_policy::Config, String> {
    let text = std::str::from_utf8(bytes).map_err(|error| format!("the stored policy file is not UTF-8: {error}"))?;
    let value: toml::Value =
        toml::from_str(text).map_err(|error| format!("the stored policy file does not parse: {error}"))?;
    let policy = value
        .get("policy")
        .ok_or("the stored policy file has no [policy] table")?;
    let inventory = value
        .get("appa_inventory")
        .cloned()
        .map(toml::Value::try_into)
        .transpose()
        .map_err(|error| format!("invalid stored inventory: {error}"))?
        .unwrap_or_default();
    let aliases = value
        .get("server_aliases")
        .cloned()
        .map(toml::Value::try_into)
        .transpose()
        .map_err(|error| format!("invalid stored aliases: {error}"))?
        .unwrap_or_default();
    let policy = resolve_served_policy(policy, naming, &inventory, &aliases)?;
    let text =
        toml::to_string(&policy).map_err(|error| format!("the stored policy table does not serialize: {error}"))?;
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

    /// The served adapter's inverse is what the runtime says where it addresses that
    /// host's model. A contract's `(selector)` is the policy's own discriminator and no
    /// part of the name the model calls; a canonical id the served host cannot name — and
    /// the wildcard, which is no tool — stays as it is.
    #[test]
    fn a_served_deployment_spells_a_recorded_name_the_way_its_host_dispatches_it() {
        let claude_code = ToolNaming::Canonical {
            adapter: appa_adapter_claude_code::adapter(),
        };
        let kagent = ToolNaming::Canonical {
            adapter: appa_adapter_kagent::adapter(),
        };
        for (naming, recorded, expected) in [
            (claude_code, "host/claude-code/Read", "Read"),
            (claude_code, "host/claude-code/Bash(command:git)", "Bash"),
            (claude_code, "mcp/github/create_issue", "mcp__github__create_issue"),
            (
                claude_code,
                appa_runtime_api::CONTROL_TOOL,
                "mcp__plugin_appa-runtime_appa__execute_remedy_plan",
            ),
            (claude_code, "agent/kagent/log-analyst", "agent/kagent/log-analyst"),
            (claude_code, "*", "*"),
            (kagent, "mcp/k8s/get_pods", "mcp:k8s/get_pods"),
            (kagent, "agent/kagent/log-analyst", "agent:kagent/log-analyst"),
            (kagent, "host/kagent-gate/outer", "gate:outer"),
            (kagent, appa_runtime_api::CONTROL_TOOL, "appa:execute_remedy_plan"),
            (kagent, "host/claude-code/Read", "host/claude-code/Read"),
        ] {
            assert_eq!(naming.model_spelling(recorded), expected, "{recorded}");
        }
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
        Deployment::load(
            config,
            &crate::builtins::ModuleRegistry::empty(),
            test_permits(),
            ToolNaming::AsAuthored,
        )
    }

    /// The hooks answer the control tool's identity before a call is classified or a result
    /// confined, so a `[deployment]` field naming it claims a treatment the runtime can never
    /// apply. A wildcard contract makes the name pass coverage, so only this refusal stops the
    /// claim from opening as a silent no-op — the same refusal a contract naming it gets.
    ///
    /// Under either spelling. This deployment is authored, so nothing else reads its tool
    /// names: a harness translates the advertised alias into the canonical id on its way to
    /// the hooks, and the field naming the alias would open and never apply.
    #[test]
    fn no_deployment_field_may_name_the_control_tool() {
        let policy = |field: &str, tool: &str| {
            format!(
                r#"
                version = 2
                [[annotator]]
                name = "any"
                builtin = "claude-code"
                [[tool]]
                name = "*"
                annotator = "any"
                [deployment]
                {field} = ["{tool}"]
            "#
            )
        };
        for tool in [
            appa_runtime_api::CONTROL_TOOL,
            appa_runtime_api::ADVERTISED_CONTROL_TOOL,
        ] {
            for field in ["assumed_tools", "provider_run_tools", "confined_results"] {
                assert!(
                    matches!(
                        load(claude_config(&policy(field, tool))),
                        Err(OpenError::ReservedTool(_))
                    ),
                    "[deployment] {field} naming {tool} must refuse to open"
                );
            }
        }
        assert!(
            load(claude_config(&policy("assumed_tools", "host/claude-code/Read"))).is_ok(),
            "every other name in the same field still opens"
        );
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
        assert!(
            Deployment::load(
                tool_level,
                &crate::builtins::ModuleRegistry::empty(),
                test_permits(),
                ToolNaming::AsAuthored
            )
            .is_ok()
        );
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

    /// Two stored policies that differ only in how their `[deployment]` field spells the
    /// tool it confines. The wildcard contract covers every name, so either spelling passes
    /// coverage at load; the contract permits the call, and only the confinement reads the
    /// name exactly.
    fn confining_policy(confined: &str) -> Config {
        claude_config(&format!(
            r#"
            version = 2
            [[annotator]]
            name = "any"
            builtin = "claude-code"
            [[tool]]
            name = "host/claude-code/Bash"
            [[tool]]
            name = "*"
            annotator = "any"
            [deployment]
            confined_results = ["{confined}"]
            "#
        ))
    }

    /// The canonical policy the served deployment serves now: different bytes from either
    /// stored one, so a trajectory recorded under those reopens through the retired branch.
    fn served_policy() -> Config {
        claude_config(
            r#"
            version = 2
            [[tool]]
            name = "host/claude-code/Bash"
            [[tool]]
            name = "host/claude-code/Read"
            [deployment]
            confined_results = ["host/claude-code/Bash"]
            "#,
        )
    }

    /// A trajectory recorded before the upgrade carries its own policy bytes, and reopening
    /// it compiles them. A served deployment derives a canonical identity for every call, so
    /// a stored policy naming a tool the host's raw way in a `[deployment]` field confines
    /// nothing while its contract still permits the call: the served runtime refuses that
    /// trajectory rather than deciding under it, and reopens a stored canonical policy.
    #[tokio::test]
    async fn a_served_deployment_refuses_a_stored_policy_its_naming_rule_rejects() {
        for (confined, refuses) in [("Bash", true), ("host/claude-code/Bash", false)] {
            let dir = tempfile::tempdir().expect("a temp dir is creatable");
            let db = dir.path().join("appa.db");
            let root = TrajectoryId("upgraded".to_string());

            // Recorded under a deployment that embeds the runtime and names tools its own way.
            let recorded = Runtime::open(confining_policy(confined), db.clone(), None).expect("the deployment opens");
            assert_eq!(
                crate::hooks::handle(
                    &recorded,
                    appa_runtime_api::HookEvent::SessionStart { root: root.clone() }
                )
                .await,
                appa_runtime_api::HookDecision::Ack
            );
            drop(recorded);

            // The upgrade: the same log, served under the canonical naming rule.
            let served = Runtime::open_served(served_policy(), db, None, appa_adapter_claude_code::adapter())
                .expect("the served deployment opens");
            let decision = crate::hooks::handle(
                &served,
                appa_runtime_api::HookEvent::ToolCall {
                    actor: Actor {
                        root: root.clone(),
                        child: None,
                    },
                    call: ProposedCall {
                        tool: "host/claude-code/Bash".to_string(),
                        arguments: raw(serde_json::json!({"command": "ls"})),
                    },
                    spawn: false,
                    ruling: None,
                },
            )
            .await;
            assert_eq!(
                matches!(decision, appa_runtime_api::HookDecision::Refuse { .. }),
                refuses,
                "the stored policy confines {confined}: {decision:?}"
            );
        }
    }

    /// Native policy names remain usable; normalization is internal and applies equally
    /// to startup and candidate reloads.
    #[test]
    fn a_served_deployment_accepts_native_policy_at_startup_and_on_reload() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let db = dir.path().join("appa.db");
        let raw = || {
            claude_config(
                r#"
                version = 2
                [[tool]]
                name = "Bash"
                "#,
            )
        };
        let initial = Runtime::open_served(raw(), db.clone(), None, appa_adapter_claude_code::adapter())
            .expect("native names load");
        drop(initial);

        let served = Runtime::open_served(served_policy(), db, None, appa_adapter_claude_code::adapter())
            .expect("the served deployment opens");
        served.reload(raw()).expect("native names reload");
        served.reload(served_policy()).expect("a canonical policy reloads");
    }

    #[test]
    fn a_known_uncovered_candidate_inventory_cannot_replace_the_serving_policy() {
        use appa_runtime_api::inventory::{ObservedTool, ToolInventory};
        let dir = tempfile::tempdir().unwrap();
        let runtime = Runtime::open_served(
            served_policy(),
            dir.path().join("appa.db"),
            None,
            appa_adapter_claude_code::adapter(),
        )
        .unwrap();
        let before = runtime
            .preflight_inventory(None, appa_adapter_claude_code::adapter(), &ToolInventory::default())
            .unwrap();
        let mut candidate = claude_config("version = 2\n[[tool]]\nname = 'Read'\n");
        candidate.inventory = ToolInventory {
            tools: vec![ObservedTool {
                name: "Bash".into(),
                tool: "builtin:Bash".into(),
            }],
            sources: vec![],
        };
        assert!(runtime.reload(candidate).is_err());
        let after = runtime
            .preflight_inventory(None, appa_adapter_claude_code::adapter(), &ToolInventory::default())
            .unwrap();
        assert_eq!(before.tools, after.tools);
    }

    #[test]
    fn observed_native_bindings_survive_reload_and_process_reopen() {
        use appa_runtime_api::inventory::{ObservedTool, ToolInventory};
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("appa.db");
        let config = || claude_config("version = 2\n[[tool]]\nname = \"read_secret\"\n");
        let adapter = appa_adapter_kagent::adapter();
        let runtime = Runtime::open_served(config(), db.clone(), None, adapter).unwrap();
        let id = TrajectoryId("inventory-root".into());
        let inventory = ToolInventory {
            tools: vec![ObservedTool {
                name: "read_secret".into(),
                tool: "mcp:demo/read_secret".into(),
            }],
            ..ToolInventory::default()
        };
        runtime.create_session_with_inventory(id.clone(), inventory).unwrap();
        runtime
            .reload(claude_config("version = 2\n[[tool]]\nname = \"other\"\n"))
            .unwrap();
        runtime.live(&id, &id).unwrap();
        let log = runtime.inner.log(&id).unwrap();
        let compiled = compile_stored_for_host(log.policy_file(), ToolNaming::Canonical { adapter }).unwrap();
        let engine = RuntimeEngine::from_policy(&compiled, ToolNaming::Canonical { adapter });
        assert!(engine.names_tool("mcp/demo/read_secret"));
        assert!(!engine.names_tool("read_secret"));
        drop(runtime);
        let reopened = Runtime::open_served(config(), db, None, adapter).unwrap();
        reopened.live(&id, &id).unwrap();
    }

    #[test]
    fn late_inventory_is_scoped_idempotent_and_cannot_rebind_after_reopen() {
        use appa_runtime_api::inventory::{ObservedTool, ToolInventory};
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("appa.db");
        let config =
            || claude_config("version = 2\n[[tool]]\nname = 'mcp/demo/read'\n[[tool]]\nname = 'mcp/other/read'\n");
        let adapter = appa_adapter_kagent::adapter();
        let runtime = Runtime::open_served(config(), db.clone(), None, adapter).unwrap();
        let actor = Actor {
            root: TrajectoryId("inventory-root".into()),
            child: None,
        };
        runtime.create_session(actor.root.clone()).unwrap();
        let before = runtime.inner.log(&actor.root).unwrap();
        let inventory = |server: &str| ToolInventory {
            tools: vec![ObservedTool {
                name: "read".into(),
                tool: format!("mcp:{server}/read"),
            }],
            ..ToolInventory::default()
        };
        runtime.observe_inventory(&actor, adapter, &inventory("demo")).unwrap();
        let accepted = runtime.inner.log(&actor.root).unwrap();
        runtime.observe_inventory(&actor, adapter, &inventory("demo")).unwrap();
        runtime
            .observe_inventory(&actor, adapter, &ToolInventory::default())
            .unwrap();
        assert_eq!(runtime.inner.log(&actor.root).unwrap().basis(), accepted.basis());
        assert!(matches!(
            runtime.observe_inventory(&actor, adapter, &inventory("other")),
            Err(EventError::InventoryRefused(_))
        ));
        assert_eq!(runtime.inner.log(&actor.root).unwrap().basis(), accepted.basis());
        let child = Actor {
            root: actor.root.clone(),
            child: Some(TrajectoryId("child".into())),
        };
        runtime.observe_inventory(&child, adapter, &inventory("other")).unwrap();
        let after = runtime.inner.log(&actor.root).unwrap();
        assert_eq!(after.facts(), before.facts());
        assert_eq!(after.policy_file(), before.policy_file());
        assert_eq!(after.inventories().len(), 2);
        drop(runtime);

        let runtime = Runtime::open_served(config(), db, None, adapter).unwrap();
        assert!(matches!(
            runtime.observe_inventory(&actor, adapter, &inventory("other")),
            Err(EventError::InventoryRefused(_))
        ));
        runtime.observe_inventory(&actor, adapter, &inventory("demo")).unwrap();
        runtime.observe_inventory(&child, adapter, &inventory("other")).unwrap();
        runtime.live(&actor.root, &actor.root).unwrap();
    }

    #[test]
    fn inventory_retries_contention_and_only_one_racing_identity_wins() {
        use appa_runtime_api::inventory::{ObservedTool, ToolInventory};
        let dir = tempfile::tempdir().unwrap();
        let adapter = appa_adapter_kagent::adapter();
        let runtime = Runtime::open_served(
            claude_config("version = 2\n[[tool]]\nname = 'read'\n"),
            dir.path().join("appa.db"),
            None,
            adapter,
        )
        .unwrap();
        let actor = Actor {
            root: TrajectoryId("racing-inventory".into()),
            child: None,
        };
        runtime.create_session(actor.root.clone()).unwrap();
        let inventory = |server: &str| ToolInventory {
            tools: vec![ObservedTool {
                name: "read".into(),
                tool: format!("mcp:{server}/read"),
            }],
            ..ToolInventory::default()
        };
        runtime.inner.store.contend_next_appends(1);
        let barrier = std::sync::Barrier::new(2);
        let results = std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                barrier.wait();
                runtime.observe_inventory(&actor, adapter, &inventory("first"))
            });
            let second = scope.spawn(|| {
                barrier.wait();
                runtime.observe_inventory(&actor, adapter, &inventory("second"))
            });
            [first.join().unwrap(), second.join().unwrap()]
        });
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(EventError::InventoryRefused(_))))
                .count(),
            1
        );
        let log = runtime.inner.log(&actor.root).unwrap();
        assert_eq!(log.inventories().len(), 1);
        assert_eq!(log.basis(), 3, "opening, injected competing batch, accepted inventory");
    }

    #[test]
    fn remote_preflight_is_read_only_and_uses_the_requested_policy() {
        use crate::tool_validation::{ToolStatus, ValidationReport};
        let dir = tempfile::tempdir().unwrap();
        let adapter = appa_adapter_kagent::adapter();
        let runtime = Runtime::open_served(
            claude_config("version = 2\n[[tool]]\nname = 'read'\n"),
            dir.path().join("appa.db"),
            None,
            adapter,
        )
        .unwrap();
        let request = |root: Option<&str>, name: &str| {
            serde_json::to_vec(&serde_json::json!({
                "protocol": appa_runtime_api::PROTOCOL, "adapter": "kagent", "root_id": root,
                "inventory": {"tools": [{"name": name, "tool": format!("mcp:demo/{name}")}]}
            }))
            .unwrap()
        };
        let report = |root, name| {
            let (status, report) = crate::tool_validation::answer(&runtime, adapter, &request(root, name));
            assert_eq!(status, 200);
            serde_json::from_value::<ValidationReport>(report).unwrap()
        };
        assert!(report(None, "read").is_valid());
        assert!(!report(None, "write").is_valid());
        assert_eq!(
            crate::tool_validation::answer(&runtime, adapter, &request(Some("preflight"), "read")).0,
            404
        );
        let root = adapter.name.root("preflight");
        assert!(matches!(runtime.inner.log(&root), Err(EventError::UnknownTrajectory)));
        runtime.create_session(root.clone()).unwrap();
        let before = runtime.inner.log(&root).unwrap();
        runtime
            .reload(claude_config("version = 2\n[[tool]]\nname = 'write'\n"))
            .unwrap();
        assert!(report(None, "write").is_valid());
        assert!(!report(None, "read").is_valid());
        assert!(report(Some("preflight"), "read").is_valid());
        assert!(!report(Some("preflight"), "write").is_valid());
        let after = runtime.inner.log(&root).unwrap();
        assert_eq!(before.basis(), after.basis());
        assert_eq!(before.policy_file(), after.policy_file());
        assert!(after.inventories().is_empty());
        let unknown = crate::tool_validation::answer(
            &runtime,
            adapter,
            br#"{"protocol":1,"adapter":"kagent","inventory":{}}"#,
        );
        let unknown: ValidationReport = serde_json::from_value(unknown.1).unwrap();
        assert!(unknown.is_valid());
        assert!(
            unknown
                .tools
                .iter()
                .any(|tool| matches!(tool.status, ToolStatus::Unknown { .. }))
        );
    }

    #[test]
    fn remote_preflight_returns_only_this_actors_durable_reservations() {
        use appa_runtime_api::inventory::{ObservedTool, ToolInventory};
        let dir = tempfile::tempdir().unwrap();
        let adapter = appa_adapter_kagent::adapter();
        let db = dir.path().join("appa.db");
        let config = || claude_config("version = 2\n[[tool]]\nname = 'read'\n");
        let runtime = Runtime::open_served(config(), db.clone(), None, adapter).unwrap();
        let root = adapter.name.root("family");
        let inventory = |server| ToolInventory {
            tools: vec![ObservedTool {
                name: "read".into(),
                tool: format!("mcp:{server}/read"),
            }],
            sources: Vec::new(),
        };
        runtime
            .create_session_with_inventory(root.clone(), inventory("parent"))
            .unwrap();
        let child = Actor {
            root: root.clone(),
            child: Some(TrajectoryId(format!("{}:child", root.0))),
        };
        runtime.observe_inventory(&child, adapter, &inventory("child")).unwrap();
        drop(runtime);
        let runtime = Runtime::open_served(config(), db, None, adapter).unwrap();
        let before = runtime.inner.log(&root).unwrap().basis();
        for (child_id, expected) in [(None, "parent"), (Some("child"), "child")] {
            for source in [expected, "replacement"] {
                let request = serde_json::to_vec(&serde_json::json!({
                    "protocol":1, "adapter":"kagent", "root_id":"family", "child_id":child_id,
                    "inventory": inventory(source)
                }))
                .unwrap();
                let (status, report) = crate::tool_validation::answer(&runtime, adapter, &request);
                assert_eq!(status, 200);
                let report: crate::tool_validation::ValidationReport = serde_json::from_value(report).unwrap();
                assert_eq!(report.accepted_tools, inventory(expected).tools);
                assert_eq!(report.is_valid(), source == expected);
            }
        }
        assert_eq!(runtime.inner.log(&root).unwrap().basis(), before);
    }

    #[test]
    fn remote_preflight_rejects_bad_envelopes_and_host_name_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = appa_adapter_kagent::adapter();
        let runtime = Runtime::open_served(
            claude_config("version = 2\n[[tool]]\nname = 'read'\n"),
            dir.path().join("appa.db"),
            None,
            adapter,
        )
        .unwrap();
        for body in [
            serde_json::json!({"protocol": 99, "adapter": "kagent", "inventory": {}}),
            serde_json::json!({"protocol": 1, "adapter": "claude-code", "inventory": {}}),
            serde_json::json!({"protocol": 1, "adapter": "kagent", "inventory": {}, "root_id": ""}),
            serde_json::json!({"protocol": 1, "adapter": "kagent", "inventory": {}, "extra": true}),
            serde_json::json!({"protocol": 1, "adapter": "kagent", "inventory": {}, "child_id": "child"}),
        ] {
            assert!(crate::tool_validation::answer(&runtime, adapter, &serde_json::to_vec(&body).unwrap()).0 >= 400);
        }
        let (status, report) = crate::tool_validation::answer(&runtime, adapter, br#"{"protocol":1,"adapter":"kagent","inventory":{"tools":[{"name":"first_read","tool":"mcp:demo/read"}]}}"#);
        assert_eq!(status, 200);
        let report: crate::tool_validation::ValidationReport = serde_json::from_value(report).unwrap();
        assert!(!report.is_valid());
        assert!(!report.errors.is_empty());
    }

    #[test]
    fn known_uncovered_inventory_does_not_open_a_trajectory() {
        use appa_runtime_api::inventory::{ObservedTool, ToolInventory};
        let dir = tempfile::tempdir().unwrap();
        let runtime = Runtime::open_served(
            claude_config("version = 2\n[[tool]]\nname = \"read\"\n"),
            dir.path().join("appa.db"),
            None,
            appa_adapter_kagent::adapter(),
        )
        .unwrap();
        let id = TrajectoryId("invalid-inventory".into());
        let inventory = ToolInventory {
            tools: vec![ObservedTool {
                name: "write".into(),
                tool: "mcp:demo/write".into(),
            }],
            ..ToolInventory::default()
        };
        assert!(runtime.create_session_with_inventory(id.clone(), inventory).is_err());
        assert!(matches!(runtime.session(&id, &id), Err(EventError::UnknownTrajectory)));
    }

    #[tokio::test]
    async fn a_late_discovered_tool_uses_its_existing_native_rule_without_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = appa_adapter_kagent::adapter();
        let db = dir.path().join("appa.db");
        let runtime = Runtime::open_served(
            claude_config(
                "version = 2\n[[tool]]\nname = \"read_secret\"\n[deployment]\nconfined_results = ['read_secret']\n",
            ),
            db.clone(),
            None,
            adapter,
        )
        .unwrap();
        let opening = serde_json::json!({
            "protocol": 1, "adapter": "kagent", "event": "session_start", "root_id": "late",
            "inventory": { "tools": [], "sources": [{"server": "demo", "status": "unavailable", "dynamic": true}] }
        });
        let (status, response) = crate::hooks::answer(&runtime, &adapter, &serde_json::to_vec(&opening).unwrap()).await;
        assert_eq!(status, 200, "{response}");
        let call = serde_json::json!({
            "protocol": 1, "adapter": "kagent", "event": "tool_call", "root_id": "late",
            "tool": "mcp:demo/read_secret", "arguments": {},
            "inventory": {
                "tools": [{"name": "read_secret", "tool": "mcp:demo/read_secret"}],
                "sources": [{"server": "demo", "status": "complete", "dynamic": true}]
            }
        });
        let (status, response) = crate::hooks::answer(&runtime, &adapter, &serde_json::to_vec(&call).unwrap()).await;
        assert_eq!(status, 200, "{response}");
        let decision = serde_json::from_value::<appa_runtime_api::WireDecision>(response)
            .unwrap()
            .into_decision()
            .unwrap();
        assert!(matches!(decision, appa_runtime_api::HookDecision::AllowCall { .. }));
        let result = br#"{"protocol":1,"adapter":"kagent","event":"tool_result","root_id":"late","tool":"mcp:demo/read_secret","arguments":{},"outcome":{"status":"success","body":{ "literal" : "mcp:demo/read_secret" }}}"#;
        let (status, response) = crate::hooks::answer(&runtime, &adapter, result).await;
        assert_eq!(status, 200, "{response}");
        assert_eq!(response["decision"], "ack", "unchanged output stays with the host");
        let root = TrajectoryId("kagent:late".into());
        let before = runtime.inner.log(&root).unwrap();
        assert!(before.facts().iter().any(|fact| matches!(fact,
            appa_engine::fact::Fact::ValueAdmitted { value, .. }
                if value.body.as_str() == r#"{ "literal" : "mcp:demo/read_secret" }"#
        )));
        runtime
            .live(&TrajectoryId("kagent:late".into()), &TrajectoryId("kagent:late".into()))
            .unwrap();
        drop(runtime);
        let reopened = Runtime::open_served(
            claude_config("version = 2\n[[tool]]\nname = 'different'\n"),
            db,
            None,
            adapter,
        )
        .unwrap();
        reopened.live(&root, &root).unwrap();
        let after = reopened.inner.log(&root).unwrap();
        assert_eq!(after.facts(), before.facts());
        assert_eq!(after.policy_file(), before.policy_file());
        assert_eq!(after.inventories(), before.inventories());
    }

    #[tokio::test]
    async fn a_remote_child_discovers_its_own_server_under_the_same_opening_registry() {
        async fn send(runtime: &Runtime, mut event: serde_json::Value) -> serde_json::Value {
            event["protocol"] = 1.into();
            event["adapter"] = "kagent".into();
            event["root_id"] = "family".into();
            let (status, response) = crate::hooks::answer(
                runtime,
                &appa_adapter_kagent::adapter(),
                &serde_json::to_vec(&event).unwrap(),
            )
            .await;
            assert_eq!(status, 200, "{response}");
            response
        }
        let dir = tempfile::tempdir().unwrap();
        let config = || {
            claude_config(
                "version = 2\n[[tool]]\nname = 'read'\n[[tool]]\nname = 'team__NS__child'\n[deployment]\ncontext_control = true\n",
            )
        };
        let db = dir.path().join("appa.db");
        let runtime = Runtime::open_served(config(), db.clone(), None, appa_adapter_kagent::adapter()).unwrap();
        let inventory = |server: &str| serde_json::json!({"tools":[{"name":"read", "tool":format!("mcp:{server}/read")}],"sources":[{"server":server,"status":"complete","dynamic":true}]});
        send(
            &runtime,
            serde_json::json!({"event":"session_start","inventory":inventory("parent-server")}),
        )
        .await;
        let root = TrajectoryId("kagent:family".into());
        let before = runtime.inner.log(&root).unwrap();
        let spawn = serde_json::json!({"event":"tool_call","tool":"agent:team/child","arguments":{}});
        let held = send(&runtime, spawn.clone()).await;
        let offer = held["offers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|offer| offer["returns"] == "as_spoken")
            .unwrap()["offer_id"]
            .as_str()
            .unwrap();
        let args = serde_json::json!({"offer_id":offer,"label":{}});
        let passed = send(
            &runtime,
            serde_json::json!({"event":"tool_call","tool":"appa:execute_remedy_plan","arguments":args}),
        )
        .await;
        assert_eq!(passed["decision"], "pass_control");
        let (offer, args) = parse_control_arguments(&args.to_string()).unwrap();
        let actor = Actor {
            root: root.clone(),
            child: None,
        };
        assert!(matches!(
            runtime.execute_remedy_with(&actor, offer, args).await,
            RemedyOutcome::Authorized { .. }
        ));
        let released = send(&runtime, spawn).await;
        assert_eq!(released["decision"], "allow_call");
        send(&runtime, serde_json::json!({"event":"child_start","child_id":"child","spawn_binding":released["spawn_binding"],"inventory":inventory("child-server")})).await;
        let allowed = send(&runtime, serde_json::json!({"event":"tool_call","child_id":"child","tool":"mcp:child-server/read","arguments":{},"inventory":inventory("child-server")})).await;
        assert_eq!(allowed["decision"], "allow_call");
        let after = runtime.inner.log(&root).unwrap();
        assert_eq!(before.policy_file(), after.policy_file());
        assert_eq!(
            after.inventories().len(),
            1,
            "only the child adds observations beyond the root snapshot"
        );
        drop(runtime);
        let reopened = Runtime::open_served(config(), db, None, appa_adapter_kagent::adapter()).unwrap();
        reopened
            .live(&root, &TrajectoryId("kagent:family:child".into()))
            .unwrap();
        assert_eq!(reopened.inner.log(&root).unwrap().facts(), after.facts());
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

    /// The offer a subagent pursued falls to its parent when the subagent
    /// ends, and the parent quotes the same id. Nothing releases what the
    /// subagent left standing — a turn end releases the actor's own vouches,
    /// and a subagent's end is not the parent's turn end — so the two quotes
    /// meet on one id. The parent is the offer's pursuer now, so its quote
    /// supersedes the stale one: the take is the parent's, carrying the
    /// ruling the parent's harness attached and not the child's.
    #[tokio::test]
    async fn a_parents_quote_supersedes_the_vouch_its_ended_child_left_standing() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = std::sync::Arc::new(
            Runtime::open(versioned_policy("first"), dir.path().join("appa.db"), None).expect("the deployment opens"),
        );
        let root = TrajectoryId("vouch-supersede".to_string());
        assert_eq!(
            crate::hooks::handle(
                &runtime,
                appa_runtime_api::HookEvent::SessionStart { root: root.clone() }
            )
            .await,
            appa_runtime_api::HookDecision::Ack
        );
        let parent = Actor {
            root: root.clone(),
            child: None,
        };
        let child = Actor {
            root: root.clone(),
            child: Some(TrajectoryId(format!("{}:c1", root.0))),
        };
        let quoted = PermitKey::offer(&OfferId("offer-1".to_string()));

        runtime.vouch(&quoted, &child, Some(appa_runtime_api::Ruling::Approve));
        crate::hooks::handle(&runtime, appa_runtime_api::HookEvent::TurnEnd { actor: parent.clone() }).await;
        assert_eq!(
            runtime.take_vouched(&quoted),
            Ok((child.clone(), Some(appa_runtime_api::Ruling::Approve))),
            "a turn end releases the actor's own vouches, and a child's is not one of them"
        );

        runtime.vouch(&quoted, &child, Some(appa_runtime_api::Ruling::Approve));
        runtime.vouch(&quoted, &parent, None);
        assert_eq!(
            runtime.take_vouched(&quoted),
            Ok((parent.clone(), None)),
            "the offer's pursuer takes its own vouch, and no one else's ruling rides it"
        );

        runtime.vouch(&quoted, &parent, None);
        runtime.release_vouches(&child);
        assert_eq!(
            runtime.take_vouched(&quoted),
            Ok((parent, None)),
            "releasing the child's vouches takes away no vouch of the parent's"
        );
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
        let quoted = PermitKey::offer(&OfferId("offer-1".to_string()));

        runtime.vouch(&quoted, &actor, None);
        assert_eq!(
            runtime.take_vouched(&quoted),
            Ok((actor.clone(), None)),
            "a standing vouch is what the tool takes"
        );

        runtime.vouch(&quoted, &actor, None);
        crate::hooks::handle(&runtime, appa_runtime_api::HookEvent::TurnEnd { actor: actor.clone() }).await;
        assert_eq!(
            runtime.take_vouched(&quoted),
            Err(Unvouched::Nobody),
            "the turn ended without spending it, so nothing later can"
        );
    }

    /// Two trajectories behind one key is a key that does not identify a caller. Both are
    /// refused, and both are told *why* — a caller told "nothing vouched for this" would
    /// make the identical call again and be told the same thing forever.
    #[tokio::test]
    async fn an_ambiguous_vouch_refuses_every_holder_and_says_so() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime =
            Runtime::open(versioned_policy("first"), dir.path().join("appa.db"), None).expect("the deployment opens");
        let key = PermitKey::Call("same-call".to_string());
        let one = Actor {
            root: TrajectoryId("cc:one".to_string()),
            child: None,
        };
        let other = Actor {
            root: TrajectoryId("cc:other".to_string()),
            child: None,
        };
        runtime.vouch(&key, &one, None);
        runtime.vouch(&key, &other, None);

        assert_eq!(runtime.take_vouched(&key), Err(Unvouched::Ambiguous));
        assert_eq!(
            runtime.take_vouched(&key),
            Err(Unvouched::Ambiguous),
            "the second caller is told the same thing, not that nobody vouched"
        );

        // Once one of them is gone the key identifies a caller again.
        runtime.release_vouches(&other);
        assert_eq!(runtime.take_vouched(&key), Ok((one, None)));
    }

    #[tokio::test]
    async fn a_management_vouch_is_exact_one_shot_and_turn_bounded() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime =
            Runtime::open(versioned_policy("first"), dir.path().join("appa.db"), None).expect("the deployment opens");
        let actor = Actor {
            root: TrajectoryId("management-vouch".to_string()),
            child: None,
        };
        let args = crate::mcp::IncludeBatteryArgs {
            actor: "management-vouch".to_string(),
            battery: "github".to_string(),
            expected_policy_key: "policy-1".to_string(),
        };
        let call = ProposedCall {
            tool: "mcp__appa__appa_include_battery".to_string(),
            arguments: serde_json::value::to_raw_value(&args).expect("arguments serialize"),
        };
        let key = call_key(&call).expect("a management call under the MCP prefix");

        runtime.vouch(&key, &actor, None);
        let other_actor = crate::mcp::IncludeBatteryArgs {
            actor: "other-trajectory".to_string(),
            battery: "github".to_string(),
            expected_policy_key: "policy-1".to_string(),
        };
        let other_key = PermitKey::call(
            "appa_include_battery",
            &serde_json::to_value(&other_actor).expect("arguments serialize"),
        );
        assert_eq!(
            runtime.take_vouched(&other_key),
            Err(Unvouched::Nobody),
            "another trajectory cannot consume the permit"
        );
        assert_eq!(runtime.take_vouched(&key), Ok((actor.clone(), None)));
        assert_eq!(runtime.take_vouched(&key), Err(Unvouched::Nobody));

        runtime.vouch(&key, &actor, None);
        runtime.release_vouches(&actor);
        assert_eq!(runtime.take_vouched(&key), Err(Unvouched::Nobody));
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

/// Spawn coverage is the deployment's, not a caller's: a served runtime reads it off the
/// adapter it answers, so it cannot serve one host under another host's rule.
#[cfg(test)]
mod spawn_coverage_tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::config::Config;
    use appa_runtime_api::{HookDecision, HookEvent};
    use axum::Router;
    use axum::extract::State;
    use axum::routing::post;

    /// The agent no contract in the fixture policy names.
    const UNNAMED: &str = "agent/NS/release_manager";

    fn root() -> TrajectoryId {
        TrajectoryId("coverage-root".to_string())
    }

    fn acting() -> Actor {
        Actor {
            root: root(),
            child: None,
        }
    }

    /// An annotator that lets everything through unchanged, counting its consults.
    async fn permissive_annotator() -> (String, Arc<Mutex<usize>>) {
        let consults = Arc::new(Mutex::new(0usize));
        let router = Router::new()
            .route(
                "/annotate",
                post(|State(consults): State<Arc<Mutex<usize>>>, _body: String| async move {
                    *consults.lock().expect("the consult counter is never poisoned") += 1;
                    axum::Json(serde_json::json!({
                        "version": 1,
                        "answer": {
                            "delta": {},
                            "requires": { "history": [], "attention": [] },
                            "emits": [],
                        }
                    }))
                }),
            )
            .with_state(Arc::clone(&consults));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("an ephemeral loopback port binds");
        let addr = listener.local_addr().expect("the bound address is readable");
        tokio::spawn(async move {
            axum::serve(listener, router).await.expect("the stub serves");
        });
        (format!("http://{addr}/annotate"), consults)
    }

    /// One agent this deployment delegates to, by the canonical name both adapters derive,
    /// and a wildcard over everything else. Every name is canonical, so a served deployment
    /// of either host loads it.
    fn config(dir: &tempfile::TempDir, url: &str) -> Config {
        let text = format!(
            r#"
[policy]
version = 2

[[policy.annotator]]
name = "gatekeeper"

[[policy.tool]]
name = "agent/NS/log_analyst"
delta = {{}}

[[policy.tool]]
name = "*"
annotator = "gatekeeper"

[policy.deployment]
context_control = true

[externals]
timeout_ms = 2000
max_body_bytes = 65536

[externals.annotators.gatekeeper]
url = "{url}"
"#
        );
        let path = dir.path().join("appa.toml");
        std::fs::write(&path, text).expect("the fixture writes");
        Config::load(&path).expect("the fixture validates")
    }

    /// The deployment under `adapter`, or — `None` — the one an embedding host opens.
    async fn opened(dir: &tempfile::TempDir, url: &str, adapter: Option<Adapter>) -> Runtime {
        let config = config(dir, url);
        let db = dir.path().join("appa.db");
        let runtime = match adapter {
            Some(adapter) => Runtime::open_served(config, db, None, adapter).expect("the served deployment opens"),
            None => Runtime::open(config, db, None).expect("the embedded deployment opens"),
        };
        assert_eq!(
            crate::hooks::handle(&runtime, HookEvent::SessionStart { root: root() }).await,
            HookDecision::Ack
        );
        runtime
    }

    async fn call(runtime: &Runtime, tool: &str, spawn: bool) -> HookDecision {
        crate::hooks::handle(
            runtime,
            HookEvent::ToolCall {
                actor: acting(),
                call: ProposedCall {
                    tool: tool.to_string(),
                    arguments: raw(serde_json::json!({ "request": "summarize the crash logs" })),
                },
                spawn,
                ruling: None,
            },
        )
        .await
    }

    /// Served under kagent: an agent no contract names cannot run as a child, and the
    /// refusal stands before any evidence is gathered. The same name proposed as an
    /// ordinary call is the wildcard's to cover, as any other call is.
    #[tokio::test]
    async fn a_kagent_deployment_covers_no_spawn_the_policy_does_not_name() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let (url, consults) = permissive_annotator().await;
        let runtime = opened(&dir, &url, Some(appa_adapter_kagent::adapter())).await;

        let denied = call(&runtime, UNNAMED, true).await;
        let HookDecision::DenyCall { offers, review, .. } = &denied else {
            panic!("an unnamed agent spawns nothing, got {denied:?}");
        };
        assert!(offers.is_empty(), "no contract, no offer: {denied:?}");
        assert!(review.is_empty(), "nothing to review: {denied:?}");
        assert_eq!(
            *consults.lock().expect("the counter reads"),
            0,
            "no consult stands in for a missing declaration"
        );

        assert_eq!(
            call(&runtime, UNNAMED, false).await,
            HookDecision::AllowCall { spawn: None },
            "the wildcard covers the ordinary call under the same name"
        );
        assert_eq!(
            *consults.lock().expect("the counter reads"),
            1,
            "the wildcard annotated the ordinary call"
        );
    }

    /// Every other deployment keeps the wildcard's cover over a spawn: the served Claude
    /// Code runtime, whose `Task` is an ordinary call the policy need not name, and a host
    /// that embeds the runtime. The spawn is judged like any call and held on the return
    /// menu, which is a decision no unnamed agent reaches under kagent.
    #[tokio::test]
    async fn every_other_deployment_lets_the_wildcard_cover_a_spawn() {
        for adapter in [Some(appa_adapter_claude_code::adapter()), None] {
            let dir = tempfile::tempdir().expect("a temp dir is creatable");
            let (url, consults) = permissive_annotator().await;
            let runtime = opened(&dir, &url, adapter).await;

            let held = call(&runtime, UNNAMED, true).await;
            let HookDecision::DenyCall { offers, .. } = &held else {
                panic!("a covered spawn is held on the return menu, got {held:?}");
            };
            assert!(!offers.is_empty(), "the wildcard covered the spawn: {held:?}");
            assert_eq!(
                *consults.lock().expect("the counter reads"),
                1,
                "the wildcard annotated the spawn as it annotates any call"
            );
        }
    }
}
