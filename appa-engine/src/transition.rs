//! The engine's one mutation boundary: the sealed batch and the view it was computed against.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::audience::AudienceEvidence;
use crate::candidate::{CallStage, ConfinedFrom, DerivedCandidate, SanitizerLineage};
use crate::check::{CheckOutcome, Gap, Narrowing};
use crate::contract::ToolAnnotation;
use crate::engine::Engine;
use crate::execute::AuthorityEvidence;
use crate::fact::{
    BoundaryKind, CloseOutcome, EffectSet, Fact, ReturnDerivation, ReturnPolicy, ReturnSanitizer, TrajectoryOpening,
};
use crate::label::{Expansions, Label, MembershipContext};
use crate::names::{AuthorityName, SanitizerName};
use crate::plan::PlannedBlock;
use crate::profile::PolicyRevision;
use crate::projection::{Projection, Views};
use crate::registry::TrustChain;
use crate::value::{
    ChildReturnId, DispatchId, FileBasis, ForkId, Provenance, RawResultDigest, ResolvedCall, TrajectoryId, ValueBody,
};

/// The identity of one complete policy-content payload for one trajectory. Runtime
/// supplies it as a nonce — it is the party that knows a delivery retry from a fresh response —
/// and the engine binds it to the payload, so repeating it returns the recorded decision and
/// reusing it for different content is refused.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProposalBatchId(String);

impl ProposalBatchId {
    pub fn new(id: impl Into<String>) -> ProposalBatchId {
        ProposalBatchId(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The record a decided proposal batch answers a repeat from: the trajectory that decided
/// it, its proposals as the validator resolved them (annotation included, so a repeat is
/// judged on the payload that was decided, never a fresh consult), its spawn mark, and the
/// pinned audience evidence the decision consumed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedBatch {
    pub trajectory: TrajectoryId,
    pub proposals: Vec<ResolvedCall>,
    pub spawn: Option<SpawnMark>,
    pub evidence: AudienceEvidence,
}

/// One model response's complete policy content: every exposed provider-run
/// result, the ordered calls it proposed for one trajectory, and at most one spawn mark — under
/// the identity that makes the act repeatable. Nothing else in a response is policy content, and
/// a response carrying none of these is no engine event at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProposalBatch {
    pub id: ProposalBatchId,
    pub trajectory: TrajectoryId,
    /// The provider-run results this response exposed, in the order it exposed them.
    /// They are admitted before any sibling is checked, because the model has already read them.
    pub provider_results: Vec<ProviderResult>,
    pub proposals: Vec<ProposedCall>,
    /// Which proposal, if any, the runtime marks as the deployment's context-controlled spawn.
    /// Runtime names it — no configuration surface does — and the marked call is checked
    /// and released like any other. The engine refuses the mark where the deployment declares no
    /// context control.
    pub spawn: Option<SpawnMark>,
    /// One fresh 256-bit random value for this act. The engine mixes it into every block
    /// and offer identity it derives here and keeps none of it: entropy is input data, never engine
    /// state. Runtime supplies it per act and allocates no offer identity of its own.
    pub offer_nonce: crate::value::OfferNonce,
    /// The typed evidence the runtime obtained for this act. A batch carrying none is the
    /// ordinary case; the engine asks only when a block turns on a fact.
    pub evidence: Vec<Evidence>,
    /// The pinned audience primitives the runtime gathered for this act: the source answers
    /// and member lookups that answer every atom the engine named in a `MembershipNeeded`
    /// refusal of this same act.
    pub audience: AudienceEvidence,
}

/// One model-directed call as the harness presents it for dispatch: a tool name and
/// untrusted argument bytes representing the value the harness would execute.
/// Only the engine turns this into a [`ResolvedCall`], so no caller can present a
/// payload under a schema the registry does not hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProposedCall {
    pub tool: crate::value::ToolName,
    pub arguments: Vec<u8>,
    /// The complete annotation the runtime obtained for this call where its declaration routes
    /// through an Annotator, with the mandate that authorized it. Payload, not decoration: the
    /// same tool and arguments under a different annotation is a different act. `None` proposes
    /// a statically declared call.
    pub annotation: Option<crate::contract::PinnedAnnotation>,
}

/// One provider-run result the response exposed: the tool the provider ran inside the
/// inference call, and the body the model has already read. There is no dispatch to name — the
/// engine never released it and cannot have.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderResult {
    pub tool: crate::value::ToolName,
    pub body: ValueBody,
}

/// The one proposal of a batch that spawns children: its position among the
/// batch's proposals, and how many children it starts. At most one exists, which is what makes it
/// an `Option` rather than a flag per proposal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SpawnMark {
    index: usize,
    kind: SpawnKind,
}

/// How many children one spawn starts under its single return declaration. A single spawn binds
/// its fork to one child. A fan-out spawn binds none itself: each child it starts gets a member
/// fork of its own, prepared when that child starts, under the same return policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum SpawnKind {
    Single,
    FanOut,
}

impl SpawnMark {
    pub const fn at(index: usize) -> SpawnMark {
        SpawnMark {
            index,
            kind: SpawnKind::Single,
        }
    }

    pub const fn fan_out(index: usize) -> SpawnMark {
        SpawnMark {
            index,
            kind: SpawnKind::FanOut,
        }
    }

    pub const fn index(self) -> usize {
        self.index
    }

    pub const fn kind(self) -> SpawnKind {
        self.kind
    }
}

/// One act the engine decides. A closed enum: an external operational failure is not an
/// event, and neither is a request, user turn, transcript, or host run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineEvent {
    Proposals(ProposalBatch),
    Outcome(ToolReport),
    ChildReturn(ChildReport),
    BindFork(ForkBinding),
    ExecuteOffer(OfferExecution),
}

/// One offer the agent selected, and what the runtime got back from the authorities it names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OfferExecution {
    pub trajectory: TrajectoryId,
    pub offer: crate::value::OfferId,
    pub outcome: OfferOutcome,
    /// The child's return policy the parent declares for a marked spawn's plan: the floor it
    /// chose, and the schema where the plan attests. `None` for every other plan.
    pub return_policy: Option<ReturnPolicy>,
    /// Fresh entropy for the offers this act may surface: a denial re-plans the blocked
    /// call in the same decision, and those plans need identities of their own.
    pub offer_nonce: crate::value::OfferNonce,
    /// Audience primitives this act reads that the offer's record did not pin; an execution
    /// starts from the offer's own pinned evidence, so this is normally empty.
    pub audience: AudienceEvidence,
}

/// What the runtime must resolve before it can execute one live offer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OfferConsult {
    /// A plain acceptance: nobody is consulted. A call-stage settlement that binds an output
    /// sanitizer names it, so the runtime can say whom taking the offer involves.
    Accept {
        sanitizer: Option<SanitizerName>,
    },
    /// No external is consulted. The selected call runs, but its result does not cross into the
    /// trajectory.
    Withhold,
    Authorities {
        call: ResolvedCall,
        required: Vec<crate::plan::RequiredRuling>,
    },
    /// An input-substitution hop: the sanitizer rewrites the arguments of the call this offer
    /// stands on. The runtime derives the sanitizer's input from the call, and annotates the
    /// rewritten call afresh — under the ordered contract its rewritten arguments select —
    /// before executing.
    Rewrite {
        sanitizer: SanitizerName,
        call: ResolvedCall,
    },
    /// An output sanitizer over a value the host withholds: a confined tool result naming
    /// the tool that produced it, or a child return naming none.
    Sanitizer {
        sanitizer: SanitizerName,
        source: RawResultDigest,
        body: ValueBody,
        tool: Option<crate::value::ToolName>,
    },
    Replay(OfferOutcome),
    Stale,
}

/// What the runtime resolved for a selected offer. There is no "no answer" variant: a consult that
/// returns nothing does not resume the act at all, and the offer simply stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OfferOutcome {
    Approved(Vec<AuthorityEvidence>),
    Denied { authority: AuthorityName },
    Derived(Evidence),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OfferFollowUp {
    Approved { call: Box<ResolvedCall> },
    Denied { block: Box<Blocked> },
    Invalidated,
    Staged(Box<Confined>),
    Substituted { block: Box<Blocked> },
    Admitted { value: ValueBody },
}

/// One confined result and the ways to move it. Not a [`Blocked`]: no call is
/// refused here. The dispatch ran, its effects stand, and what waits is the derived value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Confined {
    pub dispatch: DispatchId,
    pub candidate: crate::value::LabeledValue,
    pub residual: Narrowing,
    /// The offer opened for each next-stage plan, paired with the plan it binds: acceptance of
    /// exactly `residual`, and every sanitizer hop that strictly improves the candidate. Runtime
    /// renders these and routes `execute_remedy_plan` by them; it mints none of its own.
    pub offers: Vec<(crate::value::OfferId, crate::plan::PlanId)>,
}

/// The host's child identity for one prepared fork. Idempotent: repeating the same pair
/// appends nothing and answers the same, while naming another child for the fork — or reusing the
/// child for another fork — is refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForkBinding {
    pub fork: ForkId,
    pub child: TrajectoryId,
}

/// One child's return, addressed by the branch it ends and the exact fork that opened
/// it: a child answers only to its own `ForkId`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChildReport {
    pub child: TrajectoryId,
    pub fork: crate::value::ForkId,
    pub submission: ChildSubmission,
    /// Typed evidence the runtime resolved for this exact report: the fork's named
    /// sanitizer's derivation. An external that gave no answer contributes nothing here and
    /// resumes nothing.
    pub evidence: Vec<Evidence>,
    /// The pinned audience primitives the runtime gathered for this act: the fork's
    /// sanitizer's mandate reads them.
    pub audience: AudienceEvidence,
}

/// What the child submitted: one value, or none. The crossing path is derived from the
/// fork's own binding, never selected here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChildSubmission {
    Void,
    Value { body: ValueBody },
}

/// What one child's stop came to. `Merged` is the value the parent holds now — crossed by this
/// act, or crossed before and repeated. `Staged` is the fork's sanitizer's derivation, recorded
/// once and crossing when the child returns exactly it: a host that delivers the child's own
/// words to the parent holds the child until it says them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChildFollowUp {
    Merged { admitted: ValueBody },
    Staged { derived: ValueBody },
    Ended,
    Resolve(EvidenceRequest),
}

/// One released dispatch's outcome, as the runtime observed it. The report names the
/// dispatch the release opened — the engine reads the call it reports on out of the log, so the
/// runtime never re-derives an identity the engine already handed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolReport {
    pub dispatch: DispatchId,
    pub outcome: ToolOutcome,
    /// Typed evidence the runtime resolved for this exact report. An external that gave
    /// no answer contributes nothing here: it is not an outcome, and it resumes no act.
    pub evidence: Vec<Evidence>,
    /// Fresh entropy for the offers this act may surface: a derivation that still narrows
    /// opens its confined stage in the same decision, and those plans need identities of their own.
    pub offer_nonce: crate::value::OfferNonce,
    /// Audience primitives this act reads that the dispatch's record did not pin.
    pub audience: AudienceEvidence,
}

/// How a tool run ended. `Failure` commits no effects; `Indeterminate` commits none and
/// leaves the reservation standing, because the call may have executed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolOutcome {
    Success { body: OutcomeBody },
    Failure,
    FailureWithBody { body: ValueBody },
    Indeterminate,
}

/// What a successful run carried. Effects commit either way; only an available body can
/// go on to admission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutcomeBody {
    Available(ValueBody),
    Unavailable,
}

/// Actual typed evidence from a registered external, bound to the subject it was resolved for.
/// No variant can say "no answer": an external that failed produces no evidence and no
/// engine event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Evidence {
    /// Trusted host pin for the file operation at `position` in a proposal batch.
    File { position: usize, basis: FileBasis },
    /// An output sanitizer's derivation of a withheld value.
    Sanitizer {
        sanitizer: SanitizerName,
        source: RawResultDigest,
        derived: ValueBody,
    },
    /// An input sanitizer's rewrite of a call's arguments, with the answers the runtime obtained
    /// for the rewritten call. Annotation evidence binds the exact canonical call, so a rewrite
    /// of an Annotator-declared tool is annotated afresh whatever declaration it selects; the
    /// act's pinned audience evidence rides the batch and judges the substituted call unchanged.
    Rewrite {
        sanitizer: SanitizerName,
        source: RawResultDigest,
        derived: ValueBody,
        annotation: Option<crate::contract::PinnedAnnotation>,
    },
}

/// What the engine needs before it can finish an act. The runtime resolves it through
/// the component's configured implementation and repeats the same event with the answer attached;
/// no other call resumes the act, and a failure to resolve leaves it exactly where it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EvidenceRequest {
    Sanitizer {
        sanitizer: SanitizerName,
        source: RawResultDigest,
        body: ValueBody,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutcomeFollowUp {
    Closed {
        admitted: Option<ValueBody>,
        withheld: bool,
    },
    Resolve(EvidenceRequest),
    Staged(Box<Confined>),
}

/// One released call: the dispatch the engine opened for it, and the canonical call to invoke.
/// The runtime never re-derives the identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Released {
    pub dispatch: DispatchId,
    pub call: ResolvedCall,
    /// The fork this release prepared, when the batch marked it as the spawn. The
    /// runtime carries it until the harness names the child, then binds the two.
    pub fork: Option<ForkId>,
}

/// One proposal of a repeated batch whose dispatch has already been invoked. It cannot be
/// re-released — that would run the tool a second time — so the repeat hears what the log records
/// for it instead of hearing nothing at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settled {
    pub dispatch: DispatchId,
    pub call: ResolvedCall,
    pub outcome: SettledOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettledOutcome {
    Closed { admitted: Option<ValueBody> },
    Confined,
}

/// One refused call and the remedies that would lift it. The engine owns the block and its plans;
/// rendering them for the model is runtime feedback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Blocked {
    pub call: ResolvedCall,
    pub block: PlannedBlock,
    pub block_id: crate::value::BlockId,
    /// The kind of spawn a marked spawn's block holds, or `None` for an ordinary call.
    pub spawn: Option<SpawnKind>,
    /// The offer opened for each engine-side plan, paired with the plan it binds. Runtime renders
    /// these to the model and routes `execute_remedy_plan` by them; it mints none of its own.
    pub offers: Vec<(crate::value::OfferId, crate::plan::PlanId)>,
}

/// What the runtime does after appending the decision's batch. Delivery vocabulary —
/// transports, placeholders, transcript shape — stays outside the engine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FollowUp {
    Outcome(OutcomeFollowUp),
    Child(ChildFollowUp),
    Fork {
        child: TrajectoryId,
    },
    Offer(OfferFollowUp),
    Proposals {
        released: Vec<Released>,
        blocked: Vec<Blocked>,
        spent: Vec<ResolvedCall>,
        settled: Vec<Settled>,
    },
    Malformed {
        position: usize,
        error: crate::engine::EngineError,
    },
}

/// Why the boundary refused an event outright. A policy block is a decision, not an error: this
/// means the event cannot be processed at all — a malformed call, a fork the branch rules refuse.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TransitionError {
    #[error(transparent)]
    Call(#[from] crate::engine::EngineError),
    #[error("the view stands at policy revision {view}, later than revision {engine} this engine decides at")]
    NewerRevision {
        view: PolicyRevision,
        engine: PolicyRevision,
    },
    #[error("the engine decides at no policy revision of a deployment")]
    NoRevision,
    #[error("the event names a trajectory this family has not opened")]
    UnopenedTrajectory,
    #[error("the decision does not pass the transition validator: {0}")]
    Invalid(#[from] TransitionRefusal),
    #[error("the act reads symbolic audiences without pinned answers: {}", needed.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "))]
    MembershipNeeded { needed: Vec<crate::label::SymbolicAtom> },
    #[error("no registered audience source serves the ask: {0}")]
    UnroutableAudience(#[from] crate::audience::Unroutable),
    #[error("the act proposes Annotator-declared calls without annotations: {}", annotators.iter().map(|a| a.as_str()).collect::<Vec<_>>().join(", "))]
    AnnotationNeeded {
        annotators: Vec<crate::names::AnnotatorName>,
    },
    #[error("the pinned annotation is not this call's under its declaration: {reason}")]
    ForeignAnnotation { reason: String },
    #[error("the pinned annotation is outside its mandate: {reason}")]
    InvalidAnnotation { reason: String },
    #[error("the act's audience evidence is not admissible: {0}")]
    ForeignEvidence(#[from] crate::audience::EvidenceRefusal),
    #[error(transparent)]
    Plan(#[from] crate::execute::PlanError),
    #[error("no offer of this family carries that identity")]
    UnknownOffer,
    #[error("the offer belongs to another trajectory of this family")]
    OfferElsewhere,
    #[error("the offer's policy basis has moved, so it is stale")]
    StaleOffer,
    #[error("this offer already reached a different terminal outcome")]
    TerminalOffer,
    #[error("the offer's plan assigns no such authority")]
    UnassignedAuthority,
    #[error("the reported outcome is not one this plan's execution produces")]
    PlanOutcomeMismatch,
    #[error("the evidence does not derive this offer's candidate through the sanitizer its plan names")]
    EvidenceMismatch,
    /// A second approval for a call that already carries a current one. Proposing the call
    /// releases the approval that stands; approving it again would leave the release choosing
    /// between two plans the agent selected separately.
    #[error("this exact call already carries a current approval: propose it to release it")]
    ApprovalPending,
    /// The batch identity is already bound to other policy content, or to another trajectory.
    /// Two different acts under one identity would make the log's decision boundaries
    /// unreadable, so neither is decided.
    #[error("this proposal batch id is already bound to different policy content")]
    BatchIdentityConflict,
    #[error("a proposal batch carries at least one proposal or exposed provider-run result")]
    EmptyBatch,
    #[error("no dispatch of this family was opened under that identity")]
    UnknownDispatch,
    #[error("the report contradicts the observation this dispatch already checkpointed")]
    ObservationMismatch,
    #[error("a failure carries a body only for a call that touches a tracked file")]
    FailureBodyOutsideFile,
    #[error(
        "this dispatch closed as indeterminate and observed nothing, so a later report has no observation to check against"
    )]
    ClosedUnobserved,
    #[error("the dispatch recorded success: a failure or indeterminate outcome contradicts it")]
    ContradictedSuccess,
    #[error("the bound output sanitizer does not apply to this result")]
    SanitizerUnapplicable,
    #[error("this trajectory was never forked, so it has no return channel")]
    NotForked,
    #[error("this branch already ended its errand")]
    BranchEnded,
    #[error("this deployment does not control child context, so no proposal can be marked as its spawn")]
    SpawnUncontrolled,
    #[error("the spawn mark names no proposal of this batch")]
    SpawnMarkOutOfRange,
    #[error("the binding names no live prepared fork")]
    UnbindableFork,
    #[error("the return does not address the fork that opened this child")]
    ReturnForkMismatch,
    #[error("a fork takes an unused child identity")]
    ChildAlreadyUsed,
    #[error("the submission does not fit the fork's return shape: {0}")]
    ReturnShapeMismatch(crate::shape::ReturnMismatch),
    /// The child's return stands below the floor its parent set: the parent will not receive
    /// it, so the child keeps it. Returning nothing is always open to the child.
    #[error("the return stands below the floor the parent set for this child")]
    ReturnBelowFloor { floor: Label },
    /// The parent declared that this child returns nothing: only an empty stop ends it.
    #[error("the parent declared that this child returns nothing")]
    ReturnVoid,
    #[error(transparent)]
    ReturnPolicy(#[from] crate::plan::ReturnPolicyRefusal),
}

impl From<crate::label::MembershipNeeded> for TransitionError {
    fn from(needed: crate::label::MembershipNeeded) -> TransitionError {
        TransitionError::MembershipNeeded { needed: needed.needed }
    }
}

impl From<crate::label::MembershipNeeded> for TransitionRefusal {
    fn from(needed: crate::label::MembershipNeeded) -> TransitionRefusal {
        TransitionRefusal::UnansweredDecision { needed: needed.needed }
    }
}

/// One engine interaction's outcome: the sealed batch to append against its basis
/// revision, and the one typed follow-up package. The runtime appends the whole batch
/// before it performs any follow-up item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineDecision {
    pub append: Option<ValidatedFactBatch>,
    pub follow_up: FollowUp,
}

/// A batch the engine validated and sealed. The runtime treats it as opaque: it performs the
/// whole-batch append and advances its [`EngineView`] with it, and never reconstructs engine work
/// by reading the facts.
///
/// The batch is stamped with the policy revision that decided it, and carries that revision's
/// trust chain: the ranks its labels index. A batch holding no record decides nothing, and
/// carries no stamp, as the store derives none for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedFactBatch {
    facts: Vec<Fact>,
    basis: u64,
    revision: Option<PolicyRevision>,
    chain: TrustChain,
    family: TrajectoryId,
}

impl ValidatedFactBatch {
    /// Seal a validated batch. Crate-private on purpose: every call site is an engine transition
    /// that has already run the facts through the [`Sequence`] validator.
    pub(crate) fn seal(facts: Vec<Fact>, basis: u64, engine: &Engine, family: TrajectoryId) -> ValidatedFactBatch {
        // A batch holding no record carries no engine decision, so no revision judged it: the
        // store derives its stamp from its first record and a replay passes it unjudged.
        ValidatedFactBatch {
            basis,
            revision: engine.revision().filter(|_| !facts.is_empty()),
            chain: engine.registry().trust_chain().clone(),
            facts,
            family,
        }
    }

    /// The compare-and-swap basis: the count of accepted batches the decision was
    /// computed against.
    pub fn basis(&self) -> u64 {
        self.basis
    }

    /// The policy revision that decided this batch, and that a replay judges it under; `None`
    /// for a batch holding no record.
    pub fn revision(&self) -> Option<PolicyRevision> {
        self.revision
    }

    pub(crate) fn family(&self) -> &TrajectoryId {
        &self.family
    }

    /// The facts to append, for the store that persists them. Reading them to reconstruct
    /// released work or feedback is forbidden; the decision's follow-up carries that.
    pub fn facts(&self) -> &[Fact] {
        &self.facts
    }

    /// Serialization removes the seal: what crosses to storage is the plain records,
    /// and what comes back is untrusted until it passes the validator again.
    pub fn into_unsealed(self) -> Vec<Fact> {
        self.facts
    }
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum ViewMismatch {
    #[error("the batch was computed against revision {batch:?} but the view stands at {view:?}")]
    Stale { view: u64, batch: u64 },
    #[error("the batch belongs to another trajectory family")]
    ForeignFamily,
    #[error("the batch was decided at policy revision {batch}, earlier than revision {view} the view stands at")]
    RevisionRegressed {
        view: PolicyRevision,
        batch: PolicyRevision,
    },
    #[error("the batch names no policy revision, or the engine decides at none")]
    Unstamped,
}

/// Why [`crate::Engine::admit_batch`] did not advance a view: the batch's records failed the
/// validator, or the view is not one this engine built.
#[derive(Debug, thiserror::Error)]
pub enum BatchRefusal {
    #[error(transparent)]
    Transition(#[from] TransitionRefusal),
    #[error(transparent)]
    View(#[from] ViewMismatch),
}

/// The engine's derived working picture of one family log: the validated records and
/// the projection built from them. Opaque and disposable — the runtime stores it for the next
/// event, but every constructor and mutator here belongs to the engine.
///
/// Equality is equality of the derived picture: a view advanced batch by batch equals the view
/// a replay of the same log builds, because both reach the log through the one fold.
///
/// A view knows the policy revision of its last batch, its stamp, and the trust chain its labels
/// index. A batch decided at a later revision whose chain names other ranks crosses the view's
/// labels into that chain before its records fold ([`crate::label::TrustCrossing`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineView {
    projection: Projection,
    stamp: Option<PolicyRevision>,
    chain: TrustChain,
    family: TrajectoryId,
}

impl EngineView {
    /// Take the projection a [`Sequence`] validated under `engine`. Crate-private: the public
    /// entries are `Engine::view` and `Engine::replay`, which run that validation.
    pub(crate) fn validated(projection: Projection, engine: &Engine, family: TrajectoryId) -> EngineView {
        EngineView {
            projection,
            stamp: engine.revision(),
            chain: engine.registry().trust_chain().clone(),
            family,
        }
    }

    pub(crate) fn projection(&self) -> &Projection {
        &self.projection
    }

    /// The policy revision this view stands at: that of the last stamped batch it folded, or a
    /// later one an engine deciding at it crossed the view to.
    pub fn stamp(&self) -> Option<PolicyRevision> {
        self.stamp
    }

    /// Move past batches that fold nothing: host observations alone, or a stamp carried over. A
    /// later stamp crosses the family basis to its revision, as a folded batch's does.
    pub(crate) fn pass(&mut self, revision: Option<PolicyRevision>, position: u64) {
        if let (Some(revision), Some(stamp)) = (revision, self.stamp)
            && revision > stamp
        {
            self.projection.cross_revision();
        }
        self.stamp = revision.or(self.stamp);
        self.projection.set_position(position);
    }

    /// This view as `engine` decides on it, standing at the same position: its labels carried
    /// into `engine`'s trust chain, and — where `engine` decides at a later revision than the
    /// view stands at — its family basis advanced to that revision, so no offer or approval an
    /// earlier revision recorded is current. Borrowed unchanged when neither moves. A view never
    /// crosses back to an earlier revision; the engine refuses to decide on it instead.
    pub(crate) fn crossed_to<'v>(&'v self, engine: &Engine) -> std::borrow::Cow<'v, EngineView> {
        let arriving = engine.registry().trust_chain();
        let crossing = self.chain.crossing_to(arriving);
        let later = engine
            .revision()
            .filter(|revision| self.stamp.is_some_and(|stamp| stamp < *revision));
        if crossing.is_none() && later.is_none() {
            return std::borrow::Cow::Borrowed(self);
        }
        let mut crossed = self.clone();
        if let Some(crossing) = crossing {
            crossed.projection.cross(&crossing);
            crossed.chain = arriving.clone();
        }
        if let Some(revision) = later {
            crossed.projection.cross_revision();
            crossed.stamp = Some(revision);
        }
        std::borrow::Cow::Owned(crossed)
    }

    /// The reader this family acts for, as its opening pinned it.
    pub fn principal(&self) -> Option<&crate::label::ReaderId> {
        self.projection.principal()
    }

    /// The validated views of one trajectory in this family, for the runtime's reads. `None` for a
    /// trajectory this family never opened — a root without its opening record, a child no fork
    /// bound — so a host-supplied id cannot read a fold nothing seeded.
    pub fn views<'a>(&'a self, trajectory: &'a TrajectoryId) -> Option<crate::projection::Views<'a>> {
        self.projection
            .is_opened(trajectory)
            .then(|| self.projection.view(trajectory))
    }

    /// Which trajectory surfaced this offer, anywhere in the family.
    pub fn offer_trajectory(&self, offer: &crate::value::OfferId) -> Option<&TrajectoryId> {
        self.projection.offer_trajectory(offer)
    }

    /// The root whose log this view was built from. Public because an outer layer
    /// mints identities that must name the log they belong to, and a view is where it learns
    /// which log it is holding.
    pub fn family(&self) -> &TrajectoryId {
        &self.family
    }

    /// How many batches of the family log this view has folded.
    pub fn position(&self) -> u64 {
        self.projection.position()
    }

    /// Advance the cache by a batch the store accepted: the runtime may advance only
    /// from a sealed batch, and only through the engine.
    ///
    /// The batch's facts fold forward through the one fold every build uses — they passed the
    /// transition validator when the engine sealed them, so admitting them a second time would
    /// re-decide a decision already made.
    ///
    /// The batch's basis must be exactly where this view stands, which is what the store's
    /// conditional append already proved before accepting it. A batch computed against
    /// any other position belongs to another view, and applying it would leave records and
    /// position describing different logs. Its revision may not be earlier than the view's stamp:
    /// a family's stamps never decrease. A batch holding no record decides nothing: the view's
    /// chain and stamp stay where they were, as a replay leaves them.
    pub fn advance(&mut self, batch: &ValidatedFactBatch) -> Result<(), ViewMismatch> {
        if batch.family() != &self.family {
            return Err(ViewMismatch::ForeignFamily);
        }
        if batch.basis() != self.position() {
            return Err(ViewMismatch::Stale {
                view: self.position(),
                batch: batch.basis(),
            });
        }
        if batch.facts().is_empty() {
            self.pass(None, batch.basis() + 1);
            return Ok(());
        }
        if let (Some(batch_revision), Some(view_revision)) = (batch.revision, self.stamp)
            && batch_revision < view_revision
        {
            return Err(ViewMismatch::RevisionRegressed {
                view: view_revision,
                batch: batch_revision,
            });
        }
        if batch.revision.is_none() {
            return Err(ViewMismatch::Unstamped);
        }
        if let Some(crossing) = self.chain.crossing_to(&batch.chain) {
            self.projection.cross(&crossing);
            self.chain = batch.chain.clone();
        }
        if let (Some(batch_revision), Some(view_revision)) = (batch.revision, self.stamp)
            && batch_revision > view_revision
        {
            self.projection.cross_revision();
        }
        self.projection.fold_all(batch.facts());
        self.projection.set_position(batch.basis() + 1);
        self.stamp = batch.revision.or(self.stamp);
        Ok(())
    }
}

/// Why a family log's durable opening record cannot be trusted: the validator refuses
/// an opening that is duplicated, names another root, or is inconsistent with the policy this
/// engine was opened under. A log that does not begin with an opening at all is refused as
/// [`TransitionRefusal::Unopened`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum OpeningTransitionRefusal {
    #[error("the family log carries more than one TrajectoryOpened record")]
    Duplicate,
    #[error("the opening record names trajectory {found}, not the root being replayed")]
    WrongTrajectory { found: String },
    #[error("the opening record's declaration does not match the supplied policy's validated profile")]
    ProfileMismatch,
    #[error("the opening record names a deployment and policy revision other than the engine judging it decides at")]
    DeploymentMismatch,
    #[error("the opening record forks the root from its own family")]
    SelfFork,
    #[error("the opening record's principal {found:?} is not an address")]
    MalformedPrincipal { found: String },
    #[error("the opening record's principal is not the one its fork origin carries")]
    ForkPrincipalMismatch,
}

/// Why the transition validator refused a record. One vocabulary for both directions: a
/// candidate batch the engine has just built and a persisted log being replayed pass the same
/// rules, so a refusal always says the same thing — this record cannot follow the ones before it.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TransitionRefusal {
    #[error("the family log does not open with its TrajectoryOpened record")]
    Unopened,
    #[error("the engine decides at no policy revision of a deployment")]
    NoRevision,
    #[error("evidence entry {entry} is neither an inherited pin nor requested by its act")]
    UnrequestedEvidence { entry: String },
    #[error("the family log's opening record cannot be trusted: {0}")]
    Opening(#[from] OpeningTransitionRefusal),
    #[error("a record names a trajectory this family has not opened")]
    ForeignTrajectory,
    #[error("a batch is stamped at a policy revision other than the one judging it")]
    ForeignRevision,
    #[error("dispatched call names unregistered tool {0}")]
    UnknownTool(String),
    #[error("dispatched call payload fails its registered schema: {0}")]
    InvalidPayload(crate::params::ArgumentError),
    #[error("dispatched call digest does not match its persisted tool and arguments")]
    DigestMismatch,
    #[error("a dispatch identity opens twice")]
    DispatchReopened,
    #[error("the dispatch occurrence is not the next one for its call in this trajectory")]
    WrongOccurrence,
    #[error("the dispatch is not open")]
    DispatchNotOpen,
    #[error("the dispatch already recorded its success checkpoint")]
    RepeatCheckpoint,
    #[error("a close contradicts the dispatch's recorded success")]
    ContradictedSuccess,
    #[error("a record commits effects its contract does not declare")]
    EffectsMismatch,
    #[error("value admitted as a provider-run result of {0}, which this deployment does not run in the provider")]
    NotProviderRun(String),
    #[error("a provider-run admission follows the decision of its own batch")]
    AdmissionAfterDecision,
    #[error("a provider-run admission names a slot other than the next one of its batch")]
    WrongAdmissionPosition,
    #[error("a proposal batch identity admits or decides on two trajectories")]
    ForeignAdmission,
    #[error("a provider-run admission stands outside the declaration of the act that read it")]
    UndeclaredAdmission,
    #[error("one batch identity's exposed results are admitted across two decisions")]
    SplitAdmission,
    #[error("an admitted value does not carry the label its source derives")]
    ForgedLabel,
    #[error("a record persists audience evidence or an annotation other than what its operation consumed")]
    ForgedEvidence,
    #[error("a record pins audience evidence its operation could not have consumed: {0}")]
    ForeignEvidence(#[from] crate::audience::EvidenceRefusal),
    #[error(
        "a record claims a decision its pinned evidence cannot answer: {}",
        needed.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
    )]
    UnansweredDecision { needed: Vec<crate::label::SymbolicAtom> },
    #[error("a second value is admitted for one dispatch or child return")]
    RepeatAdmission,
    #[error("admitted value names a dispatch not opened earlier in the log")]
    UnknownDispatch,
    #[error("admitted value names a dispatch of another trajectory")]
    ForeignDispatch,
    #[error("fork record's snapshot is not the parent's frozen basis at that point in the log")]
    ForkBasisMismatch,
    #[error("fork record names a child trajectory the log already used")]
    ChildActiveBeforeFork,
    #[error("a branch forks where the deployment declares no context control")]
    ContextUncontrolled,
    #[error("the spawn mark names no proposal of its batch")]
    SpawnMarkOutOfRange,
    #[error("one release prepares one fork: this identity is already prepared")]
    ForkReprepared,
    #[error("the binding names no prepared fork")]
    UnknownFork,
    #[error("this fork already opened a child")]
    ForkAlreadyBound,
    #[error("the spawn that prepared this fork recorded a failure, so it can open no child")]
    SpawnFailed,
    #[error("one proposal batch identity is bound to two different decisions")]
    BatchIdentityConflict,
    #[error("a decision record claims a release its log never opened")]
    UnbackedDecision,
    #[error("a decision released a call the check refuses, or refused one it allows")]
    MisdecidedBatch,
    #[error("an approval's return policy is not one its plan declares, or a fork's is not the one its release spent")]
    UnbackedReturnPolicy,
    #[error("a crossing on an attested fork carries a body its stored return shape does not admit")]
    ReturnShapeViolation,
    #[error("a return record stands below the floor the parent set for its child")]
    ReturnBelowFloor,
    #[error("a return record crosses a fork whose parent declared no return")]
    ReturnVoid,
    #[error("a return or fork record names a branch that has already ended")]
    BranchEnded,
    #[error("a return record names a trajectory that was never forked")]
    NotForked,
    #[error("a return record's identity is not the next one for its child")]
    WrongReturnIdentity,
    #[error("a return lifecycle record is not one this state produces")]
    ReturnRecordMismatch,
    #[error("a recorded acceptance is not the narrowing its admission folds")]
    AcceptanceMismatch,
    #[error("a crossing ended its branch without being admitted and merged into the parent")]
    UnmergedCrossing,
    #[error("a derivation names other bytes than the dispatch's checkpoint observed")]
    ObservationMismatch,
    #[error("a record names a child return the log does not carry")]
    UnknownReturn,
    #[error("no authority registered as {0}")]
    UnknownAuthority(String),
    #[error("no sanitizer registered as {0}")]
    UnknownSanitizer(String),
    #[error("a derivation names a sanitizer this dispatch is not bound to, or one it cannot apply")]
    SanitizerUnapplicable,
    #[error("a ruling, acceptance or sanitizer binding names a dispatch that never opened")]
    DanglingRemedy,
    #[error("a sanitizer settled a result the log never admitted")]
    UnadmittedDerivation,
    #[error("a staged confined result's dispatch closes only with the admission that takes its candidate")]
    StagedClose,
    /// A record moved policy state its decision never declared. Replay would then reach
    /// a different basis than the decision stamped onto its own offers.
    #[error("the record advances a policy basis its decision did not declare")]
    UndeclaredAdvance,
    #[error("the offer identity is already open")]
    OfferReopened,
    #[error("the offer does not belong to the decision that surfaced it")]
    ForeignOffer,
    #[error("the offer's plan is not one its block offers")]
    UnbackedOffer,
    #[error("the recorded policy basis is not the one its subject stands at")]
    ForgedBasis,
    #[error("a block's offers do not cover every plan its menu carries")]
    IncompleteMenu,
    #[error("a block offers one of its plans twice")]
    PlanReoffered,
    #[error("the offers of one surfacing name more than one block")]
    SplitBlock,
    #[error("a block identity is surfaced twice")]
    BlockReused,
    #[error("a declared policy-basis advance is not backed by the decision's records")]
    UnbackedAdvance,
    #[error("a record names an offer this log never opened")]
    UnknownOffer,
    #[error("a record ends or approves an offer that already ended")]
    OfferEnded,
    #[error("the approval does not realize the plan its offer bound")]
    UnbackedApproval,
    #[error("this offer already prepared its call approval, or this call already has one")]
    ApprovalRepeated,
    #[error("an accepted offer's batch does not carry the record its plan promised")]
    UndischargedAcceptance,
    #[error("the denial record is not backed by a denial of this authority for this call")]
    UnbackedDenial,
    #[error("a record spends a call approval this log never prepared")]
    UnknownApproval,
    #[error("a record takes a call candidate this log never staged")]
    UnknownCandidate,
    #[error("the record spends an offer or approval that is no longer current")]
    StaleSpend,
}

struct PendingRelease {
    dispatch: DispatchId,
    subject: crate::basis::SubjectKey,
    call: ResolvedCall,
    prepares_fork: Option<SpawnKind>,
    evidence: AudienceEvidence,
    next: ReleasePart,
    /// Where the sequence goes once a taken candidate is recorded.
    after_take: ReleasePart,
}

#[derive(Clone, PartialEq, Eq)]
enum ReleasePart {
    /// The staged derivation this release takes, recorded before anything else it owes.
    Take(crate::basis::SubjectKey),
    Consumption(crate::value::OfferId),
    Remedy(crate::value::OfferId),
    Opening,
    Fork,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Obligation {
    Free,
    Decided,
    Consuming(crate::value::OfferId),
}

#[derive(Default, PartialEq, Eq)]
struct Remedy {
    rulings: Vec<(AuthorityName, Vec<Gap>)>,
    plans: BTreeSet<crate::plan::PlanId>,
    reviewed: Vec<crate::execute::AuthorityReview>,
    acceptance: Option<Narrowing>,
    output: Option<crate::plan::OutputRemedy>,
    contribution: Option<Label>,
    evidence: Option<AudienceEvidence>,
}

impl Remedy {
    fn pin_evidence(&mut self, evidence: &AudienceEvidence) -> Result<(), TransitionRefusal> {
        match &self.evidence {
            None => self.evidence = Some(evidence.clone()),
            Some(pinned) if pinned == evidence => {}
            Some(_) => return Err(TransitionRefusal::ForgedEvidence),
        }
        Ok(())
    }
}

/// The one sequential transition validator. It admits records one at a time against the
/// state the records before them built, and folds each admitted record into that state through
/// [`Projection::fold`] — the same fold a held view advances by, so validation and projection can
/// never describe different logs.
pub(crate) struct Sequence<'a> {
    engine: &'a Engine,
    family: TrajectoryId,
    projection: Projection,
    pending: std::collections::VecDeque<PendingRelease>,
    remedies: BTreeMap<DispatchId, Remedy>,
    derived: BTreeMap<DispatchId, crate::value::LabeledValue>,
    candidate_accepted: BTreeMap<DispatchId, Narrowing>,
    /// Crossings recorded and not yet admitted into the parent, and merges not yet made: a
    /// crossing ends the child, so a log that records one without folding it into the parent
    /// would leave the parent reading a label the crossing never restricted.
    crossed: BTreeMap<ChildReturnId, Crossing>,
    /// The return policy each marked spawn's spent approval declared, owed to the fork its
    /// release prepares next.
    spawn_policies: BTreeMap<DispatchId, ReturnPolicy>,
    menu: Option<MenuDebt>,
    declared: Option<Declaration>,
    admitting: Option<ProposalBatchId>,
    deciding: Option<ProposalBatchId>,
    owing: Option<Owed>,
    /// Interior mutability because contributions come from `&self` validation helpers.
    audit: std::cell::RefCell<crate::audience::ActLedger>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Owed {
    Approval(crate::value::OfferId),
    Derived(crate::value::OfferId),
    Accepted(crate::value::OfferId),
}

impl Owed {
    fn offer(self) -> crate::value::OfferId {
        match self {
            Owed::Approval(offer) | Owed::Derived(offer) | Owed::Accepted(offer) => offer,
        }
    }
}

struct Declaration {
    act: crate::basis::DecidedAct,
    declared: crate::basis::BasisAdvance,
    owed: crate::basis::BasisAdvance,
}

fn effect_advance(moved: bool) -> crate::basis::BasisAdvance {
    match moved {
        true => crate::basis::BasisAdvance::family(),
        false => crate::basis::BasisAdvance::default(),
    }
}

fn annotation_or_unknown<'r>(
    registry: &'r crate::registry::Registry,
    call: &'r ResolvedCall,
) -> Result<std::borrow::Cow<'r, ToolAnnotation>, TransitionRefusal> {
    registry
        .annotation_of(call)
        .ok_or_else(|| TransitionRefusal::UnknownTool(call.tool().as_str().to_string()))
}

fn sanitized_label(derived: Result<Option<Label>, crate::label::MembershipNeeded>) -> Result<Label, TransitionRefusal> {
    match derived {
        Ok(Some(label)) => Ok(label),
        Ok(None) => Err(TransitionRefusal::SanitizerUnapplicable),
        Err(needed) => Err(needed.into()),
    }
}

fn settled(menu: Option<MenuDebt>) -> Result<(), TransitionRefusal> {
    match menu {
        Some(debt) if debt.offered.len() != debt.menu.len() => Err(TransitionRefusal::IncompleteMenu),
        _ => Ok(()),
    }
}

struct MenuDebt {
    subject: crate::basis::SubjectKey,
    block: crate::value::BlockId,
    call: crate::value::CanonicalDigest,
    menu: Vec<crate::plan::ExecutableRemedyPlan>,
    offered: BTreeSet<crate::plan::PlanId>,
    evidence: AudienceEvidence,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Crossing {
    Recorded,
    Admitted,
    Merged,
}

impl<'a> Sequence<'a> {
    /// A validator over an empty log of `family` at `revision`. Nothing is a member yet: the
    /// family's opening record is what admits the root.
    pub(crate) fn empty(engine: &'a Engine, family: &TrajectoryId, revision: u64) -> Sequence<'a> {
        Sequence {
            engine,
            family: family.clone(),
            projection: Projection::empty(revision),
            pending: std::collections::VecDeque::new(),
            remedies: BTreeMap::new(),
            derived: BTreeMap::new(),
            candidate_accepted: BTreeMap::new(),
            crossed: BTreeMap::new(),
            spawn_policies: BTreeMap::new(),
            declared: None,
            admitting: None,
            deciding: None,
            menu: None,
            owing: None,
            audit: std::cell::RefCell::default(),
        }
    }

    /// A validator standing where `view` stands, for admitting the candidate records of one
    /// decision. The view's own records passed these rules already, so the state is resumed
    /// rather than re-judged.
    pub(crate) fn resuming(engine: &'a Engine, view: &EngineView) -> Sequence<'a> {
        Sequence {
            engine,
            family: view.family().clone(),
            projection: view.projection().clone(),
            pending: std::collections::VecDeque::new(),
            remedies: BTreeMap::new(),
            derived: BTreeMap::new(),
            candidate_accepted: BTreeMap::new(),
            crossed: BTreeMap::new(),
            spawn_policies: BTreeMap::new(),
            declared: None,
            admitting: None,
            deciding: None,
            menu: None,
            owing: None,
            audit: std::cell::RefCell::default(),
        }
    }

    pub(crate) fn admit(&mut self, fact: &Fact) -> Result<(), TransitionRefusal> {
        self.member(fact)?;
        let discharges_approval = self.discharge_owed(fact)?;
        let released = self.obliged(fact)?;
        if !matches!(fact, Fact::OfferOpened { .. }) {
            settled(self.menu.take())?;
        }
        let implied = self.implied_advance(fact);
        match fact {
            // Judged in full by `member`, which admits it only as the family's first record.
            Fact::TrajectoryOpened(_) => {}
            Fact::BasisAdvanced {
                act, advance, revision, ..
            } => {
                // A batch is decided at the revision judging it.
                if *revision != self.engine.revision() {
                    return Err(TransitionRefusal::ForeignRevision);
                }
                self.declare(act, advance)?
            }
            Fact::OfferOpened {
                trajectory,
                offer,
                block,
                act,
                call,
                subject,
                plan,
                basis,
                evidence,
            } => self.offer_opened(trajectory, offer, block, act, call, subject, plan, basis, evidence)?,
            Fact::OfferAccepted { trajectory, offer } => self.offer_accepted(trajectory, offer)?,
            Fact::CallApprovalConsumed { trajectory, offer, .. } => self.approval_consumed(trajectory, offer)?,
            Fact::CandidateConsumed {
                trajectory, subject, ..
            } => self.candidate_consumed(trajectory, subject)?,
            Fact::OfferDenied {
                trajectory,
                offer,
                authority,
            } => self.offer_denied(trajectory, offer, authority)?,
            Fact::OfferInvalidated { trajectory, offer } => {
                ending_offer(&self.projection.view(trajectory), trajectory, offer)?;
            }
            Fact::CallApproved {
                trajectory,
                offer,
                call,
                plan,
                acceptance,
                rulings,
                output,
                return_policy,
                basis,
                evidence,
            } => {
                self.call_approved(
                    trajectory,
                    offer,
                    call,
                    plan,
                    acceptance,
                    rulings,
                    output,
                    return_policy,
                    basis,
                    evidence,
                )?;
                // An approval stands only as what its accepted offer owes.
                if !discharges_approval {
                    return Err(TransitionRefusal::UnbackedApproval);
                }
            }
            Fact::ProposalBatchDecided {
                trajectory,
                batch,
                proposals,
                spawn,
                released,
                evidence,
            } => self.decided(trajectory, batch, proposals, *spawn, released, evidence)?,
            Fact::DispatchOpened {
                trajectory,
                dispatch,
                tool,
                arguments,
                proposed_label,
                receiving,
                proposed_effects,
                annotation,
                file_basis,
                evidence,
                terms,
                ..
            } => {
                let call = crate::projection::opened_call(tool, arguments, annotation, file_basis);
                self.opened(trajectory, dispatch, &call, released, evidence)?;
                self.dispatch_opened_contract(trajectory, &call, proposed_label, receiving, proposed_effects, terms)?;
            }
            Fact::DispatchSucceeded {
                trajectory,
                dispatch,
                effects,
                observed: _,
            } => self.dispatch_succeeded(trajectory, dispatch, effects)?,
            Fact::DispatchClosed {
                trajectory,
                dispatch,
                outcome,
            } => self.dispatch_closed(trajectory, dispatch, outcome)?,
            Fact::ValueAdmitted {
                trajectory,
                value,
                provenance,
            } => self.value_admitted(trajectory, value, provenance)?,
            Fact::CandidateDerived {
                trajectory,
                subject,
                sanitizer,
                derived,
                lineage,
                evidence,
            } => self.candidate_derived(trajectory, subject, sanitizer, derived, lineage, evidence)?,
            Fact::CandidateAccepted {
                trajectory,
                subject,
                offer,
                narrowing,
            } => self.candidate_accepted(trajectory, subject, offer, narrowing)?,
            Fact::Ruling {
                trajectory,
                dispatch,
                plan,
                authority,
                covers,
                reviewed,
                evidence,
            } => {
                self.pending_dispatch(trajectory, dispatch)?;
                self.ruling(dispatch, plan, authority, covers, reviewed, evidence)?;
            }
            Fact::Acceptance {
                trajectory,
                dispatch,
                plan,
                narrowing,
            } => {
                self.pending_dispatch(trajectory, dispatch)?;
                self.land_remedy(dispatch, plan, None)?.acceptance = Some(narrowing.clone());
            }
            Fact::OutputSanitizerBound {
                trajectory,
                dispatch,
                plan,
                sanitizer,
                contribution,
                evidence,
            } => {
                self.pending_dispatch(trajectory, dispatch)?;
                self.output_sanitizer_bound(dispatch, plan, sanitizer, contribution, evidence)?;
            }
            Fact::OutputWithheld {
                trajectory,
                dispatch,
                plan,
            } => {
                self.pending_dispatch(trajectory, dispatch)?;
                self.land_remedy(dispatch, plan, None)?.output = Some(crate::plan::OutputRemedy::Withhold);
            }
            Fact::Denial {
                trajectory,
                digest,
                authority,
            } => self.denial(trajectory, digest, authority)?,
            Fact::ChildReturn {
                trajectory,
                id,
                value,
                derivation,
                evidence,
            } => {
                self.child_return(trajectory, id, value, derivation, evidence)?;
                self.crossed.insert(id.clone(), Crossing::Recorded);
            }
            Fact::ForkPrepared {
                trajectory,
                fork,
                snapshot,
                return_policy,
                kind,
            } => self.fork_prepared(trajectory, fork, snapshot, return_policy, kind, released)?,
            Fact::ForkOpened { trajectory, fork } => self.fork_opened(trajectory, fork)?,
            Fact::Boundary { trajectory, kind } => self.boundary(trajectory, kind)?,
        }
        self.settle_advance(fact, &implied, released)?;
        self.projection.fold(fact);
        Ok(())
    }

    /// Discharges what an accepted offer owes, refusing any other record while it is owed.
    /// Returns whether this record is the approval the offer owed.
    fn discharge_owed(&mut self, fact: &Fact) -> Result<bool, TransitionRefusal> {
        let discharges_approval = matches!(
            (self.owing, fact),
            (Some(Owed::Approval(owed)), Fact::CallApproved { offer, .. }) if offer == &owed
        );
        if let Some(owed) = self.owing {
            match (owed, fact) {
                (_, Fact::OfferInvalidated { trajectory, offer }) => {
                    let views = self.projection.view(trajectory);
                    if let Some(ending) = views.offer(offer) {
                        let spent = views
                            .offer(&owed.offer())
                            .map(|recorded| (&recorded.subject, recorded.basis));
                        if Some((&ending.subject, ending.basis)) != spent {
                            return Err(TransitionRefusal::UndischargedAcceptance);
                        }
                    }
                }
                (Owed::Approval(owed), Fact::CallApproved { offer, .. }) if offer == &owed => self.owing = None,
                (
                    Owed::Derived(owed),
                    Fact::CandidateDerived {
                        derived:
                            DerivedCandidate::Result {
                                from: ConfinedFrom::Offer(offer),
                                ..
                            }
                            | DerivedCandidate::Call { from: offer, .. },
                        ..
                    },
                ) if offer == &owed => self.owing = None,
                (Owed::Accepted(owed), Fact::CandidateAccepted { offer, .. }) if offer == &owed => self.owing = None,
                _ => return Err(TransitionRefusal::UndischargedAcceptance),
            }
        }
        Ok(discharges_approval)
    }

    fn approval_consumed(
        &self,
        trajectory: &TrajectoryId,
        offer: &crate::value::OfferId,
    ) -> Result<(), TransitionRefusal> {
        let views = self.projection.view(trajectory);
        let approval = views.approval(offer).ok_or(TransitionRefusal::UnknownApproval)?;
        let (recorded, subject) = (approval.basis, crate::basis::SubjectKey::Approval(*offer));
        self.may_spend(trajectory, &subject, &recorded)
    }

    fn candidate_consumed(
        &self,
        trajectory: &TrajectoryId,
        subject: &crate::basis::SubjectKey,
    ) -> Result<(), TransitionRefusal> {
        let recorded = self
            .projection
            .view(trajectory)
            .recorded_candidate(subject)
            .ok_or(TransitionRefusal::UnknownCandidate)?
            .generation;
        self.may_take(trajectory, subject, recorded)
    }

    fn dispatch_opened_contract(
        &self,
        trajectory: &TrajectoryId,
        call: &ResolvedCall,
        proposed_label: &Label,
        receiving: &Label,
        proposed_effects: &EffectSet,
        terms: &crate::fact::ReleaseTerms,
    ) -> Result<(), TransitionRefusal> {
        let tool = call.tool();
        // The declaration this revision selects for the call's tool and arguments.
        let entry = self
            .engine
            .registry()
            .declaration(call)
            .ok_or_else(|| TransitionRefusal::UnknownTool(tool.as_str().to_string()))?;
        // The record stores a pin only where an Annotator produced one; a static
        // declaration's annotation is the registry's own, so a record that carries
        // a pin for it — or none for an Annotator-routed tool — is forged. The
        // pin's own binding — its annotator, and the digest of the exact call it
        // judged — is validate_annotation's to hold.
        let checked: std::borrow::Cow<'_, ToolAnnotation> = match (call.annotation(), entry.declared()) {
            (None, Some(compiled)) => std::borrow::Cow::Borrowed(compiled),
            (Some(pinned), None) if Some(pinned.annotator()) == entry.annotator() => {
                std::borrow::Cow::Owned(pinned.tool_annotation(entry, tool))
            }
            _ => return Err(TransitionRefusal::ForgedEvidence),
        };
        if crate::check::validate_annotation(self.engine.registry(), entry, call).is_err() {
            return Err(TransitionRefusal::ForgedEvidence);
        }
        let checked = match checked.bound_to(call.arguments()) {
            Ok(Some(bound)) => std::borrow::Cow::Owned(bound),
            Ok(None) => checked,
            Err(_) => return Err(TransitionRefusal::ForgedLabel),
        };
        let views = self.projection.view(trajectory);
        if proposed_effects != &checked.emits {
            return Err(TransitionRefusal::EffectsMismatch);
        }
        let current = views.current_label();
        if proposed_label != &current.combine(&call.output_label(&checked, &current)) || receiving != &current {
            return Err(TransitionRefusal::ForgedLabel);
        }
        // The terms a release pins are the contract it was checked under, exactly.
        if terms != &crate::fact::ReleaseTerms::of(&checked) {
            return Err(TransitionRefusal::ForgedLabel);
        }
        Ok(())
    }

    fn dispatch_succeeded(
        &self,
        trajectory: &TrajectoryId,
        dispatch: &DispatchId,
        effects: &EffectSet,
    ) -> Result<(), TransitionRefusal> {
        self.declaring(&crate::basis::DecidedAct::Outcome(dispatch.clone()))?;
        let contract = self.open_dispatch_contract(trajectory, dispatch)?;
        if self.projection.view(trajectory).is_succeeded(dispatch) {
            return Err(TransitionRefusal::RepeatCheckpoint);
        }
        if effects != &contract.contract.emits {
            return Err(TransitionRefusal::EffectsMismatch);
        }
        Ok(())
    }

    fn dispatch_closed(
        &self,
        trajectory: &TrajectoryId,
        dispatch: &DispatchId,
        outcome: &CloseOutcome,
    ) -> Result<(), TransitionRefusal> {
        self.closing_act(dispatch)?;
        let contract = self.open_dispatch_contract(trajectory, dispatch)?;
        let views = self.projection.view(trajectory);
        let checkpointed = views.is_succeeded(dispatch);
        match (outcome, checkpointed) {
            (CloseOutcome::Failure | CloseOutcome::FailureWithBody { .. } | CloseOutcome::Indeterminate, true) => {
                Err(TransitionRefusal::ContradictedSuccess)
            }
            (CloseOutcome::FailureWithBody { .. }, false)
                if views
                    .dispatch_call(dispatch)
                    .is_none_or(|call| call.file_basis().is_none()) =>
            {
                Err(TransitionRefusal::ForgedEvidence)
            }
            (CloseOutcome::Success { effects }, true) if effects != &EffectSet::default() => {
                Err(TransitionRefusal::EffectsMismatch)
            }
            (CloseOutcome::Success { effects }, false) if effects != &contract.contract.emits => {
                Err(TransitionRefusal::EffectsMismatch)
            }
            _ => Ok(()),
        }
    }

    /// Lands one remedy record on its dispatch's pending remedy, pinning the evidence it carries.
    fn land_remedy(
        &mut self,
        dispatch: &DispatchId,
        plan: &crate::plan::PlanId,
        evidence: Option<&AudienceEvidence>,
    ) -> Result<&mut Remedy, TransitionRefusal> {
        if let Some(evidence) = evidence {
            self.recorded_expansions(evidence)?;
        }
        let remedy = self.remedies.entry(dispatch.clone()).or_default();
        if let Some(evidence) = evidence {
            remedy.pin_evidence(evidence)?;
        }
        remedy.plans.insert(*plan);
        Ok(remedy)
    }

    fn known_authority(&self, authority: &AuthorityName) -> Result<(), TransitionRefusal> {
        match self.engine.registry().authority(authority) {
            Some(_) => Ok(()),
            None => Err(TransitionRefusal::UnknownAuthority(authority.as_str().to_string())),
        }
    }

    fn ruling(
        &mut self,
        dispatch: &DispatchId,
        plan: &crate::plan::PlanId,
        authority: &AuthorityName,
        covers: &[Gap],
        reviewed: &crate::execute::AuthorityReview,
        evidence: &AudienceEvidence,
    ) -> Result<(), TransitionRefusal> {
        self.known_authority(authority)?;
        let remedy = self.land_remedy(dispatch, plan, Some(evidence))?;
        remedy.reviewed.push(reviewed.clone());
        remedy.rulings.push((authority.clone(), covers.to_vec()));
        Ok(())
    }

    fn output_sanitizer_bound(
        &mut self,
        dispatch: &DispatchId,
        plan: &crate::plan::PlanId,
        sanitizer: &SanitizerName,
        contribution: &Label,
        evidence: &AudienceEvidence,
    ) -> Result<(), TransitionRefusal> {
        if self.engine.registry().sanitizer(sanitizer).is_none_or(|s| !s.on.output) {
            return Err(TransitionRefusal::UnknownSanitizer(sanitizer.as_str().to_string()));
        }
        let remedy = self.land_remedy(dispatch, plan, Some(evidence))?;
        remedy.output = Some(crate::plan::OutputRemedy::Sanitize(sanitizer.clone()));
        remedy.contribution = Some(contribution.clone());
        Ok(())
    }

    fn denial(
        &self,
        trajectory: &TrajectoryId,
        digest: &crate::value::CanonicalDigest,
        authority: &AuthorityName,
    ) -> Result<(), TransitionRefusal> {
        if let Some(open) = &self.declared
            && let crate::basis::DecidedAct::Offer(offer) = &open.act
        {
            let views = self.projection.view(trajectory);
            let selected = views.offer(offer).ok_or(TransitionRefusal::UnknownOffer)?;
            if &selected.call != digest || !selected.plan.names_authority(authority) {
                return Err(TransitionRefusal::UnbackedDenial);
            }
        }
        self.known_authority(authority)
    }

    fn fork_prepared(
        &mut self,
        trajectory: &TrajectoryId,
        fork: &ForkId,
        snapshot: &crate::fact::ForkSnapshot,
        return_policy: &ReturnPolicy,
        kind: &SpawnKind,
        released: Obligation,
    ) -> Result<(), TransitionRefusal> {
        // A member is backed by the fan-out fork its spawn's release prepared, not by a
        // release of its own: it is prepared when its child starts.
        if released == Obligation::Free && matches!(fork, ForkId::Spawn(_)) {
            return Err(TransitionRefusal::UnbackedDecision);
        }
        if !self.engine.registry().profile().context_control() {
            return Err(TransitionRefusal::ContextUncontrolled);
        }
        let views = self.projection.view(trajectory);
        if fork.dispatch().trajectory() != trajectory || views.dispatch_call(fork.dispatch()).is_none() {
            return Err(TransitionRefusal::UnknownDispatch);
        }
        if self.projection.prepared_fork(fork).is_some() {
            return Err(TransitionRefusal::ForkReprepared);
        }
        if views.has_ended(trajectory) {
            return Err(TransitionRefusal::BranchEnded);
        }
        if &views.freeze_basis() != snapshot {
            return Err(TransitionRefusal::ForkBasisMismatch);
        }
        match fork {
            // The policy is the one the release's spent approval declared, and nothing else.
            ForkId::Spawn(dispatch) => {
                if self.spawn_policies.remove(dispatch).as_ref() != Some(return_policy) {
                    return Err(TransitionRefusal::UnbackedReturnPolicy);
                }
            }
            ForkId::Member { spawn, .. } => {
                let backed = self
                    .projection
                    .prepared_fork(&ForkId::of(spawn))
                    .is_some_and(|fan_out| {
                        fan_out.kind == SpawnKind::FanOut
                            && &fan_out.parent == trajectory
                            && &fan_out.return_policy == return_policy
                    });
                if !backed || *kind != SpawnKind::Single {
                    return Err(TransitionRefusal::UnbackedReturnPolicy);
                }
            }
        }
        Ok(())
    }

    fn fork_opened(&self, trajectory: &TrajectoryId, fork: &ForkId) -> Result<(), TransitionRefusal> {
        let preparation = self
            .projection
            .prepared_fork(fork)
            .ok_or(TransitionRefusal::UnknownFork)?;
        // A fan-out fork binds no child itself, and a member binds only the child it names.
        let binds = match fork {
            ForkId::Spawn(_) => preparation.kind == SpawnKind::Single,
            ForkId::Member { child, .. } => child == trajectory,
        };
        if !binds {
            return Err(TransitionRefusal::UnknownFork);
        }
        let views = self.projection.view(trajectory);
        if self.projection.bound_child(fork).is_some() {
            return Err(TransitionRefusal::ForkAlreadyBound);
        }
        if self.projection.is_opened(trajectory) {
            return Err(TransitionRefusal::ChildActiveBeforeFork);
        }
        if views.has_ended(&preparation.parent) {
            return Err(TransitionRefusal::BranchEnded);
        }
        if views.dispatch_failed(fork.dispatch()) {
            return Err(TransitionRefusal::SpawnFailed);
        }
        // A member is seeded at the parent's label as the child starts, so nothing may
        // cross into the parent between its preparation and its binding.
        if matches!(fork, ForkId::Member { .. })
            && self.projection.view(&preparation.parent).freeze_basis() != preparation.snapshot
        {
            return Err(TransitionRefusal::ForkBasisMismatch);
        }
        Ok(())
    }

    /// One opened offer. The identity cannot be re-derived here — it mixes the
    /// act's runtime entropy, which is deliberately not in the log — so what the
    /// validator proves instead is everything the identity is supposed to stand for: the offer is
    /// new, it belongs to the decision that surfaced it, it carries a plan that decision's own
    /// block really offers, and it records the basis its subject actually stands at.
    ///
    /// That last one is the reason offers must be declared. An offer recording a basis its subject
    /// never stood at would be stale from birth or, worse, outlive a change that should have ended
    /// it.
    #[allow(clippy::too_many_arguments)]
    fn offer_opened(
        &mut self,
        trajectory: &TrajectoryId,
        offer: &crate::value::OfferId,
        block: &crate::value::BlockId,
        act: &crate::basis::DecidedAct,
        call: &crate::value::CanonicalDigest,
        subject: &crate::basis::SubjectKey,
        plan: &crate::plan::ExecutableRemedyPlan,
        basis: &crate::basis::PolicyBasis,
        evidence: &AudienceEvidence,
    ) -> Result<(), TransitionRefusal> {
        let views = self.projection.view(trajectory);
        if views.offer(offer).is_some() {
            return Err(TransitionRefusal::OfferReopened);
        }
        match &self.declared {
            Some(open) if &open.act == act => {}
            _ => return Err(TransitionRefusal::UnbackedOffer),
        }
        if let Some(debt) = self.menu.as_mut().filter(|debt| &debt.subject == subject) {
            if debt.block != *block {
                return Err(TransitionRefusal::SplitBlock);
            }
            if debt.call != *call || !debt.menu.contains(plan) {
                return Err(TransitionRefusal::UnbackedOffer);
            }
            // One surfacing reads under one set of pinned answers.
            if &debt.evidence != evidence {
                return Err(TransitionRefusal::ForgedEvidence);
            }
            if basis != &views.basis_for(subject) {
                return Err(TransitionRefusal::ForgedBasis);
            }
            return match debt.offered.insert(plan.id) {
                true => Ok(()),
                false => Err(TransitionRefusal::PlanReoffered),
            };
        }
        // Another subject's block begins: the one before it is done, complete or refused.
        settled(self.menu.take())?;
        if views.block_surfaced(block) {
            return Err(TransitionRefusal::BlockReused);
        }
        let expansions = self.recorded_expansions(evidence)?;
        let menu = self.stage_menu(&views, trajectory, act, call, subject, plan, &expansions)?;
        self.audit_reads(&expansions);
        if !menu.contains(plan) {
            return Err(TransitionRefusal::UnbackedOffer);
        }
        // The post-decision basis, which the declaration admitted just above already applied.
        if basis != &views.basis_for(subject) {
            return Err(TransitionRefusal::ForgedBasis);
        }
        self.menu = Some(MenuDebt {
            subject: subject.clone(),
            block: *block,
            call: *call,
            menu,
            offered: BTreeSet::from([plan.id]),
            evidence: evidence.clone(),
        });
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn stage_menu(
        &self,
        views: &Views,
        trajectory: &TrajectoryId,
        act: &crate::basis::DecidedAct,
        call: &crate::value::CanonicalDigest,
        subject: &crate::basis::SubjectKey,
        plan: &crate::plan::ExecutableRemedyPlan,
        expansions: &Expansions,
    ) -> Result<Vec<crate::plan::ExecutableRemedyPlan>, TransitionRefusal> {
        // The pins of the record the surfacing act continues are inherited evidence.
        match act {
            crate::basis::DecidedAct::Offer(offer) => {
                if let Some(recorded) = views.offer(offer) {
                    self.audit_inherit(&recorded.evidence)?;
                }
            }
            crate::basis::DecidedAct::Outcome(dispatch) => {
                if let Some(pinned) = views.dispatch_evidence(dispatch) {
                    self.audit_inherit(pinned)?;
                }
            }
            _ => {}
        }
        match subject {
            crate::basis::SubjectKey::Call {
                trajectory: subject_trajectory,
                batch,
                ..
            } => {
                if subject_trajectory != trajectory {
                    return Err(TransitionRefusal::ForeignOffer);
                }
                if !matches!(act, crate::basis::DecidedAct::Proposals(decided) if decided == batch)
                    && !matches!(act, crate::basis::DecidedAct::Offer(_))
                {
                    return Err(TransitionRefusal::ForeignOffer);
                }
                let candidate = views.standing_call(subject).ok_or(TransitionRefusal::UnbackedOffer)?;
                if candidate.digest() != *call {
                    return Err(TransitionRefusal::UnbackedOffer);
                }
                let contract = annotation_or_unknown(self.engine.registry(), candidate)?;
                let stage = views.call_stage(subject);
                let role = views.call_role(subject);
                let context = self.context(expansions);
                let CheckOutcome::Block(block) =
                    crate::check::evaluate(&contract, views, candidate, &stage, role, &context)
                        .map_err(TransitionRefusal::from)?
                else {
                    return Err(TransitionRefusal::UnbackedOffer);
                };
                // The live act answered this whole gate before planning; the enumeration may
                // read less, but the gathered evidence is still the operation's own ask.
                self.audit_atoms(crate::plan::block_atoms(self.engine.registry(), &contract, &block));
                let blocked = || crate::plan::BlockedCall {
                    call: candidate,
                    contract: &contract,
                    raw: &block,
                    stage: &stage,
                    role,
                };
                // A recorded act whose pinned evidence leaves an enumeration read undecided
                // reconstructed a menu the evidence cannot back.
                let menu: Vec<crate::plan::ExecutableRemedyPlan> =
                    crate::plan::plan(self.engine.registry(), views, blocked(), &context)
                        .map_err(TransitionRefusal::from)?
                        .plans
                        .iter()
                        .filter_map(crate::plan::RemedyPlan::executable)
                        .cloned()
                        .collect();
                // A log an earlier planner wrote offers an undeclared spawn its bundled menu.
                if !menu.contains(plan)
                    && crate::plan::Declaring::of(role, views, candidate) != crate::plan::Declaring::No
                {
                    let bundled = crate::plan::bundled_spawn_menu(self.engine.registry(), views, blocked(), &context)
                        .map_err(TransitionRefusal::from)?;
                    if bundled.contains(plan) {
                        return Ok(bundled);
                    }
                }
                Ok(menu)
            }
            crate::basis::SubjectKey::ConfinedResult(dispatch) => {
                if dispatch.trajectory() != trajectory || dispatch.digest() != call {
                    return Err(TransitionRefusal::ForeignOffer);
                }
                match act {
                    crate::basis::DecidedAct::Outcome(decided) if decided == dispatch => {}
                    crate::basis::DecidedAct::Offer(_) => {}
                    _ => return Err(TransitionRefusal::ForeignOffer),
                }
                let receiving = views
                    .receiving_bound(dispatch)
                    .ok_or(TransitionRefusal::UnknownDispatch)?;
                let Some(DerivedCandidate::Result {
                    value,
                    residual: Some(residual),
                    ..
                }) = views.candidate(subject)
                else {
                    return Err(TransitionRefusal::UnbackedOffer);
                };
                let lineage = views.lineage(subject);
                let contract = self.dispatch_contract(trajectory, dispatch)?;
                crate::plan::confined_stage(
                    self.engine.registry(),
                    &contract.contract,
                    receiving,
                    &value.label,
                    residual,
                    &lineage,
                    views,
                    &self.context(expansions),
                )
                .map_err(TransitionRefusal::from)
            }
            // A child return is never planned: it crosses at the child's stop or is refused there.
            crate::basis::SubjectKey::Return(_) | crate::basis::SubjectKey::Approval(_) => {
                Err(TransitionRefusal::ForeignOffer)
            }
        }
    }

    fn offer_accepted(
        &mut self,
        trajectory: &TrajectoryId,
        offer: &crate::value::OfferId,
    ) -> Result<(), TransitionRefusal> {
        use crate::basis::SubjectKey;
        let views = self.projection.view(trajectory);
        let recorded = ending_offer(&views, trajectory, offer)?;
        // The executing act inherits the accepted offer's pinned evidence.
        self.audit_inherit(&recorded.evidence)?;
        let (basis, subject) = (recorded.basis, recorded.subject.clone());
        let owed = match (&subject, recorded.plan.hop().is_some()) {
            (SubjectKey::Call { .. }, false) => Owed::Approval(*offer),
            (SubjectKey::Call { .. } | SubjectKey::ConfinedResult(_), true) => Owed::Derived(*offer),
            (SubjectKey::ConfinedResult(_), false) => Owed::Accepted(*offer),
            // No offer ever stands on an approval — that subject exists to be spent by a release —
            // or on a child return, which crosses at the child's stop or is refused there.
            (SubjectKey::Approval(_) | SubjectKey::Return(_), _) => return Err(TransitionRefusal::UnbackedOffer),
        };
        self.may_spend(trajectory, &subject, &basis)?;
        match &self.declared {
            Some(open) if open.act == crate::basis::DecidedAct::Offer(*offer) => {
                self.owing = Some(owed);
                Ok(())
            }
            _ => Err(TransitionRefusal::UnbackedOffer),
        }
    }

    fn offer_denied(
        &mut self,
        trajectory: &TrajectoryId,
        offer: &crate::value::OfferId,
        authority: &AuthorityName,
    ) -> Result<(), TransitionRefusal> {
        let views = self.projection.view(trajectory);
        let recorded = ending_offer(&views, trajectory, offer)?;
        if !recorded.plan.names_authority(authority) {
            return Err(TransitionRefusal::UnbackedDenial);
        }
        if !views
            .denied_authorities(&recorded.call)
            .is_some_and(|denied| denied.contains(authority))
        {
            return Err(TransitionRefusal::UnbackedDenial);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn call_approved(
        &self,
        trajectory: &TrajectoryId,
        offer: &crate::value::OfferId,
        call: &ResolvedCall,
        plan: &crate::plan::PlanId,
        acceptance: &Option<Narrowing>,
        rulings: &[AuthorityEvidence],
        output: &Option<crate::plan::OutputRemedy>,
        return_policy: &Option<ReturnPolicy>,
        basis: &crate::basis::PolicyBasis,
        evidence: &AudienceEvidence,
    ) -> Result<(), TransitionRefusal> {
        let views = self.projection.view(trajectory);
        let recorded = views.offer(offer).ok_or(TransitionRefusal::UnknownOffer)?;
        if recorded.trajectory != *trajectory {
            return Err(TransitionRefusal::ForeignOffer);
        }
        if recorded.end != Some(crate::projection::OfferEnd::Accepted) {
            return Err(TransitionRefusal::OfferEnded);
        }
        // One approval per offer, and one current approval per call: a second would leave the
        // release choosing between two plans the agent selected separately, and only one of them
        // is the choice it made.
        // A marked spawn's declaration stands beside the approval of its other gaps.
        let offered = &recorded.plan;
        if views.approval(offer).is_some() || views.approval_pending(call, offered.declares_only()) {
            return Err(TransitionRefusal::ApprovalRepeated);
        }
        if views.standing_call(&recorded.subject) != Some(call)
            || call.digest() != recorded.call
            || plan != &offered.id
            || acceptance.as_ref() != offered.narrowing()
            || output != &offered.output_remedy()
            || rulings.len() != offered.required.len()
        {
            return Err(TransitionRefusal::UnbackedApproval);
        }
        if rulings.iter().zip(&offered.required).any(|(given, required)| {
            given.offer != *offer || given.authority != required.authority || given.covers != required.covers
        }) {
            return Err(TransitionRefusal::UnbackedApproval);
        }
        let marked = matches!(
            views.call_role(&recorded.subject),
            crate::check::CallRole::MarkedSpawn(_)
        );
        match (offered.return_step(), return_policy) {
            (None, None) if !marked => {}
            // The plan of a declared spawn's other gaps carries the declaration that stands.
            (None, Some(policy))
                if marked
                    && views
                        .declaration(call)
                        .is_some_and(|(_, declaration)| declaration.return_policy.as_ref() == Some(policy)) => {}
            (Some(step), Some(policy)) => {
                crate::plan::declared_return_policy(self.engine.registry(), &views, step, policy)
                    .map_err(|_| TransitionRefusal::UnbackedReturnPolicy)?;
            }
            _ => return Err(TransitionRefusal::UnbackedReturnPolicy),
        }
        let contract = annotation_or_unknown(self.engine.registry(), call)?;
        let live = views.current_label();
        if rulings
            .iter()
            .any(|evidence| evidence.reviewed.tool != *call.tool() || evidence.reviewed.trajectory_label != live)
        {
            return Err(TransitionRefusal::UnbackedApproval);
        }
        if basis != &views.basis_for(&crate::basis::SubjectKey::Approval(*offer)) {
            return Err(TransitionRefusal::ForgedBasis);
        }
        let expansions = self.recorded_expansions(evidence)?;
        // The approval extends what the offer pinned, never contradicts or drops it.
        if !evidence.contains(&recorded.evidence) {
            return Err(TransitionRefusal::ForgedEvidence);
        }
        self.audit_inherit(&recorded.evidence)?;
        // The live approval act answered its whole deterministic gate — the standing block's
        // atoms and the chosen plan's — before ruling; mirror the same ask here.
        let stage = views.call_stage(&recorded.subject);
        let role = views.call_role(&recorded.subject);
        if let Ok(CheckOutcome::Block(raw)) =
            crate::check::evaluate(&contract, &views, call, &stage, role, &self.context(&expansions))
        {
            self.audit_atoms(crate::plan::block_atoms(self.engine.registry(), &contract, &raw));
        }
        self.audit_atoms(crate::plan::plan_atoms(self.engine.registry(), offered));
        self.audit_reads(&expansions);
        Ok(())
    }

    fn declare(
        &mut self,
        act: &crate::basis::DecidedAct,
        advance: &crate::basis::BasisAdvance,
    ) -> Result<(), TransitionRefusal> {
        if self.declared.as_ref().is_some_and(|open| !open.owed.is_empty()) {
            return Err(TransitionRefusal::UnbackedAdvance);
        }
        // The previous act is complete; its evidence must be inherited or asked-for.
        self.settle_audit()?;
        if advance.flows.iter().any(|flow| !self.projection.is_opened(flow)) {
            return Err(TransitionRefusal::ForeignTrajectory);
        }
        self.declared = Some(Declaration {
            act: act.clone(),
            declared: advance.clone(),
            owed: advance.clone(),
        });
        self.admitting = None;
        Ok(())
    }

    fn settle_advance(
        &mut self,
        fact: &Fact,
        implied: &crate::basis::BasisAdvance,
        obligation: Obligation,
    ) -> Result<(), TransitionRefusal> {
        if implied.is_empty() {
            return Ok(());
        }
        let own_act = self.settles_its_own_declaration(fact, obligation);
        let belongs = self
            .declared
            .as_ref()
            .is_some_and(|open| belongs_to(self, &open.act, fact));
        let Some(declaration) = self.declared.as_mut().filter(|_| belongs) else {
            return Ok(());
        };
        if implied.family {
            if !(declaration.owed.family || own_act && declaration.declared.family) {
                return Err(TransitionRefusal::UndeclaredAdvance);
            }
            declaration.owed.family = false;
        }
        for flow in &implied.flows {
            if !(declaration.owed.flows.remove(flow) || own_act && declaration.declared.flows.contains(flow)) {
                return Err(TransitionRefusal::UndeclaredAdvance);
            }
        }
        for subject in &implied.subjects {
            let position = declaration
                .owed
                .subjects
                .iter()
                .position(|owed| owed == subject)
                .ok_or(TransitionRefusal::UndeclaredAdvance)?;
            declaration.owed.subjects.remove(position);
        }
        Ok(())
    }

    fn settles_its_own_declaration(&self, fact: &Fact, obligation: Obligation) -> bool {
        let Some(open) = &self.declared else {
            return false;
        };
        match (&open.act, fact) {
            (
                crate::basis::DecidedAct::Proposals(act),
                Fact::DispatchOpened { .. }
                | Fact::ForkPrepared { .. }
                | Fact::CallApprovalConsumed { .. }
                | Fact::CandidateConsumed { .. },
            ) => obligation != Obligation::Free && self.deciding.as_ref() == Some(act),
            // Everything else `belongs_to` admits names the act it belongs to.
            _ => true,
        }
    }

    fn may_spend(
        &self,
        trajectory: &TrajectoryId,
        subject: &crate::basis::SubjectKey,
        recorded: &crate::basis::PolicyBasis,
    ) -> Result<(), TransitionRefusal> {
        let Some(open) = &self.declared else {
            return Err(TransitionRefusal::UndeclaredAdvance);
        };
        if !open.declared.subjects.contains(subject) {
            return Err(TransitionRefusal::UndeclaredAdvance);
        }
        if recorded.advanced_by(&open.owed, trajectory, subject) != self.projection.view(trajectory).basis_for(subject)
        {
            return Err(TransitionRefusal::StaleSpend);
        }
        Ok(())
    }

    /// The declaration-only approvals current beside a covering approval that is spent: they
    /// declare the same return for the same call, and the covering one carried it.
    fn declarations_beside(
        &self,
        trajectory: &TrajectoryId,
        offer: &crate::value::OfferId,
    ) -> Vec<crate::value::OfferId> {
        let views = self.projection.view(trajectory);
        let Some(spent) = views
            .approval(offer)
            .filter(|spent| spent.return_policy.is_some() && !spent.declares_only())
        else {
            return Vec::new();
        };
        views
            .approvals_for(&spent.call)
            .filter(|(declared, approval)| {
                declared != offer
                    && approval.declares_only()
                    && approval.return_policy == spent.return_policy
                    && self.stood(
                        trajectory,
                        &crate::basis::SubjectKey::Approval(*declared),
                        &approval.basis,
                    )
            })
            .map(|(declared, _)| declared)
            .collect()
    }

    /// Did a record at `recorded` stand where the act being decided began? The walk that
    /// derives the act's advance stands there itself; admission reads that point back through
    /// what the act still owes, as [`Self::may_spend`] does.
    fn stood(
        &self,
        trajectory: &TrajectoryId,
        subject: &crate::basis::SubjectKey,
        recorded: &crate::basis::PolicyBasis,
    ) -> bool {
        let now = self.projection.view(trajectory).basis_for(subject);
        match &self.declared {
            Some(open) => recorded.advanced_by(&open.owed, trajectory, subject) == now,
            None => *recorded == now,
        }
    }

    /// A staged candidate is taken the way an approval is spent, except that only its own
    /// subject must have stood still. The flow moves on every admitted value, and a narrowing
    /// must leave the derivation takeable: the proposal that takes it is judged against the
    /// narrowed label rather than refused here.
    fn may_take(
        &self,
        trajectory: &TrajectoryId,
        subject: &crate::basis::SubjectKey,
        recorded: crate::basis::SubjectGeneration,
    ) -> Result<(), TransitionRefusal> {
        let Some(open) = &self.declared else {
            return Err(TransitionRefusal::UndeclaredAdvance);
        };
        if !open.declared.subjects.contains(subject) {
            return Err(TransitionRefusal::UndeclaredAdvance);
        }
        if !self.stands(trajectory, subject, recorded) {
            return Err(TransitionRefusal::StaleSpend);
        }
        Ok(())
    }

    /// Is this staged candidate one the open decision could judge the call under? Wider than
    /// [`Self::may_take`]: the decision judges every standing derivation in turn and declares
    /// only the one it takes.
    fn stands(
        &self,
        trajectory: &TrajectoryId,
        subject: &crate::basis::SubjectKey,
        recorded: crate::basis::SubjectGeneration,
    ) -> bool {
        self.declared.as_ref().is_some_and(|open| {
            open.owed.generation_of(subject, recorded) == self.projection.view(trajectory).basis_for(subject).subject
        })
    }

    fn closing_act(&self, dispatch: &DispatchId) -> Result<(), TransitionRefusal> {
        let subject = crate::basis::SubjectKey::ConfinedResult(dispatch.clone());
        if matches!(
            self.projection.view(dispatch.trajectory()).candidate(&subject),
            Some(DerivedCandidate::Result { residual: Some(_), .. })
        ) && !self.candidate_accepted.contains_key(dispatch)
        {
            return Err(TransitionRefusal::StagedClose);
        }
        if let Some(open) = &self.declared
            && let crate::basis::DecidedAct::Offer(offer) = &open.act
            && confines(self, offer, dispatch)
        {
            return Ok(());
        }
        self.declaring(&crate::basis::DecidedAct::Outcome(dispatch.clone()))
    }

    fn declaring(&self, act: &crate::basis::DecidedAct) -> Result<(), TransitionRefusal> {
        match &self.declared {
            Some(open) if &open.act != act && !open.owed.is_empty() => Err(TransitionRefusal::UnbackedAdvance),
            _ => Ok(()),
        }
    }

    /// What a candidate batch will move, derived by walking it exactly as [`Sequence::admit`]
    /// will. The decision declares this before appending the batch, so the declaration and the
    /// rule that checks it are one derivation and cannot drift.
    pub(crate) fn advance_of(engine: &'a Engine, view: &EngineView, facts: &[Fact]) -> crate::basis::BasisAdvance {
        if facts.is_empty() {
            return crate::basis::BasisAdvance::default();
        }
        let mut sequence = Sequence::resuming(engine, view);
        let mut total = crate::basis::BasisAdvance::default();
        for fact in facts {
            total.absorb(&sequence.implied_advance(fact));
            sequence.projection.fold(fact);
        }
        total
    }

    /// The flow moves exactly when the trajectory's label does. A block and its remedies are
    /// derived from that label, so a record that leaves it where it was — a release, a metadata
    /// read, a public file — changes nothing an open offer was derived from, and the offer stands.
    fn implied_advance(&self, fact: &Fact) -> crate::basis::BasisAdvance {
        let mut advance = self.unlabelled_advance(fact);
        if let Some((trajectory, label)) = fact.inflow()
            && self.projection.admission_moves_label(trajectory, label)
        {
            advance.absorb(&crate::basis::BasisAdvance::flow(trajectory));
        }
        advance
    }

    fn unlabelled_advance(&self, fact: &Fact) -> crate::basis::BasisAdvance {
        use crate::basis::BasisAdvance;
        match fact {
            // A release moves no label: its result is unseen until it is admitted. The
            // reservation moves the family.
            Fact::DispatchOpened { proposed_effects, .. } => effect_advance(!proposed_effects.is_empty()),
            // The reservation settles into committed effects.
            Fact::DispatchSucceeded { effects, .. } => effect_advance(!effects.is_empty()),
            Fact::DispatchClosed {
                trajectory,
                dispatch,
                outcome,
            } => match outcome {
                CloseOutcome::Success { effects } => effect_advance(!effects.is_empty()),
                CloseOutcome::Failure | CloseOutcome::FailureWithBody { .. } => {
                    effect_advance(self.projection.view(trajectory).reserves(dispatch))
                }
                CloseOutcome::Indeterminate => crate::basis::BasisAdvance::default(),
            },
            Fact::ValueAdmitted {
                provenance: Provenance::ProviderRun { effects, .. },
                ..
            } => effect_advance(!effects.is_empty()),
            // A denial changes what may be offered for that rendered call.
            Fact::Denial { trajectory, .. } => BasisAdvance::flow(trajectory),
            // A covering approval carried the declaration that stood beside it, so spending it
            // spends that declaration too: a declaration covers one spawn.
            Fact::CallApprovalConsumed { trajectory, offer, .. } => BasisAdvance {
                subjects: std::iter::once(*offer)
                    .chain(self.declarations_beside(trajectory, offer))
                    .map(crate::basis::SubjectKey::Approval)
                    .collect(),
                ..BasisAdvance::default()
            },
            Fact::OfferAccepted { trajectory, offer } => match self.projection.view(trajectory).offer(offer) {
                Some(recorded) => BasisAdvance {
                    subjects: vec![recorded.subject.clone()],
                    ..BasisAdvance::default()
                },
                None => BasisAdvance::default(),
            },
            Fact::CandidateDerived { subject, .. } | Fact::CandidateConsumed { subject, .. } => BasisAdvance {
                subjects: vec![subject.clone()],
                ..BasisAdvance::default()
            },
            _ => BasisAdvance::default(),
        }
    }

    /// The validated projection, once every record has been admitted. A claim left standing —
    /// a release with no opening, a ruling with no dispatch — means the log stops mid-act.
    pub(crate) fn finish(mut self) -> Result<Projection, TransitionRefusal> {
        if !self.projection.is_opened(&self.family) {
            return Err(TransitionRefusal::Unopened);
        }
        // A log that stops mid-menu.
        settled(self.menu.take())?;
        if !self.pending.is_empty() {
            return Err(TransitionRefusal::UnbackedDecision);
        }
        if !self.remedies.is_empty() {
            return Err(TransitionRefusal::DanglingRemedy);
        }
        if !self.derived.is_empty() || !self.candidate_accepted.is_empty() {
            return Err(TransitionRefusal::UnadmittedDerivation);
        }
        if self.crossed.values().any(|crossing| crossing != &Crossing::Merged) {
            return Err(TransitionRefusal::UnmergedCrossing);
        }
        if !self.spawn_policies.is_empty() {
            return Err(TransitionRefusal::UnbackedReturnPolicy);
        }
        if self.declared.as_ref().is_some_and(|open| !open.owed.is_empty()) {
            return Err(TransitionRefusal::UnbackedAdvance);
        }
        if self.owing.is_some() {
            return Err(TransitionRefusal::UndischargedAcceptance);
        }
        self.settle_audit()?;
        Ok(self.projection)
    }

    fn obliged(&mut self, fact: &Fact) -> Result<Obligation, TransitionRefusal> {
        let Some(next) = self.pending.front() else {
            // No obligation stands; a decision record is free to create one.
            return Ok(Obligation::Free);
        };
        let prepares_fork = next.prepares_fork;
        match (next.next.clone(), fact) {
            // The take is recorded first, before whatever else the release owes.
            (ReleasePart::Take(owed), Fact::CandidateConsumed { subject, dispatch, .. })
                if subject == &owed && dispatch == &next.dispatch =>
            {
                let front = self.pending.front_mut().expect("the front was read above");
                front.next = front.after_take.clone();
                Ok(Obligation::Decided)
            }
            (ReleasePart::Consumption(owed), Fact::CallApprovalConsumed { offer, dispatch, .. })
                if offer == &owed && dispatch == &next.dispatch =>
            {
                // A marked spawn spends an approval that declares its child's return policy;
                // an ordinary call spends one that declares none.
                let declared = self
                    .projection
                    .view(dispatch.trajectory())
                    .approval(&owed)
                    .ok_or(TransitionRefusal::UnknownApproval)?
                    .return_policy
                    .clone();
                match (declared, prepares_fork.is_some()) {
                    (Some(policy), true) => {
                        self.spawn_policies.insert(dispatch.clone(), policy);
                    }
                    (None, false) => {}
                    _ => return Err(TransitionRefusal::UnbackedReturnPolicy),
                }
                self.pending.front_mut().expect("the front was read above").next = ReleasePart::Remedy(owed);
                Ok(Obligation::Consuming(owed))
            }
            (
                ReleasePart::Remedy(owed),
                Fact::Acceptance { dispatch, .. }
                | Fact::Ruling { dispatch, .. }
                | Fact::OutputSanitizerBound { dispatch, .. }
                | Fact::OutputWithheld { dispatch, .. },
            ) if dispatch == &next.dispatch => Ok(Obligation::Consuming(owed)),
            (
                ReleasePart::Opening | ReleasePart::Remedy(_),
                Fact::DispatchOpened {
                    dispatch,
                    tool,
                    arguments,
                    annotation,
                    subject,
                    evidence,
                    file_basis,
                    ..
                },
            ) if dispatch == &next.dispatch => {
                let opened = crate::projection::opened_call(tool, arguments, annotation, file_basis);
                if opened != next.call || subject != &next.subject {
                    return Err(TransitionRefusal::UnbackedDecision);
                }
                // The opening persists the pinned evidence its check read under.
                if evidence != &next.evidence {
                    return Err(TransitionRefusal::ForgedEvidence);
                }
                let authorized = match next.next {
                    ReleasePart::Remedy(owed) => Obligation::Consuming(owed),
                    _ => Obligation::Decided,
                };
                // The marked spawn's preparation is the next record; any other release is done.
                if next.prepares_fork.is_some() {
                    self.pending.front_mut().expect("the front was read above").next = ReleasePart::Fork;
                } else {
                    self.pending.pop_front();
                }
                Ok(authorized)
            }
            (ReleasePart::Fork, Fact::ForkPrepared { fork, kind, .. })
                if fork == &ForkId::of(&next.dispatch) && Some(*kind) == next.prepares_fork =>
            {
                self.pending.pop_front();
                Ok(Obligation::Decided)
            }
            _ => Err(TransitionRefusal::UnbackedDecision),
        }
    }

    /// The strict judgment of the durable opening: the family's first record, naming
    /// the family, its validated declaration byte for byte — which subsumes re-running the
    /// coverage matrix over it — and the deployment and revision the engine judging it decides
    /// at. `policy_file_key` is deliberately not judged here: it names a
    /// stored file this engine never sees, and the outer layer re-hashes the file it loaded against
    /// the record on every event.
    fn root_opened(&self, opening: &TrajectoryOpening) -> Result<(), OpeningTransitionRefusal> {
        if self.projection.is_opened(&self.family) {
            return Err(OpeningTransitionRefusal::Duplicate);
        }
        if opening.trajectory != self.family {
            return Err(OpeningTransitionRefusal::WrongTrajectory {
                found: opening.trajectory.as_str().to_string(),
            });
        }
        if opening.profile != *self.engine.registry().profile() {
            return Err(OpeningTransitionRefusal::ProfileMismatch);
        }
        if Some(&opening.deployment) != self.engine.deployment() || Some(opening.revision) != self.engine.revision() {
            return Err(OpeningTransitionRefusal::DeploymentMismatch);
        }
        if let Some(principal) = &opening.principal
            && crate::audience::session_principal(principal.as_str()).as_ref() != Some(principal)
        {
            return Err(OpeningTransitionRefusal::MalformedPrincipal {
                found: principal.as_str().to_string(),
            });
        }
        if let Some(origin) = &opening.forked_from
            && origin.principal != opening.principal
        {
            return Err(OpeningTransitionRefusal::ForkPrincipalMismatch);
        }
        Ok(())
    }

    fn member(&self, fact: &Fact) -> Result<(), TransitionRefusal> {
        let trajectory = fact.trajectory();
        if let Fact::TrajectoryOpened(opening) = fact {
            self.root_opened(opening)?;
            if let Some(origin) = &opening.forked_from
                && (origin.parent_root == opening.trajectory || origin.parent == opening.trajectory)
            {
                return Err(OpeningTransitionRefusal::SelfFork.into());
            }
            return Ok(());
        }
        if !self.projection.is_opened(&self.family) {
            return Err(TransitionRefusal::Unopened);
        }
        let hangs_from = match fact {
            Fact::ForkOpened { fork, .. } => {
                &self
                    .projection
                    .prepared_fork(fork)
                    .ok_or(TransitionRefusal::UnknownFork)?
                    .parent
            }
            _ => trajectory,
        };
        if self.projection.is_opened(hangs_from) {
            Ok(())
        } else {
            Err(TransitionRefusal::ForeignTrajectory)
        }
    }

    fn decided(
        &mut self,
        trajectory: &TrajectoryId,
        batch: &ProposalBatchId,
        proposals: &[ResolvedCall],
        spawn: Option<SpawnMark>,
        released: &[DispatchId],
        evidence: &AudienceEvidence,
    ) -> Result<(), TransitionRefusal> {
        // A mark this deployment cannot make, or one naming no proposal.
        if let Some(mark) = spawn {
            if mark.index() >= proposals.len() {
                return Err(TransitionRefusal::SpawnMarkOutOfRange);
            }
            if !self.engine.registry().profile().context_control() {
                return Err(TransitionRefusal::ContextUncontrolled);
            }
        }
        // A declaration standing over this record must be this batch's own.
        self.declaring(&crate::basis::DecidedAct::Proposals(batch.clone()))?;
        if self.projection.view(trajectory).decided_batch(batch).is_some() {
            return Err(TransitionRefusal::BatchIdentityConflict);
        }
        // An ended branch decides nothing more.
        if self.projection.view(trajectory).has_ended(trajectory) {
            return Err(TransitionRefusal::BranchEnded);
        }
        // The identity's provider half, where it has one, fixed the trajectory before this record:
        // a decision elsewhere would be a second act under one identity.
        if self
            .projection
            .view(trajectory)
            .provider_admissions(batch)
            .next()
            .is_some_and(|(admitted, ..)| admitted != trajectory)
        {
            return Err(TransitionRefusal::ForeignAdmission);
        }
        let expansions = self.recorded_expansions(evidence)?;
        for call in proposals {
            if !self.engine.registry().contains_tool(call.tool()) {
                return Err(TransitionRefusal::UnknownTool(call.tool().as_str().to_string()));
            }
            let declaration = self
                .engine
                .registry()
                .declaration(call)
                .ok_or(TransitionRefusal::InvalidPayload(
                    crate::params::ArgumentError::NoMatchingContract,
                ))?;
            if crate::check::validate_annotation(self.engine.registry(), declaration, call).is_err() {
                return Err(TransitionRefusal::ForgedEvidence);
            }
        }
        let mut working = std::borrow::Cow::Borrowed(&self.projection);
        let act =
            crate::engine::ActEvidence::validated(evidence.clone(), expansions, self.projection.principal().cloned());
        let composed = crate::engine::compose_batch(
            self.engine.registry(),
            &mut working,
            crate::engine::ComposingBatch { trajectory, id: batch },
            proposals,
            spawn,
            &|views, call| {
                views
                    .approvals_for(call)
                    .map(|(offer, approval)| (offer, approval.basis))
                    .filter(|(offer, basis)| {
                        self.may_spend(trajectory, &crate::basis::SubjectKey::Approval(*offer), basis)
                            .is_ok()
                    })
                    .map(|(offer, _)| offer)
                    .collect()
            },
            &|views, call, taken| {
                views
                    .call_candidates_for(call)
                    .filter(|(subject, recorded)| {
                        !taken.contains(*subject) && self.stands(trajectory, subject, recorded.generation)
                    })
                    .map(|(subject, _)| subject.clone())
                    .collect()
            },
            &act,
        )
        .map_err(|refusal| match refusal {
            crate::engine::ComposeRefusal::MembershipNeeded(needed) => needed.into(),
            crate::engine::ComposeRefusal::Evidence(refusal) => refusal.into(),
            crate::engine::ComposeRefusal::Malformed(error) => match error {
                crate::engine::EngineError::InvalidCall(error) => TransitionRefusal::InvalidPayload(error),
                crate::engine::EngineError::UnknownTool(tool) | crate::engine::EngineError::ProviderRunTool(tool) => {
                    TransitionRefusal::UnknownTool(tool)
                }
                other => unreachable!("composing a batch refuses only on the call it cannot build, not {other}"),
            },
        })?;
        self.audit_reads(act.expansions());
        // The live act answered every refused sibling's whole block gate before planning its
        // remedies; a planless block leaves no offer record, so mirror that ask here.
        let final_views = working.view(trajectory);
        for (position, call) in composed
            .iter()
            .enumerate()
            .filter(|(_, release)| release.is_none())
            .map(|(position, _)| (position, &proposals[position]))
        {
            let Some(contract) = self.engine.registry().annotation_of(call) else {
                continue;
            };
            let context = self.context(act.expansions());
            let role = crate::check::CallRole::of(spawn, position);
            if let Ok(CheckOutcome::Block(raw)) =
                crate::check::evaluate(&contract, &final_views, call, &CallStage::default(), role, &context)
            {
                self.audit_atoms(crate::plan::block_atoms(self.engine.registry(), &contract, &raw));
            }
        }
        let expected: Vec<&DispatchId> = composed.iter().flatten().map(|release| &release.dispatch).collect();
        if expected.into_iter().ne(released) {
            return Err(TransitionRefusal::MisdecidedBatch);
        }
        self.deciding = Some(batch.clone());
        self.pending
            .extend(
                composed
                    .into_iter()
                    .zip(proposals)
                    .enumerate()
                    .filter_map(|(position, (release, call))| {
                        release.map(|release| {
                            let after_take = match release.consumes {
                                Some(offer) => ReleasePart::Consumption(offer),
                                None => ReleasePart::Opening,
                            };
                            PendingRelease {
                                dispatch: release.dispatch,
                                subject: crate::engine::ComposingBatch { trajectory, id: batch }.subject(position),
                                call: call.clone(),
                                prepares_fork: release.prepares_fork,
                                evidence: release.evidence,
                                next: match &release.takes {
                                    Some(subject) => ReleasePart::Take(subject.clone()),
                                    None => after_take.clone(),
                                },
                                after_take,
                            }
                        })
                    }),
            );
        Ok(())
    }

    fn opened(
        &mut self,
        trajectory: &TrajectoryId,
        dispatch: &DispatchId,
        call: &ResolvedCall,
        authorized: Obligation,
        evidence: &AudienceEvidence,
    ) -> Result<(), TransitionRefusal> {
        let expansions = &self.recorded_expansions(evidence)?;
        if dispatch.trajectory() != trajectory {
            return Err(TransitionRefusal::ForeignDispatch);
        }
        if self.projection.view(trajectory).dispatch_tool(dispatch).is_some() {
            return Err(TransitionRefusal::DispatchReopened);
        }
        let contract = annotation_or_unknown(self.engine.registry(), call)?;
        contract
            .parameters
            .validate(call.arguments())
            .map_err(TransitionRefusal::InvalidPayload)?;
        let digest = call.digest();
        if dispatch.digest() != &digest {
            return Err(TransitionRefusal::DigestMismatch);
        }
        let views = self.projection.view(trajectory);
        if dispatch.occurrence() != views.dispatch_count(&digest) {
            return Err(TransitionRefusal::WrongOccurrence);
        }
        if views.has_ended(trajectory) {
            return Err(TransitionRefusal::BranchEnded);
        }
        let remedy = self.remedies.remove(dispatch);
        if remedy
            .as_ref()
            .and_then(|landed| landed.evidence.as_ref())
            .is_some_and(|pinned| pinned != evidence)
        {
            return Err(TransitionRefusal::ForgedEvidence);
        }
        match authorized {
            Obligation::Decided => {
                if remedy.is_some() {
                    Err(TransitionRefusal::DanglingRemedy)
                } else {
                    Ok(())
                }
            }
            Obligation::Consuming(offer) => {
                let approval = views.approval(&offer).ok_or(TransitionRefusal::UnknownApproval)?;
                self.audit_inherit(&approval.evidence)?;
                let lands = !approval.rulings.is_empty() || approval.acceptance.is_some() || approval.output.is_some();
                match (lands, remedy) {
                    // A plan of only a return declaration lands nothing on the dispatch: the fork
                    // preparation the obligation checked is what backs it.
                    (false, None) => {}
                    (false, Some(_)) => return Err(TransitionRefusal::DanglingRemedy),
                    (true, landed) => {
                        let expected = Remedy {
                            rulings: approval
                                .rulings
                                .iter()
                                .map(|given| (given.authority.clone(), given.covers.clone()))
                                .collect(),
                            plans: BTreeSet::from([approval.plan]),
                            reviewed: approval.rulings.iter().map(|given| given.reviewed.clone()).collect(),
                            acceptance: approval.acceptance.clone(),
                            output: approval.output.clone(),
                            contribution: match approval.output.as_ref() {
                                Some(crate::plan::OutputRemedy::Sanitize(name)) => crate::plan::bound_contribution(
                                    self.engine.registry(),
                                    &contract,
                                    &call.output_label(&contract, &views.current_label()),
                                    name,
                                    &self.context(expansions),
                                )
                                .map_err(TransitionRefusal::from)?,
                                Some(crate::plan::OutputRemedy::Withhold) | None => None,
                            },
                            evidence: (!approval.rulings.is_empty()
                                || matches!(approval.output, Some(crate::plan::OutputRemedy::Sanitize(_))))
                            .then(|| evidence.clone()),
                        };
                        if landed.is_none_or(|landed| landed != expected) {
                            return Err(TransitionRefusal::UnbackedApproval);
                        }
                    }
                }
                Ok(())
            }
            // Every opening stands on a release this validator recomputed. Nothing opens a
            // dispatch on its own account: a hop stages a derivation and opens nothing.
            Obligation::Free => Err(TransitionRefusal::UnbackedDecision),
        }
    }

    fn value_admitted(
        &mut self,
        trajectory: &TrajectoryId,
        value: &crate::value::LabeledValue,
        provenance: &Provenance,
    ) -> Result<(), TransitionRefusal> {
        match provenance {
            Provenance::ProviderRun {
                tool,
                batch,
                position,
                effects,
                evidence,
            } => {
                let contract = self
                    .engine
                    .registry()
                    .provider_run_annotation(tool)
                    .ok_or_else(|| TransitionRefusal::NotProviderRun(tool.as_str().to_string()))?;
                self.recorded_expansions(evidence)?;
                let views = self.projection.view(trajectory);
                if !self
                    .declared
                    .as_ref()
                    .is_some_and(|open| open.act == crate::basis::DecidedAct::Proposals(batch.clone()))
                {
                    return Err(TransitionRefusal::UndeclaredAdmission);
                }
                if views.has_ended(trajectory) {
                    return Err(TransitionRefusal::BranchEnded);
                }
                if views.decided_batch(batch).is_some() {
                    return Err(TransitionRefusal::AdmissionAfterDecision);
                }
                let mut admitted = views.provider_admissions(batch);
                if *position as usize != admitted.len() {
                    return Err(TransitionRefusal::WrongAdmissionPosition);
                }
                if *position > 0 && self.admitting.as_ref() != Some(batch) {
                    return Err(TransitionRefusal::SplitAdmission);
                }
                if admitted.next().is_some_and(|(first, ..)| first != trajectory) {
                    return Err(TransitionRefusal::ForeignAdmission);
                }
                if effects != &contract.emits {
                    return Err(TransitionRefusal::EffectsMismatch);
                }
                if value.label != contract.output_label() {
                    return Err(TransitionRefusal::ForgedLabel);
                }
                self.admitting = Some(batch.clone());
                Ok(())
            }
            Provenance::ToolResult { dispatch } => {
                // Inlined dispatch_contract: borrowing whole `self` here would conflict with
                // the `admitted`/`derived` field mutations below.
                if dispatch.trajectory() != trajectory {
                    return Err(TransitionRefusal::ForeignDispatch);
                }
                let call = self
                    .projection
                    .dispatch_call_of(dispatch)
                    .ok_or(TransitionRefusal::UnknownDispatch)?;
                let views = self.projection.view(trajectory);
                let contract =
                    crate::admit::outcome_contract(self.engine.registry(), call, views.release_terms(dispatch))
                        .ok_or(TransitionRefusal::UnknownDispatch)?;
                // Admission lands with the close, and only a success admits anything.
                if !views.closed_successfully(dispatch) && !views.failed_with_body(dispatch) {
                    return Err(TransitionRefusal::DispatchNotOpen);
                }
                if self.projection.admitted_result(dispatch) {
                    return Err(TransitionRefusal::RepeatAdmission);
                }
                let expected = match self.derived.remove(dispatch) {
                    Some(candidate) => {
                        if value != &candidate {
                            return Err(TransitionRefusal::ForgedLabel);
                        }
                        candidate.label
                    }
                    None => match views.candidate(&crate::basis::SubjectKey::ConfinedResult(dispatch.clone())) {
                        Some(DerivedCandidate::Result {
                            value: candidate,
                            residual,
                            ..
                        }) => {
                            if value != candidate {
                                return Err(TransitionRefusal::ForgedLabel);
                            }
                            if self.candidate_accepted.remove(dispatch).as_ref() != residual.as_ref() {
                                return Err(TransitionRefusal::AcceptanceMismatch);
                            }
                            candidate.label.clone()
                        }
                        Some(DerivedCandidate::Call { .. } | DerivedCandidate::Return { .. }) => {
                            return Err(TransitionRefusal::ForgedLabel);
                        }
                        None => {
                            if views.bound_sanitizer(dispatch).is_some() || views.withholds_output(dispatch) {
                                return Err(TransitionRefusal::ForgedLabel);
                            }
                            let digest = RawResultDigest::of(value.body.as_str().as_bytes());
                            if views.failed_with_body(dispatch) {
                                if views.failure_body_digest(dispatch) != Some(digest) {
                                    return Err(TransitionRefusal::ForgedLabel);
                                }
                            } else {
                                self.observed_as(trajectory, dispatch, &digest)?;
                            }
                            let receiving = views
                                .receiving_bound(dispatch)
                                .ok_or(TransitionRefusal::UnknownDispatch)?;
                            call.output_label_from(&contract.output_label(), receiving)
                        }
                    },
                };
                if value.label != expected {
                    return Err(TransitionRefusal::ForgedLabel);
                }
                Ok(())
            }
            Provenance::ChildReturn { child, id } => {
                let views = self.projection.view(trajectory);
                if views.parent_of(child) != Some(trajectory) {
                    return Err(TransitionRefusal::ForeignTrajectory);
                }
                let crossed = views.child_return(id).ok_or(TransitionRefusal::UnknownReturn)?;
                if id.child() != child || crossed != value {
                    return Err(TransitionRefusal::ForgedLabel);
                }
                let crossing = self.crossed.get_mut(id).ok_or(TransitionRefusal::UnknownReturn)?;
                // One crossing, one admission — and never after the merge that consumed it.
                if *crossing != Crossing::Recorded {
                    return Err(TransitionRefusal::RepeatAdmission);
                }
                *crossing = Crossing::Admitted;
                Ok(())
            }
        }
    }

    /// One crossing, re-derived from the fork's recorded policy: the label is the child's fold,
    /// or the fork's sanitizer's derivation of it; an attested body is canonical under the
    /// stored shape at a fork whose seed covers the attestation's rank; a named sanitizer's
    /// crossing is exactly the derivation the log staged for it; and the label stands at or
    /// above the floor the parent set, on the dimensions the sanitizer does not raise.
    fn child_return(
        &mut self,
        child: &TrajectoryId,
        id: &ChildReturnId,
        value: &crate::value::LabeledValue,
        derivation: &ReturnDerivation,
        evidence: &AudienceEvidence,
    ) -> Result<(), TransitionRefusal> {
        let expansions = self.recorded_expansions(evidence)?;
        let parent = self
            .projection
            .view(child)
            .parent_of(child)
            .ok_or(TransitionRefusal::NotForked)?
            .clone();
        let views = self.projection.view(&parent);
        let ReturnPolicy::Crossing(policy) = standing_return(&views, child, id)?.clone() else {
            return Err(TransitionRefusal::ReturnVoid);
        };
        let fold = views.branch_label(child);
        let expected = match (&policy.sanitizer, derivation) {
            (None, ReturnDerivation::Raw) => fold.clone(),
            (
                Some(sanitizer),
                ReturnDerivation::Sanitized {
                    sanitizer: named,
                    raw_digest,
                },
            ) => {
                if sanitizer.name() != *named {
                    return Err(TransitionRefusal::ReturnRecordMismatch);
                }
                let registered = self
                    .engine
                    .registry()
                    .sanitizer(named)
                    .ok_or_else(|| TransitionRefusal::UnknownSanitizer(named.as_str().to_string()))?;
                match sanitizer {
                    ReturnSanitizer::Attest(shape) => {
                        match shape.validate(value.body.as_str()) {
                            Ok(canonical) if canonical == value.body.as_str() => {}
                            _ => return Err(TransitionRefusal::ReturnShapeViolation),
                        }
                        if !views
                            .fork_seed(child)
                            .is_some_and(|seed| crate::plan::attest_fits(&registered.transition, seed))
                        {
                            return Err(TransitionRefusal::SanitizerUnapplicable);
                        }
                        sanitized_label(registered.derive_output(&fold, &[], &self.context(&expansions)))?
                    }
                    // The staging act derived and validated the label with its own pinned
                    // reads; the crossing reads nothing again.
                    ReturnSanitizer::Named(_) => {
                        let subject = crate::basis::SubjectKey::Return(id.clone());
                        match views.candidate(&subject) {
                            Some(DerivedCandidate::Return { source, value: staged })
                                if staged == value && source == raw_digest =>
                            {
                                staged.label.clone()
                            }
                            _ => return Err(TransitionRefusal::ReturnRecordMismatch),
                        }
                    }
                }
            }
            _ => return Err(TransitionRefusal::ReturnRecordMismatch),
        };
        if value.label != expected {
            return Err(TransitionRefusal::ForgedLabel);
        }
        self.holds_child_floor(child, &value.label)?;
        self.audit_reads(&expansions);
        Ok(())
    }

    fn holds_child_floor(&self, child: &TrajectoryId, label: &Label) -> Result<(), TransitionRefusal> {
        let floor = crate::plan::floor_of(self.engine.registry(), &self.projection.view(child))
            .expect("a forked child stands under its fork's floor");
        match floor.holds(label) {
            true => Ok(()),
            false => Err(TransitionRefusal::ReturnBelowFloor),
        }
    }

    fn boundary(&mut self, trajectory: &TrajectoryId, kind: &BoundaryKind) -> Result<(), TransitionRefusal> {
        match kind {
            BoundaryKind::Merge { child_return } => {
                let views = self.projection.view(trajectory);
                views
                    .child_return(child_return)
                    .ok_or(TransitionRefusal::UnknownReturn)?;
                if views.parent_of(child_return.child()) != Some(trajectory) {
                    return Err(TransitionRefusal::ForeignTrajectory);
                }
                let crossing = self
                    .crossed
                    .get_mut(child_return)
                    .ok_or(TransitionRefusal::UnknownReturn)?;
                // The parent folds the crossing before it punctuates the merge.
                if *crossing != Crossing::Admitted {
                    return Err(TransitionRefusal::RepeatAdmission);
                }
                *crossing = Crossing::Merged;
                Ok(())
            }
            BoundaryKind::VoidReturn => {
                let views = self.projection.view(trajectory);
                let parent = views.parent_of(trajectory).ok_or(TransitionRefusal::NotForked)?;
                if self.projection.view(parent).has_ended(trajectory) {
                    return Err(TransitionRefusal::BranchEnded);
                }
                Ok(())
            }
            // The seed is the parent's label as it stands: the record claims no other inflow.
            BoundaryKind::Resume { seed } => {
                let views = self.projection.view(trajectory);
                let parent = views.parent_of(trajectory).ok_or(TransitionRefusal::NotForked)?;
                let parent = self.projection.view(parent);
                if parent.has_ended(trajectory) {
                    return Err(TransitionRefusal::BranchEnded);
                }
                if seed != &parent.current_label() {
                    return Err(TransitionRefusal::ForgedLabel);
                }
                Ok(())
            }
        }
    }

    fn observed_as(
        &self,
        trajectory: &TrajectoryId,
        dispatch: &DispatchId,
        raw_digest: &RawResultDigest,
    ) -> Result<(), TransitionRefusal> {
        match self.projection.view(trajectory).observed_result(dispatch) {
            None => Ok(()),
            Some(crate::fact::ObservedResult::Available(observed)) if observed == raw_digest => Ok(()),
            Some(_) => Err(TransitionRefusal::ObservationMismatch),
        }
    }

    /// The membership answers a record's pinned primitives recompute to. Every pinned entry
    /// must belong to a registered source: junk or foreign evidence never validates, whatever
    /// answers it would add. The one funnel every evidence-carrying record passes, so it also
    /// pins the evidence into the open act's ledger.
    fn recorded_expansions(&self, evidence: &AudienceEvidence) -> Result<Expansions, TransitionRefusal> {
        let expansions = self
            .engine
            .registry()
            .audience()
            .expansions(evidence, self.projection.principal())
            .map_err(TransitionRefusal::from)?;
        self.audit.borrow_mut().pin(evidence).map_err(TransitionRefusal::from)?;
        Ok(expansions)
    }

    /// Fold a validation context's asks into the open act's ledger, after the record's
    /// re-derivations ran over it.
    fn audit_reads(&self, expansions: &Expansions) {
        self.audit.borrow_mut().read(expansions.reads());
    }

    /// Contribute the deterministic gate atoms the live act answered before this record's
    /// decision ran — a validation that re-derives less than the live gates demanded still
    /// justifies the evidence those gates gathered.
    fn audit_atoms(&self, atoms: impl IntoIterator<Item = crate::label::SymbolicAtom>) {
        self.audit.borrow_mut().read(atoms);
    }

    /// Count `pins` — entries pinned by a record the open act continues — as inherited.
    fn audit_inherit(&self, pins: &AudienceEvidence) -> Result<(), TransitionRefusal> {
        self.audit.borrow_mut().inherit(pins).map_err(TransitionRefusal::from)
    }

    /// Settle the open act's ledger: every pinned entry is an inherited pin or answers a
    /// collected ask, exactly the test the live act passed.
    fn settle_audit(&self) -> Result<(), TransitionRefusal> {
        let audit = std::mem::take(&mut *self.audit.borrow_mut());
        audit
            .settle(self.engine.registry().audience(), self.projection.principal())
            .map_err(|refusal| match refusal {
                crate::audience::EvidenceRefusal::UnrequestedEvidence { entry } => {
                    TransitionRefusal::UnrequestedEvidence { entry }
                }
                other => TransitionRefusal::ForeignEvidence(other),
            })
    }

    /// The membership context a record's evaluation reads: the policy's `within` assertions
    /// and registered providers beside the record's own recomputed answers.
    fn context<'e>(&'e self, expansions: &'e Expansions) -> MembershipContext<'e> {
        let audience = self.engine.registry().audience();
        MembershipContext::new(audience.within_assertions(), audience.providers(), expansions)
    }

    /// The contract a dispatch's outcome is judged under: the terms its release pinned, met with
    /// the judging revision's declaration ([`crate::admit::OutcomeContract`]).
    fn dispatch_contract(
        &self,
        trajectory: &TrajectoryId,
        dispatch: &DispatchId,
    ) -> Result<crate::admit::OutcomeContract<'_>, TransitionRefusal> {
        if dispatch.trajectory() != trajectory {
            return Err(TransitionRefusal::ForeignDispatch);
        }
        let call = self
            .projection
            .dispatch_call_of(dispatch)
            .ok_or(TransitionRefusal::UnknownDispatch)?;
        let pinned = self.projection.view(trajectory).release_terms(dispatch).cloned();
        crate::admit::outcome_contract(self.engine.registry(), call, pinned.as_ref())
            .ok_or(TransitionRefusal::UnknownDispatch)
    }

    fn open_dispatch_contract(
        &self,
        trajectory: &TrajectoryId,
        dispatch: &DispatchId,
    ) -> Result<crate::admit::OutcomeContract<'_>, TransitionRefusal> {
        let contract = self.dispatch_contract(trajectory, dispatch)?;
        if !self.projection.view(trajectory).is_open(dispatch) {
            return Err(TransitionRefusal::DispatchNotOpen);
        }
        Ok(contract)
    }

    fn candidate_derived(
        &mut self,
        trajectory: &TrajectoryId,
        subject: &crate::basis::SubjectKey,
        sanitizer: &SanitizerName,
        derived: &DerivedCandidate,
        lineage: &SanitizerLineage,
        evidence: &AudienceEvidence,
    ) -> Result<(), TransitionRefusal> {
        let expansions = self.recorded_expansions(evidence)?;
        let registered = self
            .engine
            .registry()
            .sanitizer(sanitizer)
            .ok_or_else(|| TransitionRefusal::UnknownSanitizer(sanitizer.as_str().to_string()))?;
        let (dispatch, source, from, value, residual) = match derived {
            DerivedCandidate::Call {
                source,
                from,
                call,
                label,
            } => {
                return self.substitution(
                    trajectory,
                    subject,
                    sanitizer,
                    source,
                    from,
                    call,
                    label,
                    lineage,
                    &expansions,
                );
            }
            DerivedCandidate::Result {
                dispatch,
                source,
                from,
                value,
                residual,
            } => (dispatch, source, from, value, residual),
            DerivedCandidate::Return { source: _, value } => {
                return self.return_derived(trajectory, subject, sanitizer, registered, value, lineage, &expansions);
            }
        };
        // The subject a confined candidate advances is its own dispatch's, never another's.
        if subject != &crate::basis::SubjectKey::ConfinedResult(dispatch.clone()) {
            return Err(TransitionRefusal::ForgedLabel);
        }
        let contract = self.dispatch_contract(trajectory, dispatch)?;
        self.deriving(trajectory, dispatch, residual.is_none())?;
        let views = self.projection.view(trajectory);
        let receiving = views
            .receiving_bound(dispatch)
            .ok_or(TransitionRefusal::UnknownDispatch)?
            .clone();
        let predecessor = match from {
            ConfinedFrom::Bound => {
                if views.bound_sanitizer(dispatch) != Some(sanitizer)
                    || views.candidate(subject).is_some()
                    || lineage.names() != [sanitizer.clone()]
                {
                    return Err(TransitionRefusal::SanitizerUnapplicable);
                }
                None
            }
            ConfinedFrom::Offer(offer) => {
                let recorded = hop_offer(&views, offer, sanitizer, subject)?;
                self.audit_inherit(&recorded.evidence)?;
                let current = views.candidate(subject).ok_or(TransitionRefusal::UnknownOffer)?;
                let DerivedCandidate::Result {
                    value: predecessor,
                    residual: owed,
                    ..
                } = current
                else {
                    return Err(TransitionRefusal::ForgedLabel);
                };
                extends_lineage(&views, subject, sanitizer, lineage)?;
                Some((predecessor.clone(), owed.clone()))
            }
        };
        // The bytes this hop read are the predecessor's, whichever it was.
        match &predecessor {
            None => self.observed_as(trajectory, dispatch, source)?,
            Some((body, _)) if source == &RawResultDigest::of(body.body.as_str().as_bytes()) => {}
            Some(_) => return Err(TransitionRefusal::ObservationMismatch),
        }
        let from_label = match &predecessor {
            None => self
                .projection
                .dispatch_call_of(dispatch)
                .expect("dispatch_contract resolved this dispatch's recorded call")
                .output_label_from(&contract.output_label(), &receiving),
            Some((body, _)) => body.label.clone(),
        };
        let label = sanitized_label(registered.derive_output(
            &from_label,
            &contract.contract.tags,
            &self.context(&expansions),
        ))?;
        if value.label != label {
            return Err(TransitionRefusal::ForgedLabel);
        }
        let derived_residual = crate::admit::confined_residual(&receiving, &label);
        if residual != &derived_residual {
            return Err(TransitionRefusal::AcceptanceMismatch);
        }
        if let Some((predecessor, _)) = &predecessor
            && !crate::plan::confined_hop_helps(&receiving, &predecessor.label, &label)
        {
            return Err(TransitionRefusal::SanitizerUnapplicable);
        }
        if residual.is_none() {
            self.derived.insert(dispatch.clone(), value.clone());
        }
        if let Some(pinned) = views.dispatch_evidence(dispatch) {
            self.audit_inherit(pinned)?;
        }
        self.audit_reads(&expansions);
        Ok(())
    }

    /// One staged return derivation: the fork's named sanitizer, applied once to the child's
    /// fold, before any crossing of this occurrence. The source digest is the record's own
    /// claim over bytes the log deliberately never stores.
    #[allow(clippy::too_many_arguments)]
    fn return_derived(
        &mut self,
        trajectory: &TrajectoryId,
        subject: &crate::basis::SubjectKey,
        sanitizer: &SanitizerName,
        registered: &crate::authority::Sanitizer,
        value: &crate::value::LabeledValue,
        lineage: &SanitizerLineage,
        expansions: &Expansions,
    ) -> Result<(), TransitionRefusal> {
        let crate::basis::SubjectKey::Return(id) = subject else {
            return Err(TransitionRefusal::ForgedLabel);
        };
        let child = id.child();
        let views = self.projection.view(trajectory);
        // The candidate is the parent's, like the crossing it stages.
        if views.parent_of(child) != Some(trajectory) {
            return Err(TransitionRefusal::ForeignTrajectory);
        }
        let policy = standing_return(&views, child, id)?;
        match policy.crossing().and_then(|crossing| crossing.sanitizer.as_ref()) {
            Some(ReturnSanitizer::Named(bound)) if bound == sanitizer => {}
            _ => return Err(TransitionRefusal::SanitizerUnapplicable),
        }
        if views.candidate(subject).is_some() || lineage.names() != [sanitizer.clone()] {
            return Err(TransitionRefusal::SanitizerUnapplicable);
        }
        let fold = views.branch_label(child);
        let label = sanitized_label(registered.derive_output(&fold, &[], &self.context(expansions)))?;
        if value.label != label {
            return Err(TransitionRefusal::ForgedLabel);
        }
        self.holds_child_floor(child, &label)?;
        self.audit_reads(expansions);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn substitution(
        &mut self,
        trajectory: &TrajectoryId,
        subject: &crate::basis::SubjectKey,
        sanitizer: &SanitizerName,
        source: &RawResultDigest,
        from: &crate::value::OfferId,
        call: &ResolvedCall,
        label: &Label,
        lineage: &SanitizerLineage,
        expansions: &Expansions,
    ) -> Result<(), TransitionRefusal> {
        if !matches!(subject, crate::basis::SubjectKey::Call { .. }) {
            return Err(TransitionRefusal::ForgedLabel);
        }
        let registered = self
            .engine
            .registry()
            .sanitizer(sanitizer)
            .ok_or_else(|| TransitionRefusal::UnknownSanitizer(sanitizer.as_str().to_string()))?;
        let views = self.projection.view(trajectory);
        let recorded = hop_offer(&views, from, sanitizer, subject)?;
        let predecessor = views.standing_call(subject).ok_or(TransitionRefusal::UnbackedOffer)?;
        if predecessor.digest() != recorded.call {
            return Err(TransitionRefusal::UnbackedOffer);
        }
        // The bytes this hop read are the ones the harness would have dispatched.
        if source != &RawResultDigest::of(predecessor.canonical_arguments().canonical_bytes()) {
            return Err(TransitionRefusal::ObservationMismatch);
        }
        extends_lineage(&views, subject, sanitizer, lineage)?;
        // The contract the offer was planned on judges the sanitizer's scope and the block the
        // hop improves; the contract the rewritten arguments select judges the rewrite.
        let registry = self.engine.registry();
        let before_contract = annotation_or_unknown(registry, predecessor)?;
        // A sanitizer rewrites arguments; the tool is never replaced.
        if call.tool() != predecessor.tool() {
            return Err(TransitionRefusal::ForgedLabel);
        }
        let declaration = registry
            .declaration(call)
            .ok_or(TransitionRefusal::SanitizerUnapplicable)?;
        // Annotation evidence binds the exact canonical call: whatever declaration the rewrite
        // selects, the pin the record carries is judged afresh against it.
        if crate::check::validate_annotation(registry, declaration, call).is_err() {
            return Err(TransitionRefusal::ForgedEvidence);
        }
        let contract = annotation_or_unknown(registry, call)?;
        contract
            .parameters
            .validate(call.arguments())
            .map_err(TransitionRefusal::InvalidPayload)?;
        // The sanitizer's jurisdiction reaches the contract the rewrite selects as well as the
        // one the offer was planned on.
        if !registered.applies_to(&contract.tags) {
            return Err(TransitionRefusal::SanitizerUnapplicable);
        }
        // A rewrite carries only the arguments and a fresh annotation pin, never a host-pinned
        // file basis.
        if registry.selects_alike(call, predecessor) && call.file_basis().is_some() {
            return Err(TransitionRefusal::ForgedLabel);
        }
        let stage = views.call_stage(subject);
        let context = self.context(expansions);
        let derived = sanitized_label(registered.derive_input(
            &stage.released(&views.current_label()),
            &before_contract.tags,
            &context,
        ))?;
        if label != &derived {
            return Err(TransitionRefusal::ForgedLabel);
        }
        let role = views.call_role(subject);
        let Ok(CheckOutcome::Block(before)) =
            crate::check::evaluate(&before_contract, &views, predecessor, &stage, role, &context)
        else {
            return Err(TransitionRefusal::UnbackedOffer);
        };
        let next = CallStage::substituting(derived, lineage.clone());
        let after =
            crate::check::evaluate(&contract, &views, call, &next, role, &context).map_err(TransitionRefusal::from)?;
        if !crate::plan::substitution_helps(&before, &after) {
            return Err(TransitionRefusal::SanitizerUnapplicable);
        }
        // The live hop's act gated the standing block's whole atom set before offering.
        self.audit_atoms(crate::plan::block_atoms(registry, &before_contract, &before));
        self.audit_inherit(&recorded.evidence)?;
        self.audit_reads(expansions);
        Ok(())
    }

    fn deriving(
        &self,
        trajectory: &TrajectoryId,
        dispatch: &DispatchId,
        settles: bool,
    ) -> Result<(), TransitionRefusal> {
        let views = self.projection.view(trajectory);
        if self.projection.admitted_result(dispatch) || self.derived.contains_key(dispatch) {
            return Err(TransitionRefusal::RepeatAdmission);
        }
        let confined = views.is_succeeded(dispatch);
        let admissible = match settles {
            true => confined || views.closed_successfully(dispatch),
            false => confined,
        };
        match admissible {
            true => Ok(()),
            false => Err(TransitionRefusal::DispatchNotOpen),
        }
    }

    fn candidate_accepted(
        &mut self,
        trajectory: &TrajectoryId,
        subject: &crate::basis::SubjectKey,
        offer: &crate::value::OfferId,
        narrowing: &Narrowing,
    ) -> Result<(), TransitionRefusal> {
        let crate::basis::SubjectKey::ConfinedResult(dispatch) = subject else {
            return Err(TransitionRefusal::ForgedLabel);
        };
        self.dispatch_contract(trajectory, dispatch)?;
        let views = self.projection.view(trajectory);
        let recorded = taken_offer(&views, offer)?;
        if &recorded.subject != subject || recorded.plan.narrowing() != Some(narrowing) {
            return Err(TransitionRefusal::UnbackedOffer);
        }
        let Some(DerivedCandidate::Result {
            residual: Some(owed), ..
        }) = views.candidate(subject)
        else {
            return Err(TransitionRefusal::AcceptanceMismatch);
        };
        if owed != narrowing || self.candidate_accepted.contains_key(dispatch) {
            return Err(TransitionRefusal::AcceptanceMismatch);
        }
        self.candidate_accepted.insert(dispatch.clone(), narrowing.clone());
        Ok(())
    }

    fn pending_dispatch(&self, trajectory: &TrajectoryId, dispatch: &DispatchId) -> Result<(), TransitionRefusal> {
        if dispatch.trajectory() != trajectory {
            return Err(TransitionRefusal::ForeignDispatch);
        }
        if self.projection.view(trajectory).dispatch_tool(dispatch).is_some() {
            return Err(TransitionRefusal::DanglingRemedy);
        }
        Ok(())
    }
}

fn ending_offer<'v>(
    views: &'v Views<'_>,
    trajectory: &TrajectoryId,
    offer: &crate::value::OfferId,
) -> Result<&'v crate::projection::RecordedOffer, TransitionRefusal> {
    let recorded = views.offer(offer).ok_or(TransitionRefusal::UnknownOffer)?;
    if recorded.trajectory != *trajectory {
        return Err(TransitionRefusal::ForeignOffer);
    }
    if recorded.end.is_some() {
        return Err(TransitionRefusal::OfferEnded);
    }
    Ok(recorded)
}

fn taken_offer<'v>(
    views: &'v Views<'_>,
    offer: &crate::value::OfferId,
) -> Result<&'v crate::projection::RecordedOffer, TransitionRefusal> {
    let recorded = views.offer(offer).ok_or(TransitionRefusal::UnknownOffer)?;
    if recorded.end != Some(crate::projection::OfferEnd::Accepted) {
        return Err(TransitionRefusal::OfferEnded);
    }
    Ok(recorded)
}

/// The offer an accepted hop record names: taken, planned as this sanitizer's hop, and standing
/// on this record's subject.
fn hop_offer<'v>(
    views: &'v Views<'_>,
    offer: &crate::value::OfferId,
    sanitizer: &SanitizerName,
    subject: &crate::basis::SubjectKey,
) -> Result<&'v crate::projection::RecordedOffer, TransitionRefusal> {
    let recorded = taken_offer(views, offer)?;
    if recorded.plan.hop() != Some(sanitizer) || &recorded.subject != subject {
        return Err(TransitionRefusal::UnbackedOffer);
    }
    Ok(recorded)
}

/// One name per lineage, and the chain a record claims is the chain that ran.
fn extends_lineage(
    views: &Views<'_>,
    subject: &crate::basis::SubjectKey,
    sanitizer: &SanitizerName,
    lineage: &SanitizerLineage,
) -> Result<(), TransitionRefusal> {
    match views.lineage(subject).extend(sanitizer.clone()) {
        Some(expected) if &expected == lineage => Ok(()),
        _ => Err(TransitionRefusal::SanitizerUnapplicable),
    }
}

/// The return policy of a live child's next return occurrence, which `id` must name.
fn standing_return<'v>(
    views: &'v Views<'_>,
    child: &TrajectoryId,
    id: &ChildReturnId,
) -> Result<&'v ReturnPolicy, TransitionRefusal> {
    if views.has_ended(child) {
        return Err(TransitionRefusal::BranchEnded);
    }
    if id.child() != child || id.occurrence() != views.returns_by(child) {
        return Err(TransitionRefusal::WrongReturnIdentity);
    }
    views.return_policy_of(child).ok_or(TransitionRefusal::NotForked)
}

fn confines(sequence: &Sequence<'_>, offer: &crate::value::OfferId, dispatch: &DispatchId) -> bool {
    sequence
        .projection
        .view(dispatch.trajectory())
        .offer(offer)
        .is_some_and(|recorded| recorded.subject == crate::basis::SubjectKey::ConfinedResult(dispatch.clone()))
}

fn belongs_to(sequence: &Sequence<'_>, act: &crate::basis::DecidedAct, fact: &Fact) -> bool {
    use crate::basis::DecidedAct;
    match (act, fact) {
        (DecidedAct::Proposals(act), Fact::ProposalBatchDecided { batch, .. }) => batch == act,
        // An offer names the act that surfaced its stage, so membership is what the record says.
        (act, Fact::OfferOpened { act: surfaced, .. }) => surfaced == act,
        (
            DecidedAct::Proposals(act),
            Fact::ValueAdmitted {
                provenance: crate::value::Provenance::ProviderRun { batch, .. },
                ..
            },
        ) => batch == act,
        (
            DecidedAct::Proposals(_),
            Fact::DispatchOpened { .. }
            | Fact::ForkPrepared {
                fork: ForkId::Spawn(_), ..
            }
            | Fact::CallApprovalConsumed { .. }
            | Fact::CandidateConsumed { .. },
        ) => true,
        (
            DecidedAct::Outcome(act),
            Fact::DispatchSucceeded { dispatch, .. } | Fact::DispatchClosed { dispatch, .. },
        ) => dispatch == act,
        // A confined hop belongs to the act that reported the outcome it derives from.
        (
            DecidedAct::Outcome(act),
            Fact::CandidateDerived {
                derived: DerivedCandidate::Result { dispatch, .. },
                ..
            },
        ) => dispatch == act,
        (
            DecidedAct::Outcome(act),
            Fact::ValueAdmitted {
                provenance: crate::value::Provenance::ToolResult { dispatch },
                ..
            },
        ) => dispatch == act,
        (DecidedAct::ChildReturn(act), Fact::ChildReturn { id, .. }) => id == act,
        (
            DecidedAct::ChildReturn(act),
            Fact::ValueAdmitted {
                provenance: crate::value::Provenance::ChildReturn { id, .. },
                ..
            },
        ) => id == act,
        (
            DecidedAct::ChildReturn(_),
            Fact::Boundary {
                kind: BoundaryKind::Merge { .. } | BoundaryKind::VoidReturn,
                ..
            },
        ) => true,
        (
            DecidedAct::ChildReturn(act),
            Fact::CandidateDerived {
                subject: crate::basis::SubjectKey::Return(id),
                ..
            },
        ) => id == act,
        (DecidedAct::Binding(act), Fact::ForkOpened { fork, .. }) => fork == act,
        (
            DecidedAct::Binding(act),
            Fact::ForkPrepared {
                fork: fork @ ForkId::Member { .. },
                ..
            },
        ) => fork == act,
        (
            DecidedAct::Binding(act),
            Fact::Boundary {
                trajectory,
                kind: BoundaryKind::Resume { .. },
            },
        ) => sequence.projection.view(trajectory).fork_of(trajectory) == Some(act),
        (DecidedAct::Offer(act), Fact::OfferAccepted { offer, .. }) => offer == act,
        // A hop's successor names the offer it was derived under, at any point.
        (
            DecidedAct::Offer(act),
            Fact::CandidateDerived {
                derived:
                    DerivedCandidate::Result {
                        from: ConfinedFrom::Offer(offer),
                        ..
                    }
                    | DerivedCandidate::Call { from: offer, .. },
                ..
            },
        ) => offer == act,
        (
            DecidedAct::Offer(act),
            Fact::DispatchClosed { dispatch, .. }
            | Fact::ValueAdmitted {
                provenance: crate::value::Provenance::ToolResult { dispatch },
                ..
            },
        ) => confines(sequence, act, dispatch),
        (DecidedAct::Offer(_), Fact::Denial { .. }) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{LabelRequirements, Requires, ToolAnnotation};
    use crate::label::{Audience, ReaderId, Trust};
    use crate::value::{LabeledValue, OfferNonce, ToolName, ValueBody, ValueId};

    fn traj() -> TrajectoryId {
        TrajectoryId::new("t")
    }

    fn note_tool() -> ToolAnnotation {
        ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("note"),
            tags: vec![],
            delta: crate::contract::Delta::NONE,
            forwards: crate::contract::Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(Trust::new(1)),
                    audience: vec![],
                },
                ..Requires::default()
            },
        }
    }

    fn engine() -> Engine {
        engine_over(&["suspicious", "trusted"])
    }

    /// The test policy over the trust chain `ranks`, lowest first.
    fn engine_over(ranks: &[&str]) -> Engine {
        let config = crate::registry::RegistryConfig {
            trust_chain: crate::registry::TrustChain::new(ranks.iter().map(|rank| rank.to_string()).collect()),
            tools: vec![crate::contract::ToolDeclaration::Declared(note_tool())],
            annotators: vec![],
            authorities: vec![],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
        };
        let profile = crate::profile::covering_declaration(&config);
        Engine::open(crate::profile::DeploymentPolicy {
            registry: config,
            planner_cap: crate::registry::PlannerCap::default(),
            profile,
        })
        .expect("the test policy opens")
        .at(line(), PolicyRevision::FIRST)
    }

    fn starting() -> Label {
        Label::new(Trust::new(1), Audience::public())
    }

    fn line() -> crate::profile::DeploymentName {
        crate::profile::DeploymentName::new("default")
    }

    fn opening(engine: &Engine, family: &TrajectoryId) -> Fact {
        engine
            .open_trajectory(family, crate::profile::PolicyFileKey::of(b"policy"), None)
            .expect("the opening validates against the empty log")
            .into_unsealed()
            .remove(0)
    }

    /// One `note` call proposed on `view`'s family and decided by `engine`.
    fn noted(engine: &Engine, view: &EngineView, id: &str) -> EngineDecision {
        let call = engine
            .resolve_call(ToolName::new("note"), b"{}")
            .expect("the call resolves");
        engine
            .handle(
                view,
                EngineEvent::Proposals(ProposalBatch {
                    id: ProposalBatchId::new(id),
                    trajectory: view.family().clone(),
                    provider_results: Vec::new(),
                    proposals: vec![ProposedCall {
                        tool: call.tool().clone(),
                        arguments: call.canonical_arguments().canonical_bytes().to_vec(),
                        annotation: None,
                    }],
                    spawn: None,
                    offer_nonce: nonce(),
                    evidence: Vec::new(),
                    audience: crate::audience::AudienceEvidence::default(),
                }),
            )
            .expect("the batch decides")
    }

    fn nonce() -> OfferNonce {
        OfferNonce::new([7u8; 32])
    }

    #[test]
    fn advancing_a_view_matches_rebuilding_it_from_the_records() {
        let engine = engine();
        let first = vec![opening(&engine, &traj())];
        let mut held = engine.view(&traj(), first.clone(), 1).expect("the log validates");

        let call = engine
            .resolve_call(ToolName::new("note"), b"{}")
            .expect("the call resolves");
        let decision = engine
            .handle(
                &held,
                EngineEvent::Proposals(ProposalBatch {
                    id: ProposalBatchId::new("b1"),
                    trajectory: traj(),
                    provider_results: Vec::new(),
                    proposals: vec![ProposedCall {
                        tool: call.tool().clone(),
                        arguments: call.canonical_arguments().canonical_bytes().to_vec(),
                        annotation: None,
                    }],
                    spawn: None,
                    offer_nonce: nonce(),
                    evidence: Vec::new(),
                    audience: crate::audience::AudienceEvidence::default(),
                }),
            )
            .expect("the batch decides");
        let dispatch = match &decision.follow_up {
            FollowUp::Proposals { released, .. } => released[0].dispatch.clone(),
            other => panic!("expected a release, got {other:?}"),
        };
        let released = decision.append.expect("a release appends");
        held.advance(&released).unwrap();
        let closed = engine
            .handle(
                &held,
                EngineEvent::Outcome(ToolReport {
                    dispatch: dispatch.clone(),
                    outcome: ToolOutcome::Success {
                        body: OutcomeBody::Available(ValueBody::new("the note")),
                    },
                    evidence: Vec::new(),
                    offer_nonce: nonce(),
                    audience: crate::audience::AudienceEvidence::default(),
                }),
            )
            .expect("the outcome closes the dispatch")
            .append
            .expect("the close appends");
        held.advance(&closed).unwrap();

        let whole = [first, released.facts().to_vec(), closed.facts().to_vec()].concat();
        let rebuilt = engine.view(&traj(), whole, 3).expect("the log validates");

        assert_eq!(held.position(), rebuilt.position());
        assert_eq!(held.projection(), rebuilt.projection());
        let family = traj();
        let views = held.projection().view(&family);
        assert!(!views.is_open(&dispatch));
        assert!(matches!(
            views.value_provenance(ValueId::new(0)),
            Some(Provenance::ToolResult { dispatch: produced }) if produced == &dispatch
        ));
        assert_eq!(views.current_label(), starting());

        assert_eq!(held.advance(&closed), Err(ViewMismatch::Stale { view: 3, batch: 2 }));
    }

    #[test]
    fn pinned_file_basis_changes_batch_authorization() {
        let engine = engine();
        let first = vec![opening(&engine, &traj())];
        let call = engine.resolve_call(ToolName::new("note"), b"{}").unwrap();
        let proposal = ProposedCall {
            tool: call.tool().clone(),
            arguments: call.canonical_arguments().canonical_bytes().to_vec(),
            annotation: None,
        };
        let basis = FileBasis::Read(crate::value::FileSource {
            version: "v1".into(),
            digest: "content".into(),
            label: Label::new(Trust::new(0), Audience::restricted([ReaderId::new("insider")])),
        });
        let event = EngineEvent::Proposals(ProposalBatch {
            id: ProposalBatchId::new("file-read"),
            trajectory: traj(),
            provider_results: Vec::new(),
            proposals: vec![proposal],
            spawn: None,
            offer_nonce: nonce(),
            evidence: vec![Evidence::File {
                position: 0,
                basis: basis.clone(),
            }],
            audience: crate::audience::AudienceEvidence::default(),
        });
        let decision = engine
            .handle(&engine.view(&traj(), first.clone(), 1).unwrap(), event)
            .unwrap();
        assert!(matches!(
            &decision.follow_up,
            FollowUp::Proposals { released, blocked, .. } if released.is_empty() && blocked.len() == 1
        ));
    }

    #[test]
    fn a_view_takes_no_batch_from_another_family_or_an_earlier_revision() {
        let earlier = engine().at(line(), PolicyRevision::FIRST);
        let engine = engine().at(line(), PolicyRevision::new(2));
        let opened = earlier
            .view(&traj(), vec![opening(&earlier, &traj())], 1)
            .expect("the log validates");
        let other_family = TrajectoryId::new("other");
        let elsewhere = engine
            .view(&other_family, vec![opening(&engine, &other_family)], 1)
            .expect("the log validates");
        let foreign = noted(&engine, &elsewhere, "f").append.expect("a release appends");
        // The view stands at the later revision, as the engine deciding there reads it; a batch the
        // earlier revision decided at the same position is a regression.
        let mut view = opened.crossed_to(&engine).into_owned();
        assert_eq!(view.advance(&foreign), Err(ViewMismatch::ForeignFamily));

        let regressed = noted(&earlier, &opened, "r").append.expect("a release appends");
        assert_eq!(
            view.advance(&regressed),
            Err(ViewMismatch::RevisionRegressed {
                view: PolicyRevision::new(2),
                batch: PolicyRevision::FIRST,
            })
        );
        assert!(
            matches!(
                earlier.admit_batch(&mut view, vec![]),
                Err(BatchRefusal::View(ViewMismatch::RevisionRegressed { .. }))
            ),
            "an earlier revision admits nothing after a later one"
        );
    }

    /// A batch decided at a revision whose trust chain names other ranks carries the view's labels
    /// across by rank name: a rank the new chain keeps keeps its name wherever it now sits, and the
    /// view advanced batch by batch equals a replay that judges each batch under its own revision.
    #[test]
    fn a_later_revision_carries_trust_across_by_rank_name() {
        let first = engine_over(&["suspicious", "trusted"]).at(line(), PolicyRevision::FIRST);
        let second = engine_over(&["untrusted", "suspicious", "trusted"]).at(line(), PolicyRevision::new(2));
        let opened = vec![opening(&first, &traj())];
        let mut held = first.view(&traj(), opened.clone(), 1).expect("the log validates");
        let trust = |view: &EngineView| view.views(&traj()).expect("the root is opened").current_label().trust;
        assert_eq!(trust(&held), Trust::new(1), "trusted is the first chain's top");

        let later = noted(&second, &held, "later").append.expect("a release appends");
        assert_eq!(later.revision(), Some(PolicyRevision::new(2)));
        held.advance(&later).expect("a later revision follows an earlier one");
        assert_eq!(held.stamp(), Some(PolicyRevision::new(2)));
        assert_eq!(
            trust(&held),
            Trust::new(2),
            "trusted keeps its name in the second chain"
        );

        let batches = [
            crate::engine::StampedBatch {
                revision: Some(PolicyRevision::FIRST),
                facts: &opened,
            },
            crate::engine::StampedBatch {
                revision: Some(PolicyRevision::new(2)),
                facts: later.facts(),
            },
        ];
        let replayed = second.replay(&traj(), &batches, &[&first]).expect("the log replays");
        assert_eq!(replayed, held, "replay across revisions equals the advanced view");

        assert_eq!(
            second.replay(&traj(), &batches, &[]),
            Err(crate::engine::ReplayRefusal::PolicyUnavailable {
                position: 0,
                revision: PolicyRevision::FIRST,
            }),
            "a batch is judged under its own revision or not at all"
        );
        assert_eq!(
            first.replay(&traj(), &batches, &[]),
            Err(crate::engine::ReplayRefusal::NewerRevision {
                position: 1,
                engine: PolicyRevision::FIRST,
                batch: PolicyRevision::new(2),
            })
        );
        let reversed = [batches[1], batches[0]];
        assert!(matches!(
            second.replay(&traj(), &reversed, &[&first]),
            Err(crate::engine::ReplayRefusal::RevisionRegressed { .. } | crate::engine::ReplayRefusal::Transition(_))
        ));
        let regressed = [
            batches[0],
            batches[1],
            crate::engine::StampedBatch {
                revision: Some(PolicyRevision::FIRST),
                facts: &[],
            },
        ];
        assert_eq!(
            second.replay(&traj(), &regressed, &[&first]),
            Err(crate::engine::ReplayRefusal::RevisionRegressed {
                position: 2,
                before: PolicyRevision::new(2),
                batch: PolicyRevision::FIRST,
            })
        );
    }

    /// Only a batch of host observations alone carries no stamp: it decides nothing and leaves
    /// the stamp where it was, after a publish too. A batch that holds engine records and names
    /// no revision is refused, wherever it stands.
    #[test]
    fn only_a_batch_that_decides_nothing_replays_without_a_stamp() {
        let first = engine_noting(Some(noting(None)), 1);
        let second = engine_noting(Some(noting(None)), 2);
        let (released, _, release) = released_note(&first);
        let unstamped: Vec<Fact> = release
            .iter()
            .cloned()
            .map(|fact| match fact {
                Fact::BasisAdvanced {
                    trajectory,
                    act,
                    advance,
                    ..
                } => Fact::BasisAdvanced {
                    trajectory,
                    act,
                    advance,
                    revision: None,
                },
                other => other,
            })
            .collect();
        let opened = vec![opening(&first, &traj())];
        let batch = |revision: Option<u64>, facts| crate::engine::StampedBatch {
            revision: revision.map(PolicyRevision::new),
            facts,
        };

        assert_eq!(
            second.replay(&traj(), &[batch(Some(1), &opened), batch(None, &unstamped)], &[&first]),
            Err(crate::engine::ReplayRefusal::Unstamped { position: 1 }),
            "an unstamped decision is refused"
        );
        let replayed = second
            .replay(&traj(), &[batch(Some(1), &opened), batch(Some(1), &release)], &[&first])
            .expect("the same decision, stamped, replays under its revision");
        assert_eq!(
            replayed,
            *released.crossed_to(&second),
            "the decision is the first revision's, and the view stands at the deciding one"
        );
        assert_eq!(replayed.stamp(), Some(PolicyRevision::new(2)));

        let after_a_publish = second
            .replay(
                &traj(),
                &[batch(Some(1), &opened), batch(Some(2), &[]), batch(None, &[])],
                &[&first],
            )
            .expect("a host observation after a publish decides nothing");
        assert_eq!(after_a_publish.stamp(), Some(PolicyRevision::new(2)));
        assert_eq!(after_a_publish.position(), 3);

        assert_eq!(
            second.replay(
                &traj(),
                &[batch(Some(1), &opened), batch(Some(2), &[]), batch(None, &unstamped)],
                &[&first],
            ),
            Err(crate::engine::ReplayRefusal::Unstamped { position: 2 }),
            "an unstamped decision after a publish is refused too"
        );
    }

    /// The test policy over two ranks with `note` declared as `note`, at `revision`.
    fn engine_noting(note: Option<ToolAnnotation>, revision: u64) -> Engine {
        let config = crate::registry::RegistryConfig {
            trust_chain: crate::registry::TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: note
                .into_iter()
                .map(crate::contract::ToolDeclaration::Declared)
                .collect(),
            annotators: vec![],
            authorities: vec![],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
        };
        // The family starts suspicious, so a suspicious result narrows nothing and every
        // variant of `note` releases alike: only the result's label tells them apart.
        let profile = crate::profile::ProfileDeclaration {
            starting_label: Label::new(Trust::new(0), Audience::public()),
            ..crate::profile::covering_declaration(&config)
        };
        Engine::open(crate::profile::DeploymentPolicy {
            registry: config,
            planner_cap: crate::registry::PlannerCap::default(),
            profile,
        })
        .expect("the test policy opens")
        .at(line(), PolicyRevision::FIRST)
        .at(line(), PolicyRevision::new(revision))
    }

    /// Release one `note` call on a freshly opened family under `first`, and return the view
    /// after the release with the dispatch it opened and the batch that released it.
    fn released_note(first: &Engine) -> (EngineView, DispatchId, Vec<Fact>) {
        let mut held = first
            .view(&traj(), vec![opening(first, &traj())], 1)
            .expect("the log validates");
        let decision = noted(first, &held, "b1");
        let dispatch = match &decision.follow_up {
            FollowUp::Proposals { released, .. } => released[0].dispatch.clone(),
            other => panic!("expected a release, got {other:?}"),
        };
        let batch = decision.append.expect("a release appends");
        held.advance(&batch).unwrap();
        (held, dispatch, batch.facts().to_vec())
    }

    /// Report `dispatch`'s success under `judge`, and return the label its result was admitted at
    /// once the batch replays under the revision that decided it.
    fn admitted_under(judge: &Engine, earlier: &Engine, held: &EngineView, dispatch: &DispatchId) -> Label {
        let decision = judge
            .handle(
                held,
                EngineEvent::Outcome(ToolReport {
                    dispatch: dispatch.clone(),
                    outcome: ToolOutcome::Success {
                        body: OutcomeBody::Available(ValueBody::new("the note")),
                    },
                    evidence: Vec::new(),
                    offer_nonce: nonce(),
                    audience: crate::audience::AudienceEvidence::default(),
                }),
            )
            .expect("the outcome decides");
        let batch = decision.append.expect("the close appends");
        let replayed = judge
            .extend(
                held.clone(),
                &[crate::engine::StampedBatch {
                    revision: batch.revision(),
                    facts: batch.facts(),
                }],
                &[earlier],
            )
            .expect("the outcome replays under the revision that decided it");
        assert!(!replayed.projection().view(&traj()).is_open(dispatch));
        batch
            .facts()
            .iter()
            .find_map(|fact| match fact {
                Fact::ValueAdmitted { value, .. } => Some(value.label.clone()),
                _ => None,
            })
            .expect("a success admits its result")
    }

    /// `note` with the delta `trust` and no requirement of its own.
    fn noting(trust: Option<u8>) -> ToolAnnotation {
        ToolAnnotation {
            delta: crate::contract::Delta {
                trust: trust.map(Trust::new),
                audience: None,
            },
            requires: Requires::default(),
            ..note_tool()
        }
    }

    /// A result reported after a later revision dropped its tool is admitted under the terms its
    /// release pinned: the revision judging it has no declaration of the call, and the release's
    /// own terms are what the call ran under.
    #[test]
    fn a_result_after_its_tool_is_dropped_is_admitted_under_its_release_terms() {
        let first = engine_noting(Some(noting(Some(0))), 1);
        let (held, dispatch, _) = released_note(&first);
        let second = engine_noting(None, 2);
        assert_eq!(
            admitted_under(&second, &first, &held, &dispatch),
            Label::new(Trust::new(0), Audience::public()),
            "the release's delta labels the result"
        );
    }

    /// A result reported after a later revision redeclared its tool is labelled by the meet of the
    /// release's terms and the judging revision's: a revision can tighten what a running call
    /// returns, and never loosen what its release promised.
    #[test]
    fn a_result_after_a_revision_is_labelled_by_release_terms_met_with_the_heads() {
        // Released as trusted output; the next revision says the tool returns suspicious data.
        let first = engine_noting(Some(noting(None)), 1);
        let (held, dispatch, _) = released_note(&first);
        let tightened = engine_noting(Some(noting(Some(0))), 2);
        assert_eq!(
            admitted_under(&tightened, &first, &held, &dispatch),
            Label::new(Trust::new(0), Audience::public()),
            "the head tightens a running call's result"
        );

        // Released as suspicious output; the next revision forgets the restriction.
        let first = engine_noting(Some(noting(Some(0))), 1);
        let (held, dispatch, _) = released_note(&first);
        let loosened = engine_noting(Some(noting(None)), 2);
        assert_eq!(
            admitted_under(&loosened, &first, &held, &dispatch),
            Label::new(Trust::new(0), Audience::public()),
            "the head never loosens what the release pinned"
        );
    }

    /// A rank the arriving chain drops crosses a value to its lowest rank and a floor to its
    /// highest: a crossing never loosens.
    #[test]
    fn a_dropped_rank_crosses_values_down_and_floors_up() {
        let leaving = crate::registry::TrustChain::new(vec!["low".into(), "mid".into(), "high".into()]);
        let arriving = crate::registry::TrustChain::new(vec!["low".into(), "high".into()]);
        assert!(
            leaving.crossing_to(&leaving).is_none(),
            "the same chain crosses as the identity"
        );
        let crossing = leaving.crossing_to(&arriving).expect("the chains differ");
        let crossed = |trust: u8, floor: bool| {
            let mut label = Label::new(Trust::new(trust), Audience::public());
            if floor {
                label.cross_floor(&crossing);
            } else {
                label.cross_value(&crossing);
            }
            label.trust
        };
        assert_eq!(crossed(0, false), Trust::new(0));
        assert_eq!(crossed(2, false), Trust::new(1), "high keeps its name");
        assert_eq!(
            crossed(1, false),
            Trust::new(0),
            "a value at a dropped rank drops to the lowest"
        );
        assert_eq!(
            crossed(1, true),
            Trust::new(1),
            "a floor at a dropped rank rises to the highest"
        );
        assert_eq!(
            crossed(u8::MAX, false),
            Trust::new(u8::MAX),
            "top trust is above every chain"
        );
    }

    #[test]
    fn a_record_on_an_unopened_trajectory_is_refused() {
        let engine = engine();
        let punctuation = |trajectory: TrajectoryId| Fact::Boundary {
            trajectory,
            kind: BoundaryKind::VoidReturn,
        };
        assert_eq!(
            engine.view(&traj(), vec![punctuation(traj())], 1).err(),
            Some(TransitionRefusal::Unopened)
        );
        assert_eq!(engine.view(&traj(), vec![], 0).err(), Some(TransitionRefusal::Unopened));
        let stranger = TrajectoryId::new("stranger");
        let on_stranger = Fact::ValueAdmitted {
            trajectory: stranger.clone(),
            value: LabeledValue::new(ValueBody::new("body"), Label::new(Trust::new(1), Audience::public())),
            provenance: Provenance::ChildReturn {
                child: TrajectoryId::new("kid"),
                id: crate::value::ChildReturnId::new(TrajectoryId::new("kid"), 0),
            },
        };
        assert_eq!(
            engine
                .view(&traj(), vec![opening(&engine, &traj()), on_stranger], 2)
                .err(),
            Some(TransitionRefusal::ForeignTrajectory)
        );
        assert_eq!(
            engine
                .view(&traj(), vec![opening(&engine, &traj()), punctuation(stranger)], 2)
                .err(),
            Some(TransitionRefusal::ForeignTrajectory)
        );
    }
}
