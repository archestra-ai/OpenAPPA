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
//! an authority verdict, a sanitizer derivation, or a dynamic-resolver answer.
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

use appa_engine::candidate::DerivedVia;
use appa_engine::check::UnestablishedFact;
use appa_engine::contract::{
    PinnedMembership, PinnedRequirementCast, PinnedToolResolution, RequiredAudience, RequirementSlot, ResolverReturn,
    ToolResolverUse,
};
pub(crate) use appa_engine::engine::ForkStatus;
use appa_engine::engine::{Engine, EngineError};
use appa_engine::execute::{AuthorityEvidence, AuthorityReview};
use appa_engine::fact::{BoundaryKind, CloseOutcome, EffectSet, Fact, ReturnDerivation};
use appa_engine::groups::GroupExpansion;
use appa_engine::label::{Audience, Dimension, EstablishedLabel, Label, PartialLabel, ReaderId, Trust};
use appa_engine::names::{CastName, GroupName, MarkName};
use appa_engine::plan::{ExecutableRemedyPlan, PlanId, PlannedBlock, RemedyPlan, RequiredRuling};
use appa_engine::profile::PolicyFileKey as EnginePolicyFileKey;
use appa_engine::projection::Views;
use appa_engine::registry::TrustChain;
use appa_engine::transition::Blocked as CoreBlocked;
/// The engine's own validated view is the runtime's too: the runtime adds no
/// wrapper, because everything it would carry beside the log is already on
/// the event or passed with the read.
pub(crate) use appa_engine::transition::EngineView;
use appa_engine::transition::{
    ApplicableCast, ApplicableRequirementCast, ChildFollowUp, ChildReport, ChildSubmission, EngineEvent as CoreEvent,
    Evidence, EvidenceRequest, FollowUp, ForkBinding, OfferConsult, OfferExecution, OfferFollowUp, OfferOutcome,
    OutcomeBody as CoreOutcomeBody, OutcomeFollowUp, ProposalBatch, ProposalBatchId, ProposedCall as CoreProposedCall,
    Released, SpawnMark, ToolOutcome as CoreToolOutcome, ToolReport, TransitionError, TransitionRefusal,
    ValidatedFactBatch,
};
use appa_engine::value::{
    ChildReturnId, DispatchId as EngineDispatchId, ForkId, OfferId as EngineOfferId, OfferNonce as EngineOfferNonce,
    Provenance, RawResultDigest, ResolvedCall, ToolName, TrajectoryId as EngineTrajectoryId, ValueBody, ValueId,
};
use appa_eventlog::Log;
use appa_runtime_api::{LabelDimension, UnestablishedValue};
use std::collections::BTreeMap;

use crate::api::OutcomeBody;
pub(crate) use crate::api::{OfferId, ProposedCall, SpawnBinding, ToolOutcome, TrajectoryId};
use crate::consult::{
    AuthorityAnswer, AuthorityArtifact, AuthorityDeclaration, CastAnswer, CastDeclaration, CastTool, DynamicAnswer,
    DynamicDeclaration, Requirement, Ruling, SanitizerArtifact, SanitizerDeclaration, SanitizerPoint, WireAudience,
};

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
    pub offers: Vec<OfferId>,
    /// The sources the block names as unestablished, typed; `text` says the same in prose.
    pub unestablished: Vec<UnestablishedValue>,
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
    ToolResolution {
        uses: ToolResolverUse,
        /// Exactly what this use selected: the value the request carries as `args`.
        args: serde_json::Value,
        declaration: DynamicDeclaration,
    },
    Membership {
        resolver: String,
        group: String,
    },
    /// Classify a result the model has not seen. The applicable casts travel in
    /// registration order and a constant among them arrives already resolved, so the
    /// session answers it without a call.
    PendingCast {
        source: RawResultDigest,
        ask: CastAsk,
    },
    /// Classify one admitted value a blocked act reads. Same cascade, same order: the two
    /// asks differ only in what the answer resolves.
    Cast {
        value: ValueId,
        ask: CastAsk,
    },
    /// Answer the requirement slots a proposed call's contract leaves Unknown, before the call
    /// is proposed. The casts travel in registration order; a constant among them arrives
    /// already answered.
    RequirementCast {
        call: appa_engine::value::CanonicalDigest,
        ask: RequirementAsk,
    },
}

/// One requirement ask as the session answers it: the casts still to try, and the complete
/// call every resolver consult carries under the declaration naming the slots to answer.
#[derive(Debug, Clone, PartialEq)]
pub struct RequirementAsk {
    pub casts: Vec<ApplicableRequirementCast>,
    pub declaration: DynamicDeclaration,
    pub args: serde_json::Value,
}

impl RequirementAsk {
    /// The same ask continued past `cast`: only the casts registered after it are left.
    fn after(&self, cast: &CastName) -> RequirementAsk {
        RequirementAsk {
            casts: self
                .casts
                .iter()
                .skip_while(|candidate| candidate.name != *cast)
                .skip(1)
                .cloned()
                .collect(),
            declaration: self.declaration.clone(),
            args: self.args.clone(),
        }
    }
}

/// One cast's requirement answer as the session obtained it: a declared constant echoed
/// back, or a resolver's wire answer still to be read against the policy.
#[derive(Debug, Clone, PartialEq)]
pub struct RequirementVerdict {
    pub cast: CastName,
    pub answer: RequirementLabel,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RequirementLabel {
    Declared(appa_engine::contract::RequirementAnswer),
    Classified(DynamicAnswer),
}

/// One cast ask as the session answers it: the casts still to try, in registration order,
/// and the value's bytes every classifier consult carries. Evidence carries the ask it
/// answers, so an answer the engine refuses is followed by the ask's next cast without
/// recomputing anything.
#[derive(Debug, Clone, PartialEq)]
pub struct CastAsk {
    pub casts: Vec<CastCandidate>,
    pub body: ValueBody,
}

/// One applicable cast as the session tries it: answered from the policy, or consulted
/// under its declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct CastCandidate {
    pub name: String,
    pub resolution: CandidateResolution,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CandidateResolution {
    Constant(EstablishedLabel),
    Resolver(CastDeclaration),
}

impl CastAsk {
    /// The same ask continued past `cast`: only the casts registered after it are left.
    fn after(&self, cast: &str) -> CastAsk {
        CastAsk {
            casts: self
                .casts
                .iter()
                .skip_while(|candidate| candidate.name != cast)
                .skip(1)
                .cloned()
                .collect(),
            body: self.body.clone(),
        }
    }
}

/// One cast's answer as the session obtained it.
#[derive(Debug, Clone, PartialEq)]
pub struct CastVerdict {
    pub cast: String,
    pub label: CastLabel,
}

/// Where a cast's label came from. A declared constant is the engine's own read echoed
/// back; a classified answer is a resolver's wire strings, still to be read against the
/// policy's trust chain.
#[derive(Debug, Clone, PartialEq)]
pub enum CastLabel {
    Declared(EstablishedLabel),
    Classified(CastAnswer),
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
    ToolResolution {
        resolver: String,
        /// The `args` the classifier answered for, by digest. Evidence matches that value only,
        /// so two calls sharing a resolver never take each other's answer.
        args: appa_engine::contract::ResolverArgsDigest,
        answer: DynamicAnswer,
    },
    Membership {
        resolver: String,
        group: String,
        readers: Option<Vec<String>>,
    },
    /// `None` means every cast the ask still carried was asked and none answered usably.
    PendingCast {
        source: RawResultDigest,
        verdict: Option<CastVerdict>,
        ask: CastAsk,
    },
    Cast {
        value: ValueId,
        verdict: Option<CastVerdict>,
        ask: CastAsk,
    },
    /// `None` means every cast the ask still carried was asked and none answered usably.
    RequirementCast {
        call: appa_engine::value::CanonicalDigest,
        verdict: Option<RequirementVerdict>,
        ask: RequirementAsk,
    },
}

impl ExternalEvidence {
    /// Does this answer the same cast ask as `other`? A later answer for the same source
    /// supersedes the earlier one: the cascade continued past a refused cast.
    pub(crate) fn answers_same_ask(&self, other: &ExternalEvidence) -> bool {
        match (self, other) {
            (ExternalEvidence::Cast { value: mine, .. }, ExternalEvidence::Cast { value: theirs, .. }) => {
                mine == theirs
            }
            (
                ExternalEvidence::PendingCast { source: mine, .. },
                ExternalEvidence::PendingCast { source: theirs, .. },
            ) => mine == theirs,
            (
                ExternalEvidence::RequirementCast { call: mine, .. },
                ExternalEvidence::RequirementCast { call: theirs, .. },
            ) => mine == theirs,
            _ => false,
        }
    }

    /// The ask continued past the cast that gave this answer, for an answer the engine
    /// refused. An ask with no cast left is still repeated: the session answers it with no
    /// answer, which supersedes the refused one and ends the cascade.
    fn continued(&self) -> Option<ExternalRequest> {
        match self {
            ExternalEvidence::Cast {
                value,
                verdict: Some(verdict),
                ask,
            } => Some(ExternalRequest::Cast {
                value: *value,
                ask: ask.after(&verdict.cast),
            }),
            ExternalEvidence::PendingCast {
                source,
                verdict: Some(verdict),
                ask,
            } => Some(ExternalRequest::PendingCast {
                source: *source,
                ask: ask.after(&verdict.cast),
            }),
            ExternalEvidence::RequirementCast {
                call,
                verdict: Some(verdict),
                ask,
            } => Some(ExternalRequest::RequirementCast {
                call: *call,
                ask: ask.after(&verdict.cast),
            }),
            _ => None,
        }
    }
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
        entropy: OfferNonce,
    },
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
    ReplaceOutput { placeholder: String },
    Value { value: String },
    Declined { feedback: String },
    NoAnswer { feedback: String },
    NoValue,
    Blocked { feedback: String, offers: Vec<OfferId> },
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
    /// The ids of the admitted values whose trust is still unresolved. `trust` is the
    /// established bound: every known restriction, readable while these stand.
    pub unresolved_trust: Vec<u64>,
    pub unresolved_audience: Vec<u64>,
}

/// One label rendered for a display surface: the established bound per dimension as chain
/// names and reader ids, plus the ids of the sources still unresolved on it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AuditLabel {
    pub trust: String,
    pub audience: String,
    pub unresolved_trust: Vec<u64>,
    pub unresolved_audience: Vec<u64>,
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
    Cast {
        cast: String,
        resolved: AuditLabel,
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
}

impl RuntimeEngine {
    pub fn new(engine: Engine) -> RuntimeEngine {
        RuntimeEngine { engine }
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
            unresolved_trust: label.unresolved_trust,
            unresolved_audience: label.unresolved_audience,
        })
    }

    /// The established bound by name, and the ids of the sources still unresolved beside
    /// it: an unresolved source never hides a known restriction.
    fn render_label(&self, label: &PartialLabel) -> Option<AuditLabel> {
        let chain = self.engine.registry().trust_chain();
        let bound = label.bound();
        let trust = if bound.trust == Trust::new(u8::MAX) {
            chain
                .name_of(Trust::new((chain.len() - 1) as u8))
                .expect("a validated chain names its top rank")
                .to_string()
        } else {
            match chain.name_of(bound.trust) {
                Some(name) => name.to_string(),
                None => {
                    tracing::warn!(
                        rank = bound.trust.rank(),
                        "render refused: the trust bound has no chain name"
                    );
                    return None;
                }
            }
        };
        let unresolved = |dim| label.unresolved(dim).map(ValueId::index).collect();
        Some(AuditLabel {
            trust: terminal_safe(&trust),
            audience: terminal_safe(&audience_wire(&bound.audience)),
            unresolved_trust: unresolved(Dimension::Trust),
            unresolved_audience: unresolved(Dimension::Audience),
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
        let mut prepared: std::collections::HashMap<ForkId, (String, PartialLabel)> = std::collections::HashMap::new();
        // A value's id is its admission's position in the family log, as the projection
        // numbers it; a child return is cited by the admission its merge appended.
        let mut returned: std::collections::HashMap<ChildReturnId, ValueId> = std::collections::HashMap::new();
        let mut admissions: u64 = 0;
        for fact in &facts {
            match fact {
                Fact::ForkPrepared {
                    fork,
                    snapshot,
                    trajectory,
                    ..
                } => {
                    prepared.insert(
                        fork.clone(),
                        (terminal_safe(trajectory.as_str()), snapshot.seed().clone()),
                    );
                }
                Fact::ValueAdmitted { provenance, .. } => {
                    if let Provenance::ChildReturn { id, .. } = provenance {
                        returned.insert(id.clone(), ValueId::new(admissions));
                    }
                    admissions += 1;
                }
                _ => {}
            }
        }
        let mut admissions: u64 = 0;
        let mut entries = Vec::new();
        for fact in &facts {
            let admitted = match fact {
                Fact::ValueAdmitted { .. } => {
                    admissions += 1;
                    Some(ValueId::new(admissions - 1))
                }
                Fact::ChildReturn { id, .. } => returned.get(id).copied(),
                _ => None,
            };
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
                _ => match self.audit_event(fact, admitted) {
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

    /// `admitted` is the id of the value this fact admits, where it admits one.
    fn audit_event(&self, fact: &Fact, admitted: Option<ValueId>) -> Option<Option<AuditEvent>> {
        let event = match fact {
            Fact::DispatchOpened {
                tool,
                proposed_label,
                proposed_effects,
                ..
            } => AuditEvent::Released {
                tool: terminal_safe(tool.as_str()),
                label: self.render_label(&PartialLabel::established(proposed_label.clone()))?,
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
                label: self.render_label(&value_fold(
                    admitted.expect("the audit numbers every admission it reads"),
                    &value.label,
                ))?,
            },
            Fact::Ruling { authority, .. } => AuditEvent::Ruled {
                authority: terminal_safe(authority.as_str()),
            },
            Fact::Denial { authority, .. } => AuditEvent::Denied {
                authority: terminal_safe(authority.as_str()),
            },
            Fact::Acceptance { narrowing, .. }
            | Fact::ChildReturnAcceptance { narrowing, .. }
            | Fact::CandidateAccepted { narrowing, .. } => AuditEvent::Narrowed {
                from: self.render_label(&PartialLabel::established(narrowing.from.clone()))?,
                to: self.render_label(&PartialLabel::established(narrowing.to.clone()))?,
            },
            Fact::CastApplied { cast, resolved, .. } | Fact::OutputCastApplied { cast, resolved, .. } => {
                AuditEvent::Cast {
                    cast: terminal_safe(cast.as_str()),
                    resolved: self.render_label(&PartialLabel::established(resolved.clone()))?,
                }
            }
            Fact::OutputSanitizerBound { sanitizer, .. } => AuditEvent::SanitizerBound {
                sanitizer: terminal_safe(sanitizer.as_str()),
            },
            Fact::CandidateDerived { via, .. } => match via {
                DerivedVia::Sanitizer { name, .. } => AuditEvent::Sanitized {
                    sanitizer: terminal_safe(name.as_str()),
                },
                DerivedVia::Cast { .. } => return Some(None),
            },
            Fact::ChildReturn { value, derivation, .. } => AuditEvent::ChildReturn {
                sanitizer: match derivation {
                    ReturnDerivation::Raw => None,
                    ReturnDerivation::Sanitized { sanitizer, .. } => Some(terminal_safe(sanitizer.as_str())),
                },
                label: self.render_label(&value_fold(
                    admitted.expect("a merge admits the value its crossing carries"),
                    &value.label,
                ))?,
            },
            Fact::Boundary { kind, .. } => match kind {
                BoundaryKind::Merge { .. } => AuditEvent::Merged,
                BoundaryKind::VoidReturn => AuditEvent::VoidReturn,
            },
            Fact::TrajectoryOpened { .. } | Fact::ProposalBatchDecided { .. } => return Some(None),
            Fact::OfferOpened { .. }
            | Fact::OfferAccepted { .. }
            | Fact::OfferDenied { .. }
            | Fact::OfferInvalidated { .. }
            | Fact::CallApproved { .. }
            | Fact::CallApprovalConsumed { .. }
            | Fact::BasisAdvanced { .. } => return Some(None),
            Fact::ForkPrepared { .. }
            | Fact::ForkOpened { .. }
            | Fact::ReturnSubmitted { .. }
            | Fact::ReturnRejected { .. } => return Some(None),
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
                evidence,
                entropy,
            } => self.execute_offer(view, &owner, &offer, &evidence, &entropy),
            EngineEvent::BindFork { fork, child } => self.bind_fork(view, &fork, &child),
            EngineEvent::ChildReturn {
                child,
                value,
                evidence,
                entropy,
            } => self.child_return(view, &child, value, &evidence, &entropy),
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
            Err(error) => return Ok(deny(malformed_feedback(&error))),
        };
        let owner = engine_id(trajectory);
        let Some(views) = view.views(&owner) else {
            return Err(EngineRefusal::Invariant {
                detail: "deciding a proposal for a trajectory the log has not opened".to_string(),
            });
        };
        let CallAnswers {
            tool_resolutions,
            memberships,
            requirement_cast,
        } = match self.answers_for(&views, &resolved, evidence) {
            Ok(answers) => answers,
            Err(Resolution::Feedback(text)) => return Ok(deny(text)),
            Err(Resolution::Consult(requests)) => return Ok(EngineDecision::deliver(Next::ResolveExternal(requests))),
        };
        let proposed = CoreProposedCall {
            tool: ToolName::new(call.tool.clone()),
            arguments: call.arguments.get().as_bytes().to_vec(),
            tool_resolutions,
            memberships,
            requirement_cast,
        };
        let expansions = self.membership_evidence(evidence);
        // A deployment that does not control context releases the marked call
        // unmarked, so the batch may be decided twice. The mark is all that
        // differs between the two attempts.
        let judge = |evidence: &[ExternalEvidence]| {
            let decide = |marked: bool| {
                let batch = ProposalBatch {
                    id: batch_id(entropy),
                    trajectory: engine_id(trajectory),
                    provider_results: Vec::new(),
                    proposals: vec![proposed.clone()],
                    spawn: marked.then(|| SpawnMark::at(0)),
                    offer_nonce: engine_nonce(entropy),
                    evidence: cast_evidence(self.engine.registry().trust_chain(), evidence),
                    expansions: expansions.expansions(),
                };
                self.engine.handle(view, CoreEvent::Proposals(batch))
            };
            match decide(spawn) {
                Err(TransitionError::SpawnUncontrolled) if spawn => decide(false),
                decided => decided,
            }
        };
        let decision = match judge(evidence) {
            Ok(decision) => decision,
            Err(TransitionError::MembershipNeeded { needed }) => {
                return match self.membership_consult(&expansions, needed)? {
                    MembershipConsult::Requests(requests) => {
                        Ok(EngineDecision::deliver(Next::ResolveExternal(requests)))
                    }
                    MembershipConsult::Unresolved(group) => Ok(deny(unresolved_group(&call.tool, &group))),
                };
            }
            Err(error) if refused_cast(&error) => {
                let continued = continued_casts(evidence, |subset| {
                    judge(subset).is_err_and(|error| refused_cast(&error))
                });
                return Ok(match continued.is_empty() {
                    true => deny(
                        "[appa] the classifier's answer was not admissible; the call is not decided and may be proposed again"
                            .to_string(),
                    ),
                    false => EngineDecision::deliver(Next::ResolveExternal(continued)),
                });
            }
            Err(error) => return Err(proposal_refusal(error)),
        };
        let append = decision.append.map(ValidatedFactBatch::into_unsealed);
        let then = self.deliver_proposals(view, trajectory, decision.follow_up, evidence)?;
        Ok(EngineDecision { append, then })
    }

    fn deliver_proposals(
        &self,
        view: &EngineView,
        trajectory: &TrajectoryId,
        follow_up: FollowUp,
        evidence: &[ExternalEvidence],
    ) -> Result<Next, EngineRefusal> {
        match follow_up {
            // The engine wants a value classified before it decides. Every ask goes out in one
            // round so a batch naming several unresolved sources costs one redrive, not one per
            // source; a cast already asked and unanswered is not asked again.
            FollowUp::ProposalsResolve(requests) => {
                let chain = self.engine.registry().trust_chain();
                let acting = engine_id(trajectory);
                let mut consults = Vec::new();
                let mut settled = false;
                for request in requests {
                    let EvidenceRequest::Cast { casts, value, body } = request else {
                        return Err(EngineRefusal::Invariant {
                            detail: "a proposal batch asked for something other than a cast".to_string(),
                        });
                    };
                    match cast_state(chain, evidence, value) {
                        CastAnswerState::Missing => consults.push(ExternalRequest::Cast {
                            value,
                            ask: self.cast_ask(value_source(view, &acting, value), casts, body),
                        }),
                        CastAnswerState::Unreadable(continued) => consults.push(*continued),
                        // This source will not be asked again: it answered, or it was
                        // asked and answered nothing. The rest of the batch still goes
                        // out, so one settled source does not cost the others a redrive.
                        CastAnswerState::NoAnswer | CastAnswerState::Resolved => settled = true,
                    }
                }
                match (settled, consults.is_empty()) {
                    (true, true) => Ok(deny_next(NO_CAST_ANSWERED.to_string())),
                    _ => Ok(Next::ResolveExternal(consults)),
                }
            }
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
                    let feedback = self.block_delivery(view, trajectory, &block);
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

    fn block_delivery(&self, view: &EngineView, trajectory: &TrajectoryId, block: &CoreBlocked) -> Feedback {
        let owner = engine_id(trajectory);
        let views = view
            .views(&owner)
            .expect("a block is delivered for the opened trajectory whose proposal the engine decided");
        let (text, offers) = self.rendered_block(&views, block);
        let unestablished = block
            .block
            .raw
            .unestablished
            .iter()
            .map(|fact| unestablished_value(&views, fact))
            .collect();
        Feedback {
            text,
            offers,
            unestablished,
        }
    }

    fn rendered_block(&self, views: &Views, block: &CoreBlocked) -> (String, Vec<OfferId>) {
        let offers: Vec<(OfferId, PlanId)> = block
            .offers
            .iter()
            .map(|(offer, plan)| (offer_id(offer), *plan))
            .collect();
        let text = block_feedback(views, &block.block, &offers, self.engine.registry().trust_chain());
        (text, offers.into_iter().map(|(offer, _)| offer).collect())
    }

    fn tool_outcome(
        &self,
        view: &EngineView,
        dispatch: &EngineDispatchId,
        outcome: &ToolOutcome,
        evidence: &[ExternalEvidence],
        entropy: &OfferNonce,
    ) -> Result<EngineDecision, EngineRefusal> {
        let expansions = self.membership_evidence(evidence);
        let judge = |evidence: &[ExternalEvidence]| {
            let report = ToolReport {
                dispatch: dispatch.clone(),
                outcome: engine_outcome(outcome),
                evidence: [
                    sanitizer_evidence(evidence),
                    cast_evidence(self.engine.registry().trust_chain(), evidence),
                ]
                .concat(),
                offer_nonce: engine_nonce(entropy),
                expansions: expansions.expansions(),
            };
            self.engine.handle(view, CoreEvent::Outcome(report))
        };
        let decision = match judge(evidence) {
            Ok(decision) => decision,
            // A classifier's answer the engine will not admit — over its ceiling, out of
            // scope, or disagreeing with a dimension already established. The classifier
            // misbehaved; the log is not in question, so the cascade continues with the
            // cast registered after it, and the report stays repeatable rather than
            // refusing the session.
            Err(error) if refused_cast(&error) => {
                let continued = continued_casts(evidence, |subset| {
                    judge(subset).is_err_and(|error| refused_cast(&error))
                });
                return Ok(match continued.is_empty() {
                    true => EngineDecision::deliver(Next::PresentToModel(Presentation::Blocked {
                        feedback:
                            "[appa] the classifier's answer was not admissible; the result is withheld and may be retried"
                                .to_string(),
                        offers: Vec::new(),
                    })),
                    false => EngineDecision::deliver(Next::ResolveExternal(continued)),
                });
            }
            Err(TransitionError::MembershipNeeded { needed }) => {
                return match self.membership_consult(&expansions, needed)? {
                    MembershipConsult::Requests(requests) => {
                        Ok(EngineDecision::deliver(Next::ResolveExternal(requests)))
                    }
                    MembershipConsult::Unresolved(group) => {
                        Ok(EngineDecision::deliver(Next::PresentToModel(Presentation::Blocked {
                            feedback: format!(
                                "[appa] membership of {group} could not be resolved; the result is withheld and may be retried"
                            ),
                            offers: Vec::new(),
                        })))
                    }
                };
            }
            Err(error) => return Err(outcome_refusal(error)),
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
                "[appa] no registered cast or sanitizer answered; the result is withheld and may be retried",
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
                // A rewrite whose arguments select another ordered contract is a new call under
                // it: that contract's resolvers and placeholder groups are consulted about the
                // rewritten arguments before the engine judges it. A rewrite that stays in its
                // contract carries the call's own answers, and a derivation the engine cannot
                // mint a call from is the engine's to refuse.
                // A requirement cast judged the call as proposed, never a rewrite of it, so the
                // input stage carries no requirement answer: the engine refuses a rewrite into
                // a contract that leaves a slot Unknown.
                let (tool_resolutions, memberships) = match self
                    .engine
                    .resolve_call(call.tool().clone(), derived.as_str().as_bytes())
                {
                    Ok(rewritten) if rewritten.contract_id() != call.contract_id() => {
                        match self.answers_for(&views, &rewritten, evidence) {
                            Ok(answers) => (answers.tool_resolutions, answers.memberships),
                            Err(Resolution::Consult(requests)) => {
                                return Ok(EngineDecision::deliver(Next::ResolveExternal(requests)));
                            }
                            Err(Resolution::Feedback(text)) => return Ok(no_answer(text)),
                        }
                    }
                    Ok(_) | Err(_) => (Vec::new(), Vec::new()),
                };
                OfferOutcome::Derived(Evidence::Rewrite {
                    sanitizer,
                    source,
                    derived,
                    tool_resolutions,
                    memberships,
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
        let expansions = self.membership_evidence(evidence);
        let execution = OfferExecution {
            trajectory: engine_id(trajectory),
            offer: engine_offer,
            outcome,
            offer_nonce: engine_nonce(entropy),
            expansions: expansions.expansions(),
        };
        let decision = match self.engine.handle(view, CoreEvent::ExecuteOffer(execution)) {
            Ok(decision) => decision,
            Err(TransitionError::MembershipNeeded { needed }) => {
                return match self.membership_consult(&expansions, needed)? {
                    MembershipConsult::Requests(requests) => {
                        Ok(EngineDecision::deliver(Next::ResolveExternal(requests)))
                    }
                    MembershipConsult::Unresolved(group) => Ok(no_answer(format!(
                        "[appa] membership of {group} could not be resolved; the offer stands and may be executed again"
                    ))),
                };
            }
            // A sanitizer's derivation the engine cannot use — malformed, schema-invalid, or not
            // a strict improvement — lands no fact and opens no dispatch; the offer stands for a
            // later deliberate retry. The external's answer is not an
            // integration fault, so it is not a refusal.
            Err(TransitionError::Call(_) | TransitionError::SanitizerUnapplicable) => {
                return Ok(no_answer(
                    "[appa] the sanitizer's derivation was not usable; the offer stands and may be executed again"
                        .to_string(),
                ));
            }
            Err(error) => return Err(offer_refusal(error)),
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
                Next::PresentToModel(self.offer_block_delivery(&views, &block))
            }
            FollowUp::Offer(OfferFollowUp::Substituted { block }) => {
                Next::PresentToModel(self.offer_block_delivery(&views, &block))
            }
            FollowUp::Offer(OfferFollowUp::Staged(confined)) => Next::PresentToModel(self.stage_delivery(
                "[appa] the cleaned result still narrows this session.",
                &confined.residual,
                &confined.offers,
            )),
            FollowUp::Offer(OfferFollowUp::ReturnStaged(stage)) => Next::PresentToModel(self.stage_delivery(
                "[appa] the child's return still narrows this session.",
                &stage.residual,
                &stage.offers,
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

    fn offer_block_delivery(&self, views: &Views, block: &CoreBlocked) -> Presentation {
        let (feedback, offers) = self.rendered_block(views, block);
        Presentation::Blocked { feedback, offers }
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

    fn child_return(
        &self,
        view: &EngineView,
        child: &TrajectoryId,
        value: Option<String>,
        evidence: &[ExternalEvidence],
        entropy: &OfferNonce,
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
        let expansions = self.membership_evidence(evidence);
        let judge = |evidence: &[ExternalEvidence]| {
            let report = ChildReport {
                child: engine_id(child),
                fork: fork.clone(),
                submission: submission.clone(),
                evidence: [
                    sanitizer_evidence(evidence),
                    cast_evidence(self.engine.registry().trust_chain(), evidence),
                ]
                .concat(),
                offer_nonce: engine_nonce(entropy),
                expansions: expansions.expansions(),
            };
            self.engine.handle(view, CoreEvent::ChildReturn(report))
        };
        let decision = match judge(evidence) {
            Ok(decision) => decision,
            Err(error) if refused_cast(&error) => {
                let continued = continued_casts(evidence, |subset| {
                    judge(subset).is_err_and(|error| refused_cast(&error))
                });
                return Ok(match continued.is_empty() {
                    true => EngineDecision::deliver(Next::PresentToModel(Presentation::Blocked {
                        feedback:
                            "[appa] the classifier's answer was not admissible; the return is withheld and may be retried"
                                .to_string(),
                        offers: Vec::new(),
                    })),
                    false => EngineDecision::deliver(Next::ResolveExternal(continued)),
                });
            }
            Err(TransitionError::MembershipNeeded { needed }) => {
                return match self.membership_consult(&expansions, needed)? {
                    MembershipConsult::Requests(requests) => {
                        Ok(EngineDecision::deliver(Next::ResolveExternal(requests)))
                    }
                    MembershipConsult::Unresolved(group) => {
                        Ok(EngineDecision::deliver(Next::PresentToModel(Presentation::Blocked {
                            feedback: format!(
                                "[appa] membership of {group} could not be resolved; the return is withheld and may be retried"
                            ),
                            offers: Vec::new(),
                        })))
                    }
                };
            }
            Err(error) => return Err(child_refusal(error)),
        };
        let append = decision.append.map(ValidatedFactBatch::into_unsealed);
        let then = match decision.follow_up {
            FollowUp::Child(ChildFollowUp::Merged { admitted }) => Next::PresentToModel(Presentation::Value {
                value: admitted.as_str().to_string(),
            }),
            FollowUp::Child(ChildFollowUp::Ended) => Next::PresentToModel(Presentation::NoValue),
            FollowUp::Child(ChildFollowUp::Pending(stage)) => Next::PresentToModel(self.stage_delivery(
                "[appa] the child's return still narrows this session.",
                &stage.residual,
                &stage.offers,
            )),
            FollowUp::Child(ChildFollowUp::Rejected { reason }) => Next::PresentToModel(Presentation::Blocked {
                feedback: format!("[appa] the child's return could not cross: {reason:?}"),
                offers: Vec::new(),
            }),
            FollowUp::Child(ChildFollowUp::Resolve(request)) => self.resolve_or_withhold(
                view,
                &engine_id(child),
                None,
                request,
                evidence,
                "[appa] no registered cast or return sanitizer answered; the return is withheld and may be retried",
            )?,
            other => {
                return Err(EngineRefusal::Invariant {
                    detail: format!("a child return produced an unexpected follow-up: {other:?}"),
                });
            }
        };
        Ok(EngineDecision { append, then })
    }

    /// Every answer a call's contract declares, from the evidence gathered so far, or the
    /// consults still owed: the resolvers it uses and the placeholder groups its arguments name.
    /// Every answer the call must carry before it is proposed: its tool-level resolver pins,
    /// its membership pins, and the cast answer for the requirement slots its contract leaves
    /// Unknown. Every consult still owed goes out in one round; feedback ends the proposal.
    fn answers_for(
        &self,
        views: &Views,
        resolved: &ResolvedCall,
        evidence: &[ExternalEvidence],
    ) -> Result<CallAnswers, Resolution> {
        let contract = self
            .engine
            .registry()
            .contract(resolved)
            .expect("a resolved call names its registered contract");
        let mut requests = Vec::new();
        let tools = gather(
            &mut requests,
            self.tool_resolutions_for(views, contract, resolved, evidence),
        )?;
        let memberships = gather(&mut requests, self.memberships_for(contract, resolved, evidence))?;
        let requirement = gather(&mut requests, self.requirement_cast_for(contract, resolved, evidence))?;
        match (tools, memberships, requirement) {
            (Some(tool_resolutions), Some(memberships), Some(requirement_cast)) => Ok(CallAnswers {
                tool_resolutions,
                memberships,
                requirement_cast,
            }),
            _ => {
                // A group both an argument and a cast's ceiling name is asked for once.
                let mut distinct: Vec<ExternalRequest> = Vec::with_capacity(requests.len());
                for request in requests {
                    if !distinct.contains(&request) {
                        distinct.push(request);
                    }
                }
                Err(Resolution::Consult(distinct))
            }
        }
    }

    /// The cast answer for the requirement slots `contract` leaves Unknown: none where it
    /// leaves nothing Unknown; the engine's own listing tried in order otherwise, a declared
    /// constant answering without a consult. No cast reaching the tool is a denial the model
    /// hears — for an undeclared tool, that it is undeclared. An answer the engine would refuse
    /// continues the cascade past its cast, and a cascade that runs dry is a denial.
    fn requirement_cast_for(
        &self,
        contract: &appa_engine::contract::ToolContract,
        resolved: &ResolvedCall,
        evidence: &[ExternalEvidence],
    ) -> Result<Option<PinnedRequirementCast>, Resolution> {
        let slots: Vec<RequirementSlot> = contract.requires.unknown_slots().collect();
        if slots.is_empty() {
            return Ok(None);
        }
        let tool = resolved.tool().as_str();
        let gathered = self.membership_evidence(evidence);
        let expansions = gathered.expansions();
        let listed = match self.engine.requirement_casts(resolved, &expansions) {
            Ok(listed) => listed,
            Err(TransitionError::MembershipNeeded { needed }) => {
                return match self.membership_consult(&gathered, needed) {
                    Ok(MembershipConsult::Requests(requests)) => Err(Resolution::Consult(requests)),
                    Ok(MembershipConsult::Unresolved(group)) => {
                        Err(Resolution::Feedback(unresolved_group(tool, &group)))
                    }
                    Err(refusal) => Err(Resolution::Feedback(format!("[appa] {tool}: {refusal}"))),
                };
            }
            Err(error) => return Err(Resolution::Feedback(format!("[appa] {tool}: {error}"))),
        };
        if listed.is_empty() {
            return Err(Resolution::Feedback(uncovered_requirements(
                self.engine.registry().classify(resolved.tool()),
                tool,
                &slots,
            )));
        }
        let digest = resolved.digest();
        let reported = evidence.iter().find_map(|entry| match entry {
            ExternalEvidence::RequirementCast { call, verdict, .. } if *call == digest => Some((verdict, entry)),
            _ => None,
        });
        let Some((verdict, entry)) = reported else {
            // A constant listed first answers here, with no evidence round: the engine projected
            // it onto the slots, so its pin is admitted by construction.
            if let Some(first) = listed.first()
                && let Some(answer) = first.constant.clone()
            {
                let pinned = PinnedRequirementCast::from_answer(first.name.clone(), digest, answer);
                let admitted = pinned.filter(|pinned| {
                    let judged = resolved.clone().with_requirement_cast(Some(pinned.clone()));
                    self.engine.requirement_cast_admits(&judged, &expansions)
                });
                return match admitted {
                    Some(pinned) => Ok(Some(pinned)),
                    None => Err(Resolution::Feedback(no_requirement_cast_answered(tool))),
                };
            }
            let declaration = self.requirement_declaration(&slots);
            let ask = RequirementAsk {
                casts: listed,
                declaration,
                args: contract.complete_call(resolved.tool(), resolved.arguments()),
            };
            return Err(Resolution::Consult(vec![ExternalRequest::RequirementCast {
                call: digest,
                ask,
            }]));
        };
        let Some(verdict) = verdict else {
            return Err(Resolution::Feedback(no_requirement_cast_answered(tool)));
        };
        let continued = || match entry.continued() {
            Some(continued) => Err(Resolution::Consult(vec![continued])),
            None => Err(Resolution::Feedback(no_requirement_cast_answered(tool))),
        };
        let Some(pinned) = self.requirement_pin(verdict, digest) else {
            return continued();
        };
        let judged = resolved.clone().with_requirement_cast(Some(pinned.clone()));
        match self.engine.requirement_cast_admits(&judged, &expansions) {
            true => Ok(Some(pinned)),
            false => continued(),
        }
    }

    /// What a resolver cast is told to answer: the Unknown slots as declared returns.
    fn requirement_declaration(&self, slots: &[RequirementSlot]) -> DynamicDeclaration {
        let returns = slots.iter().map(|slot| slot.resolver_return()).collect();
        self.dynamic_declaration(&returns)
    }

    /// The pin a verdict yields, or `None` where the answer cannot be read against the policy:
    /// an unknown rank, an audience ceiling where a floor was asked, or nothing answered.
    fn requirement_pin(
        &self,
        verdict: &RequirementVerdict,
        call: appa_engine::value::CanonicalDigest,
    ) -> Option<PinnedRequirementCast> {
        let answer = match &verdict.answer {
            RequirementLabel::Declared(answer) => answer.clone(),
            RequirementLabel::Classified(classified) => {
                let chain = self.engine.registry().trust_chain();
                let trust = match classified.required_trust.as_deref() {
                    Some(name) => Some(chain.rank_of(name)?),
                    None => None,
                };
                let audience = match &classified.required_audience {
                    Some(required) if required.cap.is_none() => Some(resolved_audience(required.includes.as_ref()?)),
                    Some(_) => return None,
                    None => None,
                };
                appa_engine::contract::RequirementAnswer {
                    trust,
                    audience,
                    attention: classified
                        .attention
                        .clone()
                        .map(|marks| marks.into_iter().map(MarkName::new).collect()),
                }
            }
        };
        PinnedRequirementCast::from_answer(verdict.cast.clone(), call, answer)
    }

    fn tool_resolutions_for(
        &self,
        views: &Views,
        contract: &appa_engine::contract::ToolContract,
        resolved: &ResolvedCall,
        evidence: &[ExternalEvidence],
    ) -> Result<Vec<PinnedToolResolution>, Resolution> {
        if contract.uses.is_empty() {
            return Ok(Vec::new());
        }
        let chain = self.engine.registry().trust_chain();
        // The arguments the consult carries are also the key resolver evidence is matched
        // against: an answer given for other arguments is not evidence for this call, and
        // the use consults again.
        let mut pins = Vec::new();
        let mut requests = Vec::new();
        for uses in &contract.uses {
            let args = contract.resolver_args(uses, resolved.tool(), resolved.arguments());
            let asked = contract.resolver_args_digest(uses, resolved.tool(), resolved.arguments());
            // A classification pinned to this call in an act the trajectory still has
            // prepared — an open offer, an unspent approval — stands: the re-proposal spells
            // the call the act was prepared for, and a resolver that may answer differently
            // twice is not asked twice. The record outranks evidence for the same arguments,
            // so one standing act never carries two answers for one subject.
            if let Some(pin) = views.pinned_tool_resolution(resolved, uses, asked) {
                pins.push(pin.clone());
                continue;
            }
            let answer = evidence.iter().find_map(|entry| match entry {
                ExternalEvidence::ToolResolution {
                    resolver,
                    args: answered_for,
                    answer,
                } if resolver == uses.resolver.as_str() && *answered_for == asked => Some(answer.clone()),
                _ => None,
            });
            match answer {
                None => requests.push(ExternalRequest::ToolResolution {
                    uses: uses.clone(),
                    args,
                    declaration: self.dynamic_declaration(&uses.returns),
                }),
                Some(answer) => {
                    let rank = |name: &str, what: &str| {
                        chain.rank_of(name).ok_or_else(|| {
                            Resolution::Feedback(format!(
                                "[appa] {}: dynamic resolver {} returned an unknown {what}",
                                resolved.tool().as_str(),
                                uses.resolver.as_str()
                            ))
                        })
                    };
                    let trust = answer
                        .trust
                        .as_deref()
                        .map(|name| rank(name, "trust rank"))
                        .transpose()?;
                    let audience = answer.audience.as_ref().map(resolved_audience);
                    let required_trust = answer
                        .required_trust
                        .as_deref()
                        .map(|name| rank(name, "required trust rank"))
                        .transpose()?;
                    let required_audience = answer.required_audience.as_ref().map(|required| RequiredAudience {
                        includes: required.includes.as_ref().map(resolved_audience),
                        cap: required.cap.as_ref().map(resolved_audience),
                    });
                    let attention = answer
                        .attention
                        .map(|marks| marks.into_iter().map(MarkName::new).collect());
                    match PinnedToolResolution::from_answer(
                        uses.clone(),
                        asked,
                        trust,
                        audience,
                        required_trust,
                        required_audience,
                        attention,
                    ) {
                        Some(pin) => pins.push(pin),
                        None => {
                            return Err(Resolution::Feedback(format!(
                                "[appa] {}: dynamic resolver {} returned malformed fields",
                                resolved.tool().as_str(),
                                uses.resolver.as_str()
                            )));
                        }
                    }
                }
            }
        }
        if !requests.is_empty() {
            return Err(Resolution::Consult(requests));
        }
        Ok(pins)
    }

    /// What one dynamic consult declares: the results to answer and the policy's vocabulary the
    /// answer may use — a resolver binding's own returns, or a requirement cast's asked slots.
    fn dynamic_declaration(&self, returns: &std::collections::BTreeSet<ResolverReturn>) -> DynamicDeclaration {
        let registry = self.engine.registry();
        DynamicDeclaration::of(
            returns,
            registry.trust_chain(),
            registry
                .attention_marks()
                .map(|mark| mark.as_str().to_string())
                .collect(),
        )
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
                let contract = registry
                    .contract(call)
                    .expect("a resolved call names its registered contract");
                let parameters =
                    serde_json::to_value(&contract.parameters).expect("a compiled parameter schema serializes");
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

    /// One cast ask, ready for the session: the casts the engine selected — each answered
    /// from the policy or carrying its declaration — and the value's bytes. The tool whose
    /// result the value is rides in every resolver's declaration.
    fn cast_ask(&self, source: CastSource, casts: Vec<ApplicableCast>, body: ValueBody) -> CastAsk {
        let registry = self.engine.registry();
        let CastSource { tool, call } = source;
        let tool = match call.as_ref().and_then(|call| registry.contract(call)) {
            Some(contract) => Some(CastTool::of(contract)),
            None => tool.map(|tool| CastTool {
                name: tool.as_str().to_string(),
                description: None,
            }),
        };
        let casts = casts
            .into_iter()
            .map(|applicable| CastCandidate {
                resolution: match applicable.constant {
                    Some(constant) => CandidateResolution::Constant(constant),
                    None => {
                        let registered = registry
                            .cast(&applicable.name)
                            .expect("the engine selects only registered casts");
                        CandidateResolution::Resolver(
                            CastDeclaration::of(registered, registry.trust_chain(), tool.clone())
                                .expect("a cast the engine did not answer resolves by resolver"),
                        )
                    }
                },
                name: applicable.name.as_str().to_string(),
            })
            .collect();
        CastAsk { casts, body }
    }

    /// The next step for an evidence request an outcome or a return raised: the ask, or the
    /// withheld presentation where every applicable answer was already obtained. `dispatch`
    /// is the one a pending cast classifies the result of.
    fn resolve_or_withhold(
        &self,
        view: &EngineView,
        trajectory: &EngineTrajectoryId,
        dispatch: Option<&EngineDispatchId>,
        request: EvidenceRequest,
        evidence: &[ExternalEvidence],
        withheld: &str,
    ) -> Result<Next, EngineRefusal> {
        let chain = self.engine.registry().trust_chain();
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
            EvidenceRequest::PendingCast { casts, source, body } => {
                match pending_cast_state(chain, evidence, &source) {
                    CastAnswerState::Missing => {
                        let call = dispatch.and_then(|dispatch| {
                            view.views(trajectory)
                                .and_then(|views| views.dispatch_call(dispatch).cloned())
                        });
                        Ok(Next::ResolveExternal(vec![ExternalRequest::PendingCast {
                            source,
                            ask: self.cast_ask(CastSource::from_call(call), casts, body),
                        }]))
                    }
                    CastAnswerState::Unreadable(continued) => Ok(Next::ResolveExternal(vec![*continued])),
                    CastAnswerState::NoAnswer | CastAnswerState::Resolved => blocked(),
                }
            }
            EvidenceRequest::Cast { casts, value, body } => match cast_state(chain, evidence, value) {
                CastAnswerState::Missing => Ok(Next::ResolveExternal(vec![ExternalRequest::Cast {
                    value,
                    ask: self.cast_ask(value_source(view, trajectory, value), casts, body),
                }])),
                CastAnswerState::Unreadable(continued) => Ok(Next::ResolveExternal(vec![*continued])),
                CastAnswerState::NoAnswer | CastAnswerState::Resolved => blocked(),
            },
        }
    }

    fn membership_evidence(&self, evidence: &[ExternalEvidence]) -> MembershipEvidence {
        let registry = self.engine.registry();
        let mut gathered = MembershipEvidence::default();
        let Some(resolver) = registry.membership() else {
            return gathered;
        };
        for entry in evidence {
            let ExternalEvidence::Membership {
                resolver: named,
                group,
                readers,
            } = entry
            else {
                continue;
            };
            let group = GroupName::new(group.clone());
            if named != resolver.as_str() || !registry.groups().contains(&group) {
                continue;
            }
            let expansion = readers
                .as_ref()
                .and_then(|readers| GroupExpansion::new(group.clone(), readers.iter().map(ReaderId::new)).ok());
            // An expansion outranks a no-answer for the same group, whichever was reported first.
            match (gathered.answers.get(&group), expansion) {
                (Some(Some(_)), _) => {}
                (_, expansion) => {
                    gathered.answers.insert(group, expansion);
                }
            }
        }
        gathered
    }

    fn membership_consult(
        &self,
        gathered: &MembershipEvidence,
        needed: Vec<GroupName>,
    ) -> Result<MembershipConsult, EngineRefusal> {
        let Some(resolver) = self.engine.registry().membership() else {
            // A policy that writes a group registers a resolver at load.
            return Err(EngineRefusal::Invariant {
                detail: "the engine reads a group under a policy that registers no membership resolver".to_string(),
            });
        };
        if let Some(group) = needed
            .iter()
            .find(|group| matches!(gathered.answers.get(*group), Some(None)))
        {
            return Ok(MembershipConsult::Unresolved(group.clone()));
        }
        Ok(MembershipConsult::Requests(
            needed
                .into_iter()
                .map(|group| ExternalRequest::Membership {
                    resolver: resolver.as_str().to_string(),
                    group: group.as_str().to_string(),
                })
                .collect(),
        ))
    }

    fn memberships_for(
        &self,
        contract: &appa_engine::contract::ToolContract,
        resolved: &ResolvedCall,
        evidence: &[ExternalEvidence],
    ) -> Result<Vec<PinnedMembership>, Resolution> {
        // No placeholder resolving to a group, nothing to read: `group_reads` already
        // answers empty for that, and for strictly more.
        let reads = appa_engine::check::group_reads(contract, resolved);
        if reads.is_empty() {
            return Ok(Vec::new());
        }
        let Some(resolver) = self.engine.registry().membership() else {
            let read = &reads[0];
            return Err(Resolution::Feedback(format!(
                "[appa] {}: argument {} names {}, but this deployment registers no membership resolver; the call was not checked",
                resolved.tool().as_str(),
                read.argument,
                read.group
            )));
        };
        let mut pins = Vec::new();
        let mut requests: Vec<ExternalRequest> = Vec::new();
        for read in reads {
            let group = read.group.as_str();
            let answer = evidence.iter().find_map(|entry| match entry {
                ExternalEvidence::Membership {
                    resolver: named,
                    group: expanded,
                    readers,
                } if named == resolver.as_str() && expanded == group => Some(readers.clone()),
                _ => None,
            });
            match answer {
                None => {
                    let request = ExternalRequest::Membership {
                        resolver: resolver.as_str().to_string(),
                        group: group.to_string(),
                    };
                    if !requests.contains(&request) {
                        requests.push(request);
                    }
                }
                Some(Some(readers)) => {
                    match PinnedMembership::new(read.argument.clone(), readers.into_iter().map(ReaderId::new)) {
                        Ok(pin) => pins.push(pin),
                        Err(_) => {
                            return Err(Resolution::Feedback(unresolved_group(
                                resolved.tool().as_str(),
                                &read.group,
                            )));
                        }
                    }
                }
                Some(None) => {
                    return Err(Resolution::Feedback(unresolved_group(
                        resolved.tool().as_str(),
                        &read.group,
                    )));
                }
            }
        }
        if !requests.is_empty() {
            return Err(Resolution::Consult(requests));
        }
        Ok(pins)
    }
}

/// Everything a proposal carries beyond its tool and arguments, gathered before it is judged.
struct CallAnswers {
    tool_resolutions: Vec<PinnedToolResolution>,
    memberships: Vec<PinnedMembership>,
    requirement_cast: Option<PinnedRequirementCast>,
}

/// One answer's contribution to a proposal's round of consults: the answer, or the consults it
/// still owes gathered beside the others'. Feedback ends the round.
fn gather<T>(requests: &mut Vec<ExternalRequest>, answer: Result<T, Resolution>) -> Result<Option<T>, Resolution> {
    match answer {
        Ok(answer) => Ok(Some(answer)),
        Err(Resolution::Consult(consults)) => {
            requests.extend(consults);
            Ok(None)
        }
        Err(Resolution::Feedback(text)) => Err(Resolution::Feedback(text)),
    }
}

/// The denial for a call whose contract leaves requirement slots Unknown that no cast reaches:
/// an undeclared tool with no cast covering undeclared tools, or a declared tool whose lazy
/// slots no cast's scope reaches.
fn uncovered_requirements(kind: appa_engine::registry::ToolKind, tool: &str, slots: &[RequirementSlot]) -> String {
    match kind {
        appa_engine::registry::ToolKind::Undeclared => {
            format!("[appa] tool {tool} is not declared in this policy and no cast covers undeclared tools")
        }
        _ => format!(
            "[appa] {tool}: its policy leaves {} unknown and no cast reaches it; the call was not checked",
            slots.iter().map(|slot| slot.wire_name()).collect::<Vec<_>>().join(", ")
        ),
    }
}

fn no_requirement_cast_answered(tool: &str) -> String {
    format!(
        "[appa] no registered cast answered what {tool} requires; the call is not decided yet and may be proposed again"
    )
}

fn unresolved_group(tool: &str, group: &GroupName) -> String {
    format!(
        "[appa] {tool}: membership of {group} could not be resolved; the call was not checked — propose it again later"
    )
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

#[derive(Debug)]
enum Resolution {
    Feedback(String),
    Consult(Vec<ExternalRequest>),
}

enum MembershipConsult {
    Requests(Vec<ExternalRequest>),
    Unresolved(GroupName),
}

#[derive(Debug, Default)]
struct MembershipEvidence {
    answers: BTreeMap<GroupName, Option<GroupExpansion>>,
}

impl MembershipEvidence {
    fn expansions(&self) -> Vec<GroupExpansion> {
        self.answers.values().flatten().cloned().collect()
    }
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
            unestablished: Vec::new(),
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

/// Whether the session has an answer for a cast the engine asked about. The states are what
/// stops a redrive loop: an answer the policy cannot read — a rank outside the chain — is
/// `Unreadable`, and the ask continues past the cast that gave it rather than being repeated,
/// so a classifier that cannot be read is asked once.
enum CastAnswerState {
    Missing,
    NoAnswer,
    Unreadable(Box<ExternalRequest>),
    Resolved,
}

/// Did the engine refuse a submitted cast answer — over its ceiling, out of scope, or
/// disagreeing with a dimension already established? A pending cast reports it as one
/// refusal; a lazily driven cast names the reason. Either way the log is untouched.
fn refused_cast(error: &TransitionError) -> bool {
    matches!(
        error,
        TransitionError::InadmissibleResolution | TransitionError::Resolution(_)
    )
}

/// The feedback for a call whose casts were all asked and none established the value.
const NO_CAST_ANSWERED: &str = "[appa] no registered cast could establish what this call reads; the call is not decided yet and may be proposed again";

/// Which submitted cast answers the engine refused, continued past the refusing cast. The
/// refusal names no answer, so each one is judged on its own beside the non-cast evidence
/// — the core validates every submitted answer before it asks for a missing one — and a
/// single submitted answer is the refused one without a second judgment.
fn continued_casts(
    evidence: &[ExternalEvidence],
    refuses: impl Fn(&[ExternalEvidence]) -> bool,
) -> Vec<ExternalRequest> {
    let (answers, rest): (Vec<ExternalEvidence>, Vec<ExternalEvidence>) =
        evidence.iter().cloned().partition(|entry| entry.continued().is_some());
    let refused = |answer: &ExternalEvidence| {
        if answers.len() == 1 {
            return true;
        }
        let mut alone = rest.clone();
        alone.push(answer.clone());
        refuses(&alone)
    };
    answers
        .iter()
        .filter(|answer| refused(answer))
        .filter_map(ExternalEvidence::continued)
        .collect()
}

/// The engine-side audience a classifier's wire audience resolves to.
fn resolved_audience(audience: &WireAudience) -> Audience {
    match audience {
        WireAudience::Public => Audience::Public,
        WireAudience::Readers(readers) => Audience::restricted(readers.iter().map(ReaderId::new)),
    }
}

fn cast_label(chain: &TrustChain, verdict: &CastVerdict) -> Option<EstablishedLabel> {
    match &verdict.label {
        CastLabel::Declared(label) => Some(label.clone()),
        CastLabel::Classified(answer) => {
            let trust = chain.rank_of(&answer.trust)?;
            Some(EstablishedLabel::new(trust, resolved_audience(&answer.audience)))
        }
    }
}

fn cast_evidence(chain: &TrustChain, evidence: &[ExternalEvidence]) -> Vec<Evidence> {
    evidence
        .iter()
        .filter_map(|entry| match entry {
            ExternalEvidence::PendingCast {
                source,
                verdict: Some(verdict),
                ..
            } => Some(Evidence::PendingCast {
                cast: appa_engine::names::CastName::new(verdict.cast.clone()),
                source: *source,
                resolved: cast_label(chain, verdict)?,
            }),
            ExternalEvidence::Cast {
                value,
                verdict: Some(verdict),
                ..
            } => Some(Evidence::Cast {
                cast: appa_engine::names::CastName::new(verdict.cast.clone()),
                value: *value,
                resolved: cast_label(chain, verdict)?,
            }),
            _ => None,
        })
        .collect()
}

fn pending_cast_state(chain: &TrustChain, evidence: &[ExternalEvidence], source: &RawResultDigest) -> CastAnswerState {
    for entry in evidence {
        if let ExternalEvidence::PendingCast {
            source: reported,
            verdict,
            ..
        } = entry
            && reported == source
        {
            return answer_state(chain, entry, verdict.as_ref());
        }
    }
    CastAnswerState::Missing
}

fn cast_state(chain: &TrustChain, evidence: &[ExternalEvidence], value: ValueId) -> CastAnswerState {
    for entry in evidence {
        if let ExternalEvidence::Cast {
            value: reported,
            verdict,
            ..
        } = entry
            && *reported == value
        {
            return answer_state(chain, entry, verdict.as_ref());
        }
    }
    CastAnswerState::Missing
}

fn answer_state(chain: &TrustChain, entry: &ExternalEvidence, verdict: Option<&CastVerdict>) -> CastAnswerState {
    match verdict {
        None => CastAnswerState::NoAnswer,
        Some(verdict) => match cast_label(chain, verdict) {
            Some(_) => CastAnswerState::Resolved,
            None => match entry.continued() {
                Some(continued) => CastAnswerState::Unreadable(Box::new(continued)),
                None => CastAnswerState::NoAnswer,
            },
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
        EngineError::InvalidReturnSchema(error) => {
            format!("[appa] invalid call: return_schema does not compile to a canonical shape: {error}")
        }
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

/// One admitted value's own contribution as a fold: a known dimension is the bound, an
/// unknown one names the value itself as the unresolved source.
fn value_fold(id: ValueId, label: &Label) -> PartialLabel {
    let mut fold = PartialLabel::established(EstablishedLabel::top());
    fold.fold_value(id, label);
    fold
}

fn effect_names(effects: &EffectSet) -> Vec<String> {
    effects.iter().map(|effect| terminal_safe(effect.as_str())).collect()
}

fn audience_wire(audience: &Audience) -> String {
    match audience {
        Audience::Public => "public".to_string(),
        Audience::Restricted(readers) if readers.is_empty() => "∅".to_string(),
        Audience::Restricted(readers) => {
            let shown: Vec<&str> = readers.iter().take(3).map(ReaderId::as_str).collect();
            let rest = readers.len().saturating_sub(3);
            if rest > 0 {
                format!("{}+{rest}", shown.join(","))
            } else {
                shown.join(",")
            }
        }
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
            appa_engine::label::Audience::Public => "the readers are not the public audience".to_string(),
            appa_engine::label::Audience::Restricted(readers) => {
                format!("the readers do not include {} required recipient(s)", readers.len())
            }
        },
        // The count only, as for `includes`: a cap may resolve a directory group.
        Gap::Cap { cap } => format!("the committed readers exceed the cap of {}", audience_count(cap)),
        Gap::Prior(effect) => format!("requires a prior {} effect", effect.as_str()),
        Gap::NoPrior(effect) => format!("forbidden after a {} effect", effect.as_str()),
        Gap::Attention(mark) => format!("requires attention: {}", mark.as_str()),
    }
}

/// Name a blocked value by where it came from, keeping its id: `ValueId(0)` alone tells the model
/// nothing, and the id stays because the trail cites it.
///
/// Only a value the receiving trajectory admitted itself is named this way. A blocked child return
/// is parent-facing while its facts are the child's, and `submit_result` is the ONLY channel
/// carrying child-derived data back: naming the tool the child chose would make this
/// feedback a second one, so a value the scope does not own stays id-only.
fn unestablished_source(views: &Views, value: ValueId) -> String {
    if !views.owns_value(value) {
        return format!("value {value:?}");
    }
    let source = match views.value_provenance(value) {
        Some(Provenance::ToolResult { dispatch }) => views
            .dispatch_tool(dispatch)
            .map(|tool| format!("the result of {}", tool.as_str())),
        Some(Provenance::ChildReturn { .. }) => Some("a subagent's return".to_string()),
        Some(Provenance::ProviderRun { tool, .. }) => Some(format!("the provider's {} result", tool.as_str())),
        None => None,
    };
    match source {
        Some(source) => format!("{source} ({value:?})"),
        None => format!("value {value:?}"),
    }
}

/// The tool whose result `value` is, as a cast's classifier is told it. A child return
/// originates from no tool.
/// The tool a cast classifies a value of, and the call that produced it where a dispatch did:
/// under ordered contracts the call, not the name, selects the contract whose requirements
/// the classifier sees. A provider-run value names its tool; no dispatch released it.
#[derive(Default)]
struct CastSource {
    tool: Option<ToolName>,
    call: Option<ResolvedCall>,
}

impl CastSource {
    fn from_call(call: Option<ResolvedCall>) -> Self {
        CastSource {
            tool: call.as_ref().map(|call| call.tool().clone()),
            call,
        }
    }
}

fn value_source(view: &EngineView, trajectory: &EngineTrajectoryId, value: ValueId) -> CastSource {
    let Some(views) = view.views(trajectory) else {
        return CastSource::default();
    };
    match views.value_provenance(value) {
        Some(Provenance::ToolResult { dispatch }) => CastSource::from_call(views.dispatch_call(dispatch).cloned()),
        Some(Provenance::ProviderRun { tool, .. }) => CastSource {
            tool: Some(tool.clone()),
            call: None,
        },
        Some(Provenance::ChildReturn { .. }) | None => CastSource::default(),
    }
}

/// The typed form of one unestablished source, under the same ownership rule as the prose:
/// a value the receiving trajectory did not admit itself is cited by id alone.
fn unestablished_value(views: &Views, fact: &UnestablishedFact) -> UnestablishedValue {
    let tool = match views.owns_value(fact.value) {
        false => None,
        true => match views.value_provenance(fact.value) {
            Some(Provenance::ToolResult { dispatch }) => views.dispatch_tool(dispatch).map(ToolName::as_str),
            Some(Provenance::ProviderRun { tool, .. }) => Some(tool.as_str()),
            Some(Provenance::ChildReturn { .. }) | None => None,
        },
    };
    UnestablishedValue {
        value: fact.value.index(),
        tool: tool.map(terminal_safe),
        dimensions: fact
            .dimensions
            .iter()
            .map(|dim| match dim {
                Dimension::Trust => LabelDimension::Trust,
                Dimension::Audience => LabelDimension::Audience,
            })
            .collect(),
    }
}

fn unestablished_feedback(views: &Views, facts: &[UnestablishedFact]) -> String {
    let entries: Vec<String> = facts
        .iter()
        .map(|fact| {
            let dims: Vec<String> = fact.dimensions.iter().map(|dim| format!("{dim:?}")).collect();
            format!(
                "{} is missing label facts for {}",
                unestablished_source(views, fact.value),
                dims.join(", ")
            )
        })
        .collect();
    format!(
        "{}. A fact must resolve this before a remedy can run",
        entries.join("; ")
    )
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
    match audience {
        Audience::Public => "public".to_string(),
        Audience::Restricted(readers) => match readers.len() {
            1 => "1 reader".to_string(),
            count => format!("{count} readers"),
        },
    }
}

fn remedy_instruction(plan: &ExecutableRemedyPlan, id: &OfferId) -> String {
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

fn remedy_lines(planned: &PlannedBlock, offers: &[(OfferId, PlanId)]) -> Vec<String> {
    planned
        .plans
        .iter()
        .filter_map(|plan| match plan {
            RemedyPlan::Executable(plan) => offers
                .iter()
                .find(|(_, offered)| *offered == plan.id)
                .map(|(id, _)| remedy_instruction(plan, id)),
            RemedyPlan::Redispatch(redispatch) => Some(format!(
                "  - Run {} first; it clears: {}.",
                terminal_safe(redispatch.tool().as_str()),
                terminal_safe(&redispatch.clears().iter().map(gap_text).collect::<Vec<_>>().join("; ")),
            )),
        })
        .collect()
}

fn block_feedback(views: &Views, planned: &PlannedBlock, offers: &[(OfferId, PlanId)], chain: &TrustChain) -> String {
    let mut reasons = Vec::new();
    for gap in &planned.raw.requirement_gaps {
        reasons.push(terminal_safe(&gap_text(gap)));
    }
    if let Some(narrowing) = &planned.raw.narrowing {
        reasons.extend(narrowing_feedback(narrowing, chain));
    }
    if !planned.raw.unestablished.is_empty() {
        reasons.push(terminal_safe(&unestablished_feedback(
            views,
            &planned.raw.unestablished,
        )));
    }

    let mut lines = vec![
        "[appa] Blocked: this call cannot run yet.".to_string(),
        String::new(),
        "Why:".to_string(),
    ];
    lines.extend(reasons.into_iter().map(|reason| format!("  - {reason}")));

    let remedies = remedy_lines(planned, offers);
    if !remedies.is_empty() {
        lines.push(String::new());
        lines.push("Continue:".to_string());
        lines.extend(remedies);
    }
    if let Some(advice) = &planned.fork_advice {
        lines.push(String::new());
        lines.push(if planned.raw.narrowing.is_some() {
            "Keep this session unchanged:".to_string()
        } else {
            "Alternative:".to_string()
        });
        lines.push(format!("  {}", advice.replace('\n', "\n  ")));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::{
        EngineEvent, EngineView, ExternalEvidence, ExternalRequest, Next, OfferId, OfferNonce, ProposedCall,
        Resolution, RuntimeEngine, SanitizerSubject, TrajectoryId, audience_wire, engine_id, remedy_instruction,
        remedy_lines, terminal_safe,
    };
    use crate::consult::{DynamicAnswer, RequiredAudienceAnswer, SanitizerPoint, WireAudience};
    use appa_engine::contract::RequiredAudience;
    use appa_engine::label::{Audience, ReaderId};
    use appa_engine::plan::{ExecutableRemedyPlan, PlanId, PlannedBlock, RemedyPlan, RemedyStep};
    use appa_engine::value::{RawResultDigest, ToolName, ValueBody};
    use std::collections::BTreeSet;

    #[test]
    fn a_sanitizer_consult_names_its_point_and_the_tool_the_value_belongs_to() {
        let policy = appa_policy::Config::from_toml_str(
            r#"
                version = 1
                [deployment]
                confined_child_return = true
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
        let engine = RuntimeEngine::new(policy.engine().clone());
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

    #[test]
    fn a_static_policy_mark_reaches_dynamic_consults_without_an_authority() {
        let policy = appa_policy::Config::from_toml_str(
            r#"
                version = 1
                [[dynamic_resolver]]
                name = "classifier"
                returns = ["requires.attention"]
                [[tool]]
                name = "classify"
                description = "Classifies a proposed operation."
                uses = [{ resolver = "classifier" }]
                delta = {}
                [[tool]]
                name = "blocked"
                description = "A statically blocked operation."
                delta = {}
                requires = { attention = ["blocked"] }
            "#,
        )
        .expect("a closed attention vocabulary does not require an authority");
        let engine = RuntimeEngine::new(policy.engine().clone());
        let contract = policy
            .registry()
            .variants(&ToolName::new("classify"))
            .next()
            .expect("the classifier-backed tool is registered");

        let declaration = engine.dynamic_declaration(&contract.uses[0].returns);

        assert_eq!(declaration.attention_marks, ["blocked"]);
    }

    fn classifier_policy() -> appa_policy::Config {
        classifier_policy_permitting("privacy-review")
    }

    /// The tool-level resolver policy with one authority permitting `mark`.
    fn classifier_policy_permitting(mark: &str) -> appa_policy::Config {
        appa_policy::Config::from_toml_str(&format!(
            r#"
                version = 1
                [[dynamic_resolver]]
                name = "classifier"
                returns = ["delta.trust", "delta.audience", "requires.trust", "requires.audience"]
                [[tool]]
                name = "lookup"
                description = "Looks one record up."
                uses = [{{ resolver = "classifier" }}]
                # The tool keeps its own attention mark: `requires.attention` has one owner, and
                # here it is the policy, so the classifier never sees the mark.
                requires = {{ attention = ["static-review"] }}
                [[authority]]
                name = "reviewer"
                [authority.permits]
                attention = ["{mark}"]
            "#
        ))
        .expect("the tool-level resolver policy compiles")
    }

    #[test]
    fn a_recorded_classification_answers_the_re_proposal_without_a_consult() {
        // The reviewer can clear the tool's own mark, so the classified call blocks with an
        // offer that stands for the re-proposal.
        let engine = RuntimeEngine::new(classifier_policy_permitting("static-review").engine().clone());
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
                [ExternalRequest::ToolResolution { args, .. }] => {
                    appa_engine::contract::ResolverArgsDigest::of(&appa_engine::params::canonical_bytes(args))
                }
                other => panic!("the first proposal consults the classifier once, not {other:?}"),
            },
            other => panic!("an unanswered resolver consults, not {other:?}"),
        };
        assert!(
            first.append.is_none(),
            "nothing is decided before the classifier answers"
        );

        let answer = DynamicAnswer {
            trust: Some("trusted".to_string()),
            audience: Some(WireAudience::Readers(vec!["support".to_string()])),
            required_trust: Some("trusted".to_string()),
            required_audience: Some(RequiredAudienceAnswer {
                includes: None,
                cap: Some(WireAudience::Public),
            }),
            attention: None,
        };
        let decided = propose(
            &view,
            vec![ExternalEvidence::ToolResolution {
                resolver: "classifier".to_string(),
                args: asked,
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
            "the offer the classified call was blocked with stands, so its pin answers the re-proposal"
        );
        let owner = engine_id(&trajectory);
        let views = view.views(&owner).expect("the root is opened");
        let resolved = engine
            .engine
            .resolve_call(ToolName::new("lookup"), br#"{"id": 7}"#)
            .expect("the call resolves");
        let contract = engine
            .engine
            .registry()
            .contract(&resolved)
            .expect("the call names its contract");
        let pins = engine
            .tool_resolutions_for(&views, contract, &resolved, &[])
            .expect("the recorded answer pins without evidence");
        assert_eq!(pins.len(), 1);
        assert_eq!(
            pins[0].audience(),
            Some(&Audience::restricted([ReaderId::new("support")]))
        );
        assert_eq!(pins[0].required_trust(), Some(appa_engine::label::Trust::new(1)));

        // Evidence for arguments the trajectory already classified is not a second answer:
        // the record outranks it, so the trajectory never pins two answers for one subject.
        let contradicting = ExternalEvidence::ToolResolution {
            resolver: "classifier".to_string(),
            args: asked,
            answer: DynamicAnswer {
                trust: Some("suspicious".to_string()),
                audience: Some(WireAudience::Public),
                required_trust: Some("suspicious".to_string()),
                required_audience: Some(RequiredAudienceAnswer {
                    includes: None,
                    cap: Some(WireAudience::Public),
                }),
                attention: None,
            },
        };
        let pinned = engine
            .tool_resolutions_for(&views, contract, &resolved, &[contradicting])
            .expect("the recorded answer pins over contradicting evidence");
        assert_eq!(pinned, pins);
    }

    #[test]
    fn one_tool_resolver_can_pin_delta_and_requirements_from_all_arguments() {
        let policy = classifier_policy();
        let engine = RuntimeEngine::new(policy.engine().clone());
        let call = engine
            .engine
            .resolve_call(
                ToolName::new("lookup"),
                serde_json::json!({"nested": {"id": 7}, "deep": true})
                    .to_string()
                    .as_bytes(),
            )
            .expect("the call resolves");
        let contract = engine
            .engine
            .registry()
            .contract(&call)
            .expect("the call names its contract");
        let trajectory = TrajectoryId("t".to_string());
        let view = opened_view(&engine, &trajectory);
        let owner = engine_id(&trajectory);
        let views = view.views(&owner).expect("the root is opened");
        let consulted_args = match engine.tool_resolutions_for(&views, contract, &call, &[]) {
            Err(Resolution::Consult(requests)) => match requests.as_slice() {
                [
                    ExternalRequest::ToolResolution {
                        uses,
                        args,
                        declaration,
                    },
                ] => {
                    assert_eq!(uses.resolver.as_str(), "classifier");
                    // The resolver declares no inputs, so `args` is the complete call.
                    assert_eq!(
                        args,
                        &serde_json::json!({
                            "name": "lookup",
                            "description": "Looks one record up.",
                            "arguments": {"nested": {"id": 7}, "deep": true},
                        })
                    );
                    // The declaration carries the policy's complete vocabulary and nothing of
                    // the trajectory: no current label and no call-specific requirements.
                    assert_eq!(declaration.trust_ranks, ["suspicious", "trusted"]);
                    assert_eq!(declaration.attention_marks, ["privacy-review", "static-review"]);
                    assert_eq!(
                        declaration.returns,
                        ["delta.trust", "delta.audience", "requires.trust", "requires.audience"]
                    );
                    appa_engine::contract::ResolverArgsDigest::of(&appa_engine::params::canonical_bytes(args))
                }
                other => panic!("expected one tool-resolution consult, got {other:?}"),
            },
            other => panic!("an unanswered tool resolver must consult, got {other:?}"),
        };

        let answer = || DynamicAnswer {
            trust: Some("suspicious".to_string()),
            audience: Some(WireAudience::Public),
            required_trust: Some("trusted".to_string()),
            required_audience: Some(RequiredAudienceAnswer {
                includes: Some(WireAudience::Readers(vec!["support".to_string()])),
                cap: Some(WireAudience::Public),
            }),
            attention: None,
        };
        let pins = engine
            .tool_resolutions_for(
                &views,
                contract,
                &call,
                &[ExternalEvidence::ToolResolution {
                    resolver: "classifier".to_string(),
                    args: consulted_args,
                    answer: answer(),
                }],
            )
            .expect("a complete answer pins");
        assert_eq!(pins.len(), 1);
        assert_eq!(pins[0].trust(), Some(appa_engine::label::Trust::new(0)));
        assert_eq!(pins[0].audience(), Some(&Audience::Public));
        assert_eq!(pins[0].required_trust(), Some(appa_engine::label::Trust::new(1)));
        assert_eq!(
            pins[0].required_audience(),
            Some(&RequiredAudience {
                includes: Some(Audience::restricted([ReaderId::new("support")])),
                cap: Some(Audience::Public),
            })
        );
        assert!(
            pins[0].attention().is_empty(),
            "this resolver returns no attention result: `requires.attention` is the policy's"
        );

        // An answer given for other arguments is not evidence for this call either: the
        // resolver is consulted again rather than handed a sibling's classification.
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
            engine.tool_resolutions_for(
                &views,
                contract,
                &other_call,
                &[ExternalEvidence::ToolResolution {
                    resolver: "classifier".to_string(),
                    args: consulted_args,
                    answer: answer(),
                }],
            ),
            Err(Resolution::Consult(_))
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
                unestablished: vec![],
                unknown_requirements: vec![],
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
        assert_eq!(
            remedy_lines(&planned, &offers),
            vec![
                remedy_instruction(&plan(3), &offers[1].0),
                remedy_instruction(&plan(8), &offers[0].0),
            ],
            "the plan with no offer is not shown; the rest carry their own offer"
        );
    }

    fn restricted(ids: &[&str]) -> Audience {
        Audience::Restricted(ids.iter().map(|id| ReaderId::new((*id).to_string())).collect())
    }

    #[test]
    fn audience_wire_spells_every_reader_shape() {
        assert_eq!(audience_wire(&Audience::Public), "public");
        assert_eq!(audience_wire(&Audience::Restricted(BTreeSet::new())), "∅");
        assert_eq!(audience_wire(&restricted(&["hr"])), "hr");
        assert_eq!(
            audience_wire(&restricted(&["d@x", "a@x", "c@x", "b@x"])),
            "a@x,b@x,c@x+1",
            "sorted, three shown, the rest counted",
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
