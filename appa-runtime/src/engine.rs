//! The engine boundary: the one module that speaks to `appa-engine`.
//!
//! [`RuntimeEngine::rebuild_view`] turns the store's opaque batch rows back into
//! typed facts and refuses the log before it is trusted: the log opens under
//! this root's own policy, and
//! [`appa_engine::engine::Engine::view`] then admits every record through the
//! engine's one sequential transition validator, so a log whose records could
//! not have followed one another never reaches a decision. Because
//! this runtime stores no view and rebuilds from the durable log on every
//! event, that decode step is the store-reopen trust
//! gate. [`RuntimeEngine::handle`] then translates one runtime event onto
//! [`appa_engine::engine::Engine::handle`], the engine's one mutation
//! boundary, and translates its typed follow-up back into a delivery. It
//! returns one decision: an optional fact batch to append under
//! compare-and-swap plus the follow-up the session delivers. The
//! vocabulary here is delivery adaptation, not policy: every
//! admissibility judgment is the engine's.
//!
//! Beside `handle` the boundary makes projection reads — which branch has ended,
//! which dispatches it has open, what its label renders as. They gate nothing
//! and append nothing. One of them is not a read at all:
//! [`RuntimeEngine::opens_a_second_dispatch`] is this deployment's own host
//! policy, which the engine deliberately does not enforce.
//!
//! External evidence is typed before it reaches an engine input:
//! an authority verdict, a sanitizer derivation, an annotation answer, or a
//! membership answer.
//! A missing or malformed answer stays runtime-side and fails closed
//! — no no-answer variant ever enters an engine operation.
//!
//! Offers are engine-derived remedies and engine-owned facts: the runtime
//! holds no offer state of its own, so a restart loses none of them. The
//! trajectory that may execute one comes from the harness channel carrying
//! the control act, never from the id; the quoted id is resolved
//! against the offers that trajectory's own log opened. Execution re-derives
//! the plan from the live views and matches it by value, so an offer whose
//! basis has moved declines instead of executing.

use appa_engine::audience::{AudienceEvidence, IdentityImplementation, IdentityMapping, MemberClaims, SelectorSpec};
use appa_engine::contract::{
    AudienceRequirement, Delta, HistoryRequirement, LabelRequirements, PinnedAnnotation, ProducedAnnotation,
    RecipientSpec, Requires, ToolDeclaration,
};
pub(crate) use appa_engine::engine::ForkStatus;
use appa_engine::engine::{Engine, EngineError};
use appa_engine::execute::{AuthorityEvidence, AuthorityReview};
use appa_engine::fact::{
    BoundaryKind, CloseOutcome, EffectKind, EffectSet, Fact, ReturnDerivation, ReturnPolicy, ReturnSanitizer,
};
use appa_engine::label::{Audience, Clause, DeclaredAudience, Label, ReaderId, SymbolicAtom, Trust};
use appa_engine::names::MarkName;
use appa_engine::plan::{
    ExecutableRemedyPlan, FloorStanding, ForkAdvice, PlanId, PlannedBlock, RemedyPlan, RequiredRuling,
};
use appa_engine::profile::PolicyFileKey as EnginePolicyFileKey;
use appa_engine::projection::Views;
use appa_engine::registry::TrustChain;
use appa_engine::shape::{ReturnMismatch, ReturnShape};
use appa_engine::transition::Blocked as CoreBlocked;
/// The engine's own validated view is the runtime's too: the runtime adds no
/// wrapper, because everything it would carry beside the log is already on
/// the event or passed with the read.
pub(crate) use appa_engine::transition::EngineView;
use appa_engine::transition::{
    ChildFollowUp, ChildReport, ChildSubmission, EngineEvent as CoreEvent, Evidence, EvidenceRequest, FollowUp,
    ForkBinding, OfferConsult, OfferExecution, OfferFollowUp, OfferOutcome, OutcomeBody as CoreOutcomeBody,
    OutcomeFollowUp, ProposalBatch, ProposalBatchId, ProposedCall as CoreProposedCall, Released, SpawnMark,
    ToolOutcome as CoreToolOutcome, ToolReport, TransitionError, TransitionRefusal, ValidatedFactBatch,
};
use appa_engine::value::{
    DispatchId as EngineDispatchId, ForkId, OfferId as EngineOfferId, OfferNonce as EngineOfferNonce, RawResultDigest,
    ResolvedCall, ToolName, TrajectoryId as EngineTrajectoryId, ValueBody,
};
use appa_eventlog::Log;
use std::collections::{BTreeMap, BTreeSet};

use crate::api::OutcomeBody;
pub(crate) use crate::api::{OfferId, ProposedCall, SpawnBinding, ToolOutcome, TrajectoryId};
use crate::consult::{
    AnnotationAnswer, AnnotationDeclaration, AuthorityAnswer, AuthorityArtifact, AuthorityDeclaration, HistoryEntry,
    Requirement, Ruling, SanitizerArtifact, SanitizerDeclaration, SanitizerPoint, WireAudience,
};
use appa_runtime_api::{OfferedRemedy, OfferedReturn};

/// One fresh 256-bit random number per act that can surface offers; the
/// runtime mixes it into every `OfferId` it mints.
#[derive(Debug, Clone, Copy)]
pub struct OfferNonce(pub [u8; 32]);

/// A call the engine released: the exact canonical bytes the harness must
/// execute, delivered verbatim.
#[derive(Debug, Clone, PartialEq)]
pub struct ReleasedCall {
    pub tool: String,
    pub bytes: Vec<u8>,
    /// The spawn binding, when this release prepared a fork: the
    /// runtime returns it to the harness, which echoes it on the child's
    /// start so the child names the exact fork that opened it. `None` for
    /// every ordinary released call.
    pub fork: Option<SpawnBinding>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Feedback {
    pub text: String,
    pub offers: Vec<OfferedRemedy>,
    /// Every authority an offered plan would consult, with the review a person would read.
    /// The engine lists them all; the session keeps the ones whose backend is a person.
    pub review: Vec<PendingReview>,
}

/// One authority an offered plan consults, and the review as a person reads it — the
/// same text the elicitation channel shows, rendered from the consult artifact alone.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingReview {
    pub offer: OfferId,
    pub authority: String,
    pub text: String,
}

/// One external consult the session must resolve before the same semantic
/// event replays with the answer attached.
#[derive(Debug, Clone, PartialEq)]
pub enum ExternalRequest {
    Authority {
        authority: String,
        declaration: AuthorityDeclaration,
        artifact: AuthorityArtifact,
        review: AuthorityReview,
    },
    Sanitizer {
        sanitizer: String,
        source: RawResultDigest,
        declaration: SanitizerDeclaration,
        artifact: SanitizerArtifact,
    },
    /// Produce the complete annotation for one proposed call of an Annotated tool. The
    /// request is keyed on the call's canonical digest: any rewrite is a new question.
    Annotation {
        annotator: String,
        call: appa_engine::value::CanonicalDigest,
        declaration: AnnotationDeclaration,
        /// The consult artifact: the complete call, or one value per declared input.
        args: serde_json::Value,
    },
    /// One audience source read: the members of one selector's collection at the
    /// registered source of `provider`.
    AudienceSource {
        provider: String,
        selector: String,
        /// The selector templates the policy registers for the provider: the consult's
        /// declaration.
        templates: Vec<String>,
    },
    /// One member lookup at its provider's source: the claims for one qualified reader.
    MemberLookup {
        provider: String,
        member: String,
        templates: Vec<String>,
    },
    /// One custom identity canonicalization: the principal for one member's claims. Only a
    /// policy-selected custom implementation is consulted; the shipped `verified-email`
    /// normalization is deterministic and recomputed by the engine.
    Identity {
        implementation: String,
        claims: MemberClaims,
    },
}

/// A typed external answer. `None`/`Abstain` mean the external gave
/// no usable answer; that stays runtime-side and grants nothing.
#[derive(Debug, Clone, PartialEq)]
pub enum ExternalEvidence {
    Authority {
        authority: String,
        verdict: AuthorityVerdict,
        review: AuthorityReview,
    },
    Sanitizer {
        sanitizer: String,
        source: RawResultDigest,
        derived: Option<String>,
    },
    /// The annotation produced for one exact call, by canonical digest: evidence for a
    /// rewritten call never matches, so every rewrite is annotated afresh.
    Annotation {
        annotator: String,
        call: appa_engine::value::CanonicalDigest,
        answer: AnnotationAnswer,
    },
    AudienceSource {
        provider: String,
        selector: String,
        members: Option<Vec<MemberClaims>>,
    },
    MemberLookup {
        provider: String,
        member: String,
        /// `None`: the consult produced no answer. `Some(None)`: the provider definitively
        /// does not know the member, who keeps its qualified identity.
        claims: Option<Option<MemberClaims>>,
    },
    Identity {
        implementation: String,
        id: String,
        principal: Option<ReaderId>,
    },
}

/// An authority's wire verdict: `{"ruling": "approve"|"deny"}`; anything
/// else abstains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorityVerdict {
    Approve,
    Deny,
    Abstain,
}

impl AuthorityVerdict {
    /// Parse one consult answer. Malformed answers abstain — a wire mistake
    /// must never approve or deny. The reason is diagnostic only: logged, never kept.
    pub fn from_wire(answer: &serde_json::Value) -> AuthorityVerdict {
        match AuthorityAnswer::from_wire(answer) {
            Some(AuthorityAnswer { ruling, reason }) => {
                if let Some(reason) = reason {
                    tracing::debug!(reason, "the authority gave its reason");
                }
                match ruling {
                    Ruling::Approve => AuthorityVerdict::Approve,
                    Ruling::Deny => AuthorityVerdict::Deny,
                }
            }
            None => AuthorityVerdict::Abstain,
        }
    }
}

/// One session event in the engine boundary's vocabulary. The session
/// constructs it; [`RuntimeEngine::handle`] translates it onto the
/// engine's own `handle` boundary and back.
#[derive(Debug, Clone)]
pub enum EngineEvent {
    ModelResponse {
        call: ProposedCall,
        evidence: Vec<ExternalEvidence>,
        entropy: OfferNonce,
        spawn: bool,
    },
    ToolOutcome {
        dispatch: EngineDispatchId,
        outcome: ToolOutcome,
        evidence: Vec<ExternalEvidence>,
        entropy: OfferNonce,
    },
    ExecuteOffer {
        trajectory: TrajectoryId,
        offer: OfferId,
        arguments: RemedyArguments,
        evidence: Vec<ExternalEvidence>,
        entropy: OfferNonce,
    },
    BindFork {
        fork: ForkId,
        child: TrajectoryId,
    },
    ChildReturn {
        child: TrajectoryId,
        value: Option<String>,
        evidence: Vec<ExternalEvidence>,
    },
}

/// What `execute_remedy_plan` carries beside the offer id: the floor the child's return may
/// narrow the declaring trajectory to and, for an attesting plan, the schema the return must
/// match. Read only when the offered plan declares a return; every other plan ignores them.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RemedyArguments {
    pub label: Option<LabelSpelling>,
    pub return_schema: Option<serde_json::Value>,
}

/// A label as the policy dialect spells a `delta`: a trust rank name and an audience list. An
/// omitted dimension keeps the declaring trajectory's current value.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LabelSpelling {
    pub trust: Option<String>,
    pub audience: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Next {
    Done,
    ModelResponse {
        invocations: Vec<ReleasedCall>,
        feedback: Vec<Feedback>,
    },
    PresentToModel(Presentation),
    InvokeTool(ReleasedCall),
    Approved {
        tool: String,
        bytes: Vec<u8>,
    },
    ResolveExternal(Vec<ExternalRequest>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Presentation {
    KeepOutput,
    ReplaceOutput {
        placeholder: String,
    },
    Value {
        value: String,
    },
    /// A child's return the fork's sanitizer derived, crossing when the child returns
    /// exactly it.
    Staged {
        value: String,
    },
    Declined {
        feedback: String,
    },
    NoAnswer {
        feedback: String,
    },
    NoValue,
    Blocked {
        feedback: String,
        offers: Vec<OfferId>,
    },
}

/// One engine interaction's outcome, as the session drives it: the records to
/// append, and the follow-up to deliver.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineDecision {
    pub append: Option<Vec<Fact>>,
    pub then: Next,
}

impl EngineDecision {
    fn deliver(then: Next) -> EngineDecision {
        EngineDecision { append: None, then }
    }
}

/// Why the engine boundary refused an event outright. Model-visible outcomes (a deny, a
/// declined offer) are decisions, not refusals; a refusal means the event
/// cannot be processed as it stands.
#[derive(Debug, thiserror::Error)]
pub enum EngineRefusal {
    #[error("the persisted log is refused: {detail}")]
    UntrustedLog { detail: String },
    #[error("the opening does not match the deciding policy: {detail}")]
    OpeningMismatch { detail: String },
    #[error("engine invariant breach: {detail}")]
    Invariant { detail: String },
    #[error("the trajectory has ended")]
    Ended,
    #[error("the dispatch is no longer open")]
    DispatchClosed,
    #[error("the offer is not one this family carries")]
    UnknownOffer,
    #[error("the fork and the child are already bound elsewhere")]
    Unbindable,
    #[error("tool {tool} is not declared in this policy and no wildcard covers it; the call is refused before it runs")]
    UndeclaredTool { tool: String },
    /// The offer's execution came without what its plan needs, or with an argument the
    /// policy cannot read. Nothing is appended; the offer stands.
    #[error("{detail}")]
    Arguments { detail: String },
}

/// One trajectory's current label rendered for a display surface — the
/// statusline. Chain names and reader ids as plain strings: no label type
/// leaves the engine boundary. A projection of the log;
/// it gates nothing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TrajectoryStatus {
    pub trajectory: String,
    pub trust: String,
    pub audience: String,
}

/// One label rendered for a display surface: each dimension as chain
/// names and reader ids.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AuditLabel {
    pub trust: String,
    pub audience: String,
}

/// One decision the family log recorded, in log order (the
/// audit read). Like [`TrajectoryStatus`] it is a projection: it gates
/// nothing, appends nothing, and expires no offer. Primitives only,
/// so no engine type leaves the boundary.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AuditEntry {
    pub trajectory: String,
    pub event: AuditEvent,
}

/// What one entry records. The facts the harness owns rather than the engine
/// — the model's own rounds, turn punctuation, batch identity — have no entry:
/// this is the decision log, not the transcript.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum AuditEvent {
    Forked {
        parent: String,
        seed: AuditLabel,
    },
    Released {
        tool: String,
        label: AuditLabel,
        effects: Vec<String>,
    },
    EffectsCommitted {
        effects: Vec<String>,
    },
    Closed {
        outcome: DispatchOutcome,
    },
    Admitted {
        label: AuditLabel,
    },
    Ruled {
        authority: String,
    },
    Denied {
        authority: String,
    },
    Narrowed {
        from: AuditLabel,
        to: AuditLabel,
    },
    SanitizerBound {
        sanitizer: String,
    },
    Sanitized {
        sanitizer: String,
    },
    ChildReturn {
        sanitizer: Option<String>,
        label: AuditLabel,
    },
    Merged,
    VoidReturn,
    /// The parent addressed its bound child again: the parent's label as it stood flowed into
    /// the child.
    Resumed {
        seed: AuditLabel,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum DispatchOutcome {
    Ran { effects: Vec<String> },
    Failed,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    pub policy_file_key: String,
    pub policy_identity: String,
}

/// Where a trajectory stands in its family's log, as [`RuntimeEngine::liveness`]
/// reads it: never opened, still taking events, or ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    Unopened,
    Live,
    Ended,
}

/// One dispatch a trajectory has open, as the runtime matches outcomes against
/// it. The identity is the engine's own: it is read from the
/// log and handed straight back to the engine, crossing no adapter and no
/// second persistence on the way.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenDispatch {
    pub id: EngineDispatchId,
    pub tool: String,
    pub bytes: Vec<u8>,
}

/// The engine deciding one family's events: the engine of the
/// deployment now serving when the root's stored policy file is that
/// deployment's, or the engine compiled from the root's own stored
/// bytes. Resolved per event and borrowed for it; the
/// process keeps at most one engine per distinct policy file.
pub enum PolicyEngine<'a> {
    Resident(&'a RuntimeEngine),
    Retired(std::sync::Arc<RuntimeEngine>),
}

impl PolicyEngine<'_> {
    pub(crate) fn engine(&self) -> &RuntimeEngine {
        match self {
            PolicyEngine::Resident(engine) => engine,
            PolicyEngine::Retired(engine) => engine,
        }
    }

    /// The policy identity of the deciding engine, lowercase hex — what
    /// a root's opening record must name.
    pub fn identity_hex(&self) -> String {
        hex(self.engine().engine.identity().bytes())
    }
}

/// What a root's opening record binds it to: the policy file it
/// opened under and the identity that file must still compile to. `None`
/// when the log does not open with its opening record, which the trust
/// gate refuses in full a moment later. Read before the deciding engine
/// is known, because it is what names it.
pub(crate) fn opened_under(log: &Log) -> Option<Opened> {
    match log.facts().first() {
        Some(Fact::TrajectoryOpened {
            policy_digest,
            policy_file_key,
            ..
        }) => Some(Opened {
            policy_file_key: policy_file_key.as_str().to_string(),
            policy_identity: hex(policy_digest.bytes()),
        }),
        _ => None,
    }
}

/// The one engine boundary the session drives: the immutable
/// registry-backed decision core, plus the reads of one rebuilt view
/// the runtime needs to route an event. It owns every judgment and
/// every fact; the runtime holds no engine state, and offers are the
/// engine's own durable facts. A family decides under the policy its
/// root opened with, so the deciding engine arrives per event as a
/// [`PolicyEngine`], which a configuration reload does not change.
pub struct RuntimeEngine {
    engine: Engine,
    /// The trusted hint, builtin, and consult input mapping per registered `[[annotator]]`,
    /// policy-compiled and runtime-owned. The engine sees only the enforced mandate.
    annotators: BTreeMap<String, appa_policy::AnnotatorBinding>,
}

impl RuntimeEngine {
    /// Whether the policy writes a contract for this tool's exact name. The wildcard does not
    /// count: it covers a name at a proposal, and a spawn under `SpawnCoverage::Declared` needs
    /// the name written.
    pub(crate) fn names_tool(&self, tool: &str) -> bool {
        let name = appa_engine::value::ToolName::new(tool);
        self.engine.registry().classify(&name) == Some(appa_engine::registry::ToolKind::Declared)
    }

    /// The one constructor: both halves — the decision core and the consult input
    /// mapping — come from the same compiled policy, so an engine can never carry
    /// another policy's mapping.
    pub fn from_policy(policy: &appa_policy::Config) -> RuntimeEngine {
        RuntimeEngine {
            engine: policy.engine().clone(),
            annotators: policy
                .annotators()
                .map(|(name, binding)| (name.as_str().to_string(), binding.clone()))
                .collect(),
        }
    }

    /// The opening of a fresh root: this engine's opening batch bound to the
    /// exact bytes of the policy file the root opens under. It takes
    /// the bytes, not a key, because the key on the opening record is the
    /// engine's own type and is derived here — at the one boundary that names
    /// the engine crate.
    pub fn root_opening(&self, trajectory: &TrajectoryId, policy_file: &[u8]) -> Vec<Fact> {
        self.engine
            .open_trajectory(&engine_id(trajectory), EnginePolicyFileKey::of(policy_file))
            .expect("the engine's own opening batch validates against the empty log")
            .into_unsealed()
    }

    /// Refuse one root's log before it is trusted, including the
    /// opening gate: the log's first record must be this root's opening under
    /// exactly the deciding engine's policy. The root is the log's own, so a
    /// view cannot be built against a log it does not describe.
    pub(crate) fn rebuild_view(&self, log: &Log) -> Result<EngineView, EngineRefusal> {
        let root = TrajectoryId(log.root().as_str().to_string());
        self.validated(log.facts().to_vec(), &root, log.basis())
    }

    /// Where this trajectory stands in the log:
    /// never opened — the root by its opening record, a child by its fork
    /// binding — still taking events, or ended. The one replay-derived
    /// answer; the runtime keeps no flag of its own.
    pub(crate) fn liveness(&self, view: &EngineView, trajectory: &TrajectoryId) -> Liveness {
        let id = engine_id(trajectory);
        match view.views(&id) {
            None => Liveness::Unopened,
            Some(views) if views.has_ended(&id) => Liveness::Ended,
            Some(_) => Liveness::Live,
        }
    }

    pub(crate) fn parent_of(&self, view: &EngineView, child: &TrajectoryId) -> Option<TrajectoryId> {
        let child = engine_id(child);
        view.views(&child)?
            .parent_of(&child)
            .map(|parent| TrajectoryId(parent.as_str().to_string()))
    }

    /// Would applying this batch leave the trajectory with more than one
    /// dispatch open?
    pub(crate) fn opens_a_second_dispatch(&self, view: &EngineView, trajectory: &TrajectoryId, facts: &[Fact]) -> bool {
        let owner = engine_id(trajectory);
        let mut open: std::collections::BTreeSet<_> = view
            .views(&owner)
            .expect("the drive refuses an unopened trajectory before any dispatch bookkeeping")
            .open_dispatches()
            .map(|(dispatch, _)| dispatch.clone())
            .collect();
        for fact in facts {
            match fact {
                Fact::DispatchOpened { dispatch, .. } if dispatch.trajectory() == &owner => {
                    open.insert(dispatch.clone());
                }
                Fact::DispatchClosed { dispatch, .. } => {
                    open.remove(dispatch);
                }
                _ => {}
            }
        }
        open.len() > 1
    }

    /// Which trajectory pursues this offer.
    /// The compiled policy this engine decides under.
    pub(crate) fn registry(&self) -> &appa_engine::registry::Registry {
        self.engine.registry()
    }

    /// Whom taking this offer involves, read without taking it: nobody (the plain narrowing
    /// acceptance), an authority, or a sanitizer. `None` for an offer that no longer stands.
    /// `appa replay` reads it to take the offer a trace expects, the way the model would.
    pub(crate) fn offer_kind(
        &self,
        view: &EngineView,
        trajectory: &TrajectoryId,
        offer: &OfferId,
    ) -> Option<crate::api::OfferKind> {
        let engine_offer = parse_offer(offer)?;
        match self
            .engine
            .offer_consults(view, &engine_id(trajectory), &engine_offer)
            .ok()?
        {
            OfferConsult::Accept => Some(crate::api::OfferKind::Accept),
            OfferConsult::Authorities { required, .. } => {
                let mut names: Vec<String> = required
                    .iter()
                    .map(|requirement| requirement.authority.as_str().to_string())
                    .collect();
                names.sort();
                names.dedup();
                Some(crate::api::OfferKind::Authority { names })
            }
            OfferConsult::Rewrite { sanitizer, .. } | OfferConsult::Sanitizer { sanitizer, .. } => {
                Some(crate::api::OfferKind::Sanitizer {
                    name: sanitizer.as_str().to_string(),
                })
            }
            OfferConsult::Stale | OfferConsult::Replay(_) => None,
        }
    }

    pub(crate) fn offer_pursuer(&self, view: &EngineView, offer: &OfferId) -> Option<TrajectoryId> {
        let engine_offer = parse_offer(offer)?;
        let surfaced = view.offer_trajectory(&engine_offer)?.clone();
        let views = view.views(&surfaced)?;
        let pursuer = if views.has_ended(&surfaced) {
            views.parent_of(&surfaced).cloned().unwrap_or(surfaced)
        } else {
            surfaced
        };
        Some(TrajectoryId(pursuer.as_str().to_string()))
    }

    /// The dispatches this trajectory has open, with the exact tool and
    /// canonical bytes each released. The payload is
    /// persisted once, on the opening record, so this is where a live call is
    /// read back — the runtime keeps no row of its own.
    pub(crate) fn open_dispatches(&self, view: &EngineView, trajectory: &TrajectoryId) -> Vec<OpenDispatch> {
        let owner = engine_id(trajectory);
        let Some(views) = view.views(&owner) else {
            return Vec::new();
        };
        views
            .open_dispatches()
            .map(|(dispatch, call)| OpenDispatch {
                id: dispatch.clone(),
                tool: call.tool().as_str().to_string(),
                bytes: call.canonical_arguments().canonical_bytes().to_vec(),
            })
            .collect()
    }

    /// The substituted call this trajectory has standing, if it has one:
    /// the one open dispatch no proposal batch released. An
    /// offer execution releases a replaced call on its own,
    /// so that dispatch names a call the harness never proposed — which
    /// is exactly what tells the runtime to hand it out rather than to
    /// refuse the next proposal as a second call in flight.
    pub(crate) fn substituted_release(&self, view: &EngineView, trajectory: &TrajectoryId) -> Option<OpenDispatch> {
        let owner = engine_id(trajectory);
        let views = view.views(&owner)?;
        views
            .open_dispatches()
            .find(|(dispatch, _)| !views.released_by_proposal(dispatch))
            .map(|(dispatch, call)| OpenDispatch {
                id: dispatch.clone(),
                tool: call.tool().as_str().to_string(),
                bytes: call.canonical_arguments().canonical_bytes().to_vec(),
            })
    }

    /// Where one fork stands in the rebuilt view. The runtime uses it
    /// to find the family's forks still open for binding, and the child
    /// a spawn's fork was bound to when that spawn's result arrives.
    pub(crate) fn fork_status(&self, view: &EngineView, fork: &ForkId) -> ForkStatus {
        self.engine.fork_status(view, fork)
    }

    /// The family's forks in flight: prepared, bound to no child, their
    /// spawn dispatch still open. The runtime binds a child start that
    /// names no spawn to the one fork here.
    pub(crate) fn forks_in_flight(&self, view: &EngineView) -> Vec<ForkId> {
        self.engine.forks_in_flight(view)
    }

    /// The fork one child was bound to, or `None` for a trajectory
    /// the family never forked — for a child start the harness
    /// delivers again: it names the fork it already bound.
    pub(crate) fn fork_of(&self, view: &EngineView, child: &TrajectoryId) -> Option<ForkId> {
        self.engine.fork_of(view, &engine_id(child))
    }

    fn validated(&self, facts: Vec<Fact>, family: &TrajectoryId, revision: u64) -> Result<EngineView, EngineRefusal> {
        self.engine
            .view(&engine_id(family), facts, revision)
            .map_err(|error| match error {
                TransitionRefusal::Unopened | TransitionRefusal::Opening(_) => EngineRefusal::OpeningMismatch {
                    detail: error.to_string(),
                },
                error => EngineRefusal::UntrustedLog {
                    detail: error.to_string(),
                },
            })
    }

    /// The canonical bytes of one proposed call, for the byte-exact dispatch
    /// matching of provider-run tools. `None` when the call cannot canonicalize — an
    /// unknown tool or schema-invalid arguments never match a dispatched
    /// call, whose bytes the engine validated.
    pub(crate) fn canonical_bytes(&self, call: &ProposedCall) -> Option<Vec<u8>> {
        let resolved = self
            .engine
            .resolve_call(ToolName::new(call.tool.clone()), call.arguments.get().as_bytes())
            .ok()?;
        Some(resolved.canonical_arguments().canonical_bytes().to_vec())
    }

    /// Render one trajectory's current label from the rebuilt view, for the
    /// statusline. A projection read: no engine event, no fact, nothing
    /// gated.
    pub(crate) fn trajectory_status(&self, view: &EngineView, trajectory: &TrajectoryId) -> Option<TrajectoryStatus> {
        let current = view.views(&engine_id(trajectory))?.current_label();
        let label = self.render_label(&current)?;
        Some(TrajectoryStatus {
            trajectory: terminal_safe(&trajectory.0),
            trust: label.trust,
            audience: label.audience,
        })
    }

    /// The label by name: each dimension as chain names and reader ids.
    fn render_label(&self, label: &Label) -> Option<AuditLabel> {
        let chain = self.engine.registry().trust_chain();
        let trust = if label.trust == Trust::new(u8::MAX) {
            chain
                .name_of(Trust::new((chain.len() - 1) as u8))
                .expect("a validated chain names its top rank")
                .to_string()
        } else {
            match chain.name_of(label.trust) {
                Some(name) => name.to_string(),
                None => {
                    tracing::warn!(
                        rank = label.trust.rank(),
                        "render refused: the trust bound has no chain name"
                    );
                    return None;
                }
            }
        };
        Some(AuditLabel {
            trust: terminal_safe(&trust),
            audience: terminal_safe(&audience_wire(&label.audience)),
        })
    }

    /// Render the family's recorded decisions from its persisted log. Like
    /// [`RuntimeEngine::trajectory_status`], a projection read.
    pub(crate) fn audit(&self, log: &Log) -> Result<Option<Vec<AuditEntry>>, EngineRefusal> {
        let facts = log.facts().to_vec();
        let root = TrajectoryId(log.root().as_str().to_string());
        // The validator takes the records; this read keeps its own copy of
        // them, which is why the audit — and only the audit — clones a log.
        self.validated(facts.clone(), &root, log.basis())?;
        let mut prepared: std::collections::HashMap<ForkId, (String, Label)> = std::collections::HashMap::new();
        for fact in &facts {
            if let Fact::ForkPrepared {
                fork,
                snapshot,
                trajectory,
                ..
            } = fact
            {
                prepared.insert(
                    fork.clone(),
                    (terminal_safe(trajectory.as_str()), snapshot.seed().clone()),
                );
            }
        }
        let mut entries = Vec::new();
        for fact in &facts {
            let event = match fact {
                Fact::ForkOpened { fork, .. } => match prepared.get(fork) {
                    Some((parent, seed)) => match self.render_label(seed) {
                        Some(seed) => AuditEvent::Forked {
                            parent: parent.clone(),
                            seed,
                        },
                        // A seed bound this deployment cannot name.
                        None => return Ok(None),
                    },
                    // A child opened with no recorded preparation in this read.
                    None => continue,
                },
                _ => match self.audit_event(fact) {
                    Some(Some(event)) => event,
                    // A record the audit does not show.
                    Some(None) => continue,
                    // A bound this deployment cannot name.
                    None => return Ok(None),
                },
            };
            entries.push(AuditEntry {
                trajectory: terminal_safe(fact.trajectory().as_str()),
                event,
            });
        }
        Ok(Some(entries))
    }

    fn audit_event(&self, fact: &Fact) -> Option<Option<AuditEvent>> {
        let event = match fact {
            Fact::DispatchOpened {
                tool,
                proposed_label,
                proposed_effects,
                ..
            } => AuditEvent::Released {
                tool: terminal_safe(tool.as_str()),
                label: self.render_label(proposed_label)?,
                effects: effect_names(proposed_effects),
            },
            Fact::DispatchSucceeded { effects, .. } => AuditEvent::EffectsCommitted {
                effects: effect_names(effects),
            },
            Fact::DispatchClosed { outcome, .. } => AuditEvent::Closed {
                outcome: match outcome {
                    CloseOutcome::Success { effects } => DispatchOutcome::Ran {
                        effects: effect_names(effects),
                    },
                    CloseOutcome::Failure => DispatchOutcome::Failed,
                    CloseOutcome::Indeterminate => DispatchOutcome::Unknown,
                },
            },
            Fact::ValueAdmitted { value, .. } => AuditEvent::Admitted {
                label: self.render_label(&value.label)?,
            },
            Fact::Ruling { authority, .. } => AuditEvent::Ruled {
                authority: terminal_safe(authority.as_str()),
            },
            Fact::Denial { authority, .. } => AuditEvent::Denied {
                authority: terminal_safe(authority.as_str()),
            },
            Fact::Acceptance { narrowing, .. } | Fact::CandidateAccepted { narrowing, .. } => AuditEvent::Narrowed {
                from: self.render_label(&narrowing.from)?,
                to: self.render_label(&narrowing.to)?,
            },
            Fact::OutputSanitizerBound { sanitizer, .. } => AuditEvent::SanitizerBound {
                sanitizer: terminal_safe(sanitizer.as_str()),
            },
            Fact::CandidateDerived { sanitizer, .. } => AuditEvent::Sanitized {
                sanitizer: terminal_safe(sanitizer.as_str()),
            },
            Fact::ChildReturn { value, derivation, .. } => AuditEvent::ChildReturn {
                sanitizer: match derivation {
                    ReturnDerivation::Raw => None,
                    ReturnDerivation::Sanitized { sanitizer, .. } => Some(terminal_safe(sanitizer.as_str())),
                },
                label: self.render_label(&value.label)?,
            },
            Fact::Boundary { kind, .. } => match kind {
                BoundaryKind::Merge { .. } => AuditEvent::Merged,
                BoundaryKind::VoidReturn => AuditEvent::VoidReturn,
                BoundaryKind::Resume { seed } => AuditEvent::Resumed {
                    seed: self.render_label(seed)?,
                },
            },
            Fact::TrajectoryOpened { .. } | Fact::ProposalBatchDecided { .. } => return Some(None),
            Fact::OfferOpened { .. }
            | Fact::OfferAccepted { .. }
            | Fact::OfferDenied { .. }
            | Fact::OfferInvalidated { .. }
            | Fact::CallApproved { .. }
            | Fact::CallApprovalConsumed { .. }
            | Fact::BasisAdvanced { .. } => return Some(None),
            Fact::ForkPrepared { .. } | Fact::ForkOpened { .. } => return Some(None),
        };
        Some(Some(event))
    }

    pub(crate) fn handle(
        &self,
        view: &EngineView,
        trajectory: &TrajectoryId,
        event: EngineEvent,
    ) -> Result<EngineDecision, EngineRefusal> {
        match event {
            EngineEvent::ModelResponse {
                call,
                evidence,
                entropy,
                spawn,
            } => self.model_response(view, trajectory, &call, &evidence, &entropy, spawn),
            EngineEvent::ToolOutcome {
                dispatch,
                outcome,
                evidence,
                entropy,
            } => self.tool_outcome(view, &dispatch, &outcome, &evidence, &entropy),
            EngineEvent::ExecuteOffer {
                trajectory: owner,
                offer,
                arguments,
                evidence,
                entropy,
            } => self.execute_offer(view, &owner, &offer, &arguments, &evidence, &entropy),
            EngineEvent::BindFork { fork, child } => self.bind_fork(view, &fork, &child),
            EngineEvent::ChildReturn { child, value, evidence } => self.child_return(view, &child, value, &evidence),
        }
    }

    fn model_response(
        &self,
        view: &EngineView,
        trajectory: &TrajectoryId,
        call: &ProposedCall,
        evidence: &[ExternalEvidence],
        entropy: &OfferNonce,
        spawn: bool,
    ) -> Result<EngineDecision, EngineRefusal> {
        let resolved = match self
            .engine
            .resolve_call(ToolName::new(call.tool.clone()), call.arguments.get().as_bytes())
        {
            Ok(resolved) => resolved,
            // A tool no declaration and no wildcard covers is refused before anything is
            // judged: a typed refusal, never model feedback, and nothing is appended.
            Err(EngineError::UnknownTool(tool)) => return Err(EngineRefusal::UndeclaredTool { tool }),
            Err(error) => return Ok(deny(malformed_feedback(&error))),
        };
        let owner = engine_id(trajectory);
        let Some(views) = view.views(&owner) else {
            return Err(EngineRefusal::Invariant {
                detail: "deciding a proposal for a trajectory the log has not opened".to_string(),
            });
        };
        let CallAnswers { annotation } = match self.answers_for(&views, &resolved, evidence) {
            Ok(answers) => answers,
            Err(Resolution(requests)) => return Ok(EngineDecision::deliver(Next::ResolveExternal(requests))),
        };
        let proposed = CoreProposedCall {
            tool: ToolName::new(call.tool.clone()),
            arguments: call.arguments.get().as_bytes().to_vec(),
            annotation,
        };
        // A deployment that does not control context releases the marked call
        // unmarked, so the batch may be decided twice. The mark is all that
        // differs between the two attempts.
        let judged =
            self.judge_under_audience(evidence, UnresolvedAudience::Denied { tool: &call.tool }, |audience| {
                let decide = |marked: bool| {
                    let batch = ProposalBatch {
                        id: batch_id(entropy),
                        trajectory: engine_id(trajectory),
                        provider_results: Vec::new(),
                        proposals: vec![proposed.clone()],
                        spawn: marked.then(|| SpawnMark::at(0)),
                        offer_nonce: engine_nonce(entropy),
                        evidence: Vec::new(),
                        audience: audience.clone(),
                    };
                    self.engine.handle(view, CoreEvent::Proposals(batch))
                };
                match decide(spawn) {
                    Err(TransitionError::SpawnUncontrolled) if spawn => decide(false),
                    decided => decided,
                }
            })?;
        let decision = match judged {
            AudienceRound::Judged(decision) => decision,
            AudienceRound::Presented(decision) => return Ok(decision),
            AudienceRound::Failed(error) => return Err(proposal_refusal(error)),
        };
        let append = decision.append.map(ValidatedFactBatch::into_unsealed);
        let then = self.deliver_proposals(decision.follow_up, &self.return_bounds(&views))?;
        Ok(EngineDecision { append, then })
    }

    fn deliver_proposals(&self, follow_up: FollowUp, bounds: &ReturnBounds) -> Result<Next, EngineRefusal> {
        match follow_up {
            FollowUp::Proposals {
                released: releases,
                blocked,
                spent,
                settled,
                ..
            } => {
                if let Some(release) = releases.into_iter().next() {
                    return Ok(Next::ModelResponse {
                        invocations: vec![released(&release)],
                        feedback: Vec::new(),
                    });
                }
                if let Some(block) = blocked.into_iter().next() {
                    let feedback = self.block_delivery(&block, bounds);
                    return Ok(Next::ModelResponse {
                        invocations: Vec::new(),
                        feedback: vec![feedback],
                    });
                }
                if !spent.is_empty() || !settled.is_empty() {
                    return Ok(deny_next(
                        "[appa] this call's earlier decision no longer stands; propose it again".to_string(),
                    ));
                }
                Err(EngineRefusal::Invariant {
                    detail: "a non-empty proposal produced no release, block, or repeat answer".to_string(),
                })
            }
            FollowUp::Malformed { error, .. } => Ok(deny_next(malformed_feedback(&error))),
            other => Err(EngineRefusal::Invariant {
                detail: format!("a proposal produced a non-proposal follow-up: {other:?}"),
            }),
        }
    }

    fn block_delivery(&self, block: &CoreBlocked, bounds: &ReturnBounds) -> Feedback {
        let (text, offers, review) = self.rendered_block(block, bounds);
        let offers = offers
            .into_iter()
            .map(|offer| {
                let returns = block
                    .offers
                    .iter()
                    .find(|(id, _)| offer_id(id) == offer)
                    .and_then(|(_, plan)| {
                        block.block.plans.iter().find_map(|candidate| match candidate {
                            RemedyPlan::Executable(executable) if executable.id == *plan => {
                                executable.return_step().map(|sanitizer| match sanitizer {
                                    None => OfferedReturn::AsSpoken,
                                    Some(name) => OfferedReturn::Sanitized {
                                        sanitizer: name.as_str().to_string(),
                                    },
                                })
                            }
                            _ => None,
                        })
                    });
                OfferedRemedy { id: offer.0, returns }
            })
            .collect();
        Feedback { text, offers, review }
    }

    fn rendered_block(&self, block: &CoreBlocked, bounds: &ReturnBounds) -> (String, Vec<OfferId>, Vec<PendingReview>) {
        let offers: Vec<(OfferId, PlanId)> = block
            .offers
            .iter()
            .map(|(offer, plan)| (offer_id(offer), *plan))
            .collect();
        let chain = self.engine.registry().trust_chain();
        let text = block_feedback(&block.block, &offers, chain, bounds);
        let review = self.pending_reviews(block, &offers);
        (text, offers.into_iter().map(|(offer, _)| offer).collect(), review)
    }

    /// The reviews the offered plans would raise: for each plan element that consults an
    /// authority, the consult artifact rendered as the person reads it. Built here, at the
    /// block, so a harness with its own review channel can show it before the execution.
    fn pending_reviews(&self, block: &CoreBlocked, offers: &[(OfferId, PlanId)]) -> Vec<PendingReview> {
        let registry = self.engine.registry();
        let chain = registry.trust_chain();
        let mut reviews = Vec::new();
        for plan in &block.block.plans {
            let RemedyPlan::Executable(plan) = plan else {
                continue;
            };
            let Some((offer, _)) = offers.iter().find(|(_, planned)| *planned == plan.id) else {
                continue;
            };
            for requirement in &plan.required {
                let Some(registered) = registry.authority(&requirement.authority) else {
                    continue;
                };
                let declaration = AuthorityDeclaration::of(registered, chain);
                let artifact = AuthorityArtifact {
                    tool: block.call.tool().as_str().to_string(),
                    arguments: block.call.arguments().clone(),
                    requirements: requirement
                        .covers
                        .iter()
                        .map(|gap| Requirement::of(gap, chain))
                        .collect(),
                };
                reviews.push(PendingReview {
                    offer: offer.clone(),
                    authority: requirement.authority.as_str().to_string(),
                    text: crate::elicit::review_text(requirement.authority.as_str(), &declaration, &artifact),
                });
            }
        }
        reviews
    }

    fn tool_outcome(
        &self,
        view: &EngineView,
        dispatch: &EngineDispatchId,
        outcome: &ToolOutcome,
        evidence: &[ExternalEvidence],
        entropy: &OfferNonce,
    ) -> Result<EngineDecision, EngineRefusal> {
        let judged = self.judge_under_audience(
            evidence,
            UnresolvedAudience::Withheld { subject: "result" },
            |audience| {
                let report = ToolReport {
                    dispatch: dispatch.clone(),
                    outcome: engine_outcome(outcome),
                    evidence: sanitizer_evidence(evidence),
                    offer_nonce: engine_nonce(entropy),
                    audience: audience.clone(),
                };
                self.engine.handle(view, CoreEvent::Outcome(report))
            },
        )?;
        let decision = match judged {
            AudienceRound::Judged(decision) => decision,
            AudienceRound::Presented(decision) => return Ok(decision),
            AudienceRound::Failed(error) => return Err(outcome_refusal(error)),
        };
        let append = decision.append.map(ValidatedFactBatch::into_unsealed);
        let then = match decision.follow_up {
            FollowUp::Outcome(OutcomeFollowUp::Closed { admitted }) => {
                Next::PresentToModel(outcome_presentation(outcome, admitted))
            }
            FollowUp::Outcome(OutcomeFollowUp::Resolve(request)) => self.resolve_or_withhold(
                view,
                dispatch.trajectory(),
                Some(dispatch),
                request,
                evidence,
                "[appa] no registered sanitizer answered; the result is withheld and may be retried",
            )?,
            FollowUp::Outcome(OutcomeFollowUp::Staged(confined)) => Next::PresentToModel(self.stage_delivery(
                "[appa] the cleaned result still narrows this session.",
                &confined.residual,
                &confined.offers,
            )),
            other => {
                return Err(EngineRefusal::Invariant {
                    detail: format!("an outcome produced a non-outcome follow-up: {other:?}"),
                });
            }
        };
        Ok(EngineDecision { append, then })
    }

    fn execute_offer(
        &self,
        view: &EngineView,
        trajectory: &TrajectoryId,
        offer: &OfferId,
        arguments: &RemedyArguments,
        evidence: &[ExternalEvidence],
        entropy: &OfferNonce,
    ) -> Result<EngineDecision, EngineRefusal> {
        let Some(engine_offer) = parse_offer(offer) else {
            return Ok(declined(
                "[appa] this offer no longer stands; re-propose the call".to_string(),
            ));
        };
        let owner = engine_id(trajectory);
        let Some(views) = view.views(&owner) else {
            return Err(EngineRefusal::Invariant {
                detail: "executing an offer for a trajectory the log has not opened".to_string(),
            });
        };
        let return_policy = self
            .declared_return_policy(view, &owner, &views, &engine_offer, arguments)
            .map_err(|detail| EngineRefusal::Arguments { detail })?;
        let outcome = match self
            .engine
            .offer_consults(view, &owner, &engine_offer)
            .map_err(offer_refusal)?
        {
            OfferConsult::Stale => {
                return Ok(declined(
                    "[appa] this offer no longer stands; re-propose the call".to_string(),
                ));
            }
            OfferConsult::Replay(outcome) => outcome,
            OfferConsult::Accept => OfferOutcome::Approved(Vec::new()),
            OfferConsult::Rewrite { sanitizer, call } => {
                let arguments = call.canonical_arguments();
                let source = RawResultDigest::of(arguments.canonical_bytes());
                let derived = match self.sanitizer_derived(
                    evidence,
                    &sanitizer,
                    source,
                    ValueBody::new(arguments.canonical_text()),
                    SanitizerSubject::Input { call: &call },
                ) {
                    Ok(derived) => derived,
                    Err(next) => return Ok(next),
                };
                // A rewrite into an Annotated declaration — its own or another — is
                // annotated afresh about the rewritten arguments before the engine judges
                // it: the digest is the annotation's key. Any group its contract reads is
                // asked through the act's own audience evidence, not gathered here. A
                // derivation the engine cannot mint a call from is the engine's to refuse.
                let annotation = match self
                    .engine
                    .resolve_call(call.tool().clone(), derived.as_str().as_bytes())
                {
                    Ok(rewritten) => match self.engine.registry().declaration(&rewritten) {
                        Some(declaration @ ToolDeclaration::Annotated { .. }) => {
                            match self.annotation_for(&views, declaration, &rewritten, evidence) {
                                Ok(annotation) => Some(annotation),
                                Err(Resolution(requests)) => {
                                    return Ok(EngineDecision::deliver(Next::ResolveExternal(requests)));
                                }
                            }
                        }
                        _ => None,
                    },
                    Err(_) => None,
                };
                OfferOutcome::Derived(Evidence::Rewrite {
                    sanitizer,
                    source,
                    derived,
                    annotation,
                })
            }
            OfferConsult::Sanitizer {
                sanitizer,
                source,
                body,
                tool,
            } => match self.sanitizer_derived(evidence, &sanitizer, source, body, SanitizerSubject::Output { tool }) {
                Ok(derived) => OfferOutcome::Derived(Evidence::Sanitizer {
                    sanitizer,
                    source,
                    derived,
                }),
                Err(next) => return Ok(next),
            },
            OfferConsult::Authorities { call, required } => {
                match self.offer_authorities(&views, &engine_offer, &call, &required, evidence) {
                    AuthorityOutcome::Outcome(outcome) => outcome,
                    AuthorityOutcome::Consult(requests) => {
                        return Ok(EngineDecision::deliver(Next::ResolveExternal(requests)));
                    }
                    AuthorityOutcome::NoAnswer(feedback) => return Ok(no_answer(feedback)),
                }
            }
        };
        let judged = self.judge_under_audience(evidence, UnresolvedAudience::OfferStands, |audience| {
            let execution = OfferExecution {
                trajectory: engine_id(trajectory),
                offer: engine_offer,
                outcome,
                return_policy,
                offer_nonce: engine_nonce(entropy),
                audience: audience.clone(),
            };
            self.engine.handle(view, CoreEvent::ExecuteOffer(execution))
        })?;
        let decision = match judged {
            AudienceRound::Judged(decision) => decision,
            AudienceRound::Presented(decision) => return Ok(decision),
            // A sanitizer's derivation the engine cannot use — malformed, schema-invalid, or not
            // a strict improvement — lands no fact and opens no dispatch; the offer stands for a
            // later deliberate retry. The external's answer is not an
            // integration fault, so it is not a refusal.
            AudienceRound::Failed(TransitionError::Call(_) | TransitionError::SanitizerUnapplicable) => {
                return Ok(no_answer(
                    "[appa] the sanitizer's derivation was not usable; the offer stands and may be executed again"
                        .to_string(),
                ));
            }
            // A return declaration the engine will not hold a child to: the floor is not one
            // this trajectory can set, or the declaration does not fit the plan. The offer
            // stands for a corrected declaration.
            AudienceRound::Failed(TransitionError::ReturnPolicy(refusal)) => {
                return Ok(no_answer(format!(
                    "[appa] the return declaration is refused: {refusal}; the offer stands and may be executed again"
                )));
            }
            AudienceRound::Failed(error) => return Err(offer_refusal(error)),
        };
        let append = decision.append.map(ValidatedFactBatch::into_unsealed);
        let then = match decision.follow_up {
            FollowUp::Offer(OfferFollowUp::Approved { call }) => Next::Approved {
                tool: call.tool().as_str().to_string(),
                bytes: call.canonical_arguments().canonical_bytes().to_vec(),
            },
            FollowUp::Offer(OfferFollowUp::Admitted { value }) => Next::PresentToModel(Presentation::Value {
                value: value.as_str().to_string(),
            }),
            FollowUp::Offer(OfferFollowUp::Invalidated) => Next::PresentToModel(Presentation::Declined {
                feedback: "[appa] the state changed and this offer no longer applies; re-propose the call".to_string(),
            }),
            FollowUp::Offer(OfferFollowUp::Denied { block }) => {
                Next::PresentToModel(self.offer_block_delivery(&block, &self.return_bounds(&views)))
            }
            FollowUp::Offer(OfferFollowUp::Substituted { block }) => {
                Next::PresentToModel(self.offer_block_delivery(&block, &self.return_bounds(&views)))
            }
            FollowUp::Offer(OfferFollowUp::Staged(confined)) => Next::PresentToModel(self.stage_delivery(
                "[appa] the cleaned result still narrows this session.",
                &confined.residual,
                &confined.offers,
            )),
            FollowUp::Offer(OfferFollowUp::Released(release)) => Next::InvokeTool(released(&release)),
            FollowUp::Offer(OfferFollowUp::Settled(_)) => Next::PresentToModel(Presentation::Declined {
                feedback: "[appa] the call this offer released is already settled; propose a fresh call".to_string(),
            }),
            other => {
                return Err(EngineRefusal::Invariant {
                    detail: format!("an offer produced a non-offer follow-up: {other:?}"),
                });
            }
        };
        Ok(EngineDecision { append, then })
    }

    fn offer_authorities(
        &self,
        views: &Views,
        offer: &EngineOfferId,
        call: &ResolvedCall,
        required: &[RequiredRuling],
        evidence: &[ExternalEvidence],
    ) -> AuthorityOutcome {
        let registry = self.engine.registry();
        let chain = registry.trust_chain();
        let mut approvals = Vec::new();
        let mut requests = Vec::new();
        for requirement in required {
            let name = requirement.authority.as_str().to_string();
            let review = AuthorityReview {
                tool: call.tool().clone(),
                trajectory_label: views.current_label(),
            };
            match authority_verdict(evidence, &name) {
                None => {
                    let registered = registry
                        .authority(&requirement.authority)
                        .expect("plans reference only registered authorities");
                    requests.push(ExternalRequest::Authority {
                        authority: name,
                        declaration: AuthorityDeclaration::of(registered, chain),
                        artifact: AuthorityArtifact {
                            tool: call.tool().as_str().to_string(),
                            arguments: call.arguments().clone(),
                            requirements: requirement
                                .covers
                                .iter()
                                .map(|gap| Requirement::of(gap, chain))
                                .collect(),
                        },
                        review,
                    });
                }
                Some((AuthorityVerdict::Approve, review)) => approvals.push(AuthorityEvidence {
                    offer: *offer,
                    authority: requirement.authority.clone(),
                    covers: requirement.covers.clone(),
                    reviewed: review,
                }),
                Some((AuthorityVerdict::Deny, _)) => {
                    return AuthorityOutcome::Outcome(OfferOutcome::Denied {
                        authority: requirement.authority.clone(),
                    });
                }
                Some((AuthorityVerdict::Abstain, _)) => {
                    return AuthorityOutcome::NoAnswer(format!(
                        "[appa] authority {name} gave no answer; the offer stands and may be executed again"
                    ));
                }
            }
        }
        if !requests.is_empty() {
            return AuthorityOutcome::Consult(requests);
        }
        AuthorityOutcome::Outcome(OfferOutcome::Approved(approvals))
    }

    fn offer_block_delivery(&self, block: &CoreBlocked, bounds: &ReturnBounds) -> Presentation {
        let (feedback, offers, _) = self.rendered_block(block, bounds);
        Presentation::Blocked { feedback, offers }
    }

    fn return_bounds(&self, views: &Views) -> ReturnBounds {
        ReturnBounds {
            label: views.current_label(),
            lowest: self.engine.lowest_return_trust(views),
        }
    }

    /// The return policy an offer's execution declares, where its plan ends in a return step:
    /// the floor, spelled as a delta over this trajectory's current label, and the schema an
    /// attesting plan needs. A plan with no return step takes none, whatever was passed.
    fn declared_return_policy(
        &self,
        view: &EngineView,
        owner: &EngineTrajectoryId,
        views: &Views,
        offer: &EngineOfferId,
        arguments: &RemedyArguments,
    ) -> Result<Option<ReturnPolicy>, String> {
        let Some(step) = self
            .engine
            .offer_plan(view, owner, offer)
            .and_then(|plan| plan.return_step().map(|sanitizer| sanitizer.cloned()))
        else {
            return Ok(None);
        };
        let Some(spelling) = &arguments.label else {
            return Err(
                "this plan declares the subagent's return: pass `label`, the lowest label this session \
                        accepts from the return (omit a dimension to keep this session's current value)"
                    .to_string(),
            );
        };
        let current = views.current_label();
        let delta = appa_policy::parse_delta(
            spelling.trust.as_deref(),
            spelling.audience.as_deref(),
            self.engine.registry().trust_chain(),
            "label",
        )
        .map_err(|error| format!("`label` is not a label this policy spells: {error}"))?;
        let floor = Label {
            trust: delta.trust.unwrap_or(current.trust),
            audience: delta
                .audience
                .map(|declared| Audience::of_declared(&declared))
                .unwrap_or(current.audience),
        };
        let sanitizer = match step {
            Some(name) if name.is_attest_schema() => {
                let Some(schema) = &arguments.return_schema else {
                    return Err(
                        "this plan attests the subagent's return: pass `return_schema`, the JSON schema the return \
                         must match"
                            .to_string(),
                    );
                };
                let shape = ReturnShape::compile(schema)
                    .map_err(|error| format!("`return_schema` does not compile to a return shape: {error}"))?;
                Some(ReturnSanitizer::Attest(shape))
            }
            _ if arguments.return_schema.is_some() => {
                return Err(
                    "this plan does not attest the subagent's return, so `return_schema` is not taken: declare the \
                     attest-schema offer to attest, or omit `return_schema`"
                        .to_string(),
                );
            }
            None => None,
            Some(name) => Some(ReturnSanitizer::Named(name)),
        };
        Ok(Some(ReturnPolicy { floor, sanitizer }))
    }

    /// What a starting child is told about its return: the schema an attesting fork holds it
    /// to, or the sanitizer that rewrites it and the echo that follows. A bare floor tells the
    /// child nothing it could act on.
    pub(crate) fn fork_return_contract(
        &self,
        view: &EngineView,
        parent: &TrajectoryId,
        fork: &appa_engine::value::ForkId,
    ) -> Option<String> {
        let policy = self.engine.prepared_return_policy(view, &engine_id(parent), fork)?;
        match policy.sanitizer? {
            ReturnSanitizer::Attest(shape) => Some(format!(
                "[appa] Your final message is checked when you stop: it must be one JSON object matching this \
                 schema, and nothing else. A stop that does not match is blocked with the reason; a stop that \
                 matches but is not in canonical form is blocked with the exact bytes to return, and you return \
                 them verbatim.\nSchema: {}",
                shape.normalized()
            )),
            ReturnSanitizer::Named(name) => Some(format!(
                "[appa] Your final message is checked when you stop, and sanitizer {} rewrites it before the \
                 parent receives it. The first stop is blocked with the rewritten message; return it verbatim \
                 as your next final message.",
                terminal_safe(name.as_str())
            )),
        }
    }

    /// The value `child` crossed most recently, as the harness would deliver it.
    pub(crate) fn latest_return(&self, view: &EngineView, child: &TrajectoryId) -> Option<String> {
        self.engine
            .latest_return_of(view, &engine_id(child))
            .map(|body| body.as_str().to_string())
    }

    fn label_text(&self, label: &Label) -> String {
        match self.render_label(label) {
            Some(rendered) => format!("{}/{}", rendered.trust, rendered.audience),
            None => "an unnamed label".to_string(),
        }
    }

    /// One staged delivery: the narrowing the model must still accept,
    /// and the remedies the stage opened for it. The headline names
    /// what was staged; below it a tool result and a child return read
    /// the same.
    fn stage_delivery(
        &self,
        headline: &str,
        residual: &appa_engine::check::Narrowing,
        staged: &[(EngineOfferId, PlanId)],
    ) -> Presentation {
        let offers: Vec<OfferId> = staged.iter().map(|(offer, _)| offer_id(offer)).collect();
        let feedback = stage_feedback(headline, residual, &offers, self.engine.registry().trust_chain());
        Presentation::Blocked { feedback, offers }
    }

    fn bind_fork(
        &self,
        view: &EngineView,
        fork: &ForkId,
        child: &TrajectoryId,
    ) -> Result<EngineDecision, EngineRefusal> {
        let binding = ForkBinding {
            fork: fork.clone(),
            child: engine_id(child),
        };
        let decision = self
            .engine
            .handle(view, CoreEvent::BindFork(binding))
            .map_err(bind_refusal)?;
        let append = decision.append.map(ValidatedFactBatch::into_unsealed);
        match decision.follow_up {
            FollowUp::Fork { .. } => Ok(EngineDecision {
                append,
                then: Next::Done,
            }),
            other => Err(EngineRefusal::Invariant {
                detail: format!("a fork binding produced a non-fork follow-up: {other:?}"),
            }),
        }
    }

    /// One child's stop. The engine decides what crosses: the value merged, the derivation
    /// staged for the child to echo, or nothing. A stop held below the floor or outside the
    /// fork's shape is blocked with the reason.
    fn child_return(
        &self,
        view: &EngineView,
        child: &TrajectoryId,
        value: Option<String>,
        evidence: &[ExternalEvidence],
    ) -> Result<EngineDecision, EngineRefusal> {
        let fork = self
            .engine
            .fork_of(view, &engine_id(child))
            .ok_or_else(|| EngineRefusal::Invariant {
                detail: format!("child {} returned without an open fork", child.0),
            })?;
        let submission = match value {
            None => ChildSubmission::Void,
            Some(body) => ChildSubmission::Value {
                body: ValueBody::new(body),
            },
        };
        let withheld = UnresolvedAudience::Withheld { subject: "return" };
        let blocked = |feedback: String| {
            Ok(EngineDecision::deliver(Next::PresentToModel(Presentation::Blocked {
                feedback,
                offers: Vec::new(),
            })))
        };
        let judged = self.judge_under_audience(evidence, withheld, |audience| {
            let report = ChildReport {
                child: engine_id(child),
                fork: fork.clone(),
                submission: submission.clone(),
                evidence: sanitizer_evidence(evidence),
                audience: audience.clone(),
            };
            self.engine.handle(view, CoreEvent::ChildReturn(report))
        })?;
        let decision = match judged {
            AudienceRound::Judged(decision) => decision,
            AudienceRound::Presented(decision) => return Ok(decision),
            AudienceRound::Failed(TransitionError::ReturnBelowFloor { floor }) => {
                return blocked(format!(
                    "[appa] your final message cannot cross: this session's label fell below the floor the parent \
                     set for your return ({}). Nothing this session holds now can cross; stop with an empty final \
                     message to end without a return.",
                    self.label_text(&floor)
                ));
            }
            AudienceRound::Failed(TransitionError::ReturnShapeMismatch(mismatch)) => {
                let policy = self.engine.return_policy_of(view, &engine_id(child));
                return blocked(shape_feedback(&mismatch, policy.as_ref()));
            }
            AudienceRound::Failed(TransitionError::SanitizerUnapplicable) => {
                return Ok(withheld.present("the return sanitizer's derivation was not usable"));
            }
            AudienceRound::Failed(error) => return Err(child_refusal(error)),
        };
        let append = decision.append.map(ValidatedFactBatch::into_unsealed);
        let then = match decision.follow_up {
            FollowUp::Child(ChildFollowUp::Merged { admitted }) => Next::PresentToModel(Presentation::Value {
                value: admitted.as_str().to_string(),
            }),
            FollowUp::Child(ChildFollowUp::Staged { derived }) => Next::PresentToModel(Presentation::Staged {
                value: derived.as_str().to_string(),
            }),
            FollowUp::Child(ChildFollowUp::Ended) => Next::PresentToModel(Presentation::NoValue),
            FollowUp::Child(ChildFollowUp::Resolve(request)) => self.resolve_or_withhold(
                view,
                &engine_id(child),
                None,
                request,
                evidence,
                "[appa] no registered return sanitizer answered; the return is withheld and may be retried",
            )?,
            other => {
                return Err(EngineRefusal::Invariant {
                    detail: format!("a child return produced an unexpected follow-up: {other:?}"),
                });
            }
        };
        Ok(EngineDecision { append, then })
    }

    /// Every answer a call must carry before it is proposed, from the evidence gathered so
    /// far, or the consult still owed: the produced annotation for an Annotated declaration.
    /// A declared call owes nothing here — the symbolic audiences its contract reads
    /// surface from the act's own decision as membership asks.
    fn answers_for(
        &self,
        views: &Views,
        resolved: &ResolvedCall,
        evidence: &[ExternalEvidence],
    ) -> Result<CallAnswers, Resolution> {
        let declaration = self
            .engine
            .registry()
            .declaration(resolved)
            .expect("a resolved call names its registered declaration");
        let annotation = match declaration {
            ToolDeclaration::Declared(_) => None,
            ToolDeclaration::Annotated { .. } => Some(self.annotation_for(views, declaration, resolved, evidence)?),
        };
        Ok(CallAnswers { annotation })
    }

    /// The pinned annotation an Annotated declaration owes: a pin standing on an open offer
    /// or unspent approval for this exact digest, the evidence the Annotator gave for it, or
    /// the one consult still owed. The digest is the key: any rewrite is annotated afresh.
    fn annotation_for(
        &self,
        views: &Views,
        declaration: &ToolDeclaration,
        resolved: &ResolvedCall,
        evidence: &[ExternalEvidence],
    ) -> Result<PinnedAnnotation, Resolution> {
        let annotator = declaration
            .annotator()
            .expect("only an Annotated declaration owes an annotation");
        // A produced annotation pinned to this call in an act the trajectory still has
        // prepared — an open offer, an unspent approval — stands: the re-proposal spells the
        // call the act was prepared for, and an Annotator that may answer differently twice
        // is not asked twice.
        if let Some(pin) = views.pinned_annotation(resolved) {
            return Ok(pin.clone());
        }
        let digest = resolved.digest();
        let answer = evidence.iter().find_map(|entry| match entry {
            ExternalEvidence::Annotation {
                annotator: answered_by,
                call,
                answer,
            } if answered_by == annotator.as_str() && *call == digest => Some(answer),
            _ => None,
        });
        let Some(answer) = answer else {
            let binding = self
                .annotators
                .get(annotator.as_str())
                .expect("the deployment registers every annotator the policy declares");
            return Err(Resolution(vec![ExternalRequest::Annotation {
                annotator: annotator.as_str().to_string(),
                call: digest,
                declaration: self.annotation_declaration(annotator, binding),
                args: annotation_args(&binding.inputs, declaration, resolved),
            }]));
        };
        Ok(PinnedAnnotation::new(
            annotator.clone(),
            digest,
            self.produced_annotation(answer),
        ))
    }

    /// What one annotation consult declares: the Annotator's trusted hint, resolved mandate
    /// vocabulary, and the input names its artifact carries.
    fn annotation_declaration(
        &self,
        annotator: &appa_engine::names::AnnotatorName,
        binding: &appa_policy::AnnotatorBinding,
    ) -> AnnotationDeclaration {
        let registry = self.engine.registry();
        let chain = registry.trust_chain();
        let mandate = registry
            .annotator_mandate(annotator)
            .expect("declarations name only registered annotators");
        AnnotationDeclaration {
            hint: binding.hint.as_ref().map(|hint| hint.as_str().to_string()),
            inputs: binding.inputs.keys().cloned().collect(),
            trust_ranks: mandate
                .trust_ranks()
                .filter_map(|trust| chain.name_of(trust).map(str::to_string))
                .collect(),
            audiences: mandate.audiences().map(|reader| reader.as_str().to_string()).collect(),
            attention_marks: mandate.marks().map(|mark| mark.as_str().to_string()).collect(),
            effects: mandate.effects().map(|kind| kind.as_str().to_string()).collect(),
        }
    }

    /// The produced semantics a decoded answer pins for one call; the declaration's own
    /// metadata is not restated. `from_wire` confined every leaf to the declared mandate
    /// vocabulary, so reading it back against the policy cannot fail.
    fn produced_annotation(&self, answer: &AnnotationAnswer) -> ProducedAnnotation {
        let chain = self.engine.registry().trust_chain();
        let rank = |name: &str| {
            chain
                .rank_of(name)
                .expect("a decoded annotation answer names declared ranks")
        };
        let declared = |audience: &WireAudience| match audience {
            WireAudience::Public => DeclaredAudience::Public,
            WireAudience::Readers(readers) => DeclaredAudience::restricted(readers.iter().map(ReaderId::new)),
        };
        let mut requirements = Vec::new();
        if let Some(required) = &answer.required_audience {
            if let Some(includes) = &required.includes {
                requirements.push(AudienceRequirement::Includes(RecipientSpec::Static(declared(includes))));
            }
            if let Some(cap) = &required.cap {
                requirements.push(AudienceRequirement::Cap(declared(cap)));
            }
        }
        ProducedAnnotation {
            delta: Delta {
                trust: answer.delta_trust.as_deref().map(rank),
                audience: answer.delta_audience.as_ref().map(declared),
            },
            emits: EffectSet::new(answer.emits.iter().map(|kind| EffectKind::new(kind.as_str())))
                .expect("a decoded annotation answer holds no duplicate effect"),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: answer.required_trust.as_deref().map(rank),
                    audience: requirements,
                },
                history: answer
                    .history
                    .iter()
                    .map(|entry| match entry {
                        HistoryEntry::Contains(kind) => HistoryRequirement::Prior(EffectKind::new(kind.as_str())),
                        HistoryEntry::Excludes(kind) => HistoryRequirement::NoPrior(EffectKind::new(kind.as_str())),
                    })
                    .collect(),
                attention: answer
                    .attention
                    .iter()
                    .map(|mark| MarkName::new(mark.as_str()))
                    .collect(),
            },
        }
    }

    /// One sanitizer consult: the registered declaration at the point the offer applies
    /// it, and the value with the tool it belongs to.
    fn sanitizer_request(
        &self,
        sanitizer: &appa_engine::names::SanitizerName,
        source: RawResultDigest,
        body: ValueBody,
        subject: SanitizerSubject<'_>,
    ) -> ExternalRequest {
        let registry = self.engine.registry();
        let registered = registry
            .sanitizer(sanitizer)
            .expect("plans reference only registered sanitizers");
        let (on, tool, parameters) = match subject {
            SanitizerSubject::Input { call } => {
                let declaration = registry
                    .declaration(call)
                    .expect("a resolved call names its registered declaration");
                let parameters =
                    serde_json::to_value(declaration.parameters()).expect("a compiled parameter schema serializes");
                (SanitizerPoint::ToolInput, Some(call.tool().clone()), Some(parameters))
            }
            SanitizerSubject::Output { tool } => (SanitizerPoint::ToolOutput, tool, None),
        };
        ExternalRequest::Sanitizer {
            sanitizer: sanitizer.as_str().to_string(),
            source,
            declaration: SanitizerDeclaration::of(registered, on, registry.trust_chain(), parameters),
            artifact: SanitizerArtifact {
                tool: tool.map(|tool| tool.as_str().to_string()),
                body: body.as_str().to_string(),
            },
        }
    }

    /// The sanitizer's derivation from the evidence gathered so far, or what the offer does
    /// without one: asks for it, or stands for a later deliberate retry after the sanitizer
    /// gave no answer.
    fn sanitizer_derived(
        &self,
        evidence: &[ExternalEvidence],
        sanitizer: &appa_engine::names::SanitizerName,
        source: RawResultDigest,
        body: ValueBody,
        subject: SanitizerSubject<'_>,
    ) -> Result<ValueBody, EngineDecision> {
        match sanitizer_derivation(evidence, sanitizer.as_str(), &source) {
            SanitizerAnswer::Derived(derived) => Ok(derived),
            SanitizerAnswer::Missing => Err(EngineDecision::deliver(Next::ResolveExternal(vec![
                self.sanitizer_request(sanitizer, source, body, subject),
            ]))),
            SanitizerAnswer::NoAnswer => Err(no_answer(format!(
                "[appa] sanitizer {} gave no answer; the offer stands and may be executed again",
                sanitizer.as_str()
            ))),
        }
    }

    /// The next step for an evidence request an outcome or a return raised: the ask, or the
    /// withheld presentation where every applicable answer was already obtained.
    fn resolve_or_withhold(
        &self,
        view: &EngineView,
        trajectory: &EngineTrajectoryId,
        dispatch: Option<&EngineDispatchId>,
        request: EvidenceRequest,
        evidence: &[ExternalEvidence],
        withheld: &str,
    ) -> Result<Next, EngineRefusal> {
        let blocked = || {
            Ok(Next::PresentToModel(Presentation::Blocked {
                feedback: withheld.to_string(),
                offers: Vec::new(),
            }))
        };
        match request {
            EvidenceRequest::Sanitizer {
                sanitizer,
                source,
                body,
            } => {
                if matches!(
                    sanitizer_derivation(evidence, sanitizer.as_str(), &source),
                    SanitizerAnswer::NoAnswer
                ) {
                    return blocked();
                }
                let tool = dispatch.and_then(|dispatch| {
                    view.views(trajectory)
                        .and_then(|views| views.dispatch_tool(dispatch).cloned())
                });
                Ok(Next::ResolveExternal(vec![self.sanitizer_request(
                    &sanitizer,
                    source,
                    body,
                    SanitizerSubject::Output { tool },
                )]))
            }
        }
    }

    /// The selector templates the policy registers for one provider, for a consult's
    /// declaration. Every primitive a consult asks for names a registered provider.
    fn templates_of(&self, provider: &str) -> Result<Vec<String>, EngineRefusal> {
        self.engine
            .registry()
            .audience()
            .templates(provider)
            .map(|templates| templates.iter().map(|template| template.as_str().to_string()).collect())
            .ok_or_else(|| EngineRefusal::Invariant {
                detail: format!("a consult names the unregistered audience provider {provider}"),
            })
    }

    /// One act judged under its audience evidence: the evidence gathered from the consult
    /// answers, the act judged with it pinned, and — while the engine still needs membership
    /// answers — the consults that gather them. An answer the act cannot obtain is presented
    /// the way `unresolved` names.
    fn judge_under_audience<T>(
        &self,
        evidence: &[ExternalEvidence],
        unresolved: UnresolvedAudience<'_>,
        judge: impl FnOnce(&AudienceEvidence) -> Result<T, TransitionError>,
    ) -> Result<AudienceRound<T>, EngineRefusal> {
        let act = match self.act_audience(evidence) {
            Ok(act) => act,
            Err(AudienceFailure::Consult(requests)) => {
                return Ok(AudienceRound::Presented(EngineDecision::deliver(
                    Next::ResolveExternal(requests),
                )));
            }
            Err(AudienceFailure::Refused(detail)) => return Ok(AudienceRound::Presented(unresolved.present(&detail))),
        };
        match judge(&act.payload) {
            Ok(judged) => Ok(AudienceRound::Judged(judged)),
            Err(TransitionError::MembershipNeeded { needed }) => match self.audience_consult(&act, needed)? {
                AudienceConsult::Requests(requests) => Ok(AudienceRound::Presented(EngineDecision::deliver(
                    Next::ResolveExternal(requests),
                ))),
                AudienceConsult::Unresolved(detail) => Ok(AudienceRound::Presented(unresolved.present(&detail))),
            },
            Err(error) => Ok(AudienceRound::Failed(error)),
        }
    }

    /// One act's audience evidence, gathered from the consult answers: the source claims,
    /// member lookups, and — under a custom identity implementation — the identity mappings
    /// for every claimed member. A failed consult stays runtime-side as a recorded no-answer;
    /// an answer no registered source could have served is dropped rather than carried into
    /// an act it would refuse. The gathered payload is pre-validated against the same test
    /// replay applies — the engine's, which is the one rule on duplicate or foreign answers
    /// — so an inadmissible answer is an operational refusal here, never an engine error.
    fn act_audience(&self, evidence: &[ExternalEvidence]) -> Result<ActAudience, AudienceFailure> {
        let audience = self.engine.registry().audience();
        let mut payload = AudienceEvidence::default();
        let mut unanswered = Unanswered::default();
        for entry in evidence {
            match entry {
                ExternalEvidence::AudienceSource {
                    provider,
                    selector,
                    members,
                } => {
                    let routable = audience
                        .templates(provider)
                        .is_some_and(|templates| templates.iter().any(|template| template.matches(selector)));
                    if !routable {
                        continue;
                    }
                    // A claim outside the source's own provider namespace is a broken
                    // answer; conservatively, the selector was not answered.
                    let members = members
                        .clone()
                        .filter(|members| members.iter().all(|member| qualified_by(provider, &member.id)));
                    match members {
                        Some(members) => payload.sources.push(appa_engine::audience::SourceClaims {
                            provider: provider.clone(),
                            selector: selector.clone(),
                            members,
                        }),
                        None => {
                            unanswered.selectors.insert(SelectorSpec {
                                provider: provider.clone(),
                                selector: selector.clone(),
                            });
                        }
                    }
                }
                ExternalEvidence::MemberLookup {
                    provider,
                    member,
                    claims,
                } => {
                    if !audience.providers().contains(provider) || !qualified_by(provider, member) {
                        continue;
                    }
                    // Claims for an id other than the member asked are a broken answer — a
                    // source could otherwise canonicalize its member to another provider's
                    // namespace, or pre-seat an identity mapping for a member it does not
                    // own. Conservatively, the member was not answered.
                    let claims = match claims {
                        Some(Some(answered)) if answered.id != *member => None,
                        other => other.clone(),
                    };
                    match claims {
                        Some(claims) => payload.lookups.push(appa_engine::audience::MemberLookup {
                            provider: provider.clone(),
                            member: member.clone(),
                            claims,
                        }),
                        None => {
                            unanswered.members.insert(member.clone());
                        }
                    }
                }
                ExternalEvidence::Identity {
                    implementation,
                    id,
                    principal,
                } => {
                    let named = match audience.identity() {
                        IdentityImplementation::Custom(name) => name.as_str() == implementation,
                        IdentityImplementation::VerifiedEmail => false,
                    };
                    if !named {
                        continue;
                    }
                    match principal {
                        Some(principal) => payload.identity.push(IdentityMapping {
                            id: id.clone(),
                            principal: principal.clone(),
                        }),
                        None => {
                            unanswered.identities.insert(id.clone());
                        }
                    }
                }
                _ => {}
            }
        }
        // Under a custom identity implementation every claimed member canonicalizes through
        // a pinned mapping; the ones still unmapped are this round's identity consults.
        if let IdentityImplementation::Custom(name) = audience.identity() {
            let mut requests: Vec<ExternalRequest> = Vec::new();
            // One question per id, asked about the folded claim the engine will canonicalize
            // through: a member reported twice, once silently, is one member.
            let claimed = match appa_engine::audience::folded_claims(&payload) {
                Ok(folded) => folded,
                Err(refusal) => {
                    tracing::debug!(%refusal, "gathered audience evidence refused");
                    return Err(AudienceFailure::Refused(format!(
                        "the gathered audience evidence is not admissible: {}",
                        refusal_class(&refusal)
                    )));
                }
            };
            for claims in claimed.values() {
                if payload.identity.iter().any(|mapping| mapping.id == claims.id) {
                    continue;
                }
                if unanswered.identities.contains(&claims.id) {
                    // The member id is directory data the model has not seen: it stays
                    // out of the model-visible refusal.
                    tracing::debug!(implementation = name.as_str(), id = %claims.id, "identity gave no principal");
                    return Err(AudienceFailure::Refused(format!(
                        "identity implementation {} gave no principal for a claimed member",
                        name.as_str(),
                    )));
                }
                let request = ExternalRequest::Identity {
                    implementation: name.as_str().to_string(),
                    claims: claims.clone(),
                };
                if !requests.contains(&request) {
                    requests.push(request);
                }
            }
            if !requests.is_empty() {
                return Err(AudienceFailure::Consult(requests));
            }
        }
        if let Err(refusal) = audience.expansions(&payload) {
            // The refusal's own Display can carry directory data (member ids, claimed
            // emails) the model has not seen; the model-visible detail names only the
            // failure class and its provider/selector.
            tracing::debug!(%refusal, "gathered audience evidence refused");
            return Err(AudienceFailure::Refused(format!(
                "the gathered audience evidence is not admissible: {}",
                refusal_class(&refusal)
            )));
        }
        Ok(ActAudience { payload, unanswered })
    }

    /// The consults that answer the symbolic atoms an act still needs, or the operational
    /// refusal where a needed answer already failed or no registered source serves an atom.
    fn audience_consult(&self, act: &ActAudience, needed: Vec<SymbolicAtom>) -> Result<AudienceConsult, EngineRefusal> {
        let audience = self.engine.registry().audience();
        let primitives = match audience.needed_primitives(&needed) {
            Ok(primitives) => primitives,
            // A dynamically supplied reference no source serves: an operational failure,
            // never a policy state. (A statically written one refuses at policy load.)
            Err(unroutable) => return Ok(AudienceConsult::Unresolved(unroutable.to_string())),
        };
        let mut requests: Vec<ExternalRequest> = Vec::new();
        for spec in &primitives.selectors {
            let answered = act
                .payload
                .sources
                .iter()
                .any(|claims| claims.provider == spec.provider && claims.selector == spec.selector);
            if answered {
                continue;
            }
            if act.unanswered.selectors.contains(spec) {
                return Ok(AudienceConsult::Unresolved(format!(
                    "audience source {} gave no answer for {}",
                    spec.provider, spec.selector
                )));
            }
            requests.push(ExternalRequest::AudienceSource {
                provider: spec.provider.clone(),
                selector: spec.selector.clone(),
                templates: self.templates_of(&spec.provider)?,
            });
        }
        for spec in &primitives.lookups {
            if act.payload.lookups.iter().any(|lookup| lookup.member == spec.member) {
                continue;
            }
            if act.unanswered.members.contains(&spec.member) {
                // The member can be a reader a delta wrote that the model never saw.
                return Ok(AudienceConsult::Unresolved(format!(
                    "audience source {} gave no answer for a member lookup",
                    spec.provider
                )));
            }
            requests.push(ExternalRequest::MemberLookup {
                provider: spec.provider.clone(),
                member: spec.member.clone(),
                templates: self.templates_of(&spec.provider)?,
            });
        }
        if requests.is_empty() {
            // Every primitive is answered and pre-validated, yet the act still asked.
            return Err(EngineRefusal::Invariant {
                detail: format!(
                    "the act re-asks for answered audience atoms: {}",
                    needed.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
                ),
            });
        }
        Ok(AudienceConsult::Requests(requests))
    }
}

/// One model-visible line for an evidence refusal: the failure class and, where they are
/// policy or argument data the model already holds, the provider and selector — never a
/// member id or a claimed email, which are directory data.
fn refusal_class(refusal: &appa_engine::audience::EvidenceRefusal) -> String {
    use appa_engine::audience::EvidenceRefusal;
    match refusal {
        EvidenceRefusal::DuplicateSelector { provider, selector } => {
            format!("two answers for selector {provider}:{selector} in one operation")
        }
        EvidenceRefusal::DuplicateLookup { .. } => "two lookups for one member in one operation".to_string(),
        EvidenceRefusal::DuplicateIdentity { .. } => {
            "two identity mappings for one member in one operation".to_string()
        }
        EvidenceRefusal::ForeignMember { provider, selector, .. } => {
            format!("selector {provider}:{selector} reports a member outside its own provider namespace")
        }
        EvidenceRefusal::ForeignLookup { provider, .. } => {
            format!("a lookup under provider {provider} answers outside that namespace")
        }
        EvidenceRefusal::ForeignLookupClaims { provider, .. } => {
            format!("a lookup under provider {provider} carries claims for a different id")
        }
        EvidenceRefusal::DuplicateMember { provider, selector, .. } => {
            format!("selector {provider}:{selector} reports one member twice in one answer")
        }
        EvidenceRefusal::ReservedPrincipal { .. } => "an identity mapping names a reserved principal".to_string(),
        EvidenceRefusal::ConflictingClaims { .. } => {
            "one member carries conflicting verified-email claims in one operation".to_string()
        }
        EvidenceRefusal::MalformedEmail { .. } => {
            "a member claims a verified email that does not parse as one address".to_string()
        }
        EvidenceRefusal::UnmappedIdentity { .. } => {
            "the identity implementation returned no mapping for a claimed member".to_string()
        }
        EvidenceRefusal::UnroutableSelector { provider, selector } => {
            format!("no registered audience source serves selector {provider}:{selector}")
        }
        EvidenceRefusal::UnroutableLookup { provider, .. } => {
            format!("no registered audience provider {provider} serves a member lookup")
        }
        EvidenceRefusal::UnrequestedEvidence { .. } => {
            "evidence beyond this operation's inherited pins and its own asks".to_string()
        }
        EvidenceRefusal::ContradictedPin { .. } => {
            "an answer that contradicts the one an earlier record of this chain pinned".to_string()
        }
    }
}

/// Is `id` inside `provider`'s own namespace? One rule decides qualification everywhere:
/// the engine's `ReaderId::provider_prefix`, which also decides which readers the engine
/// asks to canonicalize. A second spelling of the rule here would let an id the engine asks
/// about fall outside what this gathering records — and the act would re-ask forever.
fn qualified_by(provider: &str, id: &str) -> bool {
    appa_engine::label::ReaderId::new(id).provider_prefix() == Some(provider)
}

/// Everything a proposal carries beyond its tool and arguments, gathered before it is judged.
struct CallAnswers {
    annotation: Option<PinnedAnnotation>,
}

/// The consult artifact an annotation request carries: the complete call — its proposed
/// name, the declaration's description when the policy wrote one, and the canonical
/// arguments — or one value per declared input.
fn annotation_args(
    inputs: &BTreeMap<String, appa_policy::ToolCallSource>,
    declaration: &ToolDeclaration,
    resolved: &ResolvedCall,
) -> serde_json::Value {
    let complete = || {
        let mut call = serde_json::Map::new();
        call.insert("name".to_string(), serde_json::json!(resolved.tool().as_str()));
        if let Some(description) = declaration.description() {
            call.insert("description".to_string(), serde_json::json!(description));
        }
        call.insert("arguments".to_string(), resolved.arguments().clone());
        serde_json::Value::Object(call)
    };
    if inputs.is_empty() {
        return complete();
    }
    let mut args = serde_json::Map::new();
    for (input, source) in inputs {
        let value = match source {
            appa_policy::ToolCallSource::Call => complete(),
            appa_policy::ToolCallSource::Name => serde_json::json!(resolved.tool().as_str()),
            appa_policy::ToolCallSource::Description => serde_json::json!(
                declaration
                    .description()
                    .expect("the loader requires a description a mapped input reads")
            ),
            appa_policy::ToolCallSource::Arguments => resolved.arguments().clone(),
            appa_policy::ToolCallSource::Argument(name) => resolved
                .arguments()
                .get(name)
                .cloned()
                .expect("the loader requires a mapped argument in the schema"),
        };
        args.insert(input.clone(), value);
    }
    serde_json::Value::Object(args)
}

fn unresolved_audience(tool: &str, detail: &str) -> String {
    format!("[appa] {tool}: {detail}; the call was not checked — propose it again later")
}

enum AuthorityOutcome {
    Outcome(OfferOutcome),
    Consult(Vec<ExternalRequest>),
    NoAnswer(String),
}

enum SanitizerAnswer {
    Missing,
    NoAnswer,
    Derived(ValueBody),
}

/// The consults an answer still owes before its call can be judged.
#[derive(Debug)]
struct Resolution(Vec<ExternalRequest>);

enum AudienceConsult {
    Requests(Vec<ExternalRequest>),
    Unresolved(String),
}

/// How an act presents an audience answer it cannot obtain: a failed or inadmissible
/// consult, or a dynamically supplied reference no registered source serves.
#[derive(Clone, Copy)]
enum UnresolvedAudience<'a> {
    /// The proposed call is denied.
    Denied { tool: &'a str },
    /// The value — a tool result or a child's return — is withheld and may be retried.
    Withheld { subject: &'static str },
    /// The offer stands and may be executed again.
    OfferStands,
}

impl UnresolvedAudience<'_> {
    fn present(self, detail: &str) -> EngineDecision {
        match self {
            UnresolvedAudience::Denied { tool } => deny(unresolved_audience(tool, detail)),
            UnresolvedAudience::Withheld { subject } => {
                EngineDecision::deliver(Next::PresentToModel(Presentation::Blocked {
                    feedback: format!("[appa] {detail}; the {subject} is withheld and may be retried"),
                    offers: Vec::new(),
                }))
            }
            UnresolvedAudience::OfferStands => {
                no_answer(format!("[appa] {detail}; the offer stands and may be executed again"))
            }
        }
    }
}

/// Where one act's audience round ends: judged, presented short of a judgment (consults
/// still owed, or an unobtainable answer), or failed in the engine for another reason.
enum AudienceRound<T> {
    Judged(T),
    Presented(EngineDecision),
    Failed(TransitionError),
}

/// Why an act cannot carry audience evidence yet: the identity consults still owed, or the
/// operational refusal a failed or inadmissible answer forces.
enum AudienceFailure {
    Consult(Vec<ExternalRequest>),
    Refused(String),
}

/// One act's validated audience payload — what the act pins — beside the consults that
/// already produced no answer, which are never re-asked.
struct ActAudience {
    payload: AudienceEvidence,
    unanswered: Unanswered,
}

#[derive(Debug, Default)]
struct Unanswered {
    selectors: BTreeSet<SelectorSpec>,
    /// Qualified members whose lookup produced no answer.
    members: BTreeSet<String>,
    /// Member ids the custom identity implementation gave no principal for.
    identities: BTreeSet<String>,
}

fn deny(text: String) -> EngineDecision {
    EngineDecision::deliver(deny_next(text))
}

fn deny_next(text: String) -> Next {
    Next::ModelResponse {
        invocations: Vec::new(),
        feedback: vec![Feedback {
            text,
            offers: Vec::new(),
            review: Vec::new(),
        }],
    }
}

fn declined(feedback: String) -> EngineDecision {
    EngineDecision::deliver(Next::PresentToModel(Presentation::Declined { feedback }))
}

fn no_answer(feedback: String) -> EngineDecision {
    EngineDecision::deliver(Next::PresentToModel(Presentation::NoAnswer { feedback }))
}

pub fn engine_id(id: &TrajectoryId) -> appa_engine::value::TrajectoryId {
    appa_engine::value::TrajectoryId::new(id.0.clone())
}

fn engine_nonce(entropy: &OfferNonce) -> EngineOfferNonce {
    EngineOfferNonce::new(entropy.0)
}

fn batch_id(entropy: &OfferNonce) -> ProposalBatchId {
    ProposalBatchId::new(hex(&entropy.0))
}

fn fork_binding(fork: &ForkId) -> SpawnBinding {
    SpawnBinding(serde_json::to_string(fork).expect("a fork id serializes"))
}

/// Recover the fork one spawn binding names. `None` for a binding
/// this runtime did not mint.
pub(crate) fn parse_fork(binding: &SpawnBinding) -> Option<ForkId> {
    serde_json::from_str(&binding.0).ok()
}

const RENDERED_OFFER_CHARS: usize = 16;

fn offer_id(offer: &EngineOfferId) -> OfferId {
    let mut hex = offer.to_hex();
    hex.truncate(RENDERED_OFFER_CHARS);
    OfferId(hex)
}

fn parse_offer(offer: &OfferId) -> Option<EngineOfferId> {
    EngineOfferId::from_hex(&offer.0).ok()
}

/// The canonical identity a quoted id names, resolved against the offers this
/// log has opened.
pub(crate) fn resolve_rendered(log: &Log, rendered: &OfferId) -> Option<OfferId> {
    let mut found = None;
    for fact in log.facts() {
        let Fact::OfferOpened { offer, .. } = fact else {
            continue;
        };
        if offer_id(offer) != *rendered {
            continue;
        }
        match found {
            None => found = Some(OfferId(offer.to_hex())),
            Some(ref already) if already.0 == offer.to_hex() => {}
            Some(_) => return None,
        }
    }
    found
}

/// The exact-bytes key of one policy file, as text. The type is the
/// engine's; this is the one place allowed to name it, so the rest of the
/// runtime carries the text it renders to.
pub(crate) fn policy_file_key(bytes: &[u8]) -> String {
    EnginePolicyFileKey::of(bytes).as_str().to_string()
}

/// Every offer a log opened, as the model was shown them. A test reads the
/// identity it would have quoted back rather than reaching into the engine.
#[cfg(test)]
pub(crate) fn minted_offers(log: &Log, trajectory: &TrajectoryId) -> Vec<OfferId> {
    let owner = engine_id(trajectory);
    log.facts()
        .iter()
        .filter_map(|fact| match fact {
            Fact::OfferOpened { trajectory, offer, .. } if trajectory == &owner => Some(offer_id(offer)),
            _ => None,
        })
        .collect()
}

fn released(release: &Released) -> ReleasedCall {
    ReleasedCall {
        tool: release.call.tool().as_str().to_string(),
        bytes: release.call.canonical_arguments().canonical_bytes().to_vec(),
        fork: release.fork.as_ref().map(fork_binding),
    }
}

fn engine_outcome(outcome: &ToolOutcome) -> CoreToolOutcome {
    match outcome {
        ToolOutcome::Failure { .. } => CoreToolOutcome::Failure,
        ToolOutcome::Indeterminate => CoreToolOutcome::Indeterminate,
        ToolOutcome::Success {
            body: OutcomeBody::Unavailable,
        } => CoreToolOutcome::Success {
            body: CoreOutcomeBody::Unavailable,
        },
        ToolOutcome::Success {
            body: OutcomeBody::Available(raw),
        } => CoreToolOutcome::Success {
            body: CoreOutcomeBody::Available(ValueBody::new(raw.clone())),
        },
    }
}

fn outcome_presentation(outcome: &ToolOutcome, admitted: Option<ValueBody>) -> Presentation {
    match (outcome, admitted) {
        (
            ToolOutcome::Success {
                body: OutcomeBody::Available(raw),
            },
            Some(value),
        ) if value.as_str() == raw => Presentation::KeepOutput,
        (_, Some(value)) => Presentation::ReplaceOutput {
            placeholder: value.as_str().to_string(),
        },
        (
            ToolOutcome::Success {
                body: OutcomeBody::Unavailable,
            },
            None,
        ) => Presentation::ReplaceOutput {
            placeholder: "[appa] the result was not carried; nothing was admitted".to_string(),
        },
        (ToolOutcome::Failure { .. } | ToolOutcome::Indeterminate, None) => Presentation::KeepOutput,
        (ToolOutcome::Success { .. }, None) => Presentation::ReplaceOutput {
            placeholder: "[appa] the result is withheld".to_string(),
        },
    }
}

fn sanitizer_evidence(evidence: &[ExternalEvidence]) -> Vec<Evidence> {
    evidence
        .iter()
        .filter_map(|entry| match entry {
            ExternalEvidence::Sanitizer {
                sanitizer,
                source,
                derived: Some(derived),
            } => Some(Evidence::Sanitizer {
                sanitizer: appa_engine::names::SanitizerName::new(sanitizer.clone()),
                source: *source,
                derived: ValueBody::new(derived.clone()),
            }),
            _ => None,
        })
        .collect()
}

/// What a sanitizer consult judges: the arguments of the call about to run, or a value on
/// its way in — a tool's result naming its producer, a child's return naming none.
enum SanitizerSubject<'a> {
    Input { call: &'a ResolvedCall },
    Output { tool: Option<ToolName> },
}

fn sanitizer_derivation(evidence: &[ExternalEvidence], name: &str, source: &RawResultDigest) -> SanitizerAnswer {
    for entry in evidence {
        if let ExternalEvidence::Sanitizer {
            sanitizer,
            source: reported,
            derived,
        } = entry
            && sanitizer == name
            && reported == source
        {
            return match derived {
                Some(derived) => SanitizerAnswer::Derived(ValueBody::new(derived.clone())),
                None => SanitizerAnswer::NoAnswer,
            };
        }
    }
    SanitizerAnswer::Missing
}

fn authority_verdict(evidence: &[ExternalEvidence], name: &str) -> Option<(AuthorityVerdict, AuthorityReview)> {
    evidence.iter().find_map(|entry| match entry {
        ExternalEvidence::Authority {
            authority,
            verdict,
            review,
        } if authority == name => Some((*verdict, review.clone())),
        _ => None,
    })
}

fn malformed_feedback(error: &EngineError) -> String {
    match error {
        EngineError::UnknownTool(tool) => format!("[appa] unknown tool {tool}: not in this deployment's policy"),
        EngineError::ProviderRunTool(tool) => format!(
            "[appa] tool {tool} is provider-run: it executes inside the inference call and cannot be proposed as a tool call"
        ),
        error => format!("[appa] invalid call: {error}"),
    }
}

fn stage_feedback(
    headline: &str,
    residual: &appa_engine::check::Narrowing,
    offers: &[OfferId],
    chain: &TrustChain,
) -> String {
    let mut lines = vec![headline.to_string()];
    lines.extend(
        narrowing_feedback(residual, chain)
            .into_iter()
            .map(|change| format!("  - {change}")),
    );
    for offer in offers {
        lines.push(format!(
            "  - execute_remedy_plan(offer_id: \"{}\")",
            terminal_safe(&offer.0)
        ));
    }
    lines.join("\n")
}

fn proposal_refusal(error: TransitionError) -> EngineRefusal {
    match error {
        TransitionError::BranchEnded => EngineRefusal::Ended,
        error => EngineRefusal::Invariant {
            detail: format!("deciding a proposal: {error}"),
        },
    }
}

fn outcome_refusal(error: TransitionError) -> EngineRefusal {
    match error {
        TransitionError::BranchEnded => EngineRefusal::Ended,
        TransitionError::UnknownDispatch => EngineRefusal::DispatchClosed,
        error => EngineRefusal::Invariant {
            detail: format!("deciding an outcome: {error}"),
        },
    }
}

fn offer_refusal(error: TransitionError) -> EngineRefusal {
    match error {
        TransitionError::UnknownOffer | TransitionError::OfferElsewhere => EngineRefusal::UnknownOffer,
        TransitionError::BranchEnded => EngineRefusal::Ended,
        error => EngineRefusal::Invariant {
            detail: format!("deciding an offer: {error}"),
        },
    }
}

fn bind_refusal(error: TransitionError) -> EngineRefusal {
    match error {
        TransitionError::UnbindableFork | TransitionError::ChildAlreadyUsed => EngineRefusal::Unbindable,
        error => EngineRefusal::Invariant {
            detail: format!("binding a fork: {error}"),
        },
    }
}

fn child_refusal(error: TransitionError) -> EngineRefusal {
    match error {
        TransitionError::BranchEnded => EngineRefusal::Ended,
        error => EngineRefusal::Invariant {
            detail: format!("deciding a child return: {error}"),
        },
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn effect_names(effects: &EffectSet) -> Vec<String> {
    effects.iter().map(|effect| terminal_safe(effect.as_str())).collect()
}

fn audience_wire(audience: &Audience) -> String {
    if audience.is_public() {
        return "public".to_string();
    }
    audience.clauses().map(clause_wire).collect::<Vec<_>>().join(" ∩ ")
}

/// One union clause's summary: the chain audience, group marks, and readers in canonical
/// order — three entries shown, the rest counted; the empty clause is nobody.
fn clause_wire(clause: &Clause) -> String {
    let mut entries = crate::consult::clause_entries(clause);
    if entries.is_empty() {
        return "∅".to_string();
    }
    let rest = entries.len().saturating_sub(3);
    entries.truncate(3);
    if rest > 0 {
        format!("{}+{rest}", entries.join(","))
    } else {
        entries.join(",")
    }
}

fn terminal_safe(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() || is_format(c) { '\u{FFFD}' } else { c })
        .collect()
}

const fn is_format(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061C}'
            | '\u{06DD}'
            | '\u{070F}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08E2}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{110BD}'
            | '\u{110CD}'
            | '\u{13430}'..='\u{1343F}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0001}'
            | '\u{E0020}'..='\u{E007F}'
    )
}

fn gap_text(gap: &appa_engine::check::Gap) -> String {
    use appa_engine::check::Gap;
    match gap {
        Gap::TrustFloor { required, actual } => {
            format!("trust is {actual:?}, below the required floor {required:?}")
        }
        Gap::Includes { recipients } => match recipients {
            DeclaredAudience::Public => "the readers are not the public audience".to_string(),
            DeclaredAudience::Union(clause) => {
                format!(
                    "the readers do not include {} required recipient(s)",
                    clause_size(clause)
                )
            }
        },
        // The count only, as for `includes`: a cap may read a directory group's members.
        Gap::Cap { cap } => format!("the committed readers exceed the cap of {}", declared_count(cap)),
        Gap::Prior(effect) => format!("requires a prior {} effect", effect.as_str()),
        Gap::NoPrior(effect) => format!("forbidden after a {} effect", effect.as_str()),
        Gap::Attention(mark) => format!("requires attention: {}", mark.as_str()),
    }
}

fn trust_feedback(trust: Trust, chain: &TrustChain) -> String {
    let named = if trust == Trust::new(u8::MAX) {
        chain.name_of(Trust::new((chain.len() - 1) as u8))
    } else {
        chain.name_of(trust)
    };
    terminal_safe(
        named
            .map_or_else(|| format!("rank {}", trust.rank()), str::to_string)
            .as_str(),
    )
}

fn narrowing_feedback(narrowing: &appa_engine::check::Narrowing, chain: &TrustChain) -> Vec<String> {
    let mut changes = Vec::new();
    if narrowing.from.trust != narrowing.to.trust {
        changes.push(format!(
            "session trust would fall: {} -> {}",
            trust_feedback(narrowing.from.trust, chain),
            trust_feedback(narrowing.to.trust, chain),
        ));
    }
    if narrowing.from.audience != narrowing.to.audience {
        changes.push(format!(
            "allowed readers would narrow: {} -> {}",
            audience_count(&narrowing.from.audience),
            audience_count(&narrowing.to.audience),
        ));
    }
    changes
}

fn audience_count(audience: &Audience) -> String {
    if audience.is_public() {
        return "public".to_string();
    }
    let clauses = audience.clauses().count();
    if clauses > 1 {
        return format!("an intersection of {clauses} audiences");
    }
    let clause = audience.clauses().next().expect("a non-public audience holds a clause");
    clause_count(clause)
}

/// One clause's atom count as feedback shows it — counts only, never who.
fn clause_count(clause: &Clause) -> String {
    let symbolic = clause.groups().count() + usize::from(clause.chain().is_some());
    match (clause.readers().len(), symbolic) {
        (0, 0) => "nobody".to_string(),
        (1, 0) => "1 reader".to_string(),
        (count, 0) => format!("{count} readers"),
        (0, _) => "a symbolic audience".to_string(),
        (count, _) => format!("a symbolic audience and {count} reader(s)"),
    }
}

fn clause_size(clause: &Clause) -> usize {
    clause.readers().len() + clause.groups().count() + usize::from(clause.chain().is_some())
}

fn declared_count(audience: &DeclaredAudience) -> String {
    match audience {
        DeclaredAudience::Public => "public".to_string(),
        DeclaredAudience::Union(clause) => clause_count(clause),
    }
}

fn shape_feedback(mismatch: &ReturnMismatch, policy: Option<&ReturnPolicy>) -> String {
    let schema = policy
        .and_then(|policy| policy.sanitizer.as_ref())
        .and_then(ReturnSanitizer::shape)
        .map(|shape| shape.normalized().to_string())
        .unwrap_or_default();
    format!(
        "[appa] your final message must be one JSON object matching the schema the parent set, and nothing else; \
         it was refused: {mismatch}\nSchema: {schema}"
    )
}

/// A label in the spelling `execute_remedy_plan` takes for a return floor. An audience that is
/// an intersection of clauses has no one-list spelling; the trust rank alone is shown then, and
/// the omitted dimension keeps the trajectory's current value.
fn label_spelling(chain: &TrustChain, label: &Label) -> String {
    let quoted = |text: &str| format!("\"{}\"", terminal_safe(text));
    let top = Trust::new((chain.len() - 1) as u8);
    let trust = chain
        .name_of(if label.trust == Trust::new(u8::MAX) {
            top
        } else {
            label.trust
        })
        .map(quoted)
        .unwrap_or_else(|| "\"<rank>\"".to_string());
    let audience = if label.audience.is_public() {
        Some(vec!["public".to_string()])
    } else {
        match label.audience.clauses().collect::<Vec<_>>().as_slice() {
            [clause] => Some(crate::consult::clause_entries(clause)),
            _ => None,
        }
    };
    match audience {
        Some(entries) => format!(
            "{{trust: {trust}, audience: [{}]}}",
            entries.iter().map(|entry| quoted(entry)).collect::<Vec<_>>().join(", ")
        ),
        None => format!("{{trust: {trust}}}"),
    }
}

/// What bounds a return declaration made on a trajectory: its current label, which no floor
/// stands above, and the lowest trust its own floor lets it declare.
struct ReturnBounds {
    label: Label,
    lowest: Trust,
}

/// The spelling a return declaration's example needs: this trajectory's label as the
/// `label` argument takes it, and the trust ranks a placeholder stands for.
struct ReturnSpelling {
    floor: String,
    ranks: String,
}

impl ReturnSpelling {
    /// Only the ranks the declaration may take are named: at or below the trajectory's trust,
    /// and at or above the lowest its own floor permits. The unnamed top sentinel stands for
    /// the whole chain.
    fn of(chain: &TrustChain, bounds: &ReturnBounds) -> ReturnSpelling {
        let ReturnBounds { label, lowest } = bounds;
        let held = if label.trust == Trust::new(u8::MAX) {
            chain.len()
        } else {
            usize::from(label.trust.rank()) + 1
        };
        let lowest = usize::from(lowest.rank());
        ReturnSpelling {
            floor: label_spelling(chain, label),
            ranks: chain
                .names()
                .skip(lowest)
                .take(held.saturating_sub(lowest))
                .map(|name| format!("\"{}\"", terminal_safe(name)))
                .collect::<Vec<_>>()
                .join(", "),
        }
    }
}

fn return_instruction(
    sanitizer: Option<&appa_engine::names::SanitizerName>,
    id: &OfferId,
    spelling: &ReturnSpelling,
) -> String {
    let id = terminal_safe(&id.0);
    let ReturnSpelling { floor, ranks } = spelling;
    match sanitizer {
        None => format!(
            "  - Declare the lowest label this session accepts from the subagent's return, then propose the spawn \
             again. The subagent starts at this session's label, now {floor}, and can accept no change below the \
             floor it is given: a subagent that must read below this session's trust needs the floor at that rank, \
             and its return may then narrow this session that far. An omitted dimension keeps its current \
             value.\n    execute_remedy_plan(offer_id: \"{id}\", label: {{trust: \"<rank>\"}}), with <rank> one \
             of {ranks} (lowest first)"
        ),
        Some(name) if name.is_attest_schema() => format!(
            "  - Attest the subagent's return: declare the floor and the JSON schema its return must match. The \
             schema is strict: an object lists its `properties`, every one `required`, and is closed as written \
             (no `additionalProperties`); an integer carries `minimum` and `maximum`; a string leaf carries \
             `enum`, `const`, or `format`, never free text. The return crosses at the attestation's \
             label.\n    execute_remedy_plan(offer_id: \"{id}\", label: {floor}, return_schema: {{type: \
             \"object\", ...}})"
        ),
        Some(name) => format!(
            "  - Have sanitizer {} rewrite the subagent's return before this session receives it, and declare the \
             floor.\n    execute_remedy_plan(offer_id: \"{id}\", label: {floor})",
            terminal_safe(name.as_str())
        ),
    }
}

fn remedy_instruction(plan: &ExecutableRemedyPlan, id: &OfferId, spelling: &ReturnSpelling) -> String {
    if let Some(sanitizer) = plan.return_step() {
        return return_instruction(sanitizer, id, spelling);
    }
    let needs_approval = !plan.required.is_empty();
    let action = match (needs_approval, plan.narrowing().is_some(), plan.sanitizer()) {
        (true, _, Some(sanitizer)) => {
            format!(
                "Request approval and use sanitizer {}'s result",
                terminal_safe(sanitizer.as_str())
            )
        }
        (false, _, Some(sanitizer)) => {
            format!("Use sanitizer {}'s result", terminal_safe(sanitizer.as_str()))
        }
        (true, true, None) => "Request approval and accept this change for the rest of this session".to_string(),
        (false, true, None) => "Accept this change for the rest of this session".to_string(),
        (true, false, None) => "Request approval".to_string(),
        (false, false, None) => "Apply the offered remedy".to_string(),
    };
    format!(
        "  - {action}:\n    execute_remedy_plan(offer_id: \"{}\")",
        terminal_safe(&id.0),
    )
}

fn remedy_lines(planned: &PlannedBlock, offers: &[(OfferId, PlanId)], spelling: &ReturnSpelling) -> Vec<String> {
    planned
        .plans
        .iter()
        .filter_map(|plan| match plan {
            RemedyPlan::Executable(plan) => offers
                .iter()
                .find(|(_, offered)| *offered == plan.id)
                .map(|(id, _)| remedy_instruction(plan, id, spelling)),
            RemedyPlan::Redispatch(redispatch) => Some(format!(
                "  - Run {} first; it clears: {}.",
                terminal_safe(redispatch.tool().as_str()),
                terminal_safe(&redispatch.clears().iter().map(gap_text).collect::<Vec<_>>().join("; ")),
            )),
        })
        .collect()
}

fn block_feedback(
    planned: &PlannedBlock,
    offers: &[(OfferId, PlanId)],
    chain: &TrustChain,
    bounds: &ReturnBounds,
) -> String {
    let mut reasons = Vec::new();
    for gap in &planned.raw.requirement_gaps {
        reasons.push(terminal_safe(&gap_text(gap)));
    }
    if let Some(narrowing) = &planned.raw.narrowing {
        reasons.extend(narrowing_feedback(narrowing, chain));
    }
    if planned.plans.iter().any(|plan| match plan {
        RemedyPlan::Executable(plan) => plan.return_step().is_some(),
        RemedyPlan::Redispatch(_) => false,
    }) {
        reasons.push(format!(
            "This call starts a subagent, and this session has not declared what its return may carry. The policy's \
             trust ranks, lowest first: {}.",
            chain.names().map(terminal_safe).collect::<Vec<_>>().join(" < ")
        ));
    }

    let mut lines = vec![
        "[appa] Blocked: this call cannot run yet.".to_string(),
        String::new(),
        "Why:".to_string(),
    ];
    lines.extend(reasons.into_iter().map(|reason| format!("  - {reason}")));

    let remedies = remedy_lines(planned, offers, &ReturnSpelling::of(chain, bounds));
    if !remedies.is_empty() {
        lines.push(String::new());
        lines.push("Continue:".to_string());
        lines.extend(remedies);
    }
    if let Some(advice) = planned.fork_advice {
        let remedies_required = !planned.raw.requirement_gaps.is_empty();
        lines.push(String::new());
        lines.push(fork_heading(advice).to_string());
        lines.push(format!(
            "  {}",
            fork_advice_text(advice, remedies_required).replace('\n', "\n  ")
        ));
    }
    lines.join("\n")
}

fn fork_heading(advice: ForkAdvice) -> &'static str {
    match advice {
        ForkAdvice::SameLabel => "Alternative:",
        ForkAdvice::Narrowing {
            standing: FloorStanding::Unbound,
            ..
        } => "Keep this session unchanged:",
        ForkAdvice::Narrowing {
            standing: FloorStanding::Within,
            ..
        } => "Delegation:",
        ForkAdvice::Narrowing {
            standing: FloorStanding::Below,
            ..
        } => "Not acceptable here:",
    }
}

/// `remedies_required` when the block also carries requirement gaps a child would clear.
fn fork_advice_text(advice: ForkAdvice, remedies_required: bool) -> String {
    let ForkAdvice::Narrowing {
        standing,
        sanitized_return,
    } = advice
    else {
        return "If this trajectory's harness advertises a child-session tool, handle the work there if isolation is \
                useful.\nA child inherits the same session label, so delegation does not clear these requirements."
            .to_string();
    };
    let delegated = if remedies_required {
        "this call, its required remedies, and all work that uses its result"
    } else {
        "this call and all work that uses its result"
    };
    match (standing, sanitized_return) {
        (FloorStanding::Unbound, true) => format!(
            "If this trajectory's harness advertises a child-session tool, delegate {delegated} there.\nFinish there \
             by returning nothing, or return only a sanitized derivation. Returning the raw value applies the same \
             change to this session."
        ),
        (FloorStanding::Unbound, false) => format!(
            "If this trajectory's harness advertises a child-session tool, delegate {delegated} there.\nNo \
             registered return sanitizer carries this change back without applying it here, so finish there by \
             returning nothing: a returned value applies the same change to this session."
        ),
        (FloorStanding::Within, true) => format!(
            "This session is a subagent, and the floor its parent declared allows this change: accept it here.\nTo \
             keep this session unchanged instead, delegate {delegated} to a further subagent declared with a \
             return sanitizer; one declared with the bare floor would apply the same change here on its return."
        ),
        (FloorStanding::Within, false) => {
            "This session is a subagent, and the floor its parent declared allows this change: accept it here.\nA \
             further subagent's return would apply the same change to this session, so delegating gains nothing."
                .to_string()
        }
        (FloorStanding::Below, true) => format!(
            "This session is a subagent, and this change falls below the floor its parent declared: this session \
             cannot accept it, and a subagent started here under the bare floor cannot either.\nA subagent started \
             here with a return sanitizer can: delegate {delegated} there, and declare that sanitizer when the \
             spawn asks for the return declaration."
        ),
        (FloorStanding::Below, false) => {
            "This session is a subagent, and this change falls below the floor its parent declared: neither this \
             session nor any subagent started here can accept it, and no registered return sanitizer carries \
             this change back without applying it.\nDo not start a subagent for this. Finish without this call, \
             or return a plain note that the work needs a subagent declared with a lower floor or a return \
             sanitizer, so the parent can start one."
                .to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        EngineEvent, EngineView, ExternalEvidence, ExternalRequest, Next, OfferId, OfferNonce, ProposedCall,
        Resolution, RuntimeEngine, SanitizerSubject, TrajectoryId, audience_wire, engine_id, remedy_instruction,
        remedy_lines, terminal_safe,
    };
    use crate::consult::{AnnotationAnswer, HistoryEntry, RequiredAudienceAnswer, SanitizerPoint, WireAudience};
    use appa_engine::contract::{AudienceRequirement, HistoryRequirement, RecipientSpec};
    use appa_engine::fact::{EffectKind, EffectSet};
    use appa_engine::label::{Audience, DeclaredAudience, ReaderId, Trust};
    use appa_engine::names::{AnnotatorName, MarkName};
    use appa_engine::plan::{ExecutableRemedyPlan, PlanId, PlannedBlock, RemedyPlan, RemedyStep};
    use appa_engine::value::{RawResultDigest, ToolName, ValueBody};

    #[test]
    fn a_sanitizer_consult_names_its_point_and_the_tool_the_value_belongs_to() {
        let policy = appa_policy::Config::from_toml_str(
            r#"
                version = 2
                [deployment]
                context_control = true
                [[tool]]
                name = "fetch"
                parameters = { type = "object", properties = { url = { type = "string" } }, required = ["url"] }
                delta = {}
                [[sanitizer]]
                name = "scrub"
                on = ["tool_input", "tool_output"]
                permits = { audience = { from = ["hr"], to = ["public"] } }
            "#,
        )
        .expect("the sanitizer policy compiles");
        let engine = RuntimeEngine::from_policy(&policy);
        let scrub = appa_engine::names::SanitizerName::new("scrub");
        let request = |subject: SanitizerSubject<'_>| match engine.sanitizer_request(
            &scrub,
            RawResultDigest::of(b"raw"),
            ValueBody::new("raw"),
            subject,
        ) {
            ExternalRequest::Sanitizer {
                declaration, artifact, ..
            } => (declaration, artifact),
            other => panic!("a sanitizer request, got {other:?}"),
        };

        let call = engine
            .engine
            .resolve_call(ToolName::new("fetch"), br#"{"url":"https://example.test"}"#)
            .expect("the call resolves");
        let (declaration, artifact) = request(SanitizerSubject::Input { call: &call });
        assert_eq!(declaration.on, SanitizerPoint::ToolInput);
        assert_eq!(artifact.tool.as_deref(), Some("fetch"));
        assert_eq!(artifact.body, "raw");
        let parameters = declaration.parameters.expect("a rewrite carries the contract's schema");
        assert_eq!(parameters["required"], serde_json::json!(["url"]));

        let (declaration, artifact) = request(SanitizerSubject::Output {
            tool: Some(ToolName::new("fetch")),
        });
        assert_eq!(declaration.on, SanitizerPoint::ToolOutput);
        assert_eq!(artifact.tool.as_deref(), Some("fetch"));
        assert_eq!(declaration.parameters, None, "a result's schema is nobody's to satisfy");

        let (declaration, artifact) = request(SanitizerSubject::Output { tool: None });
        assert_eq!(declaration.on, SanitizerPoint::ToolOutput);
        assert_eq!(artifact.tool, None, "a child return originates from no tool");
    }

    fn opened_view(engine: &RuntimeEngine, trajectory: &TrajectoryId) -> EngineView {
        let opening = engine.root_opening(trajectory, b"policy");
        engine.validated(opening, trajectory, 1).expect("the opening validates")
    }

    /// The annotated-tool policy: one Annotator producing `lookup`'s semantics per call, and
    /// one authority able to clear the mark a produced annotation may require.
    fn annotator_policy() -> appa_policy::Config {
        appa_policy::Config::from_toml_str(
            r#"
                version = 2
                [[annotator]]
                name = "classifier"
                [[tool]]
                name = "lookup"
                description = "Looks one record up."
                annotator = "classifier"
                [[authority]]
                name = "reviewer"
                [authority.permits]
                attention = ["privacy-review"]
            "#,
        )
        .expect("the annotated-tool policy compiles")
    }

    fn annotator_engine(policy: &appa_policy::Config) -> RuntimeEngine {
        RuntimeEngine::from_policy(policy)
    }

    #[test]
    fn a_recorded_annotation_answers_the_re_proposal_without_a_consult() {
        // The reviewer can clear the mark the produced annotation requires, so the annotated
        // call blocks with an offer that stands for the re-proposal.
        let policy = annotator_policy();
        let engine = annotator_engine(&policy);
        let trajectory = TrajectoryId("t".to_string());
        let opening = engine.root_opening(&trajectory, b"policy");
        let view = engine
            .validated(opening.clone(), &trajectory, 1)
            .expect("the opening validates");
        let call = ProposedCall {
            tool: "lookup".to_string(),
            arguments: serde_json::value::RawValue::from_string(r#"{"id": 7}"#.to_string()).expect("valid JSON"),
        };
        let propose = |view: &EngineView, evidence: Vec<ExternalEvidence>| {
            engine
                .handle(
                    view,
                    &trajectory,
                    EngineEvent::ModelResponse {
                        call: call.clone(),
                        evidence,
                        entropy: OfferNonce([7u8; 32]),
                        spawn: false,
                    },
                )
                .expect("the proposal is handled")
        };

        let first = propose(&view, Vec::new());
        let asked = match &first.then {
            Next::ResolveExternal(requests) => match requests.as_slice() {
                [ExternalRequest::Annotation { annotator, call, .. }] => {
                    assert_eq!(annotator, "classifier");
                    *call
                }
                other => panic!("the first proposal consults the annotator once, not {other:?}"),
            },
            other => panic!("an unannotated call must consult, not {other:?}"),
        };
        assert!(
            first.append.is_none(),
            "nothing is decided before the annotator answers"
        );

        let answer = AnnotationAnswer {
            delta_trust: Some("trusted".to_string()),
            delta_audience: Some(WireAudience::Public),
            required_trust: None,
            required_audience: None,
            history: Vec::new(),
            attention: vec!["privacy-review".to_string()],
            emits: Vec::new(),
        };
        let decided = propose(
            &view,
            vec![ExternalEvidence::Annotation {
                annotator: "classifier".to_string(),
                call: asked,
                answer,
            }],
        );
        let recorded = decided.append.expect("the answered proposal is decided and recorded");
        let log = [opening, recorded].concat();
        let view = engine
            .validated(log, &trajectory, 2)
            .expect("the decided log validates");

        let again = propose(&view, Vec::new());
        assert!(
            !matches!(again.then, Next::ResolveExternal(_)),
            "the offer the annotated call was blocked with stands, so its pin answers the re-proposal"
        );
        let owner = engine_id(&trajectory);
        let views = view.views(&owner).expect("the root is opened");
        let resolved = engine
            .engine
            .resolve_call(ToolName::new("lookup"), br#"{"id": 7}"#)
            .expect("the call resolves");
        let declaration = engine
            .engine
            .registry()
            .declaration(&resolved)
            .expect("the call names its declaration");
        let pin = engine
            .annotation_for(&views, declaration, &resolved, &[])
            .expect("the recorded annotation pins without evidence");
        assert_eq!(pin.produced().requires.attention, vec![MarkName::new("privacy-review")]);

        // Evidence for a call the trajectory already annotated is not a second answer: the
        // record outranks it, so the trajectory never pins two answers for one subject.
        let contradicting = ExternalEvidence::Annotation {
            annotator: "classifier".to_string(),
            call: asked,
            answer: AnnotationAnswer {
                delta_trust: Some("suspicious".to_string()),
                delta_audience: None,
                required_trust: None,
                required_audience: None,
                history: Vec::new(),
                attention: Vec::new(),
                emits: Vec::new(),
            },
        };
        let pinned = engine
            .annotation_for(&views, declaration, &resolved, &[contradicting])
            .expect("the recorded annotation pins over contradicting evidence");
        assert_eq!(pinned, pin);
    }

    #[test]
    fn one_annotator_produces_the_complete_annotation_from_the_exact_call() {
        let policy = annotator_policy();
        let engine = annotator_engine(&policy);
        let call = engine
            .engine
            .resolve_call(
                ToolName::new("lookup"),
                serde_json::json!({"nested": {"id": 7}, "deep": true})
                    .to_string()
                    .as_bytes(),
            )
            .expect("the call resolves");
        let declaration = engine
            .engine
            .registry()
            .declaration(&call)
            .expect("the call names its declaration");
        let trajectory = TrajectoryId("t".to_string());
        let view = opened_view(&engine, &trajectory);
        let owner = engine_id(&trajectory);
        let views = view.views(&owner).expect("the root is opened");
        let asked = match engine.annotation_for(&views, declaration, &call, &[]) {
            Err(Resolution(requests)) => match requests.as_slice() {
                [
                    ExternalRequest::Annotation {
                        annotator,
                        call: digest,
                        declaration,
                        args,
                    },
                ] => {
                    assert_eq!(annotator, "classifier");
                    // The annotator maps no inputs, so `args` is the complete call.
                    assert_eq!(
                        args,
                        &serde_json::json!({
                            "name": "lookup",
                            "description": "Looks one record up.",
                            "arguments": {"nested": {"id": 7}, "deep": true},
                        })
                    );
                    // The declaration carries the mandate's complete vocabulary and nothing of
                    // the trajectory: no current label and no call-specific requirements.
                    assert!(declaration.inputs.is_empty());
                    assert_eq!(declaration.trust_ranks, ["suspicious", "trusted"]);
                    assert_eq!(declaration.attention_marks, ["privacy-review"]);
                    assert!(declaration.effects.is_empty());
                    *digest
                }
                other => panic!("expected one annotation consult, got {other:?}"),
            },
            other => panic!("an unannotated call must consult, got {other:?}"),
        };
        assert_eq!(
            asked,
            call.digest(),
            "the consult is keyed on the call's canonical digest"
        );

        let answer = || AnnotationAnswer {
            delta_trust: Some("suspicious".to_string()),
            delta_audience: Some(WireAudience::Public),
            required_trust: Some("trusted".to_string()),
            required_audience: Some(RequiredAudienceAnswer {
                includes: Some(WireAudience::Readers(vec!["support".to_string()])),
                cap: Some(WireAudience::Public),
            }),
            history: vec![HistoryEntry::Excludes("send".to_string())],
            attention: Vec::new(),
            emits: vec!["read".to_string()],
        };
        let pin = engine
            .annotation_for(
                &views,
                declaration,
                &call,
                &[ExternalEvidence::Annotation {
                    annotator: "classifier".to_string(),
                    call: asked,
                    answer: answer(),
                }],
            )
            .expect("a complete answer pins");
        assert_eq!(pin.annotator(), &AnnotatorName::new("classifier"));
        assert_eq!(pin.call(), &call.digest(), "the pin binds the exact call it answered");
        let produced = pin.produced();
        assert_eq!(produced.delta.trust, Some(Trust::new(0)));
        assert_eq!(produced.delta.audience, Some(DeclaredAudience::Public));
        assert_eq!(produced.requires.label.trust_floor, Some(Trust::new(1)));
        assert_eq!(
            produced.requires.label.audience,
            vec![
                AudienceRequirement::Includes(RecipientSpec::Static(DeclaredAudience::restricted([ReaderId::new(
                    "support"
                )]))),
                AudienceRequirement::Cap(DeclaredAudience::Public),
            ]
        );
        assert_eq!(
            produced.requires.history,
            vec![HistoryRequirement::NoPrior(EffectKind::new("send"))]
        );
        assert_eq!(produced.requires.attention, Vec::<MarkName>::new());
        assert_eq!(
            produced.emits,
            EffectSet::new([EffectKind::new("read")]).expect("one kind is no duplicate")
        );

        // An answer given for another call is not evidence for this one either: the annotator
        // is consulted again rather than handed a sibling's annotation.
        let other_call = engine
            .engine
            .resolve_call(
                ToolName::new("lookup"),
                serde_json::json!({"nested": {"id": 8}, "deep": true})
                    .to_string()
                    .as_bytes(),
            )
            .expect("the call resolves");
        assert!(matches!(
            engine.annotation_for(
                &views,
                declaration,
                &other_call,
                &[ExternalEvidence::Annotation {
                    annotator: "classifier".to_string(),
                    call: asked,
                    answer: answer(),
                }],
            ),
            Err(Resolution(_))
        ));
    }

    #[test]
    fn remedies_pair_each_plan_with_its_own_offer() {
        let plan = |id: u32| ExecutableRemedyPlan {
            id: PlanId::new(id),
            steps: vec![RemedyStep::Authorize(appa_engine::names::AuthorityName::new(format!(
                "officer-{id}"
            )))],
            required: vec![],
        };
        let planned = PlannedBlock {
            raw: appa_engine::check::RawBlock {
                requirement_gaps: vec![],
                narrowing: None,
            },
            plans: vec![
                RemedyPlan::Executable(plan(3)),
                RemedyPlan::Executable(plan(5)),
                RemedyPlan::Executable(plan(8)),
            ],
            fork_advice: None,
        };
        let offers = vec![
            (OfferId("offer-for-8".to_string()), PlanId::new(8)),
            (OfferId("offer-for-3".to_string()), PlanId::new(3)),
        ];
        let spelling = super::ReturnSpelling {
            floor: "{}".to_string(),
            ranks: String::new(),
        };
        assert_eq!(
            remedy_lines(&planned, &offers, &spelling),
            vec![
                remedy_instruction(&plan(3), &offers[1].0, &spelling),
                remedy_instruction(&plan(8), &offers[0].0, &spelling),
            ],
            "the plan with no offer is not shown; the rest carry their own offer"
        );
    }

    fn restricted(ids: &[&str]) -> Audience {
        Audience::restricted(ids.iter().map(|id| ReaderId::new((*id).to_string())))
    }

    #[test]
    fn audience_wire_spells_every_reader_shape() {
        use appa_engine::label::{ChainAudience, Clause, GroupRef};
        assert_eq!(audience_wire(&Audience::public()), "public");
        assert_eq!(audience_wire(&Audience::nobody()), "∅");
        assert_eq!(audience_wire(&restricted(&["hr"])), "hr");
        assert_eq!(
            audience_wire(&restricted(&["d@x", "a@x", "c@x", "b@x"])),
            "a@x,b@x,c@x+1",
            "sorted, three shown, the rest counted",
        );
        let symbolic = Audience::of_clauses([
            Clause::new(
                [ChainAudience::Internal],
                [GroupRef::Named(appa_engine::names::GroupName::new("finance"))],
                [],
            )
            .expect("a symbolic clause"),
            Clause::new([], [], [ReaderId::new("alice")]).expect("a reader clause"),
        ]);
        assert_eq!(
            audience_wire(&symbolic),
            "alice ∩ internal,@finance",
            "clauses intersect in canonical order; each clause unions its spelled atoms",
        );
    }

    #[test]
    fn terminal_safe_replaces_control_and_format_characters() {
        assert_eq!(terminal_safe("trusted"), "trusted");
        assert_eq!(terminal_safe("a\u{1b}[31mred"), "a\u{FFFD}[31mred");
        assert_eq!(
            terminal_safe("x\u{202E}rlo\u{200B}z\u{FEFF}"),
            "x\u{FFFD}rlo\u{FFFD}z\u{FFFD}"
        );
        assert_eq!(terminal_safe("tru\u{200B}sted"), "tru\u{FFFD}sted");
        assert_eq!(
            terminal_safe("tru\u{206A}sted"),
            "tru\u{FFFD}sted",
            "the full Cf range replaces"
        );
    }
}
