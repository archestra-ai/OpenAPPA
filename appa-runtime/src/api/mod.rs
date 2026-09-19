//! The runtime API: `Runtime` and `Session` — the harness-agnostic
//! event model this crate declares.

pub(crate) mod files;
mod host;
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
    Actor, OfferedRemedy, OutcomeBody, ProposedCall, Review, SpawnBinding, SpawnRef, ToolOutcome, TrajectoryId,
};
pub(crate) use session::{LateOpen, Session, is_control_tool};

use crate::config::Config;
use crate::elicit::Elicitation;
use crate::engine::{EngineRefusal, Liveness, PolicyEngine, RuntimeEngine};
use crate::external::{ConsultGates, ExternalServices};
#[cfg(feature = "daemon")]
use crate::yell;
use appa_eventlog::{Backend, HostObservation, Log, LogStore};
use appa_runtime_api::{Adapter, AdapterName};
use host::{HostState, host_actor, inventory_at};

/// One remedy offer as it is quoted and carried.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OfferId(pub String);

impl OfferId {
    /// Parses the fixed-width lowercase hexadecimal spelling the runtime renders in feedback.
    pub fn parse(value: &str) -> Result<Self, OfferIdRefusal> {
        if value.len() != 16 {
            return Err(OfferIdRefusal::WrongLength);
        }
        if !value.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(OfferIdRefusal::NotLowercaseHex);
        }
        Ok(Self(value.to_owned()))
    }
}

/// Why an externally supplied rendered offer id cannot identify an offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OfferIdRefusal {
    #[error("an offer id must contain exactly 16 characters")]
    WrongLength,
    #[error("an offer id must use lowercase hexadecimal characters")]
    NotLowercaseHex,
}

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
        Self::Call(hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect())
    }

    /// How a record spells this key. The two variants can never mean each other, so the
    /// spelling carries which one it is.
    pub(crate) fn wire(&self) -> String {
        match self {
            PermitKey::Offer(id) => format!("{OFFER_KEY}{id}"),
            PermitKey::Call(digest) => format!("{CALL_KEY}{digest}"),
        }
    }

    /// The key a recorded spelling names, or `None` for a spelling this build cannot read.
    ///
    /// An offer id is the one part of a key that reaches this runtime as a caller wrote it,
    /// so the shape the runtime itself renders is the whole of what it will read: this is
    /// the single place that rule is spelled, and a spelling that fails it names no offer
    /// wherever it came from.
    pub(crate) fn parse(recorded: &str) -> Option<PermitKey> {
        if let Some(id) = recorded.strip_prefix(OFFER_KEY) {
            let quoted = OfferId(id.to_string());
            return crate::engine::renders_offer(&quoted).then_some(PermitKey::Offer(quoted.0));
        }
        recorded
            .strip_prefix(CALL_KEY)
            .map(|digest| PermitKey::Call(digest.to_string()))
    }
}

const OFFER_KEY: &str = "offer:";
const CALL_KEY: &str = "call:";

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
    if !MANAGEMENT_TOOLS.contains(&bare) && !files::owns(call) {
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
        .or_else(|| tool.strip_prefix("mcp/appa/"))
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

/// A standing the store could not read or record is a standing this call does not have. The
/// tool then answers as it does when no hook ran, which is the closed end.
impl From<EventError> for Unvouched {
    fn from(_: EventError) -> Unvouched {
        Unvouched::Nobody
    }
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
        display: Vec<RemedyDisplay>,
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
    Deliver {
        value: String,
    },
    Replace {
        placeholder: String,
        presentation: Option<RemedyPresentation>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RemedyDecision {
    Authorized { call: ExactCall },
    Substituted { call: ExactCall },
    Returned { value: String },
    Declined { presentation: RemedyPresentation },
    NoAnswer { feedback: String },
}

/// Structured presentation for policy decisions offering remedies.
#[derive(Debug, Clone, PartialEq)]
pub struct RemedyPresentation {
    /// Model-facing explanation in plain text.
    pub feedback: String,
    /// Machine-readable remedy offers available for this block.
    pub offers: Vec<OfferedRemedy>,
    /// Active human-in-the-loop reviews associated with this block.
    pub review: Vec<Review>,
    /// Optional display plans formatted for embedded client presentation.
    pub display: Vec<RemedyDisplay>,
}

/// Formatted policy reasons and display plans for a blocked call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemedyDisplay {
    pub reasons: Vec<String>,
    pub plans: Vec<RemedyDisplayPlan>,
}

/// Human-readable description paired with its corresponding offer ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemedyDisplayPlan {
    pub offer_id: String,
    pub text: String,
}

/// Display configuration for an embedded host.
/// Configures control tool names and formats without altering policy engine state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedPresentationOptions {
    pub control_tool: String,
    pub supports_delegation: bool,
    /// Formats remedy call lines with an explicit plan parameter.
    pub include_display_plan: bool,
}

impl Default for EmbeddedPresentationOptions {
    fn default() -> Self {
        Self {
            control_tool: appa_runtime_api::ADVERTISED_CONTROL_TOOL.to_owned(),
            supports_delegation: true,
            include_display_plan: false,
        }
    }
}

/// A hook outcome for an embedded host.
/// Returns the standard wire decision alongside optional structured remedy presentations.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddedHookOutcome {
    pub decision: appa_runtime_api::HookDecision,
    pub presentation: Option<RemedyPresentation>,
}

/// Explains why a remedy call was refused execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemedyRefusal {
    /// The offer ID does not match the 16-character lowercase hex format.
    InvalidOfferId(OfferIdRefusal),
    /// No preceding control hook vouched for this offer.
    Unvouched,
    /// The offer belongs to a different session or actor.
    ActorMismatch,
    /// The offer is expired, already terminal, or from another trajectory.
    UnknownOffer,
    /// The offer is currently being executed by another caller.
    AlreadyExecuting,
    /// Runtime storage or execution failure.
    Runtime { detail: String },
}

impl RemedyRefusal {
    pub fn detail(&self) -> &str {
        match self {
            Self::InvalidOfferId(_) => "the offer id must be exactly 16 lowercase hexadecimal characters",
            Self::Unvouched | Self::UnknownOffer => "no live offer with this id exists",
            Self::ActorMismatch => "offer belongs to a different session",
            Self::AlreadyExecuting => "this offer is already being executed",
            Self::Runtime { detail } => detail,
        }
    }
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
    Declined { presentation: RemedyPresentation },
    NoAnswer { feedback: String },
    Refused { reason: RemedyRefusal },
}

impl From<EventError> for RemedyOutcome {
    fn from(error: EventError) -> RemedyOutcome {
        RemedyOutcome::Refused {
            reason: RemedyRefusal::Runtime {
                detail: error.to_string(),
            },
        }
    }
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

/// Why a deployment refused to serve after loading: a source the policy references gave no
/// answer or an answer outside the reader shape rule, for a selector or for a member lookup
/// its selectors owe.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProbeError {
    #[error("audience source {provider} failed the probe of selector {selector}: {reason}")]
    Selector {
        provider: String,
        selector: String,
        reason: String,
    },
    #[error("audience source {provider} failed the probe of member lookup {member}: {reason}")]
    Lookup {
        provider: String,
        member: String,
        reason: String,
    },
}

/// Every lifecycle misuse is one typed error; the adapter renders it
/// as a deny.
#[derive(Debug, thiserror::Error)]
pub(crate) enum EventError {
    #[error("a call without a host id is already outstanding; this host must propose one unidentified call at a time")]
    CallOutstanding,
    #[error("a subagent spawn is already waiting to be bound; start one subagent at a time")]
    SpawnOutstanding,
    #[error("the host reused a call id")]
    CallIdReused,
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
            | EventError::SpawnOutstanding
            | EventError::CallIdReused
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

    /// Read every selector the policy references once, through the bound sources, and ask
    /// each lookup entry for one member those answers owe; hold each answer to the reader
    /// shape rule. A source that fails or reports a malformed reader is a misconfiguration to
    /// refuse before this deployment serves, not a no-answer to discover under an agent. One
    /// lookup per provider proves the entry answers in shape at a cost that does not grow
    /// with the directory; a member it later answers badly refuses that act. Replay never
    /// runs this: it reads pins.
    async fn probe_sources(&self) -> Result<(), ProbeError> {
        use crate::external::settle_batch;
        use appa_engine::audience::AudienceEvidence;

        let audience = self.resident.registry().audience();
        // Both batches are built before they settle: a future borrowing its spec does not
        // type-check through the stream adapter as a lazy map.
        let selectors: Vec<_> = audience
            .referenced_selectors()
            .into_iter()
            .map(|spec| self.probe_selector(audience, spec))
            .collect();
        let evidence = AudienceEvidence {
            sources: settle_batch(selectors).await.into_iter().collect::<Result<_, _>>()?,
            lookups: Vec::new(),
        };
        let owed = audience.member_lookups_owed(&evidence);
        let mut probed: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        let lookups: Vec<_> = owed
            .iter()
            .filter(|spec| probed.insert(spec.provider.as_str()))
            .map(|spec| self.probe_lookup(audience, spec))
            .collect();
        for lookup in settle_batch(lookups).await {
            lookup?;
        }
        Ok(())
    }

    async fn probe_selector(
        &self,
        audience: &appa_engine::audience::AudienceRegistry,
        spec: &appa_engine::audience::SelectorSpec,
    ) -> Result<appa_engine::audience::SourceClaims, ProbeError> {
        use crate::consult::{Consult, MembersAnswer};
        use crate::external::ConsultOutcome;
        use appa_engine::audience::{AudienceEvidence, SourceClaims};
        use appa_engine::label::ReaderId;

        let refused = |reason: String| ProbeError::Selector {
            provider: spec.provider.clone(),
            selector: spec.selector.clone(),
            reason,
        };
        let consult = Consult::audience_selector(
            &spec.provider,
            &spec.selector,
            self.templates_of(audience, &spec.provider),
        );
        let answer = match self.externals.consult(&consult, None, None).await {
            ConsultOutcome::Answer(answer) => MembersAnswer::from_wire(&answer)
                .ok_or_else(|| refused("the answer is not {\"members\": [\"<reader>\", ...]}".to_string()))?,
            ConsultOutcome::NoAnswer(reason) => return Err(refused(reason.diagnostic())),
        };
        let claims = SourceClaims {
            provider: spec.provider.clone(),
            selector: spec.selector.clone(),
            members: answer.members.into_iter().map(ReaderId::new).collect(),
        };
        let evidence = AudienceEvidence {
            sources: vec![claims.clone()],
            lookups: Vec::new(),
        };
        audience
            .expansions(&evidence)
            .map_err(|refusal| refused(refusal.to_string()))?;
        Ok(claims)
    }

    /// One owed lookup, asked of the entry the member's provider names; its principal is
    /// held to the same shape rule the live pin applies.
    async fn probe_lookup(
        &self,
        audience: &appa_engine::audience::AudienceRegistry,
        spec: &appa_engine::audience::LookupSpec,
    ) -> Result<(), ProbeError> {
        use crate::consult::{Consult, LookupAnswer};
        use crate::external::ConsultOutcome;
        use appa_engine::audience::well_formed_reader;
        use appa_engine::label::ReaderId;

        let refused = |reason: String| ProbeError::Lookup {
            provider: spec.provider.clone(),
            member: spec.member.clone(),
            reason,
        };
        let answering = audience.lookup_target(&spec.provider).unwrap_or(&spec.provider);
        let consult = Consult::member_lookup(answering, &spec.member, self.templates_of(audience, &spec.provider));
        let principal = match self.externals.consult(&consult, None, None).await {
            ConsultOutcome::Answer(answer) => LookupAnswer::from_wire(&answer)
                .ok_or_else(|| refused("the answer is not {\"principal\": \"<reader>\" | null}".to_string()))?
                .principal
                .map(ReaderId::new),
            ConsultOutcome::NoAnswer(reason) => return Err(refused(reason.diagnostic())),
        };
        match principal {
            Some(principal) if !well_formed_reader(&spec.provider, &principal) => Err(refused(format!(
                "the answer names principal {:?}, which is neither an address nor a {}-qualified id",
                principal.as_str(),
                spec.provider
            ))),
            _ => Ok(()),
        }
    }

    /// A probed selector or owed lookup names a provider the policy registered.
    fn templates_of(&self, audience: &appa_engine::audience::AudienceRegistry, provider: &str) -> Vec<String> {
        crate::engine::selector_templates(audience, provider).expect("the probe reads only registered providers")
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

/// A deployment loaded for a reload and not yet serving: the previous one keeps serving
/// until [`Runtime::install`], and a failed probe here leaves it untouched.
pub struct PreparedReload {
    deployment: Arc<Deployment>,
}

impl PreparedReload {
    /// [`Runtime::probe_sources`] over the prepared deployment.
    pub async fn probe_sources(&self) -> Result<(), ProbeError> {
        self.deployment.probe_sources().await
    }
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
        let state_path = if let Backend::Sqlite { path } = &backend {
            Some(path.clone())
        } else {
            None
        };
        let store = LogStore::open(backend).map_err(|error| match error {
            appa_eventlog::OpenError::Damaged { path, detail } => OpenError::Damaged(format!("{path}: {detail}")),
            error @ appa_eventlog::OpenError::ForeignSchema { .. } => OpenError::Damaged(error.to_string()),
            error => OpenError::Storage(error.to_string()),
        })?;
        Ok(self.with_store(Arc::new(store), state_path))
    }

    fn with_store(self, store: Arc<LogStore>, state_path: Option<PathBuf>) -> Runtime {
        Runtime {
            inner: Arc::new(Inner {
                shared: Arc::new(Shared {
                    deployment: std::sync::RwLock::new(Arc::new(self.deployment)),
                    retired: std::sync::Mutex::new(std::collections::BTreeMap::new()),
                    modules: self.modules,
                    events: std::sync::Mutex::new(crate::events::EventLog::default()),
                    gates: self.gates,
                    naming: self.naming,
                    files: None,
                    state_path,
                }),
                store,
            }),
        }
    }
}

struct Inner {
    shared: Arc<Shared>,
    store: Arc<LogStore>,
}

/// Everything of a runtime but its store: one object, held by the runtime and by every
/// view [`Runtime::on`] makes of it, so a reload through any of them serves them all.
struct Shared {
    files: Option<files::FileTracking>,
    state_path: Option<PathBuf>,
    deployment: std::sync::RwLock<Arc<Deployment>>,
    retired: std::sync::Mutex<std::collections::BTreeMap<String, Arc<RuntimeEngine>>>,
    modules: crate::builtins::ModuleRegistry,
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

impl Runtime {
    /// Embed the existing runtime over host-managed storage without changing
    /// policy naming, hook semantics, or starting an HTTP server.
    pub fn open_with_store(
        config: Config,
        store: Arc<LogStore>,
        modules: Option<PathBuf>,
    ) -> Result<Runtime, OpenError> {
        Ok(Prepared::new(config, modules, ToolNaming::AsAuthored)?.with_store(store, None))
    }

    /// This runtime over another store: the same deployment, consult gates and diagnostics,
    /// with every log read and write going to `store`. A host that leases a connection for
    /// one dispatch ([`LogStore::lease`]) handles that dispatch through such a view, so the
    /// engine's records land in the transaction the lease holds.
    pub fn on(&self, store: Arc<LogStore>) -> Runtime {
        Runtime {
            inner: Arc::new(Inner {
                shared: Arc::clone(&self.inner.shared),
                store,
            }),
        }
    }

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
        self.shared
            .events
            .lock()
            .expect("the event mutex is never poisoned: no panic runs while it is held")
            .record(root, event);
    }

    /// See [`crate::events::EventLog::events`].
    #[cfg(feature = "daemon")]
    pub(crate) fn events(&self, root: &TrajectoryId) -> crate::events::Events {
        self.shared
            .events
            .lock()
            .expect("the event mutex is never poisoned: no panic runs while it is held")
            .events(root)
    }

    /// See [`crate::events::EventLog::recent_root`].
    #[cfg(feature = "daemon")]
    pub(crate) fn recent_root(&self, window: std::time::Duration) -> crate::events::Recent {
        self.shared
            .events
            .lock()
            .expect("the event mutex is never poisoned: no panic runs while it is held")
            .recent_root(window)
    }

    fn deployment(&self) -> Arc<Deployment> {
        Arc::clone(
            &self
                .shared
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
            .shared
            .retired
            .lock()
            .expect("the retired-engine mutex is never poisoned: no panic runs while it is held")
            .get(key)
        {
            return Ok(Arc::clone(engine));
        }
        let naming = self.shared.naming;
        let compiled = compile_stored_for_host(bytes, naming).map_err(EventError::PolicyUnavailable)?;
        let engine = Arc::new(RuntimeEngine::from_policy(&compiled, naming));
        Ok(Arc::clone(
            self.shared
                .retired
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
            .map_err(read_refused)
    }

    /// Record one host observation in this root's log, at the position the append is tried
    /// at, unconditionally. A write that depends on what stands at that position derives it
    /// through [`Inner::append_host_with`] instead.
    fn append_host(&self, root: &TrajectoryId, observation: &HostObservation) -> Result<(), EventError> {
        self.append_host_with(root, |_| Ok((Some(observation.clone()), ())))
    }

    /// Record one host observation derived from the very position it will be written at, so
    /// a writer that loses the compare-and-swap decides again against what won rather than
    /// replaying a decision the log has moved past. `None` means there is nothing left to
    /// record.
    ///
    /// The derivation is the enforcement point for every rule the host records carry: it
    /// sees only what is durable at the position it writes at, so a standing another writer
    /// ended between the read and the write is ended for this writer too.
    ///
    /// The error is the caller's, so a derivation that refuses — an offer already executing,
    /// a standing that is no longer the one being spent — says so in its own vocabulary
    /// instead of through this one.
    ///
    /// Read, derive, append; on a lost compare-and-swap, read again and derive again. The
    /// answer handed back is the one the derivation that landed produced, so a caller learns
    /// what was true where its record went and not what an earlier read showed it.
    fn append_host_with<Refusal: From<EventError>, Answer>(
        &self,
        root: &TrajectoryId,
        derive: impl Fn(&Log) -> Result<(Option<HostObservation>, Answer), Refusal>,
    ) -> Result<Answer, Refusal> {
        for _ in 0..HOST_ATTEMPTS {
            let log = self.log(root)?;
            let (observation, answer) = derive(&log)?;
            let Some(observation) = observation else {
                return Ok(answer);
            };
            match self.store.append_host(&log, &[], &observation) {
                Ok(()) => return Ok(answer),
                Err(appa_eventlog::AppendError::Conflict { .. }) => continue,
                Err(error) => {
                    self.note_store_error(Some(root), crate::events::StoreOperation::Append, &error);
                    return Err(EventError::Storage(error.to_string()).into());
                }
            }
        }
        Err(EventError::Contended {
            attempts: HOST_ATTEMPTS,
        }
        .into())
    }
}

/// How many times a host write re-derives against a position another writer took before it
/// gives up. Every contender records one observation, so a family loses this many races only
/// when it is being written by far more than it has actors.
const HOST_ATTEMPTS: u32 = 8;

/// What this family's records say right now. Read at the position the answer is about: a
/// claim's bound and a turn's end are both about when, so a reduction is only true of the
/// moment it was taken at.
fn reduced(log: &Log) -> HostState {
    HostState::fold(log.host_records(), std::time::SystemTime::now())
}

/// Why [`Runtime::open_root_fork`] opened no independent conversation-root fork.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RootForkRefusal {
    #[error("the parent trajectory is not open in its family")]
    ParentUnavailable,
    #[error("the new root ID names its source family, source trajectory, or an unrelated existing root")]
    RootIdConflict,
    #[error("the root fork could not be opened: {0}")]
    Refused(String),
}

fn read_refused(error: appa_eventlog::ReadError) -> EventError {
    match error {
        appa_eventlog::ReadError::UnknownRoot { .. } => EventError::UnknownTrajectory,
        appa_eventlog::ReadError::Undecodable(detail) => EventError::UntrustedLog(detail),
        error @ appa_eventlog::ReadError::PolicyFileMissing { .. } => EventError::PolicyUnavailable(error.to_string()),
        error => EventError::Storage(error.to_string()),
    }
}

impl Runtime {
    #[cfg(feature = "daemon")]
    pub(crate) fn file_initial_label(
        &self,
        trust: &str,
        audience: appa_engine::label::DeclaredAudience,
    ) -> Result<appa_engine::label::Label, OpenError> {
        let deployment = self.inner.deployment();
        let rank = deployment
            .resident
            .registry()
            .trust_chain()
            .rank_of(trust)
            .ok_or_else(|| OpenError::UnsupportedPolicy(format!("unknown initial file trust rank: {trust}")))?;
        Ok(appa_engine::label::Label::new(
            rank,
            appa_engine::label::Audience::of_declared(&audience),
        ))
    }

    /// Enable experimental Read/Write/Edit tracking, not a supported security boundary.
    /// Runtime-owned tools require native alternatives and implicit reads disabled.
    /// Inference and final responses remain unmediated. Use disposable fixtures only.
    /// Configure this before sharing the runtime. `Some(initial)`
    /// explicitly initializes all existing files with the operator's source Label; `None`
    /// requires an existing ledger. Never initialize again to recover a lost ledger.
    /// Only exclusively owned Unix workspaces are supported. The host must also keep its
    /// configuration, plugins, credentials and other execution-control files outside the root.
    pub fn with_file_tracking(
        mut self,
        workspace: PathBuf,
        ledger: PathBuf,
        initial: Option<appa_engine::label::Label>,
    ) -> Result<Self, OpenError> {
        let inner = Arc::get_mut(&mut self.inner)
            .and_then(|inner| Arc::get_mut(&mut inner.shared))
            .ok_or_else(|| OpenError::Storage("enable file tracking before sharing the runtime".into()))?;
        if !matches!(inner.naming, ToolNaming::Canonical { adapter } if adapter.name == AdapterName::ClaudeCode) {
            return Err(OpenError::Storage(
                "file tracking requires the Claude Code adapter".into(),
            ));
        }
        let workspace = std::fs::canonicalize(workspace).map_err(|error| OpenError::Storage(error.to_string()))?;
        if let Some(path) = &inner.state_path
            && std::fs::canonicalize(path)
                .map_err(|error| OpenError::Storage(error.to_string()))?
                .starts_with(&workspace)
        {
            return Err(OpenError::Storage(
                "the runtime database must be outside the tracked workspace".into(),
            ));
        }
        let deployment = Arc::clone(
            inner
                .deployment
                .get_mut()
                .expect("the deployment lock is never poisoned: no panic runs while it is held"),
        );
        if deployment.resident.registry().sanitizers().next().is_some() {
            return Err(OpenError::Storage(
                "file tracking does not support sanitizer or rewrite routes".into(),
            ));
        }
        let policy_key = crate::engine::policy_file_key(deployment.config.policy_file().bytes());
        if let Some(label) = &initial
            && deployment
                .resident
                .registry()
                .trust_chain()
                .name_of(label.trust)
                .is_none()
        {
            return Err(OpenError::Storage(
                "initial file trust must be a configured policy rank".into(),
            ));
        }
        let store = match initial {
            Some(label) => appa_eventlog::files::FileStore::initialize(&ledger, &workspace, &policy_key, &label),
            None => appa_eventlog::files::FileStore::open(
                &ledger,
                &workspace,
                &policy_key,
                &appa_engine::label::Label::top(),
            ),
        }
        .map_err(|error| OpenError::Storage(error.to_string()))?;
        inner.files = Some(files::FileTracking {
            store,
            policy_key,
            workspace,
            #[cfg(feature = "daemon")]
            ledger,
            process_backend: None,
        });
        tracing::warn!(
            "experimental file tools require Claude Code launched with native tools and implicit filesystem reads disabled"
        );
        Ok(self)
    }

    /// Enable the host-installed, pinned agentsh runner for declared-input processing.
    /// The backend and system toolchain are trusted host code, outside the managed workspace.
    pub fn with_file_process_backend(mut self, backend: PathBuf) -> Result<Self, OpenError> {
        let inner = Arc::get_mut(&mut self.inner)
            .and_then(|inner| Arc::get_mut(&mut inner.shared))
            .ok_or_else(|| OpenError::Storage("enable processing before sharing the runtime".into()))?;
        let files = inner
            .files
            .as_mut()
            .ok_or_else(|| OpenError::Storage("processing requires file tracking".into()))?;
        let backend = std::fs::canonicalize(backend).map_err(|error| OpenError::Storage(error.to_string()))?;
        if backend.starts_with(&files.workspace)
            || ["agentsh", "agentsh-unixwrap", "run.py"].iter().any(|name| {
                std::fs::canonicalize(backend.join(name))
                    .map_or(true, |path| !path.is_file() || path.starts_with(&files.workspace))
            })
        {
            return Err(OpenError::Storage(
                "processing requires a complete backend outside the workspace".into(),
            ));
        }
        files.process_backend = Some(backend);
        Ok(self)
    }

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
    #[cfg(feature = "daemon")]
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
        self.inner.shared.naming.model_spelling(recorded)
    }

    /// The deployment `appa replay` runs: the same session and engine over a log that lives
    /// only as long as this value, with every authority and sanitizer answered in process —
    /// approve, and the body unchanged — as if the bound party had. Annotators, audience
    /// sources, and lookups stay bound as configured. Nothing of the run survives the process.
    pub fn open_in_memory(config: Config, modules: Option<PathBuf>) -> Result<Runtime, OpenError> {
        let mut prepared = Prepared::new(config, modules, ToolNaming::AsAuthored)?;
        prepared.deployment.stand_in_for_remedies();
        prepared.assemble(Backend::Memory)
    }

    /// The policy file key the serving deployment answers under. An install compares it
    /// against the key of the configuration it just validated: a process that kept
    /// running across the install serves the policy it loaded at startup, and only a
    /// difference here is worth reloading.
    #[cfg(feature = "daemon")]
    pub(crate) fn serving_policy_key(&self) -> String {
        let serving = self
            .inner
            .shared
            .deployment
            .read()
            .expect("the deployment lock is never poisoned: no panic runs while it is held");
        crate::engine::policy_file_key(serving.config.policy_file().bytes())
    }

    /// Consult every selector the serving policy references once, and refuse on the first
    /// source that fails or answers outside the reader shape rule. `serve` runs this after
    /// [`Runtime::open`] and before it binds a listener, so a broken source stops the start.
    pub async fn probe_sources(&self) -> Result<(), ProbeError> {
        self.inner.deployment().probe_sources().await
    }

    /// Replace the serving deployment with the one this configuration
    /// declares, without stopping the process (
    /// reloading a policy). The caller reads the file; the runtime never
    /// learns where a configuration came from, so an embedding host
    /// reloads a composed policy the same way. A host that probes the new
    /// sources first prepares, probes, then installs.
    ///
    /// How this deployment names tools is its own state and no caller's choice, so the
    /// reload holds the naming it opened with: a served deployment's rule that the policy
    /// names every tool canonically survives the reload, and a refused candidate changes
    /// nothing — the deployment that was serving keeps serving.
    pub fn reload(&self, config: Config) -> Result<Reloaded, OpenError> {
        Ok(self.install(self.prepare_reload(config)?))
    }

    /// Load the deployment a configuration declares without installing it: every open-time
    /// gate runs, and the result is held for a probe before [`Runtime::install`] swaps it in.
    pub fn prepare_reload(&self, config: Config) -> Result<PreparedReload, OpenError> {
        let deployment = Deployment::load(
            config,
            &self.inner.shared.modules,
            self.inner.shared.gates.clone(),
            self.inner.shared.naming,
        )?;
        Ok(PreparedReload {
            deployment: Arc::new(deployment),
        })
    }

    /// Swap a prepared deployment in as the serving one.
    pub fn install(&self, prepared: PreparedReload) -> Reloaded {
        let deployment = prepared.deployment;
        let identity = deployment.resident().identity_hex();
        // The gate's bound and the serving snapshot change as one transition under the
        // deployment lock, so two reloads racing cannot leave the gate bound by the
        // deployment that lost.
        let previous = {
            let mut serving = self
                .inner
                .shared
                .deployment
                .write()
                .expect("the deployment lock is never poisoned: no panic runs while it is held");
            self.inner
                .shared
                .gates
                .serve_llm(deployment.config.externals.llm_bound());
            std::mem::replace(&mut *serving, Arc::clone(&deployment))
        };
        // Every reload retires at most one more policy, so clearing here bounds the
        // cache by the reloads since the last one instead of by the life of the
        // process. A trajectory still replaying under a dropped entry recompiles it.
        self.inner
            .shared
            .retired
            .lock()
            .expect("the retired-engine mutex is never poisoned: no panic runs while it is held")
            .clear();

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
        Reloaded {
            policy_key: key,
            policy_identity: identity,
            changed,
        }
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
        let deployment = Deployment::load(
            config,
            &self.inner.shared.modules,
            self.inner.shared.gates.clone(),
            self.inner.shared.naming,
        )
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
        self.inner.append_host_with(&actor.root, |log| {
            let previous = inventory_at(log, actor, adapter)?;
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
                return Ok((None, ()));
            }
            Ok((
                Some(HostObservation::Inventory {
                    actor: scope.clone(),
                    adapter: adapter.name,
                    // Persist only the new evidence, not another full copy of history.
                    inventory: delta,
                }),
                (),
            ))
        })
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
                let log = self.inner.log(&actor.root)?;
                let mut report = self.check_inventory_at(&log, adapter, inventory)?;
                let scope = crate::engine::engine_id(acting_trajectory(actor));
                report.actor_opened = log.facts().iter().any(|fact| {
                    matches!(
                        fact,
                        appa_engine::fact::Fact::TrajectoryOpened(appa_engine::fact::TrajectoryOpening { trajectory, .. })
                        | appa_engine::fact::Fact::ForkOpened { trajectory, .. } if trajectory == &scope
                    )
                });
                let previous = inventory_at(&log, actor, adapter)?;
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
        self.check_inventory_at(&log, adapter, inventory)
    }

    fn check_inventory_at(
        &self,
        log: &Log,
        adapter: Adapter,
        inventory: &appa_runtime_api::inventory::ToolInventory,
    ) -> Result<crate::tool_validation::ValidationReport, EventError> {
        self.inner.resolve_policy(&self.inner.deployment(), log)?;
        #[derive(serde::Deserialize)]
        struct Rules {
            policy: toml::Value,
            #[serde(default)]
            server_aliases: crate::config::ServerBindings,
        }
        let source = std::str::from_utf8(log.policy_file())
            .map_err(|_| EventError::PolicyUnavailable("stored policy is not UTF-8".into()))?;
        let rules: Rules = toml::from_str(source)
            .map_err(|_| EventError::PolicyUnavailable("stored validation rules do not decode".into()))?;
        Ok(crate::tool_validation::resolve(&rules.policy, adapter, inventory, &rules.server_aliases).report)
    }

    /// Open `new_root` as an independent conversation-root fork of `parent`, a trajectory in the
    /// family rooted at `parent_root`. The new root starts from the parent's current label, its
    /// family's committed effects and unsettled effect reservations, and the parent's denials,
    /// under the policy the parent's family opened with. After that the two families share
    /// nothing: what either admits, emits, settles or is denied never reaches the other, and each
    /// keeps its own dispatches, offers and turns. This creates no same-family spawned child: it
    /// has no prepared spawn, `ChildStart`/`ChildEnd`, or return contract. The embedding integration
    /// identifies the source trajectory; an ordinary `SessionStart` does not infer that
    /// relationship. Opening the same root fork again is not an error, even once the parent has
    /// ended: a root already open is recognized before the parent is read.
    pub fn open_root_fork(
        &self,
        parent_root: &TrajectoryId,
        parent: &TrajectoryId,
        new_root: &TrajectoryId,
    ) -> Result<(), RootForkRefusal> {
        if new_root == parent_root || new_root == parent {
            return Err(RootForkRefusal::RootIdConflict);
        }
        let standing = self
            .inner
            .store
            .has_root(&crate::engine::engine_id(new_root))
            .inspect_err(|error| {
                self.inner
                    .note_store_error(Some(new_root), crate::events::StoreOperation::Read, error)
            })
            .map_err(|error| RootForkRefusal::Refused(error.to_string()))?;
        if standing {
            return self.root_fork_already_open(new_root, parent_root, parent);
        }
        let refused = |error: EventError| RootForkRefusal::Refused(error.to_string());
        let deployment = self.inner.deployment();
        let log = self.inner.log(parent_root).map_err(|error| match error {
            EventError::UnknownTrajectory => RootForkRefusal::ParentUnavailable,
            error => refused(error),
        })?;
        let policy = self.inner.resolve_policy(&deployment, &log).map_err(refused)?;
        let view = policy
            .engine()
            .rebuild_view(&log)
            .map_err(|refusal| refused(EventError::from(refusal)))?;
        let origin = policy
            .engine()
            .root_fork_origin(&view, parent)
            .ok_or(RootForkRefusal::ParentUnavailable)?;
        let opening = policy
            .engine()
            .root_fork_opening(new_root, log.policy_file(), origin)
            .map_err(|refusal| RootForkRefusal::Refused(refusal.to_string()))?;
        match self.inner.store.create_root(opening, log.policy_file()) {
            Ok(_) => Ok(()),
            // A concurrent caller created this root ID after the check above.
            Err(appa_eventlog::CreateError::AlreadyExists { .. }) => {
                self.root_fork_already_open(new_root, parent_root, parent)
            }
            Err(error) => {
                self.inner
                    .note_store_error(Some(new_root), crate::events::StoreOperation::Open, &error);
                Err(RootForkRefusal::Refused(error.to_string()))
            }
        }
    }

    /// A repeat [`Runtime::open_root_fork`]: `fork_root` exists, which is the same root fork
    /// only when its opening record forked this parent.
    fn root_fork_already_open(
        &self,
        fork_root: &TrajectoryId,
        parent_root: &TrajectoryId,
        parent: &TrajectoryId,
    ) -> Result<(), RootForkRefusal> {
        let log = self
            .inner
            .log(fork_root)
            .map_err(|error| RootForkRefusal::Refused(error.to_string()))?;
        match log.facts().first() {
            Some(appa_engine::fact::Fact::TrajectoryOpened(appa_engine::fact::TrajectoryOpening {
                forked_from: Some(origin),
                ..
            })) if origin.is_from(
                &crate::engine::engine_id(parent_root),
                &crate::engine::engine_id(parent),
            ) =>
            {
                Ok(())
            }
            _ => Err(RootForkRefusal::RootIdConflict),
        }
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
        self.session_with_presentation(root, trajectory, EmbeddedPresentationOptions::default())
    }

    pub(crate) fn session_with_presentation(
        &self,
        root: &TrajectoryId,
        trajectory: &TrajectoryId,
        presentation: EmbeddedPresentationOptions,
    ) -> Result<Session, EventError> {
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
        Ok(Session::attach_with_presentation(
            Arc::clone(&self.inner),
            self.inner.deployment(),
            trajectory.clone(),
            root.clone(),
            presentation,
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
    #[cfg(feature = "daemon")]
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
    #[cfg(feature = "daemon")]
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
            )
            .with_hostname(request.hostname.clone());
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
    #[cfg(feature = "daemon")]
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

    /// Executes an embedded remedy plan for an authenticated actor.
    /// Returns a structured `RemedyOutcome`.
    pub async fn execute_embedded_remedy(&self, actor: &Actor, args: ExecuteRemedyPlanArgs) -> RemedyOutcome {
        self.execute_embedded_remedy_with_options(actor, args, EmbeddedPresentationOptions::default())
            .await
    }

    /// Executes one admitted embedded remedy with request-scoped display capabilities.
    pub async fn execute_embedded_remedy_with_options(
        &self,
        actor: &Actor,
        args: ExecuteRemedyPlanArgs,
        presentation: EmbeddedPresentationOptions,
    ) -> RemedyOutcome {
        self.execute_remedy_outcome(args, None, Some(actor), presentation, true)
            .await
    }

    /// Executes a remedy plan for the MCP daemon, returning an MCP tool reply.
    #[cfg(feature = "daemon")]
    pub(crate) async fn execute_remedy_plan(
        &self,
        args: ExecuteRemedyPlanArgs,
        elicitation: Option<&Elicitation>,
        expected_actor: Option<&Actor>,
    ) -> RemedyReply {
        self.render_remedy(
            self.execute_remedy_outcome(
                args,
                elicitation,
                expected_actor,
                EmbeddedPresentationOptions::default(),
                false,
            )
            .await,
        )
    }

    async fn execute_remedy_outcome(
        &self,
        args: ExecuteRemedyPlanArgs,
        elicitation: Option<&Elicitation>,
        expected_actor: Option<&Actor>,
        presentation: EmbeddedPresentationOptions,
        strict_freshness: bool,
    ) -> RemedyOutcome {
        let quoted = match OfferId::parse(&args.offer_id) {
            Ok(quoted) => quoted,
            Err(reason) => {
                return RemedyOutcome::Refused {
                    reason: RemedyRefusal::InvalidOfferId(reason),
                };
            }
        };
        let arguments = RemedyArguments::from(args);
        let key = PermitKey::offer(&quoted);
        if let Some(expected) = expected_actor {
            match self.peek_vouched(&key) {
                Ok((actor, _)) if actor != *expected => {
                    return RemedyOutcome::Refused {
                        reason: RemedyRefusal::ActorMismatch,
                    };
                }
                Err(_) => {
                    return RemedyOutcome::Refused {
                        reason: RemedyRefusal::Unvouched,
                    };
                }
                Ok(_) => {}
            }
        }
        let (acting, ruling) = match self.take_vouched(&key) {
            Ok(consumed) => consumed,
            Err(_) => {
                return RemedyOutcome::Refused {
                    reason: RemedyRefusal::Unvouched,
                };
            }
        };
        let started = std::time::Instant::now();
        let outcome = if strict_freshness && self.offer_kind(&acting.root, &quoted).is_none() {
            RemedyOutcome::Refused {
                reason: RemedyRefusal::UnknownOffer,
            }
        } else {
            self.remedy_with_presentation(&acting, quoted.clone(), arguments, elicitation, ruling, presentation)
                .await
        };
        // Recorded from the typed outcome, before rendering turns it into the text the
        // model reads: a remedy that takes a minute and then declines is the shape of "APPA
        // is in the way", and neither the duration nor the offer it quoted is in the
        // trajectory.
        self.record(
            // The family, never the acting trajectory: see `Session::timed_consult`.
            Some(&acting.root),
            crate::events::RuntimeEvent::Control {
                call: crate::events::ControlCall::Remedy {
                    offer: quoted.0,
                    dispatch: None,
                },
                outcome: (&outcome).into(),
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            },
        );
        outcome
    }

    /// The runtime keys the released call on its canonical identity, which is not a name any
    /// host advertises. What the model is told to call is that identity spelled the way its own
    /// harness dispatches it ([`Runtime::model_spelling`]).
    pub fn render_remedy(&self, outcome: RemedyOutcome) -> RemedyReply {
        let answer = |text: String| RemedyReply { text, is_error: false };
        match outcome {
            RemedyOutcome::Authorized { call } => answer(format!(
                "[appa] Authorized. Call the {} tool again with exactly these arguments: {}",
                self.model_spelling(&call.tool),
                call.arguments.get(),
            )),
            RemedyOutcome::Substituted { call } => answer(format!(
                "[appa] Substituted. The sanitizer replaced the arguments and the call is released. \
                 Call the {} tool with exactly these arguments to run it: {}",
                self.model_spelling(&call.tool),
                call.arguments.get(),
            )),
            RemedyOutcome::Returned { value } => answer(value),
            RemedyOutcome::Declined { presentation } => answer(presentation.feedback),
            RemedyOutcome::NoAnswer { feedback } => answer(feedback),
            RemedyOutcome::Refused { reason } => RemedyReply {
                text: reason.detail().to_string(),
                is_error: true,
            },
        }
    }
}

/// The control call's arguments as a model spells them — `offer_id`, and for a plan
/// declaring a subagent's return `label` and `return_schema` — for a harness that routes the
/// control tool itself.
pub fn parse_control_arguments(arguments: &str) -> Result<(OfferId, RemedyArguments), String> {
    let args: ExecuteRemedyPlanArgs =
        serde_json::from_str(arguments).map_err(|error| format!("execute_remedy_plan arguments: {error}"))?;
    let offer = OfferId::parse(&args.offer_id).map_err(|error| format!("execute_remedy_plan offer_id: {error}"))?;
    Ok((offer, RemedyArguments::from(args)))
}

/// The arguments the control tool takes. The schema derive is what the MCP tool
/// declaration is built from, and one reading of them serves every caller.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ExecuteRemedyPlanArgs {
    pub offer_id: String,
    /// For a plan that declares a subagent's return: the lowest label this session accepts
    /// from the return, in the policy's `delta` spelling. An omitted dimension keeps this
    /// session's current value.
    #[serde(default)]
    pub label: Option<LabelArgs>,
    /// For a plan that attests a subagent's return: the JSON schema the return must match.
    #[serde(default)]
    pub return_schema: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LabelArgs {
    #[serde(default)]
    pub trust: Option<String>,
    #[serde(default)]
    pub audience: Option<Vec<String>>,
}

impl From<ExecuteRemedyPlanArgs> for RemedyArguments {
    fn from(args: ExecuteRemedyPlanArgs) -> RemedyArguments {
        RemedyArguments {
            label: args.label.map(|label| LabelSpelling {
                trust: label.trust,
                audience: label.audience,
            }),
            return_schema: args.return_schema,
        }
    }
}

/// One control call's answer as the model reads it: the text, and whether the
/// call failed. A transport turns it into whatever its own protocol calls a
/// tool result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemedyReply {
    pub text: String,
    pub is_error: bool,
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
        self.remedy_with_presentation(
            acting,
            quoted,
            arguments,
            elicitation,
            ruling,
            EmbeddedPresentationOptions::default(),
        )
        .await
    }

    pub(crate) async fn remedy_with_presentation(
        &self,
        acting: &Actor,
        quoted: OfferId,
        arguments: RemedyArguments,
        elicitation: Option<&Elicitation>,
        ruling: Option<appa_runtime_api::Ruling>,
        presentation: EmbeddedPresentationOptions,
    ) -> RemedyOutcome {
        let unknown = || RemedyOutcome::Refused {
            reason: RemedyRefusal::UnknownOffer,
        };
        let root = acting.root.clone();
        let trajectory = acting.child.clone().unwrap_or_else(|| root.clone());
        let Some((offer, pursuer)) = self.resolve_in(&root, &quoted) else {
            return unknown();
        };
        if pursuer != trajectory {
            return unknown();
        }
        // The id the model quoted, which is the one its vouch was recorded under: claiming
        // spends that standing, and every quote that resolves at all is this one spelling.
        let claimed = PermitKey::offer(&quoted);
        let actor = host_actor(acting);
        let until = std::time::SystemTime::now() + self.execution_bound(elicitation.is_some());
        // Two calls naming one offer must not both reach its authorities: a human review
        // holds its call open for minutes, and a second dialog for one decision is the
        // thing this refuses. The claim is derived at the position it is written at, so a
        // writer that loses the compare-and-swap sees the winner's claim and refuses.
        if let Err(refused) =
            self.inner
                .append_host_with::<RemedyOutcome, _>(&root, |log| match reduced(log).claimed(&claimed) {
                    true => Err(RemedyOutcome::Refused {
                        reason: RemedyRefusal::AlreadyExecuting,
                    }),
                    false => Ok((
                        Some(HostObservation::Claimed {
                            actor: actor.clone(),
                            key: claimed.wire(),
                            until,
                        }),
                        (),
                    )),
                })
        {
            return refused;
        }
        let mut claim = Claimed {
            inner: Arc::clone(&self.inner),
            root: root.clone(),
            actor,
            key: claimed.wire(),
            released: false,
        };
        let outcome = self
            .run_remedy(acting, offer, arguments, elicitation, ruling, presentation)
            .await;
        claim.release();
        outcome
    }

    async fn run_remedy(
        &self,
        acting: &Actor,
        offer: OfferId,
        arguments: RemedyArguments,
        elicitation: Option<&Elicitation>,
        ruling: Option<appa_runtime_api::Ruling>,
        presentation: EmbeddedPresentationOptions,
    ) -> RemedyOutcome {
        let root = &acting.root;
        let pursuer = acting.child.as_ref().unwrap_or(root);
        let session = match self.session_with_presentation(root, pursuer, presentation) {
            Ok(session) => session,
            Err(error) => {
                return RemedyOutcome::Refused {
                    reason: RemedyRefusal::Runtime {
                        detail: error.to_string(),
                    },
                };
            }
        };
        match session.on_remedy(offer, arguments, elicitation, ruling).await {
            Ok(RemedyDecision::Authorized { call }) => RemedyOutcome::Authorized { call: call.proposed() },
            Ok(RemedyDecision::Substituted { call }) => RemedyOutcome::Substituted { call: call.proposed() },
            Ok(RemedyDecision::Returned { value }) => RemedyOutcome::Returned { value },
            Ok(RemedyDecision::Declined { presentation }) => RemedyOutcome::Declined { presentation },
            Ok(RemedyDecision::NoAnswer { feedback }) => RemedyOutcome::NoAnswer { feedback },
            Err(error) => RemedyOutcome::Refused {
                reason: RemedyRefusal::Runtime {
                    detail: error.to_string(),
                },
            },
        }
    }

    /// How long one execution's claim stands without word from the execution itself: the
    /// person's budget where a review reaches one, and the machine-consult budget — every
    /// resolution round of it — otherwise. Only a hard crash ever reaches the bound.
    fn execution_bound(&self, reviewed: bool) -> std::time::Duration {
        if reviewed {
            return self.review_timeout();
        }
        let externals = &self.inner.deployment().config.externals;
        externals.timeout.max(externals.claude_code.timeout) * session::RESOLUTION_ROUNDS
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
    ///
    /// A standing that does not land is no standing: the tool then finds nothing vouched
    /// and says so, which is the same answer a hook that never ran would leave.
    pub(crate) fn vouch(&self, key: &PermitKey, acting: &Actor, ruling: Option<appa_runtime_api::Ruling>) {
        if let Err(error) = self.inner.append_host(
            &acting.root,
            &HostObservation::Vouched {
                actor: host_actor(acting),
                key: key.wire(),
                ruling,
            },
        ) {
            tracing::warn!(root = %acting.root.0, %error, "this trajectory's standing was not recorded");
        }
    }

    /// The trajectory vouched for this key, taken once, with the ruling its harness
    /// attached.
    ///
    /// The request naming no session is the whole problem: the caller is found by asking the
    /// store which families named this key at all, and reducing each one's host records. The
    /// runtime remembers nothing between the vouch and the take, so a standing another
    /// process recorded reads the same as one this process did.
    ///
    /// Two trajectories standing behind one key is not a tie to break: it means the key does
    /// not identify a caller, and answering either one would put one session's standing
    /// behind another session's call. That case keeps the record rather than consuming it —
    /// destroying it would make the *next* identical call look like one nothing vouched for,
    /// and the two need different answers. The turn's end releases it either way.
    ///
    /// A read that fails answers nobody. A partial view of who stands behind a key cannot
    /// tell a lone holder from one of two, so authorizing from it would answer the ambiguous
    /// case with one session's standing.
    /// Who currently stands behind this key, without spending the standing.
    fn peek_vouched(&self, key: &PermitKey) -> Result<(Actor, Option<appa_runtime_api::Ruling>), Unvouched> {
        if PermitKey::parse(&key.wire()).as_ref() != Some(key) {
            return Err(Unvouched::Nobody);
        }
        let root = self.sole_holder(key)?;
        let log = self.inner.log(&root).map_err(|_| Unvouched::Nobody)?;
        if self.live_holder(key, Some(&root))?.is_some() {
            return Err(Unvouched::Ambiguous);
        }
        reduced(&log).vouched(key)
    }

    pub(crate) fn take_vouched(&self, key: &PermitKey) -> Result<(Actor, Option<appa_runtime_api::Ruling>), Unvouched> {
        // The spelling is read before the store is asked anything. An MCP request names the
        // key, so a caller over a non-loopback listener can ask about ids this runtime never
        // minted, and learning that no such offer exists must not cost a walk over every
        // family's records.
        if PermitKey::parse(&key.wire()).as_ref() != Some(key) {
            return Err(Unvouched::Nobody);
        }
        let root = self.sole_holder(key)?;
        // Taking is one-shot, and the consumption is what makes it one: the answer is
        // whatever stood where the release landed, not what an earlier read showed. A turn
        // that ended in between ended this standing, and a sibling that vouched in between
        // made the key ambiguous; both refuse here with nothing written.
        //
        // The compare-and-swap is this root's alone — the release lands on its log — while
        // ambiguity is a fact about every root, so the re-discovery is a read and not a
        // second position to swap on. What that leaves is one window: a sibling's vouch that
        // lands after this re-read and before this append is not seen, and the release goes
        // through. The log records both, so the next take reads the ambiguity; and it is a
        // strictly narrower window than the in-memory map this replaced, which was one
        // process's mutex and never saw another process's vouch at any moment at all.
        let consumed = self.inner.append_host_with::<Unvouched, _>(&root, |log| {
            // What this root holds where the release will land, and then that no sibling has
            // taken up the key since: one live holder in one family is the whole condition.
            // The loop already read this root at the position it writes at, so the re-read
            // is the siblings only — and where the key names no sibling, which is every
            // offer and every ticket only one session quoted, there is nothing to read.
            let (actor, ruling) = reduced(log).vouched(key)?;
            if self.live_holder(key, Some(&root))?.is_some() {
                return Err(Unvouched::Ambiguous);
            }
            let release = HostObservation::Released {
                actor: host_actor(&actor),
                key: key.wire(),
            };
            Ok((Some(release), (actor, ruling)))
        });
        if consumed.is_err() {
            tracing::warn!(root = %root.0, "the standing behind this call was not consumed, so it is not spent");
        }
        consumed
    }

    /// The one root holding the one live standing behind this key, which is the family whose
    /// log the release will land on.
    fn sole_holder(&self, key: &PermitKey) -> Result<TrajectoryId, Unvouched> {
        self.live_holder(key, None)?.ok_or(Unvouched::Nobody)
    }

    /// Which family holds a live standing behind this key, found by asking the store which
    /// families named it and reducing each one's host records, and leaving out the one the
    /// caller has already folded for itself.
    ///
    /// Nothing held is nothing; two — in one family or across two — are `Ambiguous`, because
    /// then the key does not identify a caller. A read that fails answers nobody, because a
    /// partial view cannot tell a lone holder from one of two and would answer the ambiguous
    /// case with one session's standing.
    fn live_holder(&self, key: &PermitKey, folded: Option<&TrajectoryId>) -> Result<Option<TrajectoryId>, Unvouched> {
        let candidates = match self.inner.store.roots_mentioning(&key.wire()) {
            Ok(candidates) => candidates,
            Err(error) => {
                tracing::warn!(%error, "the families standing behind this key did not read, so nothing stands");
                self.inner
                    .note_store_error(None, crate::events::StoreOperation::Read, &error);
                return Err(Unvouched::Nobody);
            }
        };
        let mut held: Option<TrajectoryId> = None;
        for candidate in candidates {
            let root = TrajectoryId(candidate.as_str().to_string());
            if folded == Some(&root) {
                continue;
            }
            // Every host record, not the rows that name the key: a turn's end releases
            // every standing its actor held and says so without naming one.
            let log = match self.inner.log(&root) {
                Ok(log) => log,
                Err(error) => {
                    tracing::warn!(root = %root.0, %error, "this family's records did not read, so nothing stands");
                    return Err(Unvouched::Nobody);
                }
            };
            match reduced(&log).vouched(key) {
                Ok(_) if held.is_none() => held = Some(root),
                Ok(_) | Err(Unvouched::Ambiguous) => return Err(Unvouched::Ambiguous),
                Err(Unvouched::Nobody) => {}
            }
        }
        Ok(held)
    }

    /// A prompt reached this actor, which is the sign that its previous turn is over
    /// however it ended.
    pub(crate) fn record_prompt(&self, acting: &Actor) -> Result<(), EventError> {
        self.inner.append_host(
            &acting.root,
            &HostObservation::PromptSeen {
                actor: host_actor(acting),
            },
        )
    }

    /// What the prompt left open is settled, and this actor's standing survives it.
    pub(crate) fn record_prompt_settled(&self, acting: &Actor) -> Result<(), EventError> {
        self.inner.append_host(
            &acting.root,
            &HostObservation::PromptSettled {
                actor: host_actor(acting),
            },
        )
    }

    /// This actor's turn is over: its prompt mark and every vouch it still held end here.
    /// A vouch is recorded when the actor quotes an offer at the control tool's hook and
    /// taken when the tool itself runs, both inside one turn. One still standing at the
    /// turn's end was never spent — the harness declined the call, or the tool never ran —
    /// and nothing later can spend it.
    pub(crate) fn record_turn_end(&self, acting: &Actor) -> Result<(), EventError> {
        self.inner.append_host(
            &acting.root,
            &HostObservation::TurnEnded {
                actor: host_actor(acting),
            },
        )
    }

    /// Whether a prompt reached this actor and nothing has settled what it left behind. A
    /// family with no log, or one the store cannot read, has been reached by nothing.
    pub(crate) fn prompted(&self, acting: &Actor) -> bool {
        let marked = crate::engine::engine_id(acting_trajectory(acting));
        self.inner
            .log(&acting.root)
            .is_ok_and(|log| host::prompted(log.host_records(), &marked))
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
                .handle(&view, trajectory, event, &EmbeddedPresentationOptions::default())
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

    /// Leave the claim an execution that never returned would have left, for the tests
    /// that pin how one is read back.
    #[cfg(all(test, feature = "daemon"))]
    pub(crate) fn claim_until(&self, acting: &Actor, offer: &OfferId, until: std::time::SystemTime) {
        self.inner
            .append_host(
                &acting.root,
                &HostObservation::Claimed {
                    actor: host_actor(acting),
                    key: PermitKey::offer(offer).wire(),
                    until,
                },
            )
            .expect("the claim lands");
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
    #[cfg(feature = "daemon")]
    pub(crate) fn agent_yell(&self) -> bool {
        self.inner.deployment().config.reporting.agent_yell
    }
}

/// One offer's execution, released when the act that claimed it ends — including by a
/// client that walked away, which drops the future between two awaits and reaches no exit
/// of its own.
///
/// The release is a record, so it is written here rather than left to the claim's bound:
/// the bound is the hard-crash backstop, and a dropped request is not a crash. Appending in
/// `Drop` is what the log allows — every store operation is synchronous and none awaits —
/// and a release that cannot be written is logged and let go, because a destructor has
/// nowhere to report it.
struct Claimed {
    inner: Arc<Inner>,
    root: TrajectoryId,
    actor: appa_eventlog::HostActor,
    key: String,
    released: bool,
}

impl Claimed {
    fn release(&mut self) {
        if std::mem::replace(&mut self.released, true) {
            return;
        }
        if let Err(error) = self.inner.append_host(
            &self.root,
            &HostObservation::Released {
                actor: self.actor.clone(),
                key: self.key.clone(),
            },
        ) {
            tracing::warn!(root = %self.root.0, %error, "the executed offer's claim was not released; it stands until it expires");
        }
    }
}

impl Drop for Claimed {
    fn drop(&mut self) {
        self.release();
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
    // Every provider the policy references is bound, and so is every entry a provider's
    // `lookup` names. A bound provider the policy never references stays idle rather than
    // refused: a battery binds its own source, and a deployment may include the battery
    // for its tool rules alone. A roster answers member lookups only, so a provider whose
    // selectors the policy reads never binds one.
    let providers: std::collections::BTreeSet<&str> = rc
        .audience
        .sources
        .iter()
        .map(|source| source.provider.as_str())
        .collect();
    let targets: std::collections::BTreeSet<&str> = providers
        .iter()
        .filter_map(|provider| externals.audience.get(*provider))
        .filter_map(|binding| binding.lookup.as_deref())
        .collect();
    no_unbound(
        "audience source",
        providers.iter().chain(targets.iter()).copied(),
        &externals.audience,
    )?;
    for provider in providers {
        if matches!(
            externals.audience[provider].implementation,
            crate::config::AudienceImplementation::Readers(_)
        ) {
            return Err(OpenError::UnsupportedPolicy(format!(
                "[externals.audience.{provider}] is a readers table, but the policy reads selectors from {provider}; a roster answers member lookups only"
            )));
        }
    }
    Ok(())
}

fn bound_exactly<'a, Implementation>(
    kind: &'static str,
    registered: impl Iterator<Item = &'a str>,
    bound: &std::collections::BTreeMap<String, Implementation>,
) -> Result<(), OpenError> {
    let registered: std::collections::BTreeSet<&str> = registered.collect();
    no_unbound(kind, registered.iter().copied(), bound)?;
    no_undeclared(kind, registered.into_iter(), bound)
}

fn no_unbound<'a, Implementation>(
    kind: &'static str,
    registered: impl Iterator<Item = &'a str>,
    bound: &std::collections::BTreeMap<String, Implementation>,
) -> Result<(), OpenError> {
    let mut registered = registered;
    match registered.find(|name| !bound.contains_key(*name)) {
        Some(name) => Err(OpenError::UnboundExternal {
            kind,
            name: name.to_string(),
        }),
        None => Ok(()),
    }
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
    appa_policy::Config::from_toml_str_routed(
        &text,
        config.externals.lookup_targets(),
        config.externals.source_registrations(),
    )
    .map_err(|error| OpenError::Policy(Box::new(error)))
}

/// The `[policy]` table of a stored policy file, with the key of the bytes it came from.
///
/// A trajectory's own bytes rather than the deployment's current ones wherever there is a
/// trajectory: the log pins its policy file, so a reload since the session opened does not
/// rewrite the rules a report explains. The key comes from the same bytes, so the document and
/// its fingerprint are one snapshot.
#[cfg(feature = "daemon")]
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
    aliases: &crate::config::ServerBindings,
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
    let sources = crate::config::source_registrations_of(&value)
        .map_err(|error| format!("the stored policy declares its audience sources badly: {error}"))?;
    appa_policy::Config::from_toml_str_routed(&text, crate::config::lookup_targets_of(&value), sources)
        .map_err(|error| format!("the stored policy does not load: {error}"))
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
    use crate::config::{AnnotatorImplementation, Endpoint, HostDefaults};

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
                "mcp__appa__execute_remedy_plan",
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
    fn claude_config(document: &str) -> Config {
        Config::hosted(
            document,
            HostDefaults {
                consult_timeout: Duration::from_secs(30),
                max_body_bytes: 65_536,
            },
        )
        .expect("the hosted document validates")
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
                [policy]
                version = 2
                [[policy.annotator]]
                name = "any"
                builtin = "claude-code"
                [[policy.tool]]
                name = "*"
                annotator = "any"
                [policy.deployment]
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
                [policy]
                version = 2
                [[policy.annotator]]
                name = "classifier"
                builtin = "claude-code"
                [[policy.tool]]
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
                [policy]
                version = 2
                [[policy.annotator]]
                name = "classifier"
                builtin = "claude-code"
                [[policy.tool]]
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
                [policy]
                version = 2
                [[policy.annotator]]
                name = "bash-classifier"
                builtin = "claude-code"
                [[policy.annotator]]
                name = "other-classifier"
                [[policy.tool]]
                name = "Bash"
                description = "Runs one shell command."
                annotator = "bash-classifier"
                [[policy.tool]]
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
            [policy]
            version = 2
            [[policy.authority]]
            name = "reviewer"
            [policy.authority.permits]
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
            [policy]
            version = 2
            [[policy.annotator]]
            name = "classifier"
            [[policy.tool]]
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
                [policy]
                version = 2
                [[policy.annotator]]
                name = "classifier"
                builtin = "claude-code"
                [[policy.tool]]
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
            [policy]
            version = 2
            [[policy.annotator]]
            name = "classifier"
            builtin = "llm"
            [[policy.tool]]
            name = "lookup"
            description = "Looks one record up."
            annotator = "classifier"
        "#;
        let with_profile = || {
            claude_config(&format!(
                "{policy}\n[externals.llm]\nprovider = \"ollama\"\nmodel = \"llama\"\n"
            ))
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
            [policy]
            version = 2
            [[policy.annotator]]
            name = "classifier"
            builtin = "llm"
            [[policy.tool]]
            name = "lookup"
            description = "Looks one record up."
            annotator = "classifier"
            [[policy.authority]]
            name = "auditor"
            [policy.authority.permits]
            attention = ["irreversible"]
        "#;
        let with_pool = |max_concurrent: u32| {
            claude_config(&format!(
                "{policy}\n[externals.llm]\nprovider = \"ollama\"\nmodel = \"llama\"\nmax_concurrent = {max_concurrent}\n"
            ))
        };
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(with_pool(2), dir.path().join("appa.db"), None).expect("the deployment opens");
        assert_eq!(runtime.inner.shared.gates.llm_permits(), 2);

        // A candidate that validates but cannot build its externals declares a wider pool
        // and never serves: the gate stays as the serving deployment declared it.
        let mut refused = with_pool(5);
        refused.externals.authorities.insert(
            "auditor".to_string(),
            crate::config::Implementation::Builtin("no-such".to_string()),
        );
        assert!(matches!(runtime.reload(refused), Err(OpenError::Modules(_))));
        assert_eq!(runtime.inner.shared.gates.llm_permits(), 2);

        assert!(runtime.reload(with_pool(3)).is_ok());
        assert_eq!(runtime.inner.shared.gates.llm_permits(), 3);
        assert!(
            runtime.reload(claude_config(policy)).is_err(),
            "no profile, no declared llm"
        );
        assert_eq!(runtime.inner.shared.gates.llm_permits(), 3);

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
            .shared
            .deployment
            .read()
            .expect("the deployment lock is never poisoned")
            .config
            .externals
            .llm_bound();
        assert_eq!(runtime.inner.shared.gates.llm_permits(), serving);
    }

    /// Two policies that differ only in a tool's description, so a root opened
    /// under one replays against the other through the retired branch.
    fn versioned_policy(description: &str) -> Config {
        claude_config(&format!(
            r#"
            [policy]
            version = 2
            [[policy.tool]]
            name = "fetch"
            description = "{description}"
            "#
        ))
    }

    #[tokio::test]
    async fn a_view_records_in_its_own_store_under_the_deployment_it_shares() {
        let runtime = Runtime::open_in_memory(versioned_policy("first"), None).expect("the deployment opens");
        let other = Arc::new(LogStore::open(appa_eventlog::Backend::Memory).expect("a second store opens"));
        let view = runtime.on(Arc::clone(&other));
        let root = TrajectoryId("viewed".to_string());
        assert_eq!(
            crate::hooks::handle(&view, appa_runtime_api::HookEvent::SessionStart { root: root.clone() }).await,
            appa_runtime_api::HookDecision::Ack
        );
        let id = crate::engine::engine_id(&root);
        assert!(other.has_root(&id).expect("the view's store reads"));
        assert!(
            !runtime.store().has_root(&id).expect("the runtime's store reads"),
            "a view writes nothing to the store of the runtime it was made from"
        );

        let reloaded = runtime
            .reload(versioned_policy("second"))
            .expect("the second deployment loads");
        let later = TrajectoryId("viewed-after-reload".to_string());
        crate::hooks::handle(&view, appa_runtime_api::HookEvent::SessionStart { root: later.clone() }).await;
        let opened_under = other
            .log(&crate::engine::engine_id(&later))
            .expect("the later root reads");
        assert_eq!(
            crate::engine::policy_file_key(opened_under.policy_file()),
            reloaded.policy_key,
            "a reload through the runtime serves the views made before it"
        );
    }

    /// OPENAPPA_TEST_DATABASE_URL=... cargo test -p appa --features postgres -- --ignored
    #[cfg(feature = "postgres")]
    #[tokio::test]
    #[ignore = "requires OPENAPPA_TEST_DATABASE_URL and host migrations"]
    async fn a_view_on_a_lease_records_under_the_session_lock_the_lease_holds() {
        let url = std::env::var("OPENAPPA_TEST_DATABASE_URL").expect("test database URL");
        let store = Arc::new(
            LogStore::open(appa_eventlog::Backend::Postgres {
                url,
                max_connections: std::num::NonZeroUsize::new(2).expect("a pool holds a connection"),
            })
            .expect("the PostgreSQL store opens"),
        );
        let runtime =
            Runtime::open_with_store(versioned_policy("leased"), Arc::clone(&store), None).expect("the runtime opens");
        let unique = tempfile::tempdir().expect("a unique root name exists");
        let root = TrajectoryId(format!("leased:{}", unique.path().display()));
        let id = crate::engine::engine_id(&root);

        // A host serializes a trajectory across its replicas with a session-level lock on
        // the key an append locks too. Only the connection that holds it can append.
        let lease = Arc::new(store.lease().expect("a connection leases"));
        let key = root.0.clone();
        lease
            .postgres()
            .expect("the PostgreSQL API is present")
            .with_client(move |client| {
                client.query_one("SELECT pg_advisory_lock(hashtextextended($1, 0))", &[&key])?;
                Ok(())
            })
            .expect("the host's session lock is taken");
        let key = root.0.clone();
        let free_elsewhere = store
            .lease()
            .expect("a second connection leases")
            .postgres()
            .expect("the PostgreSQL API is present")
            .with_client(move |client| {
                Ok(client
                    .query_one("SELECT pg_try_advisory_lock(hashtextextended($1, 0))", &[&key])?
                    .get::<_, bool>(0))
            })
            .expect("another connection asks for the lock");
        assert!(!free_elsewhere, "the lock shuts out every other connection of the pool");

        assert_eq!(
            crate::hooks::handle(
                &runtime.on(Arc::clone(&lease)),
                appa_runtime_api::HookEvent::SessionStart { root: root.clone() }
            )
            .await,
            appa_runtime_api::HookDecision::Ack
        );
        assert!(store.has_root(&id).expect("the store reads"));

        let key = root.0.clone();
        lease
            .postgres()
            .expect("the PostgreSQL API is present")
            .with_client(move |client| {
                client.execute("DELETE FROM openappa_events WHERE root=$1", &[&key])?;
                Ok(())
            })
            .expect("the test root cleans up");
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

    #[cfg(feature = "daemon")]
    /// Two stored policies that differ only in how their `[deployment]` field spells the
    /// tool it confines. The wildcard contract covers every name, so either spelling passes
    /// coverage at load; the contract permits the call, and only the confinement reads the
    /// name exactly.
    fn confining_policy(confined: &str) -> Config {
        claude_config(&format!(
            r#"
            [policy]
            version = 2
            [[policy.annotator]]
            name = "any"
            builtin = "claude-code"
            [[policy.tool]]
            name = "host/claude-code/Bash"
            [[policy.tool]]
            name = "*"
            annotator = "any"
            [policy.deployment]
            confined_results = ["{confined}"]
            "#
        ))
    }

    #[cfg(feature = "daemon")]
    /// The canonical policy the served deployment serves now: different bytes from either
    /// stored one, so a trajectory recorded under those reopens through the retired branch.
    fn served_policy() -> Config {
        claude_config(
            r#"
            [policy]
            version = 2
            [[policy.tool]]
            name = "host/claude-code/Bash"
            [[policy.tool]]
            name = "host/claude-code/Read"
            [policy.deployment]
            confined_results = ["host/claude-code/Bash"]
            "#,
        )
    }

    #[cfg(feature = "daemon")]
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
                    call_id: None,
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

    #[cfg(feature = "daemon")]
    /// Native policy names remain usable; normalization is internal and applies equally
    /// to startup and candidate reloads.
    #[test]
    fn a_served_deployment_accepts_native_policy_at_startup_and_on_reload() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let db = dir.path().join("appa.db");
        let raw = || {
            claude_config(
                r#"
                [policy]
                version = 2
                [[policy.tool]]
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

    #[cfg(feature = "daemon")]
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
        let mut candidate = claude_config("[policy]\nversion = 2\n[[policy.tool]]\nname = 'Read'\n");
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

    #[cfg(feature = "daemon")]
    #[test]
    fn observed_native_bindings_survive_reload_and_process_reopen() {
        use appa_runtime_api::inventory::{ObservedTool, ToolInventory};
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("appa.db");
        let config = || claude_config("[policy]\nversion = 2\n[[policy.tool]]\nname = \"read_secret\"\n");
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
            .reload(claude_config(
                "[policy]\nversion = 2\n[[policy.tool]]\nname = \"other\"\n",
            ))
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

    #[cfg(feature = "daemon")]
    #[test]
    fn late_inventory_is_scoped_idempotent_and_cannot_rebind_after_reopen() {
        use appa_runtime_api::inventory::{ObservedTool, ToolInventory};
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("appa.db");
        let config = || {
            claude_config(
                "[policy]\nversion = 2\n[[policy.tool]]\nname = 'mcp/demo/read'\n[[policy.tool]]\nname = 'mcp/other/read'\n",
            )
        };
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
        assert_eq!(inventories(&after).len(), 2);
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

    #[cfg(feature = "daemon")]
    #[test]
    fn inventory_retries_contention_and_only_one_racing_identity_wins() {
        use appa_runtime_api::inventory::{ObservedTool, ToolInventory};
        let dir = tempfile::tempdir().unwrap();
        let adapter = appa_adapter_kagent::adapter();
        let runtime = Runtime::open_served(
            claude_config("[policy]\nversion = 2\n[[policy.tool]]\nname = 'read'\n"),
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
        assert_eq!(inventories(&log).len(), 1);
        assert_eq!(log.basis(), 3, "opening, injected competing batch, accepted inventory");
    }

    #[cfg(feature = "daemon")]
    #[test]
    fn remote_preflight_is_read_only_and_uses_the_requested_policy() {
        use crate::tool_validation::{ToolStatus, ValidationReport};
        let dir = tempfile::tempdir().unwrap();
        let adapter = appa_adapter_kagent::adapter();
        let runtime = Runtime::open_served(
            claude_config("[policy]\nversion = 2\n[[policy.tool]]\nname = 'read'\n"),
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
            .reload(claude_config(
                "[policy]\nversion = 2\n[[policy.tool]]\nname = 'write'\n",
            ))
            .unwrap();
        assert!(report(None, "write").is_valid());
        assert!(!report(None, "read").is_valid());
        assert!(report(Some("preflight"), "read").is_valid());
        assert!(!report(Some("preflight"), "write").is_valid());
        let after = runtime.inner.log(&root).unwrap();
        assert_eq!(before.basis(), after.basis());
        assert_eq!(before.policy_file(), after.policy_file());
        assert!(inventories(&after).is_empty());
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

    #[cfg(feature = "daemon")]
    #[test]
    fn remote_preflight_returns_only_this_actors_durable_reservations() {
        use appa_runtime_api::inventory::{ObservedTool, ToolInventory};
        let dir = tempfile::tempdir().unwrap();
        let adapter = appa_adapter_kagent::adapter();
        let db = dir.path().join("appa.db");
        let config = || claude_config("[policy]\nversion = 2\n[[policy.tool]]\nname = 'read'\n");
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

    #[cfg(feature = "daemon")]
    #[test]
    fn remote_preflight_rejects_bad_envelopes_and_host_name_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = appa_adapter_kagent::adapter();
        let runtime = Runtime::open_served(
            claude_config("[policy]\nversion = 2\n[[policy.tool]]\nname = 'read'\n"),
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

    #[cfg(feature = "daemon")]
    #[test]
    fn known_uncovered_inventory_does_not_open_a_trajectory() {
        use appa_runtime_api::inventory::{ObservedTool, ToolInventory};
        let dir = tempfile::tempdir().unwrap();
        let runtime = Runtime::open_served(
            claude_config("[policy]\nversion = 2\n[[policy.tool]]\nname = \"read\"\n"),
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

    #[cfg(feature = "daemon")]
    #[tokio::test]
    async fn a_late_discovered_tool_uses_its_existing_native_rule_without_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = appa_adapter_kagent::adapter();
        let db = dir.path().join("appa.db");
        let runtime = Runtime::open_served(
            claude_config(
                "[policy]\nversion = 2\n[[policy.tool]]\nname = \"read_secret\"\n[policy.deployment]\nconfined_results = ['read_secret']\n",
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
            claude_config("[policy]\nversion = 2\n[[policy.tool]]\nname = 'different'\n"),
            db,
            None,
            adapter,
        )
        .unwrap();
        reopened.live(&root, &root).unwrap();
        let after = reopened.inner.log(&root).unwrap();
        assert_eq!(after.facts(), before.facts());
        assert_eq!(after.policy_file(), before.policy_file());
        assert_eq!(inventories(&after), inventories(&before));
    }

    #[cfg(feature = "daemon")]
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
                "[policy]\nversion = 2\n[[policy.tool]]\nname = 'read'\n[[policy.tool]]\nname = 'team__NS__child'\n[policy.deployment]\ncontext_control = true\n",
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
        let child_actor = Actor {
            root: root.clone(),
            child: Some(TrajectoryId("kagent:family:child".into())),
        };
        let empty = appa_runtime_api::inventory::ToolInventory::default();
        let check = runtime
            .preflight_inventory(Some(&child_actor), appa_adapter_kagent::adapter(), &empty)
            .unwrap();
        assert!(!check.actor_opened);
        send(&runtime, serde_json::json!({"event":"child_start","child_id":"child","spawn_binding":released["spawn_binding"],"inventory":{"tools":[],"sources":[]}})).await;
        let check = runtime
            .preflight_inventory(Some(&child_actor), appa_adapter_kagent::adapter(), &empty)
            .unwrap();
        assert!(check.actor_opened, "opening is independent of the tool count");
        assert!(check.accepted_tools.is_empty());
        let allowed = send(&runtime, serde_json::json!({"event":"tool_call","child_id":"child","tool":"mcp:child-server/read","arguments":{},"inventory":inventory("child-server")})).await;
        assert_eq!(allowed["decision"], "allow_call");
        let after = runtime.inner.log(&root).unwrap();
        assert_eq!(before.policy_file(), after.policy_file());
        assert_eq!(
            inventories(&after).len(),
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

    #[cfg(feature = "daemon")]
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

    #[cfg(feature = "daemon")]
    /// The inventory evidence one log holds, in append order.
    fn inventories(log: &Log) -> Vec<&appa_runtime_api::inventory::ToolInventory> {
        log.host_records()
            .iter()
            .filter_map(|record| match &record.observation {
                HostObservation::Inventory { inventory, .. } => Some(inventory),
                _ => None,
            })
            .collect()
    }

    fn retired_len(runtime: &Runtime) -> usize {
        runtime
            .inner
            .shared
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
        let quoted = PermitKey::offer(&OfferId("0ffe000000000001".to_string()));

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
        crate::hooks::handle(&runtime, appa_runtime_api::HookEvent::TurnEnd { actor: child.clone() }).await;
        assert_eq!(
            runtime.take_vouched(&quoted),
            Ok((parent, None)),
            "ending the child's turn takes away no vouch of the parent's"
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
        let quoted = PermitKey::offer(&OfferId("0ffe000000000001".to_string()));

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
        let runtime = std::sync::Arc::new(
            Runtime::open(versioned_policy("first"), dir.path().join("appa.db"), None).expect("the deployment opens"),
        );
        let key = PermitKey::Call("same-call".to_string());
        let mut actors = Vec::new();
        for id in ["cc:one", "cc:other"] {
            let root = TrajectoryId(id.to_string());
            crate::hooks::handle(
                &runtime,
                appa_runtime_api::HookEvent::SessionStart { root: root.clone() },
            )
            .await;
            actors.push(Actor { root, child: None });
        }
        let [one, other] = <[Actor; 2]>::try_from(actors).expect("two sessions");
        runtime.vouch(&key, &one, None);
        runtime.vouch(&key, &other, None);

        assert_eq!(runtime.take_vouched(&key), Err(Unvouched::Ambiguous));
        assert_eq!(
            runtime.take_vouched(&key),
            Err(Unvouched::Ambiguous),
            "the second caller is told the same thing, not that nobody vouched"
        );

        // Once one of them is gone the key identifies a caller again.
        crate::hooks::handle(&runtime, appa_runtime_api::HookEvent::TurnEnd { actor: other }).await;
        assert_eq!(runtime.take_vouched(&key), Ok((one, None)));
    }

    /// Two independent sessions standing behind one call key are two logs, and the take
    /// reads both: a key that does not identify a caller is refused wherever the standing
    /// was recorded, never answered from whichever log the runtime happened to look at.
    #[tokio::test]
    async fn two_families_behind_one_ticket_are_ambiguous_across_their_logs() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = std::sync::Arc::new(
            Runtime::open(versioned_policy("first"), dir.path().join("appa.db"), None).expect("the deployment opens"),
        );
        let ticket = crate::yell::YellArgs {
            message: "the hook blocked".to_string(),
            with_trajectory: true,
        }
        .ticket();
        let mut actors = Vec::new();
        for id in ["cc:first", "cc:second"] {
            let root = TrajectoryId(id.to_string());
            crate::hooks::handle(
                &runtime,
                appa_runtime_api::HookEvent::SessionStart { root: root.clone() },
            )
            .await;
            let actor = Actor { root, child: None };
            runtime.vouch(&ticket, &actor, None);
            actors.push(actor);
        }
        assert_eq!(runtime.take_vouched(&ticket), Err(Unvouched::Ambiguous));

        crate::hooks::handle(
            &runtime,
            appa_runtime_api::HookEvent::TurnEnd {
                actor: actors[1].clone(),
            },
        )
        .await;
        assert_eq!(runtime.take_vouched(&ticket), Ok((actors[0].clone(), None)));
    }

    /// The take answers for the position it consumes at, not the one it looked at. A turn
    /// that ends between the two ended this standing, and a take that answered anyway would
    /// authorize a call for a turn that is over.
    #[tokio::test]
    async fn a_turn_that_ends_under_a_take_takes_the_standing_with_it() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = std::sync::Arc::new(
            Runtime::open(versioned_policy("first"), dir.path().join("appa.db"), None).expect("the deployment opens"),
        );
        let root = TrajectoryId("cc:raced".to_string());
        let actor = Actor {
            root: root.clone(),
            child: None,
        };
        let ticket = crate::yell::YellArgs {
            message: "the hook blocked".to_string(),
            with_trajectory: true,
        }
        .ticket();
        crate::hooks::handle(
            &runtime,
            appa_runtime_api::HookEvent::SessionStart { root: root.clone() },
        )
        .await;
        runtime.vouch(&ticket, &actor, None);

        // The writer that wins the consume's first compare-and-swap ends this actor's turn,
        // so the position the release would have landed at no longer has a standing to spend.
        let engine_root = crate::engine::engine_id(&root);
        runtime.store().contend_next_append_with(
            &engine_root,
            &engine_root,
            &HostObservation::TurnEnded {
                actor: host_actor(&actor),
            },
        );
        assert_eq!(
            runtime.take_vouched(&ticket),
            Err(Unvouched::Nobody),
            "the standing the take looked at was gone where it would have consumed it"
        );
        assert!(
            !released(&runtime, &root),
            "and a take that answered nobody consumed nothing"
        );
    }

    /// The key names one caller or it names none. A second family standing behind the same
    /// ticket after the take chose the first is exactly the case that does not identify a
    /// caller, and consuming the first would hand this call one session's standing while
    /// another session was also waiting behind it.
    #[tokio::test]
    async fn a_sibling_that_vouches_under_a_take_makes_the_key_ambiguous() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = std::sync::Arc::new(
            Runtime::open(versioned_policy("first"), dir.path().join("appa.db"), None).expect("the deployment opens"),
        );
        let ticket = crate::yell::YellArgs {
            message: "the hook blocked".to_string(),
            with_trajectory: true,
        }
        .ticket();
        let mut actors = Vec::new();
        for id in ["cc:first", "cc:second"] {
            let root = TrajectoryId(id.to_string());
            crate::hooks::handle(
                &runtime,
                appa_runtime_api::HookEvent::SessionStart { root: root.clone() },
            )
            .await;
            actors.push(Actor { root, child: None });
        }
        runtime.vouch(&ticket, &actors[0], None);

        // The writer that wins the consume's first compare-and-swap puts the second family
        // behind the same ticket, so the re-derivation meets two holders where one stood.
        runtime.store().contend_next_append_with(
            &crate::engine::engine_id(&actors[0].root),
            &crate::engine::engine_id(&actors[1].root),
            &HostObservation::Vouched {
                actor: host_actor(&actors[1]),
                key: ticket.wire(),
                ruling: None,
            },
        );
        assert_eq!(
            runtime.take_vouched(&ticket),
            Err(Unvouched::Ambiguous),
            "a key two families stand behind names no caller"
        );
        for actor in &actors {
            assert!(
                !released(&runtime, &actor.root),
                "and an ambiguous take consumes neither standing"
            );
        }
    }

    /// Whether this family's log holds a spent standing.
    fn released(runtime: &Runtime, root: &TrajectoryId) -> bool {
        runtime
            .store()
            .log(&crate::engine::engine_id(root))
            .expect("the family reads")
            .host_records()
            .iter()
            .any(|record| matches!(&record.observation, HostObservation::Released { .. }))
    }

    /// The mark is the latest record about it, and every other actor's records are not about
    /// it. A family whose stream is long, and whose marks belong to several actors, answers
    /// for the actor asked about and for its last word only.
    #[tokio::test]
    async fn a_prompt_mark_is_whatever_this_actors_last_record_about_it_said() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = std::sync::Arc::new(
            Runtime::open(versioned_policy("first"), dir.path().join("appa.db"), None).expect("the deployment opens"),
        );
        let root = TrajectoryId("cc:marked".to_string());
        let parent = Actor {
            root: root.clone(),
            child: None,
        };
        let child = Actor {
            root: root.clone(),
            child: Some(TrajectoryId("cc:marked:a1".to_string())),
        };
        crate::hooks::handle(
            &runtime,
            appa_runtime_api::HookEvent::SessionStart { root: root.clone() },
        )
        .await;
        assert!(!runtime.prompted(&parent), "a family nothing prompted has no mark");

        runtime.record_prompt(&parent).expect("the mark records");
        assert!(runtime.prompted(&parent));
        assert!(!runtime.prompted(&child), "and it is not the child's mark");

        // A long tail of records about other things, and about another actor's mark.
        for _ in 0..8 {
            runtime.vouch(&PermitKey::Call("deadbeef".to_string()), &parent, None);
        }
        runtime.record_prompt(&child).expect("the child's mark records");
        runtime.record_turn_end(&child).expect("the child's turn ends");
        assert!(
            runtime.prompted(&parent),
            "another actor's prompt and turn end say nothing about this one"
        );
        assert!(!runtime.prompted(&child));

        runtime.record_prompt_settled(&parent).expect("the mark settles");
        assert!(!runtime.prompted(&parent));
        runtime.record_prompt(&parent).expect("a second prompt arrives");
        runtime.record_turn_end(&parent).expect("and its turn ends");
        assert!(!runtime.prompted(&parent), "the turn's end is the last word");
    }

    /// Losing the position is not losing the standing: a take raced by a writer that changed
    /// nothing about it still consumes it, once.
    #[tokio::test]
    async fn a_take_that_loses_a_race_to_an_unrelated_writer_still_consumes_once() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = std::sync::Arc::new(
            Runtime::open(versioned_policy("first"), dir.path().join("appa.db"), None).expect("the deployment opens"),
        );
        let root = TrajectoryId("cc:raced".to_string());
        let actor = Actor {
            root: root.clone(),
            child: None,
        };
        let ticket = crate::yell::YellArgs {
            message: "the hook blocked".to_string(),
            with_trajectory: true,
        }
        .ticket();
        crate::hooks::handle(
            &runtime,
            appa_runtime_api::HookEvent::SessionStart { root: root.clone() },
        )
        .await;
        runtime.vouch(&ticket, &actor, None);

        runtime.store().contend_next_appends(1);
        assert_eq!(runtime.take_vouched(&ticket), Ok((actor, None)));
        assert_eq!(
            runtime.take_vouched(&ticket),
            Err(Unvouched::Nobody),
            "and the replay spent it exactly once"
        );
    }

    /// A take reads every family that named the key, and a family it cannot read is not a
    /// family that vouched for nobody: answering the readable one would hand this call one
    /// session's standing while the other's is unread.
    #[tokio::test]
    async fn a_family_whose_records_do_not_read_refuses_the_take_it_could_have_answered() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = std::sync::Arc::new(
            Runtime::open(versioned_policy("first"), dir.path().join("appa.db"), None).expect("the deployment opens"),
        );
        let ticket = crate::yell::YellArgs {
            message: "the hook blocked".to_string(),
            with_trajectory: true,
        }
        .ticket();
        for id in ["cc:first", "cc:second"] {
            let root = TrajectoryId(id.to_string());
            crate::hooks::handle(
                &runtime,
                appa_runtime_api::HookEvent::SessionStart { root: root.clone() },
            )
            .await;
            runtime.vouch(&ticket, &Actor { root, child: None }, None);
        }

        // A host row that names no key, so the query that finds the families still hands
        // this one over, and the damage is met where its whole stream is read.
        let damaged = TrajectoryId("cc:second".to_string());
        let actor = Actor {
            root: damaged.clone(),
            child: None,
        };
        crate::hooks::handle(
            &runtime,
            appa_runtime_api::HookEvent::Prompt {
                actor,
                text: "go on".to_string(),
            },
        )
        .await;
        let engine_root = crate::engine::engine_id(&damaged);
        let at = runtime
            .store()
            .log(&engine_root)
            .expect("the family's host records read")
            .host_records()
            .iter()
            .find(|record| matches!(&record.observation, HostObservation::PromptSeen { .. }))
            .expect("the prompt mark landed")
            .seq;
        runtime
            .store()
            .corrupt_batch(&engine_root, at, br#"{"kind":"prompt_seen""#);
        assert!(
            !runtime
                .store()
                .roots_mentioning(&ticket.wire())
                .expect("the families that named the key still read")
                .is_empty(),
            "the damaged row names no key, so this family is still a candidate"
        );

        assert_eq!(
            runtime.take_vouched(&ticket),
            Err(Unvouched::Nobody),
            "an unreadable family is not an absent one"
        );
    }

    /// The vouch is in the log, so a runtime that never saw the hook still finds it: nothing
    /// routes a take but the records.
    #[tokio::test]
    async fn a_vouch_outlives_the_runtime_that_recorded_it() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let db = dir.path().join("appa.db");
        let root = TrajectoryId("cc:reopened".to_string());
        let actor = Actor {
            root: root.clone(),
            child: None,
        };
        let ticket = crate::yell::YellArgs {
            message: "the hook blocked".to_string(),
            with_trajectory: true,
        }
        .ticket();
        {
            let runtime = std::sync::Arc::new(
                Runtime::open(versioned_policy("first"), db.clone(), None).expect("the deployment opens"),
            );
            crate::hooks::handle(
                &runtime,
                appa_runtime_api::HookEvent::SessionStart { root: root.clone() },
            )
            .await;
            runtime.vouch(&ticket, &actor, None);
        }

        let reopened =
            Runtime::open(versioned_policy("first"), db, None).expect("the deployment reopens over the same log");
        assert_eq!(
            reopened.take_vouched(&ticket),
            Ok((actor, None)),
            "a runtime that never saw the hook reads the standing the log holds"
        );
        assert_eq!(
            reopened.take_vouched(&ticket),
            Err(Unvouched::Nobody),
            "and taking it is still one-shot"
        );
    }

    #[cfg(feature = "daemon")]
    #[test]
    fn daemon_sqlite_receipts_outlive_the_runtime_that_recorded_them() {
        use appa_eventlog::{
            OfferOwnerKey, OfferOwnerRecord, OperationClaim, OperationKey, OperationRequest, ReceiptBinding,
            ReceiptScope,
        };

        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let db = dir.path().join("appa.db");
        let scope = ReceiptScope {
            organization_id: "daemon".to_owned(),
            caller_id: Some("caller".to_owned()),
            session_id: "session".to_owned(),
            binding: ReceiptBinding::Caller,
        };
        let owner = OfferOwnerRecord {
            scope: scope.clone(),
            offer_id: "0123456789abcdef".to_owned(),
            root: "cc:daemon-receipts".to_owned(),
            parent_id: None,
            arguments: None,
            tool: None,
            spelling: None,
        };
        let request = OperationRequest {
            key: OperationKey {
                scope: scope.clone(),
                operation_id: "remedy-1".to_owned(),
            },
            root: owner.root.clone(),
            input: serde_json::json!({"offer_id": owner.offer_id}),
            context: None,
        };
        {
            let runtime = Runtime::open(versioned_policy("first"), db.clone(), None).expect("the deployment opens");
            runtime
                .store()
                .store_offer_owner(owner.clone())
                .expect("the owner stores");
            assert!(matches!(
                runtime.store().claim_operation(request.clone()),
                Ok(OperationClaim::Claimed)
            ));
            runtime
                .store()
                .complete_operation(request.key.clone(), serde_json::json!({"decision":"mcp_result"}))
                .expect("the operation completes");
        }
        let reopened = Runtime::open(versioned_policy("first"), db, None).expect("the deployment reopens");
        assert_eq!(
            reopened
                .store()
                .offer_owner(OfferOwnerKey {
                    organization_id: scope.organization_id,
                    offer_id: owner.offer_id.clone(),
                })
                .expect("the owner reads"),
            Some(owner),
            "the daemon's SQLite receipts survive a process restart"
        );
        assert!(matches!(
            reopened.store().claim_operation(request),
            Ok(OperationClaim::Complete { .. })
        ));
    }

    #[cfg(feature = "daemon")]
    #[tokio::test]
    async fn a_management_vouch_is_exact_one_shot_and_turn_bounded() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = std::sync::Arc::new(
            Runtime::open(versioned_policy("first"), dir.path().join("appa.db"), None).expect("the deployment opens"),
        );
        let root = TrajectoryId("management-vouch".to_string());
        crate::hooks::handle(
            &runtime,
            appa_runtime_api::HookEvent::SessionStart { root: root.clone() },
        )
        .await;
        let actor = Actor { root, child: None };
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
        crate::hooks::handle(&runtime, appa_runtime_api::HookEvent::TurnEnd { actor: actor.clone() }).await;
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
#[cfg(all(test, feature = "daemon"))]
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
                call_id: None,
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
