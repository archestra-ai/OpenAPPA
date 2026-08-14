//! The engine's one mutation boundary: the sealed batch and the view it was computed against.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::check::{CheckOutcome, Gap, Narrowing, RawBlock};
use crate::contract::ToolContract;
use crate::execute::AuthorityEvidence;
use crate::fact::{
    BoundaryKind, CloseOutcome, EffectSet, Fact, FactBatch, ForkSnapshot, ReturnDerivation, ReturnPolicy, Revision,
};
use crate::label::{EstablishedLabel, Label};
use crate::names::{AuthorityName, SanitizerName};
use crate::plan::PlannedBlock;
use crate::profile::PolicyIdentityV1;
use crate::projection::{Projection, Views};
use crate::registry::Registry;
use crate::value::{
    ChildReturnId, DispatchId, ForkId, Provenance, RawResultDigest, ResolvedCall, TrajectoryId, ValueBody, ValueId,
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

/// One model response's policy content: the ordered proposals it made for one
/// trajectory, under the identity that makes the act repeatable. Structurally plural from the
/// start — a deployment's hook gates one call at a time today, and the atomic
/// multi-proposal composition is `T01`'s.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProposalBatch {
    pub id: ProposalBatchId,
    pub trajectory: TrajectoryId,
    pub proposals: Vec<ResolvedCall>,
    /// Which proposal, if any, the runtime marks as the deployment's context-controlled spawn
    /// (`Q27`). Runtime names it — no configuration surface does — and the marked call is checked
    /// and released like any other. The engine refuses the mark where the deployment declares no
    /// context control.
    pub spawn: Option<SpawnMark>,
    /// One fresh 256-bit random value for this act. The engine mixes it into every block
    /// and offer identity it derives here and keeps none of it: entropy is input data, never engine
    /// state. Runtime supplies it per act and allocates no offer identity of its own.
    pub offer_nonce: crate::value::OfferNonce,
}

/// The one proposal of a batch that spawns a child: its position among the
/// batch's proposals. At most one exists, which is what makes it an `Option` rather than a flag
/// per proposal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SpawnMark(usize);

impl SpawnMark {
    pub const fn at(index: usize) -> SpawnMark {
        SpawnMark(index)
    }

    pub const fn index(self) -> usize {
        self.0
    }
}

/// One act the engine decides. A closed enum: an external operational failure is not an
/// event, and neither is a request, user turn, transcript, or host run.
///
/// Tool outcome, offer execution and child return join it as they move off the composed
/// operations; forking joins as `T39`'s `BindFork`, in its final shape rather than an interim
/// child-start variant this boundary would have to unpublish.
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
    /// Fresh entropy for the offers this act may surface: a denial re-plans the blocked
    /// call in the same decision, and those plans need identities of their own.
    pub offer_nonce: crate::value::OfferNonce,
}

/// What the runtime resolved for a selected offer. There is no "no answer" variant: a consult that
/// returns nothing does not resume the act at all, and the offer simply stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OfferOutcome {
    Approved(Vec<AuthorityEvidence>),
    Denied { authority: AuthorityName },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OfferFollowUp {
    Approved { call: ResolvedCall },
    Denied { block: Box<Blocked> },
    Invalidated,
}

/// The host's child identity for one prepared fork. Idempotent: repeating the same pair
/// appends nothing and answers the same, while naming another child for the fork — or reusing the
/// child for another fork — is refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForkBinding {
    pub fork: ForkId,
    pub child: TrajectoryId,
}

/// One child's return, addressed by the branch it ends. A child is forked once and
/// returns once, so the child identity names exactly one fork and one crossing; the fork identity
/// the parent's later offers bind is `T21`'s and `T23`'s to add.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChildReport {
    pub child: TrajectoryId,
    pub submission: ChildSubmission,
}

/// What the child submitted: one value, or none. The crossing path is derived from the
/// fork's own binding, never selected here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChildSubmission {
    Void,
    Value { body: ValueBody },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChildFollowUp {
    Merged { admitted: ValueBody },
    Ended,
    Blocked {
        narrowing: Narrowing,
        plans: Vec<crate::branch::ReturnPlan>,
    },
    Unresolved(Vec<crate::check::UnestablishedFact>),
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
}

/// How a tool run ended. `Failure` commits no effects; `Indeterminate` commits none and
/// leaves the reservation standing, because the call may have executed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolOutcome {
    Success { body: OutcomeBody },
    Failure,
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
    Sanitizer {
        sanitizer: SanitizerName,
        source: RawResultDigest,
        derived: ValueBody,
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
    Closed { admitted: Option<ValueBody> },
    Resolve(EvidenceRequest),
}

/// One released call: the dispatch the engine opened for it, and the canonical call to invoke.
/// The runtime never re-derives the identity — deriving it twice is what `T31` removes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Released {
    pub dispatch: DispatchId,
    pub call: ResolvedCall,
    /// The fork this release prepared, when the batch marked it as the spawn. The
    /// runtime carries it until the harness names the child, then binds the two.
    pub fork: Option<ForkId>,
}

/// One proposal of a repeated batch whose dispatch has already been invoked (`Q24`). It cannot be
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
    Fork { child: TrajectoryId },
    Offer(OfferFollowUp),
    Proposals {
        released: Vec<Released>,
        blocked: Vec<Blocked>,
        forks: Vec<ForkId>,
        spent: Vec<ResolvedCall>,
        settled: Vec<Settled>,
    },
}

/// Why the boundary refused an event outright. A policy block is a decision, not an error: this
/// means the event cannot be processed at all — a malformed call, a fork the branch rules refuse.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TransitionError {
    #[error(transparent)]
    Call(#[from] crate::engine::EngineError),
    #[error("the view was built under another policy identity")]
    ForeignView,
    #[error("the decision does not pass the transition validator: {0}")]
    Invalid(#[from] TransitionRefusal),
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
    #[error("a proposal batch carries at least one proposal")]
    EmptyBatch,
    #[error("more than one proposal in a batch awaits ordered in-batch composition (T01)")]
    UncomposedBatch,
    #[error("no dispatch of this family was opened under that identity")]
    UnknownDispatch,
    #[error("the report contradicts the observation this dispatch already checkpointed")]
    ObservationMismatch,
    #[error("the dispatch recorded success: a failure or indeterminate outcome contradicts it")]
    ContradictedSuccess,
    #[error("this contract's output is confined awaiting a cast, which this boundary does not yet resolve")]
    ConfinedResult,
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
    #[error("a fork takes an unused child identity")]
    ChildAlreadyUsed,
    #[error("a fork bound to a return sanitizer crosses through the composed path until T23")]
    BoundReturnSanitizer,
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
/// whole-batch revision append and advances its [`EngineView`] with it, and never
/// reconstructs engine work by reading the facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedFactBatch {
    batch: FactBatch,
    policy: PolicyIdentityV1,
    family: TrajectoryId,
}

impl ValidatedFactBatch {
    /// Seal a validated batch. Crate-private on purpose: every call site is an engine transition
    /// that has already run the batch through the [`Sequence`] validator.
    pub(crate) fn seal(batch: FactBatch, policy: PolicyIdentityV1, family: TrajectoryId) -> ValidatedFactBatch {
        ValidatedFactBatch { batch, policy, family }
    }

    pub fn basis(&self) -> Revision {
        self.batch.basis
    }

    pub(crate) fn policy(&self) -> PolicyIdentityV1 {
        self.policy
    }

    pub(crate) fn family(&self) -> &TrajectoryId {
        &self.family
    }

    /// The facts to append, for the store that persists them. Reading them to reconstruct
    /// released work or feedback is forbidden; the decision's follow-up carries that.
    pub fn facts(&self) -> &[Fact] {
        &self.batch.facts
    }

    /// Serialization removes the seal: what crosses to storage is the plain batch, and
    /// what comes back is untrusted until it passes the validator again.
    pub fn into_unsealed(self) -> FactBatch {
        self.batch
    }
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum ViewMismatch {
    #[error("the batch was computed against revision {batch:?} but the view stands at {view:?}")]
    Stale { view: Revision, batch: Revision },
    #[error("the batch belongs to another trajectory family")]
    ForeignFamily,
    #[error("the batch was decided under another policy identity")]
    ForeignPolicy,
}

/// The engine's derived working picture of one family log: the validated records and
/// the projection built from them. Opaque and disposable — the runtime stores it for the next
/// event, but every constructor and mutator here belongs to the engine.
#[derive(Clone, Debug)]
pub struct EngineView {
    projection: Projection,
    policy: PolicyIdentityV1,
    family: TrajectoryId,
}

impl EngineView {
    /// Take the projection a [`Sequence`] validated. Crate-private: the public entry is
    /// `Engine::view`, which runs that validation.
    pub(crate) fn validated(projection: Projection, policy: PolicyIdentityV1, family: TrajectoryId) -> EngineView {
        EngineView {
            projection,
            policy,
            family,
        }
    }

    pub(crate) fn projection(&self) -> &Projection {
        &self.projection
    }

    /// The validated views of one trajectory in this family, for the composed operations a runtime
    /// still drives. It exists for the window in which those operations are public: `handle` needs
    /// no such accessor, and the cutover removes both together.
    pub fn views<'a>(&'a self, trajectory: &'a TrajectoryId) -> crate::projection::Views<'a> {
        self.projection.view(trajectory)
    }

    pub(crate) fn policy(&self) -> PolicyIdentityV1 {
        self.policy
    }

    pub(crate) fn family(&self) -> &TrajectoryId {
        &self.family
    }

    pub fn revision(&self) -> Revision {
        self.projection.revision()
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
    /// any other revision belongs to another view, and applying it would leave records and
    /// revision describing different logs.
    pub fn advance(&mut self, batch: &ValidatedFactBatch) -> Result<(), ViewMismatch> {
        if batch.policy() != self.policy {
            return Err(ViewMismatch::ForeignPolicy);
        }
        if batch.family() != &self.family {
            return Err(ViewMismatch::ForeignFamily);
        }
        if batch.basis() != self.revision() {
            return Err(ViewMismatch::Stale {
                view: self.revision(),
                batch: batch.basis(),
            });
        }
        for fact in batch.facts() {
            self.projection.fold(fact);
        }
        self.projection.set_revision(batch.basis().next());
        Ok(())
    }
}

/// Why the transition validator refused a record. One vocabulary for both directions: a
/// candidate batch the engine has just built and a persisted log being replayed pass the same
/// rules, so a refusal always says the same thing — this record cannot follow the ones before it.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TransitionRefusal {
    #[error("a record names a trajectory outside this family")]
    ForeignTrajectory,
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
    #[error("a released dispatch is one the check refuses and no recorded remedy admits")]
    UnreleasedDispatch,
    #[error("the dispatch is not open")]
    DispatchNotOpen,
    #[error("the dispatch already recorded its success checkpoint")]
    RepeatCheckpoint,
    #[error("a close contradicts the dispatch's recorded success")]
    ContradictedSuccess,
    #[error("a record commits effects its contract does not declare")]
    EffectsMismatch,
    #[error("an admitted value does not carry the label its source derives")]
    ForgedLabel,
    #[error("a second value is admitted for one dispatch or child return")]
    RepeatAdmission,
    #[error("cast record names a value not admitted earlier in the log")]
    CastBeforeSource,
    #[error("cast record's trajectory neither admitted nor inherited the value it resolves")]
    ForeignResolution,
    #[error("cast record resolves a source that is already fully established")]
    RepeatResolution,
    #[error("cast record names unregistered cast {0}")]
    UnknownCast(String),
    #[error("cast record's resolution is not admissible for its source under the registered cast")]
    InadmissibleResolution,
    #[error("cast record's scope does not cover the source's originating tool")]
    OutOfScopeResolution,
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
    #[error("a decision record carries other than the one proposal this engine composes (T01)")]
    UncomposedDecision,
    #[error("fork record's return policy is not the deployment's child-return binding")]
    ForkReturnPolicyMismatch,
    #[error("a return or fork record names a branch that has already ended")]
    BranchEnded,
    #[error("a return record names a trajectory that was never forked")]
    NotForked,
    #[error("a return record's identity is not the next one for its child")]
    WrongReturnIdentity,
    #[error("a crossing does not match the fork's return policy")]
    ReturnPolicyMismatch,
    #[error("a merge that narrows the parent records no acceptance, or another than the one it folds")]
    ReturnNarrowsParent,
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
    #[error("a ruling claims a gap its mandate does not cover, or the block does not carry")]
    RulingOutsideMandate,
    #[error("a derivation names a sanitizer this dispatch is not bound to, or one it cannot apply")]
    SanitizerUnapplicable,
    #[error("a ruling, acceptance or sanitizer binding names a dispatch that never opened")]
    DanglingRemedy,
    #[error("a cast or sanitizer settled a result the log never admitted")]
    UnadmittedDerivation,
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
    #[error("the denial record is not backed by a denial of this authority for this call")]
    UnbackedDenial,
    #[error("a record spends a call approval this log never prepared")]
    UnknownApproval,
    #[error("the record spends an offer or approval that is no longer current")]
    StaleSpend,
}

struct PendingRelease {
    dispatch: DispatchId,
    call: ResolvedCall,
    prepares_fork: bool,
    next: ReleasePart,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReleasePart {
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
    sanitizer: Option<SanitizerName>,
}

/// The one sequential transition validator. It admits records one at a time against the
/// state the records before them built, and folds each admitted record into that state through
/// [`Projection::fold`] — the same fold a held view advances by, so validation and projection can
/// never describe different logs.
pub(crate) struct Sequence<'a> {
    registry: &'a Registry,
    child_return: &'a ReturnPolicy,
    projection: Projection,
    members: BTreeSet<TrajectoryId>,
    pending: std::collections::VecDeque<PendingRelease>,
    remedies: BTreeMap<DispatchId, Remedy>,
    derived: BTreeMap<DispatchId, Derived>,
    cast_accepted: BTreeMap<DispatchId, Narrowing>,
    admitted: BTreeSet<DispatchId>,
    /// Crossings recorded and not yet admitted into the parent, and merges not yet made: a
    /// crossing ends the child, so a log that records one without folding it into the parent
    /// would leave the parent reading a label the crossing never restricted.
    crossed: BTreeMap<ChildReturnId, Crossing>,
    accepted: BTreeMap<ChildReturnId, Narrowing>,
    menu: Option<(
        crate::basis::SubjectKey,
        crate::value::CanonicalDigest,
        Vec<crate::plan::ExecutableRemedyPlan>,
    )>,
    declared: Option<Declaration>,
}

struct Declaration {
    act: crate::basis::DecidedAct,
    declared: crate::basis::BasisAdvance,
    owed: crate::basis::BasisAdvance,
}

enum Derived {
    Cast(EstablishedLabel, RawResultDigest),
    Sanitized(Label),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Crossing {
    Recorded,
    Admitted,
    Merged,
}

impl<'a> Sequence<'a> {
    pub(crate) fn empty(
        registry: &'a Registry,
        child_return: &'a ReturnPolicy,
        family: &TrajectoryId,
        revision: Revision,
    ) -> Sequence<'a> {
        Sequence {
            registry,
            child_return,
            projection: Projection::empty(revision),
            members: BTreeSet::from([family.clone()]),
            pending: std::collections::VecDeque::new(),
            remedies: BTreeMap::new(),
            derived: BTreeMap::new(),
            cast_accepted: BTreeMap::new(),
            admitted: BTreeSet::new(),
            crossed: BTreeMap::new(),
            accepted: BTreeMap::new(),
            declared: None,
            menu: None,
        }
    }

    /// A validator standing where `view` stands, for admitting the candidate records of one
    /// decision. The view's own records passed these rules already, so the state is resumed
    /// rather than re-judged.
    pub(crate) fn resuming(registry: &'a Registry, child_return: &'a ReturnPolicy, view: &EngineView) -> Sequence<'a> {
        Sequence {
            registry,
            child_return,
            projection: view.projection().clone(),
            members: view
                .projection()
                .trajectories()
                .chain(std::iter::once(view.family()))
                .cloned()
                .collect(),
            pending: std::collections::VecDeque::new(),
            remedies: BTreeMap::new(),
            derived: BTreeMap::new(),
            cast_accepted: BTreeMap::new(),
            admitted: view.projection().admitted_dispatches(),
            crossed: BTreeMap::new(),
            accepted: BTreeMap::new(),
            declared: None,
            menu: None,
        }
    }

    pub(crate) fn admit(&mut self, fact: &Fact) -> Result<(), TransitionRefusal> {
        self.member(fact)?;
        let released = self.obliged(fact)?;
        if !matches!(fact, Fact::OfferOpened { .. }) {
            self.menu = None;
        }
        let implied = self.implied_advance(fact);
        match fact {
            Fact::TrajectoryOpened { .. } => {}
            Fact::BasisAdvanced { act, advance, .. } => self.declare(act, advance)?,
            Fact::OfferOpened {
                trajectory,
                offer,
                batch,
                call,
                subject,
                plan,
                basis,
                ..
            } => self.offer_opened(trajectory, offer, batch, call, subject, plan, basis)?,
            Fact::OfferAccepted { trajectory, offer } => self.offer_accepted(trajectory, offer)?,
            Fact::CallApprovalConsumed { trajectory, offer, .. } => {
                let views = self.projection.view(trajectory);
                let approval = views.approval(offer).ok_or(TransitionRefusal::UnknownApproval)?;
                let (recorded, subject) = (approval.basis, crate::basis::SubjectKey::Approval(*offer));
                self.may_spend(trajectory, &subject, &recorded)?;
            }
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
                sanitizer,
                basis,
            } => self.call_approved(trajectory, offer, call, plan, acceptance, rulings, sanitizer, basis)?,
            Fact::ProposalBatchDecided {
                trajectory,
                batch,
                proposals,
                spawn,
                released,
            } => self.decided(trajectory, batch, proposals, *spawn, released)?,
            Fact::DispatchOpened {
                trajectory,
                dispatch,
                tool,
                arguments,
                proposed_label,
                proposed_effects,
                dynamic_resolutions,
            } => {
                let call = ResolvedCall::new(tool.clone(), arguments.clone())
                    .with_dynamic_resolutions(dynamic_resolutions.clone());
                if call.dynamic_resolutions() != dynamic_resolutions.as_slice() {
                    return Err(TransitionRefusal::ForgedLabel);
                }
                self.opened(trajectory, dispatch, &call, released)?;
                let contract = self
                    .registry
                    .tool(tool)
                    .ok_or_else(|| TransitionRefusal::UnknownTool(tool.as_str().to_string()))?;
                if proposed_effects != &contract.emits {
                    return Err(TransitionRefusal::EffectsMismatch);
                }
                // And the bound it pins is the one the release committed.
                let views = self.projection.view(trajectory);
                if proposed_label
                    != crate::check::committed_label_for_call(contract, &views.current_label(), &call).bound()
                {
                    return Err(TransitionRefusal::ForgedLabel);
                }
            }
            Fact::DispatchSucceeded {
                trajectory,
                dispatch,
                effects,
                observed: _,
            } => {
                self.declaring(&crate::basis::DecidedAct::Outcome(dispatch.clone()))?;
                let contract = self.open_dispatch_contract(trajectory, dispatch)?;
                if self.projection.view(trajectory).is_succeeded(dispatch) {
                    return Err(TransitionRefusal::RepeatCheckpoint);
                }
                if effects != &contract.emits {
                    return Err(TransitionRefusal::EffectsMismatch);
                }
            }
            Fact::DispatchClosed {
                trajectory,
                dispatch,
                outcome,
            } => {
                self.declaring(&crate::basis::DecidedAct::Outcome(dispatch.clone()))?;
                let contract = self.open_dispatch_contract(trajectory, dispatch)?;
                let checkpointed = self.projection.view(trajectory).is_succeeded(dispatch);
                match (outcome, checkpointed) {
                    (CloseOutcome::Failure | CloseOutcome::Indeterminate, true) => {
                        return Err(TransitionRefusal::ContradictedSuccess);
                    }
                    (CloseOutcome::Success { effects }, true) if effects != &EffectSet::default() => {
                        return Err(TransitionRefusal::EffectsMismatch);
                    }
                    (CloseOutcome::Success { effects }, false) if effects != &contract.emits => {
                        return Err(TransitionRefusal::EffectsMismatch);
                    }
                    _ => {}
                }
            }
            Fact::ValueAdmitted {
                trajectory,
                value,
                provenance,
            } => self.value_admitted(trajectory, value, provenance)?,
            Fact::CastApplied {
                trajectory,
                value,
                resolved,
                cast,
            } => self.cast_applied(trajectory, *value, resolved, cast)?,
            Fact::OutputCastApplied {
                trajectory,
                dispatch,
                cast,
                resolved,
                raw_digest,
            } => {
                let contract = self.dispatch_contract(trajectory, dispatch)?;
                self.settling(trajectory, dispatch)?;
                self.observed_as(trajectory, dispatch, raw_digest)?;
                let raw = self.output_label(trajectory, dispatch, contract);
                crate::admit::validate_pending_cast(self.registry, contract, &raw, cast, resolved)
                    .map_err(|_| TransitionRefusal::InadmissibleResolution)?;
                self.derived
                    .insert(dispatch.clone(), Derived::Cast(resolved.clone(), *raw_digest));
            }
            Fact::OutputCastLapsed {
                trajectory,
                dispatch,
                cast,
                resolved,
                raw_digest,
            } => {
                let contract = self.dispatch_contract(trajectory, dispatch)?;
                self.settling(trajectory, dispatch)?;
                self.observed_as(trajectory, dispatch, raw_digest)?;
                let raw = self.output_label(trajectory, dispatch, contract);
                crate::admit::validate_pending_cast(self.registry, contract, &raw, cast, resolved)
                    .map_err(|_| TransitionRefusal::InadmissibleResolution)?;
            }
            Fact::OutputCastAccepted {
                trajectory,
                dispatch,
                narrowing,
            } => {
                self.dispatch_contract(trajectory, dispatch)?;
                let Some(Derived::Cast(resolved, _)) = self.derived.get(dispatch) else {
                    return Err(TransitionRefusal::AcceptanceMismatch);
                };
                if self.cast_accepted.contains_key(dispatch) {
                    return Err(TransitionRefusal::AcceptanceMismatch);
                }
                if crate::admit::pending_cast_narrowing(&self.projection.view(trajectory), resolved).as_ref()
                    != Some(narrowing)
                {
                    return Err(TransitionRefusal::AcceptanceMismatch);
                }
                self.cast_accepted.insert(dispatch.clone(), narrowing.clone());
            }
            Fact::OutputSanitizerApplied {
                trajectory,
                dispatch,
                sanitizer,
                transition,
                raw_digest,
            } => {
                let contract = self.dispatch_contract(trajectory, dispatch)?;
                self.settling(trajectory, dispatch)?;
                self.observed_as(trajectory, dispatch, raw_digest)?;
                let raw = self.output_label(trajectory, dispatch, contract);
                let registered = self
                    .registry
                    .sanitizer(sanitizer)
                    .ok_or_else(|| TransitionRefusal::UnknownSanitizer(sanitizer.as_str().to_string()))?;
                let bound = self.projection.view(trajectory).bound_sanitizer(dispatch) == Some(sanitizer);
                let derived = registered.derive_output(&raw);
                match derived {
                    Some(label) if bound && &registered.transition == transition => {
                        self.derived.insert(dispatch.clone(), Derived::Sanitized(label));
                    }
                    _ => return Err(TransitionRefusal::SanitizerUnapplicable),
                }
            }
            Fact::Ruling {
                trajectory,
                dispatch,
                plan,
                authority,
                covers,
                reviewed,
            } => {
                self.pending_dispatch(trajectory, dispatch)?;
                if self.registry.authority(authority).is_none() {
                    return Err(TransitionRefusal::UnknownAuthority(authority.as_str().to_string()));
                }
                let remedy = self.remedies.entry(dispatch.clone()).or_default();
                remedy.reviewed.push(reviewed.clone());
                remedy.rulings.push((authority.clone(), covers.clone()));
                remedy.plans.insert(*plan);
            }
            Fact::Acceptance {
                trajectory,
                dispatch,
                plan,
                narrowing,
            } => {
                self.pending_dispatch(trajectory, dispatch)?;
                let remedy = self.remedies.entry(dispatch.clone()).or_default();
                remedy.acceptance = Some(narrowing.clone());
                remedy.plans.insert(*plan);
            }
            Fact::OutputSanitizerBound {
                trajectory,
                dispatch,
                plan,
                sanitizer,
            } => {
                self.pending_dispatch(trajectory, dispatch)?;
                if self.registry.sanitizer(sanitizer).is_none_or(|s| !s.on.output) {
                    return Err(TransitionRefusal::UnknownSanitizer(sanitizer.as_str().to_string()));
                }
                let remedy = self.remedies.entry(dispatch.clone()).or_default();
                remedy.sanitizer = Some(sanitizer.clone());
                remedy.plans.insert(*plan);
            }
            Fact::Denial {
                trajectory,
                digest,
                authority,
            } => {
                if let Some(open) = &self.declared
                    && let crate::basis::DecidedAct::Offer(offer) = &open.act
                {
                    let views = self.projection.view(trajectory);
                    let selected = views.offer(offer).ok_or(TransitionRefusal::UnknownOffer)?;
                    if &selected.call != digest || !selected.plan.names_authority(authority) {
                        return Err(TransitionRefusal::UnbackedDenial);
                    }
                }
                if self.registry.authority(authority).is_none() {
                    return Err(TransitionRefusal::UnknownAuthority(authority.as_str().to_string()));
                }
            }
            Fact::ChildReturn {
                trajectory,
                id,
                value,
                derivation,
            } => {
                self.child_return(trajectory, id, value, derivation)?;
                self.crossed.insert(id.clone(), Crossing::Recorded);
            }
            Fact::ChildReturnAcceptance {
                trajectory,
                child_return,
                narrowing,
            } => {
                if self.projection.view(trajectory).parent_of(child_return.child()) != Some(trajectory) {
                    return Err(TransitionRefusal::ForeignTrajectory);
                }
                let pending = self
                    .crossed
                    .get(child_return)
                    .is_some_and(|crossing| matches!(crossing, Crossing::Recorded));
                if !pending || self.accepted.insert(child_return.clone(), narrowing.clone()).is_some() {
                    return Err(TransitionRefusal::AcceptanceMismatch);
                }
            }
            Fact::ForkPrepared {
                trajectory,
                fork,
                snapshot,
                return_policy,
            } => {
                if released == Obligation::Free {
                    return Err(TransitionRefusal::UnbackedDecision);
                }
                if !self.registry.profile().context_control() {
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
                if return_policy != self.child_return {
                    return Err(TransitionRefusal::ForkReturnPolicyMismatch);
                }
                if &views.freeze_basis() != snapshot {
                    return Err(TransitionRefusal::ForkBasisMismatch);
                }
            }
            Fact::ForkOpened { trajectory, fork } => {
                let preparation = self
                    .projection
                    .prepared_fork(fork)
                    .ok_or(TransitionRefusal::UnknownFork)?;
                let views = self.projection.view(trajectory);
                if self.projection.bound_child(fork).is_some() {
                    return Err(TransitionRefusal::ForkAlreadyBound);
                }
                if trajectory == &preparation.parent || views.is_active(trajectory) {
                    return Err(TransitionRefusal::ChildActiveBeforeFork);
                }
                if views.has_ended(&preparation.parent) {
                    return Err(TransitionRefusal::BranchEnded);
                }
                if views.dispatch_failed(fork.dispatch()) {
                    return Err(TransitionRefusal::SpawnFailed);
                }
            }
            Fact::Boundary { trajectory, kind } => self.boundary(trajectory, kind)?,
            // Transcript memory: algebraically inert, and outside this validator by `IMP-4`.
            Fact::AssistantMessage { .. } | Fact::BlockFeedback { .. } => {}
        }
        self.settle_advance(fact, &implied)?;
        self.projection.fold(fact);
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
        batch: &ProposalBatchId,
        call: &crate::value::CanonicalDigest,
        subject: &crate::basis::SubjectKey,
        plan: &crate::plan::ExecutableRemedyPlan,
        basis: &crate::basis::PolicyBasis,
    ) -> Result<(), TransitionRefusal> {
        let views = self.projection.view(trajectory);
        if views.offer(offer).is_some() {
            return Err(TransitionRefusal::OfferReopened);
        }
        match &self.declared {
            Some(open) if open.act == crate::basis::DecidedAct::Proposals(batch.clone()) => {}
            Some(open) if matches!(open.act, crate::basis::DecidedAct::Offer(_)) => {}
            _ => return Err(TransitionRefusal::UnbackedOffer),
        }
        let crate::basis::SubjectKey::Call {
            trajectory: subject_trajectory,
            batch: subject_batch,
            position,
        } = subject
        else {
            return Err(TransitionRefusal::ForeignOffer);
        };
        if subject_trajectory != trajectory || subject_batch != batch {
            return Err(TransitionRefusal::ForeignOffer);
        }
        if let Some((derived_for, derived_from, offered)) = &self.menu
            && derived_for == subject
        {
            if derived_from != call || !offered.contains(plan) {
                return Err(TransitionRefusal::UnbackedOffer);
            }
            return match basis == &views.basis_for(subject) {
                true => Ok(()),
                false => Err(TransitionRefusal::ForgedBasis),
            };
        }
        let proposal = match views.decided_batch(batch) {
            Some(decided) if decided.trajectory == *trajectory => decided
                .proposals
                .get(*position as usize)
                .ok_or(TransitionRefusal::UnbackedOffer)?
                .clone(),
            // An offer naming a decision this log never made, or one of another branch.
            _ => return Err(TransitionRefusal::UnbackedOffer),
        };
        if proposal.digest() != *call {
            return Err(TransitionRefusal::UnbackedOffer);
        }
        let proposal = &proposal;
        let contract = self
            .registry
            .tool(proposal.tool())
            .ok_or_else(|| TransitionRefusal::UnknownTool(proposal.tool().as_str().to_string()))?;
        let CheckOutcome::Block(block) = crate::check::evaluate(contract, &views, proposal) else {
            return Err(TransitionRefusal::UnbackedOffer);
        };
        let offered = crate::plan::plan(self.registry, &views, proposal, &block);
        let offered: Vec<_> = offered
            .plans
            .iter()
            .filter_map(crate::plan::RemedyPlan::executable)
            .cloned()
            .collect();
        if !offered.contains(plan) {
            return Err(TransitionRefusal::UnbackedOffer);
        }
        self.menu = Some((subject.clone(), *call, offered));
        // The post-decision basis, which the declaration admitted just above already applied.
        if basis != &views.basis_for(subject) {
            return Err(TransitionRefusal::ForgedBasis);
        }
        Ok(())
    }

    fn offer_accepted(
        &mut self,
        trajectory: &TrajectoryId,
        offer: &crate::value::OfferId,
    ) -> Result<(), TransitionRefusal> {
        let views = self.projection.view(trajectory);
        let recorded = ending_offer(&views, trajectory, offer)?;
        let (basis, subject) = (recorded.basis, recorded.subject.clone());
        self.may_spend(trajectory, &subject, &basis)?;
        match &self.declared {
            Some(open) if open.act == crate::basis::DecidedAct::Offer(*offer) => Ok(()),
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
        sanitizer: &Option<SanitizerName>,
        basis: &crate::basis::PolicyBasis,
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
        if views.approval(offer).is_some() || views.current_approval(call).is_some() {
            return Err(TransitionRefusal::ApprovalRepeated);
        }
        let offered = &recorded.plan;
        if call.digest() != recorded.call
            || plan != &offered.id
            || acceptance.as_ref() != offered.narrowing()
            || sanitizer.as_ref() != offered.sanitizer()
            || rulings.len() != offered.required.len()
        {
            return Err(TransitionRefusal::UnbackedApproval);
        }
        if rulings.iter().zip(&offered.required).any(|(given, required)| {
            given.offer != *offer || given.authority != required.authority || given.covers != required.covers
        }) {
            return Err(TransitionRefusal::UnbackedApproval);
        }
        let contract = self
            .registry
            .tool(call.tool())
            .ok_or_else(|| TransitionRefusal::UnknownTool(call.tool().as_str().to_string()))?;
        let live = views.current_label();
        if rulings
            .iter()
            .any(|evidence| evidence.reviewed.tool != contract.name || evidence.reviewed.trajectory_label != live)
        {
            return Err(TransitionRefusal::UnbackedApproval);
        }
        if basis != &views.basis_for(&crate::basis::SubjectKey::Approval(*offer)) {
            return Err(TransitionRefusal::ForgedBasis);
        }
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
        if advance.flows.iter().any(|flow| !self.members.contains(flow)) {
            return Err(TransitionRefusal::ForeignTrajectory);
        }
        self.declared = Some(Declaration {
            act: act.clone(),
            declared: advance.clone(),
            owed: advance.clone(),
        });
        Ok(())
    }

    fn settle_advance(&mut self, fact: &Fact, implied: &crate::basis::BasisAdvance) -> Result<(), TransitionRefusal> {
        if implied.is_empty() {
            return Ok(());
        }
        if !self.declared.as_ref().is_some_and(|open| belongs_to(&open.act, fact)) {
            return Ok(());
        }
        let Some(declaration) = self.declared.as_mut() else {
            return Ok(());
        };
        if implied.family {
            if !declaration.owed.family {
                return Err(TransitionRefusal::UndeclaredAdvance);
            }
            declaration.owed.family = false;
        }
        for flow in &implied.flows {
            if !declaration.owed.flows.remove(flow) {
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
        if recorded.advanced_by(&open.declared, trajectory, subject)
            != self.projection.view(trajectory).basis_for(subject)
        {
            return Err(TransitionRefusal::StaleSpend);
        }
        Ok(())
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
    pub(crate) fn advance_of(
        registry: &'a Registry,
        child_return: &'a ReturnPolicy,
        view: &EngineView,
        facts: &[Fact],
    ) -> crate::basis::BasisAdvance {
        let mut sequence = Sequence::resuming(registry, child_return, view);
        let mut total = crate::basis::BasisAdvance::default();
        for fact in facts {
            total.absorb(&sequence.implied_advance(fact));
            sequence.projection.fold(fact);
        }
        total
    }

    fn implied_advance(&self, fact: &Fact) -> crate::basis::BasisAdvance {
        use crate::basis::BasisAdvance;
        match fact {
            Fact::DispatchOpened {
                trajectory,
                dispatch,
                tool,
                proposed_effects,
                ..
            } => {
                let mut advance = BasisAdvance::default();
                if !proposed_effects.is_empty() {
                    advance.absorb(&BasisAdvance::family());
                }
                if self.result_can_restrict(tool, dispatch) {
                    advance.absorb(&BasisAdvance::flow(trajectory));
                }
                advance
            }
            // The reservation settles into committed effects.
            Fact::DispatchSucceeded { effects, .. } => self.effect_advance(!effects.is_empty()),
            Fact::DispatchClosed {
                trajectory,
                dispatch,
                outcome,
            } => match outcome {
                CloseOutcome::Success { effects } => self.effect_advance(!effects.is_empty()),
                CloseOutcome::Failure => self.effect_advance(self.projection.view(trajectory).reserves(dispatch)),
                CloseOutcome::Indeterminate => crate::basis::BasisAdvance::default(),
            },
            Fact::CastApplied { value, .. } => {
                let mut advance = BasisAdvance::family();
                advance.flows.extend(
                    self.members
                        .iter()
                        .filter(|member| self.projection.view(member).may_resolve(*value))
                        .cloned(),
                );
                advance
            }
            // The trajectory's label and unresolved sources move with every value it admits.
            Fact::ValueAdmitted { trajectory, .. } => BasisAdvance::flow(trajectory),
            // A denial changes what may be offered for that rendered call.
            Fact::Denial { trajectory, .. } => BasisAdvance::flow(trajectory),
            Fact::CallApprovalConsumed { offer, .. } => BasisAdvance {
                subjects: vec![crate::basis::SubjectKey::Approval(*offer)],
                ..BasisAdvance::default()
            },
            Fact::OfferAccepted { trajectory, offer } => match self.projection.view(trajectory).offer(offer) {
                Some(recorded) => BasisAdvance {
                    subjects: vec![recorded.subject.clone()],
                    ..BasisAdvance::default()
                },
                None => BasisAdvance::default(),
            },
            _ => BasisAdvance::default(),
        }
    }

    fn effect_advance(&self, moved: bool) -> crate::basis::BasisAdvance {
        if moved {
            crate::basis::BasisAdvance::family()
        } else {
            crate::basis::BasisAdvance::default()
        }
    }

    /// Can this release's result restrict the trajectory, leave it unresolved, or arrive through a
    /// bound sanitizer? An unannotated contract admits at `Unknown`, so it can;
    /// the deliberate neutral `delta = {}` cannot.
    fn result_can_restrict(&self, tool: &crate::value::ToolName, dispatch: &DispatchId) -> bool {
        if self
            .projection
            .view(dispatch.trajectory())
            .bound_sanitizer(dispatch)
            .is_some()
        {
            return true;
        }
        match self.registry.tool(tool) {
            Some(contract) => contract.delta.as_ref().is_none_or(|delta| !delta.is_none()),
            None => true,
        }
    }

    /// The validated projection, once every record has been admitted. A claim left standing —
    /// a release with no opening, a ruling with no dispatch — means the log stops mid-act.
    pub(crate) fn finish(self) -> Result<Projection, TransitionRefusal> {
        if !self.pending.is_empty() {
            return Err(TransitionRefusal::UnbackedDecision);
        }
        if !self.remedies.is_empty() {
            return Err(TransitionRefusal::DanglingRemedy);
        }
        if !self.derived.is_empty() || !self.cast_accepted.is_empty() {
            return Err(TransitionRefusal::UnadmittedDerivation);
        }
        if self.crossed.values().any(|crossing| crossing != &Crossing::Merged) {
            return Err(TransitionRefusal::UnmergedCrossing);
        }
        if self.declared.as_ref().is_some_and(|open| !open.owed.is_empty()) {
            return Err(TransitionRefusal::UnbackedAdvance);
        }
        Ok(self.projection)
    }

    fn obliged(&mut self, fact: &Fact) -> Result<Obligation, TransitionRefusal> {
        let Some(next) = self.pending.front() else {
            // No obligation stands; a decision record is free to create one.
            return Ok(Obligation::Free);
        };
        match (next.next, fact) {
            (ReleasePart::Consumption(owed), Fact::CallApprovalConsumed { offer, dispatch, .. })
                if offer == &owed && dispatch == &next.dispatch =>
            {
                self.pending.front_mut().expect("the front was read above").next = ReleasePart::Remedy(owed);
                Ok(Obligation::Consuming(owed))
            }
            (
                ReleasePart::Remedy(owed),
                Fact::Acceptance { dispatch, .. }
                | Fact::Ruling { dispatch, .. }
                | Fact::OutputSanitizerBound { dispatch, .. },
            ) if dispatch == &next.dispatch => Ok(Obligation::Consuming(owed)),
            (
                ReleasePart::Opening | ReleasePart::Remedy(_),
                Fact::DispatchOpened {
                    dispatch,
                    tool,
                    arguments,
                    dynamic_resolutions,
                    ..
                },
            ) if dispatch == &next.dispatch => {
                let opened = ResolvedCall::new(tool.clone(), arguments.clone())
                    .with_dynamic_resolutions(dynamic_resolutions.clone());
                if opened != next.call {
                    return Err(TransitionRefusal::UnbackedDecision);
                }
                let authorized = match next.next {
                    ReleasePart::Remedy(owed) => Obligation::Consuming(owed),
                    _ => Obligation::Decided,
                };
                // The marked spawn's preparation is the next record; any other release is done.
                if next.prepares_fork {
                    self.pending.front_mut().expect("the front was read above").next = ReleasePart::Fork;
                } else {
                    self.pending.pop_front();
                }
                Ok(authorized)
            }
            (ReleasePart::Fork, Fact::ForkPrepared { fork, .. }) if fork == &ForkId::of(&next.dispatch) => {
                self.pending.pop_front();
                Ok(Obligation::Decided)
            }
            _ => Err(TransitionRefusal::UnbackedDecision),
        }
    }

    fn member(&mut self, fact: &Fact) -> Result<(), TransitionRefusal> {
        let trajectory = fact.trajectory();
        let opens_from = match fact {
            Fact::Boundary {
                kind: BoundaryKind::Fork { parent, .. },
                ..
            } => Some(parent.clone()),
            Fact::ForkOpened { fork, .. } => Some(
                self.projection
                    .prepared_fork(fork)
                    .ok_or(TransitionRefusal::UnknownFork)?
                    .parent
                    .clone(),
            ),
            _ => None,
        };
        if let Some(parent) = opens_from {
            if !self.members.contains(&parent) {
                return Err(TransitionRefusal::ForeignTrajectory);
            }
            self.members.insert(trajectory.clone());
            return Ok(());
        }
        if self.members.contains(trajectory) {
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
    ) -> Result<(), TransitionRefusal> {
        // A mark this deployment cannot make, or one naming no proposal.
        if let Some(mark) = spawn {
            if mark.index() >= proposals.len() {
                return Err(TransitionRefusal::SpawnMarkOutOfRange);
            }
            if !self.registry.profile().context_control() {
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
        let [call] = proposals else {
            return Err(TransitionRefusal::UncomposedDecision);
        };
        let views = self.projection.view(trajectory);
        let contract = self
            .registry
            .tool(call.tool())
            .ok_or_else(|| TransitionRefusal::UnknownTool(call.tool().as_str().to_string()))?;
        contract
            .parameters
            .validate(call.arguments())
            .map_err(TransitionRefusal::InvalidPayload)?;
        let dispatch = DispatchId::new(trajectory.clone(), call.digest(), views.dispatch_count(&call.digest()));
        let consumes = views
            .approvals_for(call)
            .map(|(offer, approval)| (offer, approval.basis))
            .find(|(offer, basis)| {
                self.may_spend(trajectory, &crate::basis::SubjectKey::Approval(*offer), basis)
                    .is_ok()
            })
            .map(|(offer, _)| offer);
        let expected: Vec<DispatchId> = match crate::check::evaluate(contract, &views, call) {
            CheckOutcome::Allow => vec![dispatch],
            CheckOutcome::Block(_) if consumes.is_some() => vec![dispatch],
            CheckOutcome::Block(_) => Vec::new(),
        };
        if expected != released {
            return Err(TransitionRefusal::MisdecidedBatch);
        }
        self.pending.extend(expected.into_iter().map(|dispatch| PendingRelease {
            dispatch,
            call: call.clone(),
            prepares_fork: matches!(spawn, Some(mark) if mark.index() == 0),
            next: match consumes {
                Some(offer) => ReleasePart::Consumption(offer),
                None => ReleasePart::Opening,
            },
        }));
        Ok(())
    }

    fn opened(
        &mut self,
        trajectory: &TrajectoryId,
        dispatch: &DispatchId,
        call: &ResolvedCall,
        authorized: Obligation,
    ) -> Result<(), TransitionRefusal> {
        if dispatch.trajectory() != trajectory {
            return Err(TransitionRefusal::ForeignDispatch);
        }
        if self.projection.view(trajectory).dispatch_tool(dispatch).is_some() {
            return Err(TransitionRefusal::DispatchReopened);
        }
        let contract = self
            .registry
            .tool(call.tool())
            .ok_or_else(|| TransitionRefusal::UnknownTool(call.tool().as_str().to_string()))?;
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
        match authorized {
            Obligation::Decided => {
                return if remedy.is_some() {
                    Err(TransitionRefusal::DanglingRemedy)
                } else {
                    Ok(())
                };
            }
            Obligation::Consuming(offer) => {
                let approval = views.approval(&offer).ok_or(TransitionRefusal::UnknownApproval)?;
                let expected = Remedy {
                    rulings: approval
                        .rulings
                        .iter()
                        .map(|given| (given.authority.clone(), given.covers.clone()))
                        .collect(),
                    plans: BTreeSet::from([approval.plan]),
                    reviewed: approval.rulings.iter().map(|given| given.reviewed.clone()).collect(),
                    acceptance: approval.acceptance.clone(),
                    sanitizer: approval.sanitizer.clone(),
                };
                if remedy.is_none_or(|landed| landed != expected) {
                    return Err(TransitionRefusal::UnbackedApproval);
                }
                return match crate::check::evaluate(contract, &views, call) {
                    CheckOutcome::Block(block) if block.unestablished.is_empty() => Ok(()),
                    _ => Err(TransitionRefusal::UnreleasedDispatch),
                };
            }
            Obligation::Free => {}
        }
        match crate::check::evaluate(contract, &views, call) {
            // Likewise for a release the check allows as it stands.
            CheckOutcome::Allow if remedy.is_some() => Err(TransitionRefusal::DanglingRemedy),
            CheckOutcome::Allow => Ok(()),
            CheckOutcome::Block(block) => match remedy {
                Some(remedy) => {
                    let live = views.current_label();
                    if remedy
                        .reviewed
                        .iter()
                        .any(|review| &review.tool != call.tool() || review.trajectory_label != live)
                    {
                        return Err(TransitionRefusal::RulingOutsideMandate);
                    }
                    admits_block(self.registry, contract, call, &live, &block, &remedy)
                }
                None => Err(TransitionRefusal::UnreleasedDispatch),
            },
        }
    }

    fn value_admitted(
        &mut self,
        trajectory: &TrajectoryId,
        value: &crate::value::LabeledValue,
        provenance: &Provenance,
    ) -> Result<(), TransitionRefusal> {
        match provenance {
            Provenance::UserInput => Ok(()),
            Provenance::ToolResult { dispatch } => {
                let contract = self.dispatch_contract(trajectory, dispatch)?;
                let views = self.projection.view(trajectory);
                // Admission lands with the close, and only a success admits anything.
                if !views.closed_successfully(dispatch) {
                    return Err(TransitionRefusal::DispatchNotOpen);
                }
                if !self.admitted.insert(dispatch.clone()) {
                    return Err(TransitionRefusal::RepeatAdmission);
                }
                let accepted = self.cast_accepted.remove(dispatch);
                let expected = match self.derived.remove(dispatch) {
                    Some(Derived::Cast(resolved, raw_digest)) => {
                        if RawResultDigest::of(value.body.as_str().as_bytes()) != raw_digest {
                            return Err(TransitionRefusal::ForgedLabel);
                        }
                        if crate::admit::pending_cast_narrowing(&views, &resolved) != accepted {
                            return Err(TransitionRefusal::AcceptanceMismatch);
                        }
                        resolved.into_label()
                    }
                    // A sanitizer's derivation crossed, not the raw.
                    Some(Derived::Sanitized(label)) => label,
                    None => {
                        if contract.pending_cast_dim().is_some() || views.bound_sanitizer(dispatch).is_some() {
                            return Err(TransitionRefusal::ForgedLabel);
                        }
                        // And these are the bytes the checkpoint observed, where one was recorded.
                        self.observed_as(
                            trajectory,
                            dispatch,
                            &RawResultDigest::of(value.body.as_str().as_bytes()),
                        )?;
                        self.output_label(trajectory, dispatch, contract)
                    }
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
                if let Some(bound) = EstablishedLabel::from_label(&value.label) {
                    let current = views.current_label();
                    let candidate = current.bound().combine(&bound);
                    let owed = (&candidate != current.bound()).then(|| Narrowing {
                        from: current.bound().clone(),
                        to: candidate,
                    });
                    if self.accepted.get(id) != owed.as_ref() {
                        return Err(TransitionRefusal::ReturnNarrowsParent);
                    }
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

    fn cast_applied(
        &mut self,
        trajectory: &TrajectoryId,
        value: ValueId,
        resolved: &EstablishedLabel,
        cast: &crate::names::CastName,
    ) -> Result<(), TransitionRefusal> {
        let views = self.projection.view(trajectory);
        let prior = views.value_label(value).ok_or(TransitionRefusal::CastBeforeSource)?;
        if !views.may_resolve(value) {
            return Err(TransitionRefusal::ForeignResolution);
        }
        if EstablishedLabel::from_label(prior).is_some() {
            return Err(TransitionRefusal::RepeatResolution);
        }
        let registered = self
            .registry
            .cast(cast)
            .ok_or_else(|| TransitionRefusal::UnknownCast(cast.as_str().to_string()))?;
        let applicable = match views
            .value_provenance(value)
            .expect("the value_label lookup proved the record exists")
        {
            Provenance::ToolResult { dispatch } => views
                .dispatch_tool(dispatch)
                .and_then(|tool| self.registry.tool(tool))
                .is_some_and(|contract| registered.scope.covers(&contract.tags)),
            Provenance::UserInput | Provenance::ChildReturn { .. } => registered.scope.is_unscoped(),
        };
        if !applicable {
            return Err(TransitionRefusal::OutOfScopeResolution);
        }
        registered
            .resolution
            .validate(prior, resolved)
            .map_err(|_| TransitionRefusal::InadmissibleResolution)?;
        Ok(())
    }

    fn child_return(
        &mut self,
        child: &TrajectoryId,
        id: &ChildReturnId,
        value: &crate::value::LabeledValue,
        derivation: &ReturnDerivation,
    ) -> Result<(), TransitionRefusal> {
        let parent = self
            .projection
            .view(child)
            .parent_of(child)
            .ok_or(TransitionRefusal::NotForked)?
            .clone();
        let views = self.projection.view(&parent);
        if views.has_ended(child) {
            return Err(TransitionRefusal::BranchEnded);
        }
        if id.child() != child || id.occurrence() != views.returns_by(child) {
            return Err(TransitionRefusal::WrongReturnIdentity);
        }
        let fold = views.branch_label(child);
        let policy = views.return_policy_of(child).ok_or(TransitionRefusal::NotForked)?;
        let expected = match (policy, derivation) {
            (ReturnPolicy::Raw, ReturnDerivation::Raw) => {
                if !fold.is_fully_established() {
                    return Err(TransitionRefusal::ForgedLabel);
                }
                fold.bound().clone().into_label()
            }
            (ReturnPolicy::Raw, ReturnDerivation::Sanitized { sanitizer, .. }) => {
                let offered = match crate::branch::check_child_return(self.registry, &views, child) {
                    Ok(crate::branch::ReturnCheck::Block(crate::branch::ReturnBlock::Narrowing { plans, .. })) => plans,
                    _ => return Err(TransitionRefusal::ReturnPolicyMismatch),
                };
                if !offered.iter().any(|plan| {
                    matches!(plan, crate::branch::ReturnPlan::Sanitize { sanitizer: offered, .. } if offered == sanitizer)
                }) {
                    return Err(TransitionRefusal::ReturnPolicyMismatch);
                }
                self.sanitized_crossing(sanitizer, derivation, &fold)?
            }
            (ReturnPolicy::Sanitized(bound), ReturnDerivation::Sanitized { sanitizer, .. }) => {
                if bound != sanitizer {
                    return Err(TransitionRefusal::ReturnPolicyMismatch);
                }
                self.sanitized_crossing(sanitizer, derivation, &fold)?
            }
            _ => return Err(TransitionRefusal::ReturnPolicyMismatch),
        };
        if value.label != expected {
            return Err(TransitionRefusal::ForgedLabel);
        }
        Ok(())
    }

    fn sanitized_crossing(
        &self,
        sanitizer: &SanitizerName,
        derivation: &ReturnDerivation,
        fold: &crate::label::PartialLabel,
    ) -> Result<Label, TransitionRefusal> {
        let ReturnDerivation::Sanitized { transition, .. } = derivation else {
            return Err(TransitionRefusal::ReturnPolicyMismatch);
        };
        let registered = self
            .registry
            .sanitizer(sanitizer)
            .ok_or_else(|| TransitionRefusal::UnknownSanitizer(sanitizer.as_str().to_string()))?;
        if !fold.is_fully_established() {
            return Err(TransitionRefusal::ForgedLabel);
        }
        match registered.derive_output(&fold.bound().clone().into_label()) {
            Some(label) if &registered.transition == transition => Ok(label),
            _ => Err(TransitionRefusal::SanitizerUnapplicable),
        }
    }

    fn boundary(&mut self, trajectory: &TrajectoryId, kind: &BoundaryKind) -> Result<(), TransitionRefusal> {
        match kind {
            BoundaryKind::TurnEnd => Ok(()),
            BoundaryKind::Fork {
                parent,
                snapshot,
                return_policy,
            } => self.forked(trajectory, parent, snapshot, return_policy),
            BoundaryKind::Merge { child_return } => {
                let views = self.projection.view(trajectory);
                let crossed = views
                    .child_return(child_return)
                    .ok_or(TransitionRefusal::UnknownReturn)?;
                if views.parent_of(child_return.child()) != Some(trajectory) {
                    return Err(TransitionRefusal::ForeignTrajectory);
                }
                let _ = crossed;
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
        }
    }

    fn forked(
        &mut self,
        child: &TrajectoryId,
        parent: &TrajectoryId,
        snapshot: &ForkSnapshot,
        return_policy: &ReturnPolicy,
    ) -> Result<(), TransitionRefusal> {
        if !self.registry.profile().context_control() {
            return Err(TransitionRefusal::ContextUncontrolled);
        }
        let views = self.projection.view(parent);
        if views.is_active(child) || child == parent {
            return Err(TransitionRefusal::ChildActiveBeforeFork);
        }
        if views.has_ended(parent) {
            return Err(TransitionRefusal::BranchEnded);
        }
        if return_policy != self.child_return {
            return Err(TransitionRefusal::ForkReturnPolicyMismatch);
        }
        if &views.freeze_basis() != snapshot {
            return Err(TransitionRefusal::ForkBasisMismatch);
        }
        Ok(())
    }

    fn settling(&self, trajectory: &TrajectoryId, dispatch: &DispatchId) -> Result<(), TransitionRefusal> {
        let views = self.projection.view(trajectory);
        if !views.closed_successfully(dispatch) {
            return Err(TransitionRefusal::DispatchNotOpen);
        }
        if views.has_lapsed(dispatch) || self.admitted.contains(dispatch) || self.derived.contains_key(dispatch) {
            return Err(TransitionRefusal::RepeatAdmission);
        }
        Ok(())
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

    fn dispatch_contract(
        &self,
        trajectory: &TrajectoryId,
        dispatch: &DispatchId,
    ) -> Result<&'a ToolContract, TransitionRefusal> {
        if dispatch.trajectory() != trajectory {
            return Err(TransitionRefusal::ForeignDispatch);
        }
        let views = self.projection.view(trajectory);
        let tool = views
            .dispatch_tool(dispatch)
            .ok_or(TransitionRefusal::UnknownDispatch)?;
        self.registry
            .tool(tool)
            .ok_or_else(|| TransitionRefusal::UnknownTool(tool.as_str().to_string()))
    }

    fn open_dispatch_contract(
        &self,
        trajectory: &TrajectoryId,
        dispatch: &DispatchId,
    ) -> Result<&'a ToolContract, TransitionRefusal> {
        let contract = self.dispatch_contract(trajectory, dispatch)?;
        if !self.projection.view(trajectory).is_open(dispatch) {
            return Err(TransitionRefusal::DispatchNotOpen);
        }
        Ok(contract)
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

    fn output_label(&self, trajectory: &TrajectoryId, dispatch: &DispatchId, contract: &ToolContract) -> Label {
        contract.output_label_for_resolutions(
            self.projection
                .view(trajectory)
                .dynamic_resolutions(dispatch)
                .unwrap_or_default(),
        )
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

fn belongs_to(act: &crate::basis::DecidedAct, fact: &Fact) -> bool {
    use crate::basis::DecidedAct;
    match (act, fact) {
        (DecidedAct::Proposals(act), Fact::ProposalBatchDecided { batch, .. } | Fact::OfferOpened { batch, .. }) => {
            batch == act
        }
        (
            DecidedAct::Proposals(_),
            Fact::DispatchOpened { .. } | Fact::ForkPrepared { .. } | Fact::CallApprovalConsumed { .. },
        ) => true,
        (
            DecidedAct::Outcome(act),
            Fact::DispatchSucceeded { dispatch, .. }
            | Fact::DispatchClosed { dispatch, .. }
            | Fact::OutputSanitizerApplied { dispatch, .. }
            | Fact::OutputCastApplied { dispatch, .. }
            | Fact::OutputCastAccepted { dispatch, .. }
            | Fact::OutputCastLapsed { dispatch, .. },
        ) => dispatch == act,
        (
            DecidedAct::Outcome(act),
            Fact::ValueAdmitted {
                provenance: crate::value::Provenance::ToolResult { dispatch },
                ..
            },
        ) => dispatch == act,
        (DecidedAct::ChildReturn(act), Fact::ChildReturn { id, .. }) => id == act,
        (DecidedAct::ChildReturn(act), Fact::ChildReturnAcceptance { child_return, .. }) => child_return == act,
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
        (DecidedAct::Binding(act), Fact::ForkOpened { fork, .. }) => fork == act,
        (DecidedAct::Offer(act), Fact::OfferAccepted { offer, .. }) => offer == act,
        (DecidedAct::Offer(_), Fact::Denial { .. }) => true,
        _ => false,
    }
}

fn admits_block(
    registry: &Registry,
    contract: &ToolContract,
    call: &ResolvedCall,
    current: &crate::label::PartialLabel,
    block: &RawBlock,
    remedy: &Remedy,
) -> Result<(), TransitionRefusal> {
    if !block.unestablished.is_empty() {
        return Err(TransitionRefusal::UnreleasedDispatch);
    }
    crate::execute::rulings_cover(
        registry,
        contract,
        block,
        remedy
            .rulings
            .iter()
            .map(|(authority, gaps)| (authority, gaps.as_slice())),
    )
    .map_err(|_| TransitionRefusal::RulingOutsideMandate)?;
    let offered = crate::plan::narrowing_remedies(registry, current, contract, call, block.narrowing.as_ref());
    if offered
        .iter()
        .any(|settlement| settlement.accept == remedy.acceptance && settlement.sanitize == remedy.sanitizer)
    {
        Ok(())
    } else {
        Err(TransitionRefusal::UnreleasedDispatch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::label::{Audience, Dim, PartialLabel, Trust};
    use crate::value::{LabeledValue, ValueBody};

    fn traj() -> TrajectoryId {
        TrajectoryId::new("t")
    }

    fn admit(trust: u8) -> Fact {
        Fact::ValueAdmitted {
            trajectory: traj(),
            value: LabeledValue::new(
                ValueBody::new("body"),
                Label::new(Dim::Known(Trust::new(trust)), Dim::Known(Audience::Public)),
            ),
            provenance: Provenance::UserInput,
        }
    }

    fn engine() -> crate::engine::Engine {
        let config = crate::registry::RegistryConfig {
            trust_chain: crate::registry::TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![],
        };
        let profile = crate::profile::covering_declaration(&config);
        crate::engine::Engine::open(crate::profile::DeploymentPolicy {
            registry: config,
            planner_cap: crate::registry::PlannerCap::default(),
            dialect: crate::profile::PolicyDialectVersion::new(1),
            child_return: ReturnPolicy::Raw,
            profile,
        })
        .expect("the test policy opens")
    }

    #[test]
    fn advancing_a_view_matches_rebuilding_it_from_the_records() {
        let engine = engine();
        let first = vec![admit(3)];
        let mut held = engine
            .view(&traj(), first.clone(), Revision::new(1))
            .expect("the log validates");
        let batch = engine
            .seal(&held, FactBatch::new(Revision::new(1), vec![admit(1)]))
            .expect("the candidate validates");
        held.advance(&batch).unwrap();

        let whole = [first, batch.facts().to_vec()].concat();
        let rebuilt = engine
            .view(&traj(), whole, Revision::new(2))
            .expect("the log validates");

        assert_eq!(held.revision(), rebuilt.revision());
        assert_eq!(
            held.projection().view(&traj()).current_label(),
            rebuilt.projection().view(&traj()).current_label()
        );
        assert_eq!(
            held.projection().view(&traj()).current_label(),
            PartialLabel::established(EstablishedLabel::new(Trust::new(1), Audience::Public))
        );

        assert_eq!(
            held.advance(&batch),
            Err(ViewMismatch::Stale {
                view: Revision::new(2),
                batch: Revision::new(1),
            })
        );
    }

    #[test]
    fn a_view_takes_no_batch_from_another_family_or_policy() {
        let engine = engine();
        let mut view = engine
            .view(&traj(), vec![admit(3)], Revision::new(1))
            .expect("the log validates");
        let other_family = TrajectoryId::new("other");
        let elsewhere = engine
            .view(
                &other_family,
                vec![Fact::ValueAdmitted {
                    trajectory: TrajectoryId::new("other"),
                    value: LabeledValue::new(
                        ValueBody::new("body"),
                        Label::new(Dim::Known(Trust::new(3)), Dim::Known(Audience::Public)),
                    ),
                    provenance: Provenance::UserInput,
                }],
                Revision::new(1),
            )
            .expect("the log validates");
        let foreign = engine
            .seal(&elsewhere, FactBatch::new(Revision::new(1), vec![]))
            .expect("the candidate validates");
        assert_eq!(view.advance(&foreign), Err(ViewMismatch::ForeignFamily));

        let other_policy = ValidatedFactBatch::seal(
            FactBatch::new(Revision::new(1), vec![]),
            crate::profile::PolicyIdentityV1::of(
                &crate::registry::RegistryConfig {
                    trust_chain: crate::registry::TrustChain::new(vec!["suspicious".into()]),
                    tools: vec![],
                    authorities: vec![],
                    sanitizers: vec![],
                    casts: vec![],
                },
                &ReturnPolicy::Raw,
                engine.profile(),
            ),
            traj(),
        );
        assert_eq!(view.advance(&other_policy), Err(ViewMismatch::ForeignPolicy));
    }
}
