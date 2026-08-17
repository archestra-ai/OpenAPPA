//! The engine's one mutation boundary: the sealed batch and the view it was computed against.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::candidate::{CallStage, ConfinedFrom, DerivedCandidate, DerivedVia, SanitizerLineage};
use crate::check::{CheckOutcome, Gap, Narrowing, RawBlock};
use crate::contract::ToolContract;
use crate::engine::Engine;
use crate::execute::AuthorityEvidence;
use crate::fact::{BoundaryKind, CloseOutcome, EffectSet, Fact, ReturnDerivation, ReturnPolicy};
use crate::label::{EstablishedLabel, Label};
use crate::names::{AuthorityName, SanitizerName};
use crate::plan::PlannedBlock;
use crate::profile::{DeploymentProfile, OpenVector, PolicyDialectVersion, PolicyIdentityV1};
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

/// One call the model proposed, as it wrote it: a tool name and untrusted argument bytes.
/// Only the engine turns this into a [`ResolvedCall`], so no caller can present a
/// payload under a schema the registry does not hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProposedCall {
    pub tool: crate::value::ToolName,
    pub arguments: Vec<u8>,
    /// The answers the runtime resolved for this call's dynamic audience bindings, if any.
    /// They are payload, not decoration: the same tool and arguments under
    /// different resolved recipients is a different act.
    pub dynamic_resolutions: Vec<crate::contract::PinnedDynamicResolution>,
    /// The membership expansions the runtime resolved for this call's `@group` placeholder
    /// arguments. Payload on the same terms as the dynamic answers.
    pub memberships: Vec<crate::contract::PinnedMembership>,
}

/// One provider-run result the response exposed: the tool the provider ran inside the
/// inference call, and the body the model has already read. There is no dispatch to name — the
/// engine never released it and cannot have.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderResult {
    pub tool: crate::value::ToolName,
    pub body: ValueBody,
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

/// What the runtime must resolve before it can execute one live offer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OfferConsult {
    Accept,
    Authorities {
        call: ResolvedCall,
        required: Vec<crate::plan::RequiredRuling>,
    },
    Sanitizer {
        sanitizer: SanitizerName,
        source: RawResultDigest,
        body: ValueBody,
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
    Approved { call: ResolvedCall },
    Denied { block: Box<Blocked> },
    Invalidated,
    Staged(Box<Confined>),
    Substituted { block: Box<Blocked> },
    Released(Box<Released>),
    Settled(Box<Settled>),
    Admitted { value: ValueBody },
    ReturnStaged(Box<PendingReturnStage>),
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
    /// Typed evidence the runtime resolved for this exact report: the mandatory
    /// sanitizer's derivation, or a cast answer the return's planning requested. An external
    /// that gave no answer contributes nothing here and resumes nothing.
    pub evidence: Vec<Evidence>,
    /// Fresh entropy for the offers this act may surface: a narrowing submission opens
    /// its return stage in the same decision, and those plans need identities of their own.
    pub offer_nonce: crate::value::OfferNonce,
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
    Pending(Box<PendingReturnStage>),
    Resolve(EvidenceRequest),
    Rejected { reason: crate::fact::ReturnRejection },
}

/// One pending child return's open stage. Not a [`Confined`]: the custody
/// holder is a return record, not a dispatch, and the raw submission's bytes never ride along.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingReturnStage {
    pub id: ChildReturnId,
    /// The label the candidate standing now would fold into the parent: the submitted fold's
    /// established bound, or the derived candidate's label after a mandatory or staged hop.
    pub label: Label,
    pub residual: Narrowing,
    /// The offer opened for each next-stage plan, paired with the plan it binds: acceptance of
    /// exactly `residual`, and every applicable helpful sanitizer hop.
    pub offers: Vec<(crate::value::OfferId, crate::plan::PlanId)>,
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
    Cast {
        cast: crate::names::CastName,
        value: crate::value::ValueId,
        resolved: crate::label::EstablishedLabel,
    },
    PendingCast {
        cast: crate::names::CastName,
        source: RawResultDigest,
        resolved: crate::label::EstablishedLabel,
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
    Cast {
        cast: crate::names::CastName,
        value: crate::value::ValueId,
        body: ValueBody,
    },
    PendingCast {
        casts: Vec<crate::names::CastName>,
        source: RawResultDigest,
        body: ValueBody,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutcomeFollowUp {
    Closed { admitted: Option<ValueBody> },
    Resolve(EvidenceRequest),
    Staged(Box<Confined>),
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
    #[error("the view was built under another policy identity")]
    ForeignView,
    #[error("the event names a trajectory this family has not opened")]
    UnopenedTrajectory,
    #[error("the decision does not pass the transition validator: {0}")]
    Invalid(#[from] TransitionRefusal),
    #[error("the proposals name groups without pinned membership expansions")]
    MembershipNeeded { needed: Vec<crate::check::GroupRead> },
    #[error("membership pin for argument {argument} is not one this call reads")]
    ForeignMembership { argument: String },
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
    #[error("the dispatch recorded success: a failure or indeterminate outcome contradicts it")]
    ContradictedSuccess,
    #[error("the cast resolution is not admissible for this confined result")]
    InadmissibleResolution,
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
    #[error(transparent)]
    Resolution(#[from] crate::admit::CastError),
    #[error("a fork takes an unused child identity")]
    ChildAlreadyUsed,
    #[error("a fork bound to a return sanitizer crosses through the composed path until T23")]
    BoundReturnSanitizer,
    #[error("the submission does not fit the fork's return shape: {0}")]
    ReturnShapeMismatch(crate::shape::ReturnMismatch),
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
    facts: Vec<Fact>,
    basis: u64,
    policy: PolicyIdentityV1,
    family: TrajectoryId,
}

impl ValidatedFactBatch {
    /// Seal a validated batch. Crate-private on purpose: every call site is an engine transition
    /// that has already run the facts through the [`Sequence`] validator.
    pub(crate) fn seal(
        facts: Vec<Fact>,
        basis: u64,
        policy: PolicyIdentityV1,
        family: TrajectoryId,
    ) -> ValidatedFactBatch {
        ValidatedFactBatch {
            facts,
            basis,
            policy,
            family,
        }
    }

    /// The compare-and-swap basis: the count of accepted batches the decision was
    /// computed against.
    pub fn basis(&self) -> u64 {
        self.basis
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
    #[error("the batch was decided under another policy identity")]
    ForeignPolicy,
}

/// The engine's derived working picture of one family log: the validated records and
/// the projection built from them. Opaque and disposable — the runtime stores it for the next
/// event, but every constructor and mutator here belongs to the engine.
#[derive(Debug)]
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

    pub(crate) fn policy(&self) -> PolicyIdentityV1 {
        self.policy
    }

    /// The root whose log this view was built from. Public because an outer layer
    /// mints identities that must name the log they belong to, and a view is where it learns
    /// which log it is holding.
    pub fn family(&self) -> &TrajectoryId {
        &self.family
    }

    pub fn revision(&self) -> u64 {
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
        self.projection.set_revision(batch.basis() + 1);
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
    #[error("the opening record carries policy dialect version {found}, which this engine does not support")]
    UnsupportedDialect { found: u32 },
    #[error("the opening record's policy digest does not match the supplied policy")]
    DigestMismatch,
    #[error("the opening record's declaration does not match the supplied policy's validated profile")]
    ProfileMismatch,
    #[error("the opening record's open vectors are not the set derived from its declaration")]
    VectorMismatch,
}

/// Why the transition validator refused a record. One vocabulary for both directions: a
/// candidate batch the engine has just built and a persisted log being replayed pass the same
/// rules, so a refusal always says the same thing — this record cannot follow the ones before it.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TransitionRefusal {
    #[error("the family log does not open with its TrajectoryOpened record")]
    Unopened,
    #[error("the family log's opening record cannot be trusted: {0}")]
    Opening(#[from] OpeningTransitionRefusal),
    #[error("a record names a trajectory this family has not opened")]
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
    #[error("a call carries membership expansions other than the ones its placeholders spell")]
    ForgedMembership,
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
    #[error("fork record's return policy is not the deployment's child-return binding")]
    ForkReturnPolicyMismatch,
    #[error("fork record's shape is not the one its own spawn call authors")]
    ForkShapeMismatch,
    #[error("a crossing on a shaped fork carries a body its stored return shape does not admit")]
    ReturnShapeViolation,
    #[error("a return or fork record names a branch that has already ended")]
    BranchEnded,
    #[error("a return record names a trajectory that was never forked")]
    NotForked,
    #[error("a return record's identity is not the next one for its child")]
    WrongReturnIdentity,
    #[error("a return lifecycle record is not one this state produces")]
    ReturnRecordMismatch,
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
    #[error("the record spends an offer or approval that is no longer current")]
    StaleSpend,
}

struct PendingRelease {
    dispatch: DispatchId,
    subject: crate::basis::SubjectKey,
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
    contribution: Option<EstablishedLabel>,
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
    derived: BTreeMap<DispatchId, Derived>,
    candidate_accepted: BTreeMap<DispatchId, Narrowing>,
    cast_restated: BTreeSet<DispatchId>,
    admitted: BTreeSet<DispatchId>,
    /// Crossings recorded and not yet admitted into the parent, and merges not yet made: a
    /// crossing ends the child, so a log that records one without folding it into the parent
    /// would leave the parent reading a label the crossing never restricted.
    crossed: BTreeMap<ChildReturnId, Crossing>,
    return_settling: BTreeSet<ChildReturnId>,
    accepted: BTreeMap<ChildReturnId, Narrowing>,
    menu: Option<MenuDebt>,
    declared: Option<Declaration>,
    admitting: Option<ProposalBatchId>,
    deciding: Option<ProposalBatchId>,
    owing: Option<Owed>,
    substituted: Option<Substitution>,
}

struct Substitution {
    call: ResolvedCall,
    subject: crate::basis::SubjectKey,
    stage: CallStage,
    released: bool,
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

fn settled(menu: Option<MenuDebt>) -> Result<(), TransitionRefusal> {
    match menu {
        Some(debt) if debt.offered.len() != debt.menu.len() => Err(TransitionRefusal::IncompleteMenu),
        Some(_) | None => Ok(()),
    }
}

struct MenuDebt {
    subject: crate::basis::SubjectKey,
    block: crate::value::BlockId,
    call: crate::value::CanonicalDigest,
    menu: Vec<crate::plan::ExecutableRemedyPlan>,
    offered: BTreeSet<crate::plan::PlanId>,
}

enum Derived {
    Cast(EstablishedLabel, RawResultDigest),
    Sanitized(crate::value::LabeledValue),
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
            cast_restated: BTreeSet::new(),
            admitted: BTreeSet::new(),
            crossed: BTreeMap::new(),
            return_settling: BTreeSet::new(),
            accepted: BTreeMap::new(),
            declared: None,
            admitting: None,
            deciding: None,
            menu: None,
            owing: None,
            substituted: None,
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
            cast_restated: BTreeSet::new(),
            admitted: view.projection().admitted_dispatches(),
            crossed: BTreeMap::new(),
            return_settling: BTreeSet::new(),
            accepted: BTreeMap::new(),
            declared: None,
            admitting: None,
            deciding: None,
            menu: None,
            owing: None,
            substituted: None,
        }
    }

    pub(crate) fn admit(&mut self, fact: &Fact) -> Result<(), TransitionRefusal> {
        self.member(fact)?;
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
                            | DerivedCandidate::Return {
                                from: ConfinedFrom::Offer(offer),
                                ..
                            }
                            | DerivedCandidate::Call { from: offer, .. },
                        ..
                    },
                ) if offer == &owed => self.owing = None,
                (Owed::Accepted(owed), Fact::CandidateAccepted { offer, .. }) if offer == &owed => self.owing = None,
                (Owed::Accepted(owed), Fact::ChildReturn { id, .. })
                    if self
                        .projection
                        .view(fact.trajectory())
                        .offer(&owed)
                        .is_some_and(|recorded| recorded.subject == crate::basis::SubjectKey::Return(id.clone())) =>
                {
                    self.owing = None
                }
                _ => return Err(TransitionRefusal::UndischargedAcceptance),
            }
        }
        let released = self.obliged(fact)?;
        if !matches!(fact, Fact::OfferOpened { .. }) {
            settled(self.menu.take())?;
        }
        let implied = self.implied_advance(fact);
        match fact {
            // Judged in full by `member`, which admits it only as the family's first record.
            Fact::TrajectoryOpened { .. } => {}
            Fact::BasisAdvanced { act, advance, .. } => self.declare(act, advance)?,
            Fact::OfferOpened {
                trajectory,
                offer,
                block,
                act,
                call,
                subject,
                plan,
                basis,
            } => self.offer_opened(trajectory, offer, block, act, call, subject, plan, basis)?,
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
                receiving,
                proposed_effects,
                dynamic_resolutions,
                memberships,
                subject,
            } => {
                let call = ResolvedCall::new(tool.clone(), arguments.clone())
                    .with_dynamic_resolutions(dynamic_resolutions.clone())
                    .with_memberships(memberships.clone());
                if call.dynamic_resolutions() != dynamic_resolutions.as_slice() {
                    return Err(TransitionRefusal::ForgedLabel);
                }
                if call.memberships() != memberships.as_slice() {
                    return Err(TransitionRefusal::ForgedMembership);
                }
                self.opened(trajectory, dispatch, &call, subject.as_ref(), released)?;
                let contract = self
                    .engine
                    .registry()
                    .tool(tool)
                    .ok_or_else(|| TransitionRefusal::UnknownTool(tool.as_str().to_string()))?;
                if crate::check::validate_memberships(contract, &call).is_err() {
                    return Err(TransitionRefusal::ForgedMembership);
                }
                if proposed_effects != &contract.emits {
                    return Err(TransitionRefusal::EffectsMismatch);
                }
                let views = self.projection.view(trajectory);
                let current = views.current_label();
                if proposed_label != crate::check::committed_label_for_call(contract, &current, &call).bound()
                    || receiving != current.bound()
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
                self.closing_act(dispatch)?;
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
            } => self.output_cast_applied(trajectory, dispatch, cast, resolved, raw_digest)?,
            Fact::CandidateDerived {
                trajectory,
                subject,
                via,
                derived,
                lineage,
            } => self.candidate_derived(trajectory, subject, via, derived, lineage)?,
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
            } => {
                self.pending_dispatch(trajectory, dispatch)?;
                if self.engine.registry().authority(authority).is_none() {
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
                contribution,
            } => {
                self.pending_dispatch(trajectory, dispatch)?;
                if self.engine.registry().sanitizer(sanitizer).is_none_or(|s| !s.on.output) {
                    return Err(TransitionRefusal::UnknownSanitizer(sanitizer.as_str().to_string()));
                }
                let remedy = self.remedies.entry(dispatch.clone()).or_default();
                remedy.sanitizer = Some(sanitizer.clone());
                remedy.contribution = Some(contribution.clone());
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
                if self.engine.registry().authority(authority).is_none() {
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
            Fact::ReturnSubmitted {
                trajectory,
                id,
                fork,
                parent,
                label,
                digest,
                body,
                policy,
            } => self.return_submitted(trajectory, id, fork, parent, label, digest, body, policy)?,
            Fact::ReturnRejected {
                trajectory,
                id,
                fork,
                digest: _,
                reason,
            } => self.return_rejected(trajectory, id, fork, reason)?,
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
                shape,
            } => {
                if released == Obligation::Free {
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
                if return_policy != self.engine.child_return() {
                    return Err(TransitionRefusal::ForkReturnPolicyMismatch);
                }
                if &views.freeze_basis() != snapshot {
                    return Err(TransitionRefusal::ForkBasisMismatch);
                }
                let call = views
                    .dispatch_call(fork.dispatch())
                    .expect("the dispatch gate above proved the spawn call recorded");
                let expected =
                    crate::engine::marked_return_shape(call).map_err(|_| TransitionRefusal::ForkShapeMismatch)?;
                if shape != &expected {
                    return Err(TransitionRefusal::ForkShapeMismatch);
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
                if self.projection.is_opened(trajectory) {
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
        }
        self.settle_advance(fact, &implied, released)?;
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
        block: &crate::value::BlockId,
        act: &crate::basis::DecidedAct,
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
        let menu = self.stage_menu(&views, trajectory, act, call, subject)?;
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
        });
        Ok(())
    }

    fn stage_menu(
        &self,
        views: &Views,
        trajectory: &TrajectoryId,
        act: &crate::basis::DecidedAct,
        call: &crate::value::CanonicalDigest,
        subject: &crate::basis::SubjectKey,
    ) -> Result<Vec<crate::plan::ExecutableRemedyPlan>, TransitionRefusal> {
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
                let candidate = subject_call(views, subject).ok_or(TransitionRefusal::UnbackedOffer)?;
                if candidate.digest() != *call {
                    return Err(TransitionRefusal::UnbackedOffer);
                }
                let contract = self
                    .engine
                    .registry()
                    .tool(candidate.tool())
                    .ok_or_else(|| TransitionRefusal::UnknownTool(candidate.tool().as_str().to_string()))?;
                let stage = views.call_stage(subject);
                let CheckOutcome::Block(block) = crate::check::evaluate(contract, views, &candidate, &stage) else {
                    return Err(TransitionRefusal::UnbackedOffer);
                };
                Ok(crate::plan::plan(
                    self.engine.registry(),
                    views,
                    &candidate,
                    &block,
                    &stage,
                    views.call_role(subject),
                )
                .plans
                .iter()
                .filter_map(crate::plan::RemedyPlan::executable)
                .cloned()
                .collect())
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
                Ok(crate::plan::confined_stage(
                    self.engine.registry(),
                    contract,
                    receiving,
                    &value.label,
                    residual,
                    &lineage,
                ))
            }
            crate::basis::SubjectKey::Return(id) => {
                let child = id.child();
                // The offers of a pending return belong to the parent trajectory.
                if views.parent_of(child) != Some(trajectory) {
                    return Err(TransitionRefusal::ForeignOffer);
                }
                match act {
                    crate::basis::DecidedAct::ChildReturn(decided) if decided == id => {}
                    crate::basis::DecidedAct::Offer(_) => {}
                    _ => return Err(TransitionRefusal::ForeignOffer),
                }
                let pending = views.pending_return(id).ok_or(TransitionRefusal::UnbackedOffer)?;
                let spawn = views
                    .dispatch_call(pending.fork.dispatch())
                    .ok_or(TransitionRefusal::UnknownDispatch)?;
                if spawn.digest() != *call {
                    return Err(TransitionRefusal::UnbackedOffer);
                }
                let fold = views.branch_label(child);
                let receiving = pending.receiving.clone();
                let (candidate, body, residual) = match views.candidate(subject) {
                    Some(DerivedCandidate::Return {
                        value,
                        residual: Some(residual),
                        ..
                    }) => (value.label.clone(), value.body.clone(), residual.clone()),
                    Some(_) => return Err(TransitionRefusal::UnbackedOffer),
                    None => {
                        let label = fold.bound().clone().into_label();
                        let to = receiving.combine(&label.established_part());
                        if to == receiving {
                            return Err(TransitionRefusal::UnbackedOffer);
                        }
                        (
                            label,
                            pending.body().clone(),
                            Narrowing {
                                from: receiving.clone(),
                                to,
                            },
                        )
                    }
                };
                let lineage = views.lineage(subject);
                match crate::plan::return_stage(
                    self.engine.registry(),
                    views,
                    child,
                    &fold,
                    &candidate,
                    &body,
                    &residual,
                    &lineage,
                ) {
                    crate::plan::ReturnStagePlan::Stage(plans) => Ok(plans),
                    crate::plan::ReturnStagePlan::Resolve { .. } => Err(TransitionRefusal::UnbackedOffer),
                }
            }
            crate::basis::SubjectKey::Approval(_) => Err(TransitionRefusal::ForeignOffer),
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
        let (basis, subject) = (recorded.basis, recorded.subject.clone());
        let owed = match (&subject, recorded.plan.hop().is_some()) {
            (SubjectKey::Call { .. }, false) => Owed::Approval(*offer),
            (SubjectKey::Call { .. } | SubjectKey::ConfinedResult(_) | SubjectKey::Return(_), true) => {
                Owed::Derived(*offer)
            }
            (SubjectKey::ConfinedResult(_) | SubjectKey::Return(_), false) => Owed::Accepted(*offer),
            // No offer ever stands on an approval — that subject exists to be spent by a release.
            (SubjectKey::Approval(_), _) => return Err(TransitionRefusal::UnbackedOffer),
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
        if subject_call(&views, &recorded.subject).as_ref() != Some(call)
            || call.digest() != recorded.call
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
            .engine
            .registry()
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
        if advance.flows.iter().any(|flow| !self.projection.is_opened(flow)) {
            return Err(TransitionRefusal::ForeignTrajectory);
        }
        self.declared = Some(Declaration {
            act: act.clone(),
            declared: advance.clone(),
            owed: advance.clone(),
        });
        self.admitting = None;
        self.substituted = None;
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
        if !self
            .declared
            .as_ref()
            .is_some_and(|open| belongs_to(self, &open.act, fact))
        {
            return Ok(());
        }
        let Some(declaration) = self.declared.as_mut() else {
            return Ok(());
        };
        if implied.family {
            if !declaration.owed.family && !(own_act && declaration.declared.family) {
                return Err(TransitionRefusal::UndeclaredAdvance);
            }
            declaration.owed.family = false;
        }
        for flow in &implied.flows {
            if !declaration.owed.flows.remove(flow) && !(own_act && declaration.declared.flows.contains(flow)) {
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
                Fact::DispatchOpened { .. } | Fact::ForkPrepared { .. } | Fact::CallApprovalConsumed { .. },
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
                    self.projection
                        .trajectories()
                        .filter(|member| self.projection.view(member).may_resolve(*value))
                        .cloned(),
                );
                advance
            }
            Fact::ValueAdmitted {
                trajectory, provenance, ..
            } => {
                let mut advance = BasisAdvance::flow(trajectory);
                if let Provenance::ProviderRun { effects, .. } = provenance {
                    advance.absorb(&self.effect_advance(!effects.is_empty()));
                }
                advance
            }
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
            Fact::CandidateDerived { subject, .. } => BasisAdvance {
                subjects: vec![subject.clone()],
                ..BasisAdvance::default()
            },
            Fact::ReturnSubmitted { id, .. } => BasisAdvance {
                subjects: vec![crate::basis::SubjectKey::Return(id.clone())],
                ..BasisAdvance::default()
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
        match self.engine.registry().tool(tool) {
            Some(contract) => contract.delta.as_ref().is_none_or(|delta| !delta.is_none()),
            None => true,
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
        if !self.derived.is_empty() || !self.candidate_accepted.is_empty() || !self.cast_restated.is_empty() {
            return Err(TransitionRefusal::UnadmittedDerivation);
        }
        if self.crossed.values().any(|crossing| crossing != &Crossing::Merged) {
            return Err(TransitionRefusal::UnmergedCrossing);
        }
        if !self.return_settling.is_empty() {
            return Err(TransitionRefusal::UnadmittedDerivation);
        }
        if self.declared.as_ref().is_some_and(|open| !open.owed.is_empty()) {
            return Err(TransitionRefusal::UnbackedAdvance);
        }
        if self.owing.is_some() {
            return Err(TransitionRefusal::UndischargedAcceptance);
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
                    memberships,
                    subject,
                    ..
                },
            ) if dispatch == &next.dispatch => {
                let opened = ResolvedCall::new(tool.clone(), arguments.clone())
                    .with_dynamic_resolutions(dynamic_resolutions.clone())
                    .with_memberships(memberships.clone());
                if opened != next.call || subject.as_ref() != Some(&next.subject) {
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

    /// The strict judgment of the durable opening: the family's first record, naming
    /// the family, at a dialect this engine reads, carrying this policy's digest, its validated
    /// declaration byte for byte — which subsumes re-running the coverage matrix over it — and
    /// the open vectors that declaration derives. `policy_file_key` is deliberately not judged
    /// here: it names a stored file this engine never sees, and the outer layer re-hashes the
    /// file it loaded against the record on every event.
    fn root_opened(
        &self,
        trajectory: &TrajectoryId,
        dialect: PolicyDialectVersion,
        profile: &DeploymentProfile,
        policy_digest: &PolicyIdentityV1,
        open_vectors: &[OpenVector],
    ) -> Result<(), OpeningTransitionRefusal> {
        if self.projection.is_opened(&self.family) {
            return Err(OpeningTransitionRefusal::Duplicate);
        }
        if trajectory != &self.family {
            return Err(OpeningTransitionRefusal::WrongTrajectory {
                found: trajectory.as_str().to_string(),
            });
        }
        if dialect != self.engine.dialect() {
            return Err(OpeningTransitionRefusal::UnsupportedDialect { found: dialect.value() });
        }
        if policy_digest != &self.engine.identity() {
            return Err(OpeningTransitionRefusal::DigestMismatch);
        }
        if profile != self.engine.registry().profile() {
            return Err(OpeningTransitionRefusal::ProfileMismatch);
        }
        if open_vectors != self.engine.open_vectors() {
            return Err(OpeningTransitionRefusal::VectorMismatch);
        }
        Ok(())
    }

    fn member(&self, fact: &Fact) -> Result<(), TransitionRefusal> {
        let trajectory = fact.trajectory();
        if let Fact::TrajectoryOpened {
            trajectory,
            dialect,
            profile,
            policy_digest,
            open_vectors,
            ..
        } = fact
        {
            return Ok(self.root_opened(trajectory, *dialect, profile, policy_digest, open_vectors)?);
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
            .provider_admissions(batch)
            .next()
            .is_some_and(|(admitted, ..)| admitted != trajectory)
        {
            return Err(TransitionRefusal::ForeignAdmission);
        }
        for call in proposals {
            let contract = self
                .engine
                .registry()
                .tool(call.tool())
                .ok_or_else(|| TransitionRefusal::UnknownTool(call.tool().as_str().to_string()))?;
            if crate::check::validate_memberships(contract, call).is_err() {
                return Err(TransitionRefusal::ForgedMembership);
            }
        }
        let mut working = std::borrow::Cow::Borrowed(&self.projection);
        let composed = crate::engine::compose_batch(
            self.engine.registry(),
            self.engine.child_return(),
            &mut working,
            crate::engine::ComposingBatch { trajectory, id: batch },
            proposals,
            spawn,
            &|views, call| {
                views
                    .approvals_for(call)
                    .map(|(offer, approval)| (offer, approval.basis))
                    .find(|(offer, basis)| {
                        self.may_spend(trajectory, &crate::basis::SubjectKey::Approval(*offer), basis)
                            .is_ok()
                    })
                    .map(|(offer, _)| offer)
            },
        )
        .map_err(|(_, error)| match error {
            crate::engine::EngineError::InvalidCall(error) => TransitionRefusal::InvalidPayload(error),
            crate::engine::EngineError::UnknownTool(tool) | crate::engine::EngineError::ProviderRunTool(tool) => {
                TransitionRefusal::UnknownTool(tool)
            }
            crate::engine::EngineError::InvalidReturnSchema(_) => TransitionRefusal::ForkShapeMismatch,
            other => unreachable!("composing a batch refuses only on the call it cannot build, not {other}"),
        })?;
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
                        release.map(|release| PendingRelease {
                            dispatch: release.dispatch,
                            subject: crate::engine::ComposingBatch { trajectory, id: batch }.subject(position),
                            call: call.clone(),
                            prepares_fork: release.prepares_fork.is_some(),
                            next: match release.consumes {
                                Some(offer) => ReleasePart::Consumption(offer),
                                None => ReleasePart::Opening,
                            },
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
        subject: Option<&crate::basis::SubjectKey>,
        authorized: Obligation,
    ) -> Result<(), TransitionRefusal> {
        if dispatch.trajectory() != trajectory {
            return Err(TransitionRefusal::ForeignDispatch);
        }
        if self.projection.view(trajectory).dispatch_tool(dispatch).is_some() {
            return Err(TransitionRefusal::DispatchReopened);
        }
        let contract = self
            .engine
            .registry()
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
        let (stage, earned) = match &mut self.substituted {
            Some(substitution)
                if &substitution.call == call && subject == Some(&substitution.subject) && !substitution.released =>
            {
                substitution.released = true;
                (substitution.stage.clone(), true)
            }
            _ => (CallStage::default(), false),
        };
        if subject.is_some() && !earned && matches!(authorized, Obligation::Free) {
            return Err(TransitionRefusal::UnbackedDecision);
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
                    contribution: approval
                        .sanitizer
                        .as_ref()
                        .and_then(|name| crate::plan::bound_contribution(self.engine.registry(), contract, call, name)),
                };
                if remedy.is_none_or(|landed| landed != expected) {
                    return Err(TransitionRefusal::UnbackedApproval);
                }
                return match crate::check::evaluate(contract, &views, call, &stage) {
                    CheckOutcome::Block(block) if block.unestablished.is_empty() => Ok(()),
                    _ => Err(TransitionRefusal::UnreleasedDispatch),
                };
            }
            Obligation::Free => {}
        }
        match crate::check::evaluate(contract, &views, call, &stage) {
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
                    admits_block(self.engine.registry(), contract, call, &live, &block, &remedy)
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
            Provenance::ProviderRun {
                tool,
                batch,
                position,
                effects,
            } => {
                let contract = self
                    .engine
                    .registry()
                    .provider_run_contract(tool)
                    .ok_or_else(|| TransitionRefusal::NotProviderRun(tool.as_str().to_string()))?;
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
                let contract = self.dispatch_contract(trajectory, dispatch)?;
                let views = self.projection.view(trajectory);
                // Admission lands with the close, and only a success admits anything.
                if !views.closed_successfully(dispatch) {
                    return Err(TransitionRefusal::DispatchNotOpen);
                }
                if !self.admitted.insert(dispatch.clone()) {
                    return Err(TransitionRefusal::RepeatAdmission);
                }
                let expected = match self.derived.remove(dispatch) {
                    Some(Derived::Cast(resolved, raw_digest)) => {
                        if RawResultDigest::of(value.body.as_str().as_bytes()) != raw_digest {
                            return Err(TransitionRefusal::ForgedLabel);
                        }
                        let receiving = views
                            .receiving_bound(dispatch)
                            .ok_or(TransitionRefusal::UnknownDispatch)?;
                        if crate::admit::confined_residual(receiving, &resolved.clone().into_label()).is_some() {
                            return Err(TransitionRefusal::AcceptanceMismatch);
                        }
                        resolved.into_label()
                    }
                    Some(Derived::Sanitized(candidate)) => {
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
                            let subject = crate::basis::SubjectKey::ConfinedResult(dispatch.clone());
                            if matches!(views.candidate_via(&subject), Some(DerivedVia::Cast { .. }))
                                && !self.cast_restated.remove(dispatch)
                            {
                                return Err(TransitionRefusal::ForgedLabel);
                            }
                            candidate.label.clone()
                        }
                        Some(DerivedCandidate::Call { .. } | DerivedCandidate::Return { .. }) => {
                            return Err(TransitionRefusal::ForgedLabel);
                        }
                        None => {
                            if contract.pending_cast_dim().is_some() || views.bound_sanitizer(dispatch).is_some() {
                                return Err(TransitionRefusal::ForgedLabel);
                            }
                            self.observed_as(
                                trajectory,
                                dispatch,
                                &RawResultDigest::of(value.body.as_str().as_bytes()),
                            )?;
                            self.output_label(trajectory, dispatch, contract)
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
                if let Some(bound) = EstablishedLabel::from_label(&value.label) {
                    let baseline = match views.submitted_return(id) {
                        Some(submitted) => submitted.receiving.clone(),
                        None => views.current_label().bound().clone(),
                    };
                    let candidate = baseline.combine(&bound);
                    let owed = (candidate != baseline).then_some(Narrowing {
                        from: baseline,
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
            .engine
            .registry()
            .cast(cast)
            .ok_or_else(|| TransitionRefusal::UnknownCast(cast.as_str().to_string()))?;
        if !registered
            .scope
            .reaches(self.engine.registry(), &views, value)
            .unwrap_or(false)
        {
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
        let pending = views.pending_return(id).cloned();
        if views.has_ended(child) && pending.is_none() {
            return Err(TransitionRefusal::BranchEnded);
        }
        if id.child() != child || id.occurrence() != views.returns_by(child) {
            return Err(TransitionRefusal::WrongReturnIdentity);
        }
        if let (None, Some(shape)) = (&pending, views.return_shape_of(child)) {
            match shape.validate(value.body.as_str()) {
                Ok(canonical) if canonical == value.body.as_str() => {}
                _ => return Err(TransitionRefusal::ReturnShapeViolation),
            }
        }
        let fold = views.branch_label(child);
        let policy = views.return_policy_of(child).ok_or(TransitionRefusal::NotForked)?;
        let expected = match (policy, derivation) {
            (ReturnPolicy::Raw, ReturnDerivation::Raw) => {
                if let Some(pending) = &pending
                    && RawResultDigest::of(value.body.as_str().as_bytes()) != pending.digest
                {
                    return Err(TransitionRefusal::ReturnRecordMismatch);
                }
                fold.bound().clone().into_label()
            }
            (ReturnPolicy::Raw, ReturnDerivation::Sanitized { sanitizer, .. }) => match &pending {
                Some(pending) => self.consumed_candidate(&views, id, pending, value, sanitizer, derivation)?,
                None => {
                    let offered = match crate::branch::check_child_return(self.engine.registry(), &views, child) {
                        Ok(crate::branch::ReturnCheck::Block(block)) => block.plans,
                        _ => return Err(TransitionRefusal::ReturnPolicyMismatch),
                    };
                    if !offered.iter().any(|plan| {
                        matches!(plan, crate::branch::ReturnPlan::Sanitize { sanitizer: offered, .. } if offered == sanitizer)
                    }) {
                        return Err(TransitionRefusal::ReturnPolicyMismatch);
                    }
                    self.sanitized_crossing(sanitizer, derivation, &fold)?
                }
            },
            (ReturnPolicy::Sanitized(bound), ReturnDerivation::Sanitized { sanitizer, .. }) => match &pending {
                Some(pending) => self.consumed_candidate(&views, id, pending, value, sanitizer, derivation)?,
                None => {
                    if bound != sanitizer {
                        return Err(TransitionRefusal::ReturnPolicyMismatch);
                    }
                    self.sanitized_crossing(sanitizer, derivation, &fold)?
                }
            },
            _ => return Err(TransitionRefusal::ReturnPolicyMismatch),
        };
        if value.label != expected {
            return Err(TransitionRefusal::ForgedLabel);
        }
        // A terminal derivation's crossing is the batch this record lands in.
        self.return_settling.remove(id);
        Ok(())
    }

    fn consumed_candidate(
        &self,
        views: &Views,
        id: &ChildReturnId,
        pending: &crate::projection::SubmittedReturn,
        value: &crate::value::LabeledValue,
        sanitizer: &SanitizerName,
        derivation: &ReturnDerivation,
    ) -> Result<Label, TransitionRefusal> {
        let ReturnDerivation::Sanitized {
            raw_digest, transition, ..
        } = derivation
        else {
            return Err(TransitionRefusal::ReturnPolicyMismatch);
        };
        let subject = crate::basis::SubjectKey::Return(id.clone());
        let Some(DerivedCandidate::Return { value: candidate, .. }) = views.candidate(&subject) else {
            return Err(TransitionRefusal::ReturnRecordMismatch);
        };
        // The crossing is the candidate, body and label alike — never a value beside it.
        if candidate != value || raw_digest != &pending.digest {
            return Err(TransitionRefusal::ReturnRecordMismatch);
        }
        let lineage = views.lineage(&subject);
        if lineage.names().last() != Some(sanitizer) {
            return Err(TransitionRefusal::ReturnRecordMismatch);
        }
        let registered = self
            .engine
            .registry()
            .sanitizer(sanitizer)
            .ok_or_else(|| TransitionRefusal::UnknownSanitizer(sanitizer.as_str().to_string()))?;
        if &registered.transition != transition {
            return Err(TransitionRefusal::SanitizerUnapplicable);
        }
        Ok(candidate.label.clone())
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
            .engine
            .registry()
            .sanitizer(sanitizer)
            .ok_or_else(|| TransitionRefusal::UnknownSanitizer(sanitizer.as_str().to_string()))?;
        if !fold.is_established(registered.transition.dimension()) {
            return Err(TransitionRefusal::ForgedLabel);
        }
        match registered.derive_output(&fold.bound().clone().into_label(), &[]) {
            Some(label) if &registered.transition == transition => Ok(label),
            _ => Err(TransitionRefusal::SanitizerUnapplicable),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn return_submitted(
        &self,
        child: &TrajectoryId,
        id: &ChildReturnId,
        fork: &crate::value::ForkId,
        parent: &TrajectoryId,
        label: &crate::label::PartialLabel,
        digest: &RawResultDigest,
        body: &crate::value::ValueBody,
        policy: &ReturnPolicy,
    ) -> Result<(), TransitionRefusal> {
        let views = self.projection.view(parent);
        if views.parent_of(child) != Some(parent) {
            return Err(TransitionRefusal::ForeignTrajectory);
        }
        // Custody transfers only on a fork-bound child, addressed to its own fork.
        if views.fork_of(child) != Some(fork) {
            return Err(TransitionRefusal::ReturnRecordMismatch);
        }
        if views.has_ended(child) {
            return Err(TransitionRefusal::BranchEnded);
        }
        if id.child() != child || id.occurrence() != views.returns_by(child) {
            return Err(TransitionRefusal::WrongReturnIdentity);
        }
        if digest != &RawResultDigest::of(body.as_str().as_bytes()) {
            return Err(TransitionRefusal::ReturnRecordMismatch);
        }
        // The recorded policy is the deployment's binding, like the fork records before it.
        if policy != self.engine.child_return() {
            return Err(TransitionRefusal::ForkReturnPolicyMismatch);
        }
        // A shaped fork's submission is the canonical text its stored shape admits.
        if let Some(shape) = views.return_shape_of(child) {
            match shape.validate(body.as_str()) {
                Ok(canonical) if canonical == body.as_str() => {}
                _ => return Err(TransitionRefusal::ReturnShapeViolation),
            }
        }
        let fold = views.branch_label(child);
        if label != &fold {
            return Err(TransitionRefusal::ForgedLabel);
        }
        match policy {
            ReturnPolicy::Raw => {
                let receiving = views.current_label().bound().clone();
                if receiving.combine(fold.bound()) == receiving {
                    return Err(TransitionRefusal::ReturnRecordMismatch);
                }
            }
            ReturnPolicy::Sanitized(name) => {
                let registered = self
                    .engine
                    .registry()
                    .sanitizer(name)
                    .ok_or_else(|| TransitionRefusal::UnknownSanitizer(name.as_str().to_string()))?;
                if !fold.is_established(registered.transition.dimension())
                    || registered
                        .derive_output(&fold.bound().clone().into_label(), &[])
                        .is_none()
                {
                    return Err(TransitionRefusal::ReturnRecordMismatch);
                }
            }
        }
        Ok(())
    }

    /// One terminal rejection: only a mandatory binding rejects, and the typed reason
    /// is re-derived from the child fold — never taken on the record's word. The digest is the
    /// record's own claim over bytes the log deliberately never stores.
    fn return_rejected(
        &self,
        child: &TrajectoryId,
        id: &ChildReturnId,
        fork: &crate::value::ForkId,
        reason: &crate::fact::ReturnRejection,
    ) -> Result<(), TransitionRefusal> {
        let parent = self
            .projection
            .view(child)
            .parent_of(child)
            .ok_or(TransitionRefusal::NotForked)?
            .clone();
        let views = self.projection.view(&parent);
        if views.fork_of(child) != Some(fork) {
            return Err(TransitionRefusal::ReturnRecordMismatch);
        }
        if views.has_ended(child) {
            return Err(TransitionRefusal::BranchEnded);
        }
        if id.child() != child || id.occurrence() != views.returns_by(child) {
            return Err(TransitionRefusal::WrongReturnIdentity);
        }
        let policy = views.return_policy_of(child).ok_or(TransitionRefusal::NotForked)?;
        let ReturnPolicy::Sanitized(name) = policy else {
            return Err(TransitionRefusal::ReturnRecordMismatch);
        };
        let registered = self
            .engine
            .registry()
            .sanitizer(name)
            .ok_or_else(|| TransitionRefusal::UnknownSanitizer(name.as_str().to_string()))?;
        let fold = views.branch_label(child);
        let dim = registered.transition.dimension();
        let derives = match reason {
            crate::fact::ReturnRejection::ConsumedDimensionUnresolvable => {
                !fold.is_established(dim)
                    && crate::plan::resolvable_source(self.engine.registry(), &views, &fold, dim).is_none()
            }
            crate::fact::ReturnRejection::MandateUnmet => {
                fold.is_established(dim)
                    && registered
                        .derive_output(&fold.bound().clone().into_label(), &[])
                        .is_none()
            }
            crate::fact::ReturnRejection::PreconditionUnmet => name.is_attest_schema(),
        };
        if !derives {
            return Err(TransitionRefusal::ReturnRecordMismatch);
        }
        Ok(())
    }

    fn boundary(&mut self, trajectory: &TrajectoryId, kind: &BoundaryKind) -> Result<(), TransitionRefusal> {
        match kind {
            BoundaryKind::TurnEnd => Ok(()),
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

    fn output_cast_applied(
        &mut self,
        trajectory: &TrajectoryId,
        dispatch: &DispatchId,
        cast: &crate::names::CastName,
        resolved: &crate::label::EstablishedLabel,
        raw_digest: &RawResultDigest,
    ) -> Result<(), TransitionRefusal> {
        let contract = self.dispatch_contract(trajectory, dispatch)?;
        self.settling(trajectory, dispatch)?;
        self.observed_as(trajectory, dispatch, raw_digest)?;
        let views = self.projection.view(trajectory);
        let subject = crate::basis::SubjectKey::ConfinedResult(dispatch.clone());
        if let Some(DerivedVia::Cast { name }) = views.candidate_via(&subject) {
            let Some(DerivedCandidate::Result { source, value, .. }) = views.candidate(&subject) else {
                unreachable!("a subject with a recorded provenance holds its candidate")
            };
            let restated = crate::label::EstablishedLabel::from_label(&value.label)
                .is_some_and(|label| &label == resolved && name == cast && source == raw_digest);
            if !restated {
                return Err(TransitionRefusal::InadmissibleResolution);
            }
            if !self.cast_restated.insert(dispatch.clone()) {
                return Err(TransitionRefusal::RepeatAdmission);
            }
            return Ok(());
        }
        let raw = self.output_label(trajectory, dispatch, contract);
        crate::admit::validate_pending_cast(self.engine.registry(), contract, &raw, cast, resolved)
            .map_err(|_| TransitionRefusal::InadmissibleResolution)?;
        self.derived
            .insert(dispatch.clone(), Derived::Cast(resolved.clone(), *raw_digest));
        Ok(())
    }

    fn settling(&self, trajectory: &TrajectoryId, dispatch: &DispatchId) -> Result<(), TransitionRefusal> {
        let views = self.projection.view(trajectory);
        if !views.closed_successfully(dispatch) {
            return Err(TransitionRefusal::DispatchNotOpen);
        }
        if self.admitted.contains(dispatch) || self.derived.contains_key(dispatch) {
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
        self.engine
            .registry()
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

    fn candidate_derived(
        &mut self,
        trajectory: &TrajectoryId,
        subject: &crate::basis::SubjectKey,
        via: &DerivedVia,
        derived: &DerivedCandidate,
        lineage: &SanitizerLineage,
    ) -> Result<(), TransitionRefusal> {
        let (sanitizer, transition) = match via {
            DerivedVia::Sanitizer { name, transition } => (name, transition),
            DerivedVia::Cast { name } => return self.cast_candidate(trajectory, subject, name, derived, lineage),
        };
        let registered = self
            .engine
            .registry()
            .sanitizer(sanitizer)
            .ok_or_else(|| TransitionRefusal::UnknownSanitizer(sanitizer.as_str().to_string()))?;
        if &registered.transition != transition {
            return Err(TransitionRefusal::SanitizerUnapplicable);
        }
        let (dispatch, source, from, value, residual) = match derived {
            DerivedCandidate::Call {
                source,
                from,
                call,
                label,
            } => return self.substitution(trajectory, subject, sanitizer, source, from, call, label, lineage),
            DerivedCandidate::Result {
                dispatch,
                source,
                from,
                value,
                residual,
            } => (dispatch, source, from, value, residual),
            DerivedCandidate::Return {
                source,
                from,
                value,
                residual,
            } => {
                return self.return_derived(
                    trajectory, subject, sanitizer, registered, source, from, value, residual, lineage,
                );
            }
        };
        // The subject a confined candidate advances is its own dispatch's, never another's.
        if subject != &crate::basis::SubjectKey::ConfinedResult(dispatch.clone()) {
            return Err(TransitionRefusal::ForgedLabel);
        }
        let contract = self.dispatch_contract(trajectory, dispatch)?;
        let settles = crate::admit::settles_now(&self.projection.view(trajectory), dispatch, residual.as_ref());
        self.deriving(trajectory, dispatch, settles)?;
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
                let recorded = taken_offer(&views, offer)?;
                if recorded.plan.hop() != Some(sanitizer) || &recorded.subject != subject {
                    return Err(TransitionRefusal::UnbackedOffer);
                }
                let current = views.candidate(subject).ok_or(TransitionRefusal::UnknownOffer)?;
                let DerivedCandidate::Result {
                    value: predecessor,
                    residual: owed,
                    ..
                } = current
                else {
                    return Err(TransitionRefusal::ForgedLabel);
                };
                let expected = views.lineage(subject).extend(sanitizer.clone());
                if expected.as_ref() != Some(lineage) {
                    return Err(TransitionRefusal::SanitizerUnapplicable);
                }
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
            None => self.output_label(trajectory, dispatch, contract),
            Some((body, _)) => body.label.clone(),
        };
        let label = registered
            .derive_output(&from_label, &contract.tags)
            .ok_or(TransitionRefusal::SanitizerUnapplicable)?;
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
        if settles {
            self.derived.insert(dispatch.clone(), Derived::Sanitized(value.clone()));
        }
        Ok(())
    }

    fn cast_candidate(
        &mut self,
        trajectory: &TrajectoryId,
        subject: &crate::basis::SubjectKey,
        cast: &crate::names::CastName,
        derived: &DerivedCandidate,
        lineage: &SanitizerLineage,
    ) -> Result<(), TransitionRefusal> {
        let DerivedCandidate::Result {
            dispatch,
            source,
            from,
            value,
            residual,
        } = derived
        else {
            return Err(TransitionRefusal::ForgedLabel);
        };
        if subject != &crate::basis::SubjectKey::ConfinedResult(dispatch.clone()) {
            return Err(TransitionRefusal::ForgedLabel);
        }
        // A resolution is no hop: nothing precedes it, and no sanitizer is spent beside it.
        if from != &ConfinedFrom::Bound || !lineage.names().is_empty() {
            return Err(TransitionRefusal::ForgedLabel);
        }
        let contract = self.dispatch_contract(trajectory, dispatch)?;
        let views = self.projection.view(trajectory);
        if views.candidate(subject).is_some() {
            return Err(TransitionRefusal::InadmissibleResolution);
        }
        self.deriving(trajectory, dispatch, false)?;
        self.observed_as(trajectory, dispatch, source)?;
        if source != &RawResultDigest::of(value.body.as_str().as_bytes()) {
            return Err(TransitionRefusal::ObservationMismatch);
        }
        let Some(resolved) = crate::label::EstablishedLabel::from_label(&value.label) else {
            return Err(TransitionRefusal::ForgedLabel);
        };
        let raw = self.output_label(trajectory, dispatch, contract);
        crate::admit::validate_pending_cast(self.engine.registry(), contract, &raw, cast, &resolved)
            .map_err(|_| TransitionRefusal::InadmissibleResolution)?;
        let views = self.projection.view(trajectory);
        let receiving = views
            .receiving_bound(dispatch)
            .ok_or(TransitionRefusal::UnknownDispatch)?;
        match crate::admit::confined_residual(receiving, &value.label) {
            derived_residual @ Some(_) if residual == &derived_residual => Ok(()),
            _ => Err(TransitionRefusal::AcceptanceMismatch),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn return_derived(
        &mut self,
        trajectory: &TrajectoryId,
        subject: &crate::basis::SubjectKey,
        sanitizer: &SanitizerName,
        registered: &crate::authority::Sanitizer,
        source: &RawResultDigest,
        from: &ConfinedFrom,
        value: &crate::value::LabeledValue,
        residual: &Option<Narrowing>,
        lineage: &SanitizerLineage,
    ) -> Result<(), TransitionRefusal> {
        let crate::basis::SubjectKey::Return(id) = subject else {
            return Err(TransitionRefusal::ForgedLabel);
        };
        let child = id.child();
        let views = self.projection.view(trajectory);
        // The candidate is the parent's, like the offers that stand on it.
        if views.parent_of(child) != Some(trajectory) {
            return Err(TransitionRefusal::ForeignTrajectory);
        }
        let pending = views.pending_return(id).ok_or(TransitionRefusal::UnknownReturn)?;
        let receiving = pending.receiving.clone();
        let pending_digest = pending.digest;
        let pending_body = pending.body().clone();
        let policy = views
            .return_policy_of(child)
            .ok_or(TransitionRefusal::NotForked)?
            .clone();
        let fold = views.branch_label(child);
        if !fold.is_established(registered.transition.dimension()) {
            return Err(TransitionRefusal::ForgedLabel);
        }
        let (from_label, from_body) = match from {
            ConfinedFrom::Bound => {
                match &policy {
                    ReturnPolicy::Sanitized(bound) if bound == sanitizer => {}
                    _ => return Err(TransitionRefusal::SanitizerUnapplicable),
                }
                if views.candidate(subject).is_some() || lineage.names() != [sanitizer.clone()] {
                    return Err(TransitionRefusal::SanitizerUnapplicable);
                }
                if source != &pending_digest {
                    return Err(TransitionRefusal::ObservationMismatch);
                }
                (fold.bound().clone().into_label(), pending_body)
            }
            ConfinedFrom::Offer(offer) => {
                let recorded = taken_offer(&views, offer)?;
                if recorded.plan.hop() != Some(sanitizer) || &recorded.subject != subject {
                    return Err(TransitionRefusal::UnbackedOffer);
                }
                let expected = views.lineage(subject).extend(sanitizer.clone());
                if expected.as_ref() != Some(lineage) {
                    return Err(TransitionRefusal::SanitizerUnapplicable);
                }
                match views.candidate(subject) {
                    Some(DerivedCandidate::Return { value: predecessor, .. }) => {
                        if source != &RawResultDigest::of(predecessor.body.as_str().as_bytes()) {
                            return Err(TransitionRefusal::ObservationMismatch);
                        }
                        (predecessor.label.clone(), predecessor.body.clone())
                    }
                    Some(_) => return Err(TransitionRefusal::ForgedLabel),
                    None => {
                        if policy != ReturnPolicy::Raw {
                            return Err(TransitionRefusal::SanitizerUnapplicable);
                        }
                        if source != &pending_digest {
                            return Err(TransitionRefusal::ObservationMismatch);
                        }
                        (fold.bound().clone().into_label(), pending_body)
                    }
                }
            }
        };
        if sanitizer.is_attest_schema() {
            if value.body != from_body {
                return Err(TransitionRefusal::ForgedLabel);
            }
            if !crate::plan::attest_applicable(&views, child, &value.body, &registered.transition) {
                return Err(TransitionRefusal::SanitizerUnapplicable);
            }
        }
        let label = registered
            .derive_output(&from_label, &[])
            .ok_or(TransitionRefusal::SanitizerUnapplicable)?;
        if value.label != label {
            return Err(TransitionRefusal::ForgedLabel);
        }
        if matches!(from, ConfinedFrom::Offer(_)) && !crate::plan::confined_hop_helps(&receiving, &from_label, &label) {
            return Err(TransitionRefusal::SanitizerUnapplicable);
        }
        let derived_residual = crate::admit::confined_residual(&receiving, &label);
        if residual != &derived_residual {
            return Err(TransitionRefusal::AcceptanceMismatch);
        }
        // A derivation that narrows nothing settles now: its crossing lands in this same batch.
        if derived_residual.is_none() {
            self.return_settling.insert(id.clone());
        }
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
        let recorded = taken_offer(&views, from)?;
        if recorded.plan.hop() != Some(sanitizer) || &recorded.subject != subject {
            return Err(TransitionRefusal::UnbackedOffer);
        }
        let predecessor = subject_call(&views, subject).ok_or(TransitionRefusal::UnbackedOffer)?;
        if predecessor.digest() != recorded.call {
            return Err(TransitionRefusal::UnbackedOffer);
        }
        // The bytes this hop read are the ones the harness would have dispatched.
        if source != &RawResultDigest::of(predecessor.canonical_arguments().canonical_bytes()) {
            return Err(TransitionRefusal::ObservationMismatch);
        }
        // One name per lineage, and the chain this record claims is the chain that ran.
        let expected = views.lineage(subject).extend(sanitizer.clone());
        if expected.as_ref() != Some(lineage) {
            return Err(TransitionRefusal::SanitizerUnapplicable);
        }
        let contract = self
            .engine
            .registry()
            .tool(predecessor.tool())
            .ok_or_else(|| TransitionRefusal::UnknownTool(predecessor.tool().as_str().to_string()))?;
        contract
            .parameters
            .validate(call.arguments())
            .map_err(TransitionRefusal::InvalidPayload)?;
        if call != &predecessor.substituting(call.canonical_arguments().clone()) {
            return Err(TransitionRefusal::ForgedLabel);
        }
        let stage = views.call_stage(subject);
        let derived = registered
            .derive_input(&stage.released(&views.current_label()), &contract.tags)
            .ok_or(TransitionRefusal::SanitizerUnapplicable)?;
        if label != &derived {
            return Err(TransitionRefusal::ForgedLabel);
        }
        let CheckOutcome::Block(before) = crate::check::evaluate(contract, &views, &predecessor, &stage) else {
            return Err(TransitionRefusal::UnbackedOffer);
        };
        if !before.unestablished.is_empty() {
            return Err(TransitionRefusal::UnreleasedDispatch);
        }
        let next = CallStage::substituting(derived, lineage.clone());
        if !crate::plan::substitution_helps(&before, &crate::check::evaluate(contract, &views, call, &next)) {
            return Err(TransitionRefusal::SanitizerUnapplicable);
        }
        self.substituted = Some(Substitution {
            call: call.clone(),
            subject: subject.clone(),
            stage: next,
            released: false,
        });
        Ok(())
    }

    fn deriving(
        &self,
        trajectory: &TrajectoryId,
        dispatch: &DispatchId,
        settles: bool,
    ) -> Result<(), TransitionRefusal> {
        let views = self.projection.view(trajectory);
        if self.admitted.contains(dispatch) || self.derived.contains_key(dispatch) {
            return Err(TransitionRefusal::RepeatAdmission);
        }
        let confined = views.is_open(dispatch) && views.is_succeeded(dispatch);
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

fn subject_call(views: &Views<'_>, subject: &crate::basis::SubjectKey) -> Option<ResolvedCall> {
    if let Some(candidate) = views.call_candidate(subject) {
        return Some(candidate.clone());
    }
    let crate::basis::SubjectKey::Call { batch, position, .. } = subject else {
        return None;
    };
    let decided = views.decided_batch(batch)?;
    (&decided.trajectory == views.trajectory())
        .then(|| decided.proposals.get(*position as usize).cloned())
        .flatten()
}

fn confines(sequence: &Sequence<'_>, offer: &crate::value::OfferId, dispatch: &DispatchId) -> bool {
    sequence
        .projection
        .view(dispatch.trajectory())
        .offer(offer)
        .is_some_and(|recorded| recorded.subject == crate::basis::SubjectKey::ConfinedResult(dispatch.clone()))
}

fn returns(sequence: &Sequence<'_>, offer: &crate::value::OfferId, id: &ChildReturnId) -> bool {
    sequence
        .projection
        .view(id.child())
        .offer(offer)
        .is_some_and(|recorded| recorded.subject == crate::basis::SubjectKey::Return(id.clone()))
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
            Fact::DispatchOpened { .. } | Fact::ForkPrepared { .. } | Fact::CallApprovalConsumed { .. },
        ) => true,
        (
            DecidedAct::Outcome(act),
            Fact::DispatchSucceeded { dispatch, .. }
            | Fact::DispatchClosed { dispatch, .. }
            | Fact::OutputCastApplied { dispatch, .. },
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
        (
            DecidedAct::ChildReturn(act),
            Fact::ReturnSubmitted { id, .. }
            | Fact::ReturnRejected { id, .. }
            | Fact::CandidateDerived {
                subject: crate::basis::SubjectKey::Return(id),
                ..
            },
        ) => id == act,
        (DecidedAct::ChildReturn(_), Fact::CastApplied { .. }) => true,
        (
            DecidedAct::Offer(act),
            Fact::ChildReturn { id, .. }
            | Fact::ChildReturnAcceptance { child_return: id, .. }
            | Fact::ValueAdmitted {
                provenance: crate::value::Provenance::ChildReturn { id, .. },
                ..
            }
            | Fact::Boundary {
                kind: BoundaryKind::Merge { child_return: id },
                ..
            },
        ) => returns(sequence, act, id),
        (DecidedAct::Binding(act), Fact::ForkOpened { fork, .. }) => fork == act,
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
                    | DerivedCandidate::Return {
                        from: ConfinedFrom::Offer(offer),
                        ..
                    }
                    | DerivedCandidate::Call { from: offer, .. },
                ..
            },
        ) => offer == act,
        (DecidedAct::Offer(_), Fact::DispatchOpened { dispatch, .. }) => sequence
            .substituted
            .as_ref()
            .is_some_and(|substitution| dispatch.digest() == &substitution.call.digest()),
        (
            DecidedAct::Offer(act),
            Fact::DispatchClosed { dispatch, .. }
            | Fact::OutputCastApplied { dispatch, .. }
            | Fact::ValueAdmitted {
                provenance: crate::value::Provenance::ToolResult { dispatch },
                ..
            },
        ) => confines(sequence, act, dispatch),
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
    let settles = offered
        .iter()
        .any(|settlement| settlement.accept == remedy.acceptance && settlement.sanitize == remedy.sanitizer);
    let legacy = !settles
        && remedy.sanitizer.is_some()
        && offered
            .iter()
            .any(|settlement| settlement.accept.is_none() && settlement.sanitize == remedy.sanitizer)
        && remedy.acceptance
            == remedy
                .sanitizer
                .as_ref()
                .and_then(|name| crate::plan::predicted_residual(registry, contract, call, name, current));
    if !settles && !legacy {
        return Err(TransitionRefusal::UnreleasedDispatch);
    }
    let promised = remedy
        .sanitizer
        .as_ref()
        .and_then(|name| crate::plan::bound_contribution(registry, contract, call, name));
    if remedy.contribution != promised {
        return Err(TransitionRefusal::SanitizerUnapplicable);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::label::{Audience, Dim, PartialLabel, Trust};
    use crate::profile::PolicyFileKey;
    use crate::value::{LabeledValue, OfferNonce, ToolName, ValueBody, ValueId};

    fn traj() -> TrajectoryId {
        TrajectoryId::new("t")
    }

    fn note_tool() -> ToolContract {
        ToolContract {
            name: ToolName::new("note"),
            tags: vec![],
            delta: Some(crate::contract::Delta::NONE),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: crate::contract::Requires::default(),
        }
    }

    fn engine() -> Engine {
        let config = crate::registry::RegistryConfig {
            trust_chain: crate::registry::TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![note_tool()],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![],
            membership: None,
        };
        let profile = crate::profile::covering_declaration(&config);
        Engine::open(crate::profile::DeploymentPolicy {
            registry: config,
            planner_cap: crate::registry::PlannerCap::default(),
            dialect: PolicyDialectVersion::new(1),
            child_return: ReturnPolicy::Raw,
            profile,
        })
        .expect("the test policy opens")
    }

    fn starting() -> PartialLabel {
        PartialLabel::established(EstablishedLabel::new(Trust::new(1), Audience::Public))
    }

    fn opening(engine: &Engine, family: &TrajectoryId) -> Fact {
        engine
            .open_trajectory(family, PolicyFileKey::of(b"policy"))
            .expect("the opening validates against the empty log")
            .into_unsealed()
            .remove(0)
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
                        dynamic_resolutions: Vec::new(),
                        memberships: Vec::new(),
                    }],
                    spawn: None,
                    offer_nonce: nonce(),
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
                }),
            )
            .expect("the outcome closes the dispatch")
            .append
            .expect("the close appends");
        held.advance(&closed).unwrap();

        let whole = [first, released.facts().to_vec(), closed.facts().to_vec()].concat();
        let rebuilt = engine.view(&traj(), whole, 3).expect("the log validates");

        assert_eq!(held.revision(), rebuilt.revision());
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
    fn a_view_takes_no_batch_from_another_family_or_policy() {
        let engine = engine();
        let mut view = engine
            .view(&traj(), vec![opening(&engine, &traj())], 1)
            .expect("the log validates");
        let other_family = TrajectoryId::new("other");
        let elsewhere = engine
            .view(&other_family, vec![opening(&engine, &other_family)], 1)
            .expect("the log validates");
        let foreign = engine.seal(&elsewhere, vec![]).expect("the candidate validates");
        assert_eq!(view.advance(&foreign), Err(ViewMismatch::ForeignFamily));

        let other_policy = ValidatedFactBatch::seal(
            vec![],
            1,
            PolicyIdentityV1::of(
                &crate::registry::RegistryConfig {
                    trust_chain: crate::registry::TrustChain::new(vec!["suspicious".into()]),
                    tools: vec![],
                    authorities: vec![],
                    sanitizers: vec![],
                    casts: vec![],
                    membership: None,
                },
                &ReturnPolicy::Raw,
                engine.profile(),
            ),
            traj(),
        );
        assert_eq!(view.advance(&other_policy), Err(ViewMismatch::ForeignPolicy));
    }

    #[test]
    fn a_record_on_an_unopened_trajectory_is_refused() {
        let engine = engine();
        let turn_end = |trajectory: TrajectoryId| Fact::Boundary {
            trajectory,
            kind: BoundaryKind::TurnEnd,
        };
        assert_eq!(
            engine.view(&traj(), vec![turn_end(traj())], 1).err(),
            Some(TransitionRefusal::Unopened)
        );
        assert_eq!(engine.view(&traj(), vec![], 0).err(), Some(TransitionRefusal::Unopened));
        let stranger = TrajectoryId::new("stranger");
        let on_stranger = Fact::ValueAdmitted {
            trajectory: stranger.clone(),
            value: LabeledValue::new(
                ValueBody::new("body"),
                Label::new(Dim::Known(Trust::new(1)), Dim::Known(Audience::Public)),
            ),
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
                .view(&traj(), vec![opening(&engine, &traj()), turn_end(stranger)], 2)
                .err(),
            Some(TransitionRefusal::ForeignTrajectory)
        );
    }
}
