//! The engine boundary: the one module that speaks to `appa-engine`.
//!
//! [`EngineSeam::rebuild_view`] turns the store's opaque batch rows back into
//! typed facts and refuses the log before it is trusted: the log opens under
//! this root's own policy, and
//! [`appa_engine::engine::Engine::view`] then admits every record through the
//! engine's one sequential transition validator, so a log whose records could
//! not have followed one another never reaches a decision. Because
//! this runtime stores no view and rebuilds from the durable log on every
//! event, that decode step is the store-reopen trust
//! gate. [`EngineSeam::handle`] then translates one runtime event onto
//! [`appa_engine::engine::Engine::handle`], the engine's one mutation
//! boundary, and translates its typed follow-up back into a delivery. It
//! returns one decision: an optional fact batch to append under
//! compare-and-swap plus the follow-up the session delivers. The
//! vocabulary here is delivery adaptation, not policy: every
//! admissibility judgment is the engine's.
//!
//! Beside `handle` the seam makes projection reads — which branch has ended,
//! which dispatches it has open, what its label renders as. They gate nothing
//! and append nothing. One of them is not a read at all:
//! [`EngineSeam::opens_a_second_dispatch`] is this deployment's own host
//! policy, which the engine deliberately does not enforce.
//!
//! External evidence is typed before it reaches an engine input:
//! an authority verdict, a sanitizer derivation, a dynamic-resolver reader
//! set. A missing or malformed answer stays runtime-side and fails closed
//! — no no-answer variant ever enters an engine operation.
//!
//! Offers are engine-derived remedies and engine-owned facts: the runtime
//! holds no offer state of its own, so a restart loses none of them. The
//! trajectory that may execute one comes from the harness channel carrying
//! the control act, never from the id; the quoted id is resolved
//! against the offers that trajectory's own log opened. Execution re-derives
//! the plan from the live views and matches it by value, so an offer whose
//! basis has moved declines instead of executing.

use std::collections::BTreeMap;
#[cfg(test)]
use std::sync::Mutex;

use appa_engine::candidate::DerivedVia;
use appa_engine::check::UnestablishedFact;
use appa_engine::contract::{PinnedDynamicResolution, PinnedMembership};
pub(crate) use appa_engine::engine::ForkStatus;
use appa_engine::engine::{Engine, EngineError};
use appa_engine::execute::{AuthorityEvidence, AuthorityReview};
use appa_engine::fact::{BoundaryKind, CloseOutcome, EffectSet, Fact, ReturnDerivation};
use appa_engine::groups::GroupExpansion;
use appa_engine::label::{Audience, Dim, Dimension, EstablishedLabel, Label, PartialLabel, ReaderId, Trust};
use appa_engine::names::GroupName;
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
    ApplicableCast, ChildFollowUp, ChildReport, ChildSubmission, Confined, EngineDecision as CoreDecision,
    EngineEvent as CoreEvent, Evidence, EvidenceRequest, FollowUp, ForkBinding, OfferConsult, OfferExecution,
    OfferFollowUp, OfferOutcome, OutcomeBody as CoreOutcomeBody, OutcomeFollowUp, PendingReturnStage, ProposalBatch,
    ProposalBatchId, ProposedCall as CoreProposedCall, Released, SpawnMark, ToolOutcome as CoreToolOutcome, ToolReport,
    TransitionError, TransitionRefusal, ValidatedFactBatch,
};
use appa_engine::value::{
    DispatchId as EngineDispatchId, ForkId, OfferId as EngineOfferId, OfferNonce as EngineOfferNonce, Provenance,
    RawResultDigest, ResolvedCall, ToolName, ValueBody, ValueId,
};
use appa_eventlog::Log;

use crate::api::OutcomeBody;
pub(crate) use crate::api::{DispatchId, OfferId, ProposedCall, SpawnBinding, ToolOutcome, TrajectoryId};
use crate::external::{CastAnswer, CastAudience};

/// One fresh 256-bit random number per act that can surface offers; the
/// runtime mixes it into every `OfferId` it mints.
#[derive(Debug, Clone, Copy)]
pub struct OfferNonce(pub [u8; 32]);

/// A call the engine released: the exact canonical bytes the harness must
/// execute, delivered verbatim.
#[derive(Debug, Clone, PartialEq)]
pub struct ReleasedCall {
    pub dispatch: DispatchId,
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
}

/// One external consult the session must resolve before the same semantic
/// event replays with the answer attached.
#[derive(Debug, Clone, PartialEq)]
pub enum ExternalRequest {
    Authority {
        authority: String,
        payload: serde_json::Value,
        review: AuthorityReview,
    },
    Sanitizer {
        sanitizer: String,
        source: RawResultDigest,
        body: ValueBody,
    },
    Dynamic {
        resolver: String,
        tool: String,
        argument: String,
        value: String,
    },
    Membership {
        resolver: String,
        group: String,
    },
    /// Classify a result the model has not seen. The applicable casts travel in
    /// registration order and a constant among them arrives already resolved, so the
    /// session answers it without a call.
    PendingCast {
        casts: Vec<ApplicableCast>,
        source: RawResultDigest,
        body: ValueBody,
    },
    /// Classify one admitted value a blocked act reads. Same cascade, same order: the two
    /// asks differ only in what the answer resolves.
    Cast {
        casts: Vec<ApplicableCast>,
        value: ValueId,
        body: ValueBody,
    },
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
    Dynamic {
        resolver: String,
        argument: String,
        readers: Option<Vec<String>>,
    },
    Membership {
        resolver: String,
        group: String,
        readers: Option<Vec<String>>,
    },
    /// `None` means every applicable cast was asked and none answered usably.
    PendingCast {
        source: RawResultDigest,
        verdict: Option<CastVerdict>,
    },
    Cast {
        value: ValueId,
        verdict: Option<CastVerdict>,
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
    /// must never approve or deny.
    pub fn from_wire(answer: &serde_json::Value) -> AuthorityVerdict {
        match answer.get("ruling").and_then(|r| r.as_str()) {
            Some("approve") => AuthorityVerdict::Approve,
            Some("deny") => AuthorityVerdict::Deny,
            _ => AuthorityVerdict::Abstain,
        }
    }
}

/// One session event in the seam's vocabulary. The session constructs it; the
/// seam translates it onto the engine's `handle` boundary and back.
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

/// Why the seam refused an event outright. Model-visible outcomes (a deny, a
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
}

/// One label rendered for a display surface: chain names and reader ids as
/// plain strings, per dimension, with `unknown` where a source is still
/// unresolved.
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
    fn engine(&self) -> &RuntimeEngine {
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

/// The one engine boundary the session drives: who decides,
/// and nothing else. The deciding engine arrives per call as a
/// [`PolicyEngine`], because a family decides under the policy its root
/// opened with, which a configuration reload does not change.
/// The runtime holds no offer state — offers are the
/// engine's durable facts, routed by id.
pub enum EngineSeam {
    Real,
    #[cfg(test)]
    Test(TestSeam),
}

impl EngineSeam {
    /// Refuse one root's log before it is trusted, including the
    /// opening gate: the log's first record must be this root's opening under
    /// exactly the deciding engine's policy. The root is the log's own, so a
    /// view cannot be built against a log it does not describe.
    pub fn rebuild_view(&self, policy: &PolicyEngine<'_>, log: &Log) -> Result<EngineView, EngineRefusal> {
        policy.engine().rebuild_view(log)
    }

    /// What a root's opening record binds it to: the policy file it
    /// opened under and the identity that file must still compile to. `None`
    /// when the log does not open with its opening record, which the trust
    /// gate refuses in full a moment later.
    pub fn opened_under(&self, log: &Log) -> Option<Opened> {
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

    /// Where this trajectory stands in the log:
    /// never opened — the root by its opening record, a child by its fork
    /// binding — still taking events, or ended. The one replay-derived
    /// answer; the runtime keeps no flag of its own.
    pub fn liveness(&self, view: &EngineView, trajectory: &TrajectoryId) -> Liveness {
        let id = engine_id(trajectory);
        match view.views(&id) {
            None => Liveness::Unopened,
            Some(views) if views.has_ended(&id) => Liveness::Ended,
            Some(_) => Liveness::Live,
        }
    }

    pub fn parent_of(&self, view: &EngineView, child: &TrajectoryId) -> Option<TrajectoryId> {
        let child = engine_id(child);
        view.views(&child)?
            .parent_of(&child)
            .map(|parent| TrajectoryId(parent.as_str().to_string()))
    }

    /// Would applying this batch leave the trajectory with more than one
    /// dispatch open?
    pub fn opens_a_second_dispatch(&self, view: &EngineView, trajectory: &TrajectoryId, facts: &[Fact]) -> bool {
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
    pub fn offer_pursuer(&self, view: &EngineView, offer: &OfferId) -> Option<TrajectoryId> {
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
    pub fn open_dispatches(&self, view: &EngineView, trajectory: &TrajectoryId) -> Vec<OpenDispatch> {
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
    pub fn substituted_release(&self, view: &EngineView, trajectory: &TrajectoryId) -> Option<OpenDispatch> {
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

    pub fn handle(
        &self,
        policy: &PolicyEngine<'_>,
        view: &EngineView,
        trajectory: &TrajectoryId,
        event: EngineEvent,
    ) -> Result<EngineDecision, EngineRefusal> {
        match self {
            EngineSeam::Real => policy.engine().handle(view, trajectory, event),
            #[cfg(test)]
            EngineSeam::Test(seam) => Ok(seam.next(event)),
        }
    }

    /// The canonical bytes of one proposed call, for the byte-exact dispatch
    /// matching of provider-run tools. `None` when the call cannot canonicalize — an
    /// unknown tool or schema-invalid arguments never match a dispatched
    /// call, whose bytes the engine validated.
    pub fn canonical_bytes(&self, policy: &PolicyEngine<'_>, call: &ProposedCall) -> Option<Vec<u8>> {
        match self {
            EngineSeam::Real => policy.engine().canonical_bytes(call),
            #[cfg(test)]
            EngineSeam::Test(_) => serde_json::to_vec(call).ok(),
        }
    }

    #[cfg(test)]
    pub fn enqueue(&self, decision: EngineDecision) {
        match self {
            EngineSeam::Test(seam) => seam.enqueue(decision),
            EngineSeam::Real => panic!("only the test seam takes enqueued decisions"),
        }
    }

    #[cfg(test)]
    pub fn seen(&self) -> Vec<EngineEvent> {
        match self {
            EngineSeam::Test(seam) => seam.seen(),
            EngineSeam::Real => panic!("only the test seam records seen events"),
        }
    }

    /// Render one trajectory's current label from the rebuilt view, for the
    /// statusline. A projection read: no engine event, no fact, nothing
    /// gated.
    pub fn trajectory_status(
        &self,
        policy: &PolicyEngine<'_>,
        view: &EngineView,
        trajectory: &TrajectoryId,
    ) -> Option<TrajectoryStatus> {
        match self {
            EngineSeam::Real => policy.engine().trajectory_status(view, trajectory),
            #[cfg(test)]
            EngineSeam::Test(_) => Some(TrajectoryStatus {
                trajectory: String::new(),
                trust: String::new(),
                audience: String::new(),
            }),
        }
    }

    /// Where one fork stands in the rebuilt view: a
    /// projection read on the real compiled engine in both modes,
    /// because what it reads is the log itself. The runtime uses it to find
    /// the family's forks still open for binding, and the child a spawn's fork
    /// was bound to when that spawn's result arrives.
    pub fn fork_status(&self, policy: &PolicyEngine<'_>, view: &EngineView, fork: &ForkId) -> ForkStatus {
        policy.engine().engine.fork_status(view, fork)
    }

    /// The family's forks in flight: prepared, bound to
    /// no child, their spawn dispatch still open. A projection read
    /// on the real compiled engine in both modes; the runtime binds a child
    /// start that names no spawn to the one fork here.
    pub fn forks_in_flight(&self, policy: &PolicyEngine<'_>, view: &EngineView) -> Vec<ForkId> {
        policy.engine().engine.forks_in_flight(view)
    }

    /// The fork one child was bound to, or `None` for a trajectory
    /// the family never forked. A projection read on the real
    /// compiled engine in both modes, for a child start the harness delivers
    /// again: it names the fork it already bound.
    pub fn fork_of(&self, policy: &PolicyEngine<'_>, view: &EngineView, child: &TrajectoryId) -> Option<ForkId> {
        policy.engine().engine.fork_of(view, &engine_id(child))
    }

    /// Render the family's recorded decisions from its persisted log. Like
    /// [`EngineSeam::trajectory_status`], a projection read — and
    /// like the replay gates, it runs on the real compiled engine in both
    /// modes, because what it renders is the log itself.
    pub fn audit(&self, policy: &PolicyEngine<'_>, log: &Log) -> Result<Option<Vec<AuditEntry>>, EngineRefusal> {
        policy.engine().audit(log)
    }
}

/// The real engine behind the seam: the immutable registry-backed decision
/// core. It owns every judgment and every fact; the runtime holds
/// no engine state, and offers are the engine's own durable facts.
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

    fn rebuild_view(&self, log: &Log) -> Result<EngineView, EngineRefusal> {
        let root = TrajectoryId(log.root().as_str().to_string());
        self.validated(log.facts().to_vec(), &root, log.basis())
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

    fn canonical_bytes(&self, call: &ProposedCall) -> Option<Vec<u8>> {
        let resolved = self
            .engine
            .resolve_call(ToolName::new(call.tool.clone()), call.arguments.get().as_bytes())
            .ok()?;
        Some(resolved.canonical_arguments().canonical_bytes().to_vec())
    }

    fn trajectory_status(&self, view: &EngineView, trajectory: &TrajectoryId) -> Option<TrajectoryStatus> {
        let current = view.views(&engine_id(trajectory))?.current_label();
        let label = self.render_label(&as_label(&current))?;
        Some(TrajectoryStatus {
            trajectory: terminal_safe(&trajectory.0),
            trust: label.trust,
            audience: label.audience,
        })
    }

    fn render_label(&self, label: &Label) -> Option<AuditLabel> {
        let chain = self.engine.registry().trust_chain();
        let trust = match label.trust {
            Dim::Known(bound) if bound == Trust::new(u8::MAX) => chain
                .name_of(Trust::new((chain.len() - 1) as u8))
                .expect("a validated chain names its top rank")
                .to_string(),
            Dim::Known(bound) => match chain.name_of(bound) {
                Some(name) => name.to_string(),
                None => {
                    tracing::warn!(rank = bound.rank(), "render refused: the trust bound has no chain name");
                    return None;
                }
            },
            Dim::Unknown => "unknown".to_string(),
        };
        let audience = match &label.audience {
            Dim::Known(audience) => audience_wire(audience),
            Dim::Unknown => "unknown".to_string(),
        };
        Some(AuditLabel {
            trust: terminal_safe(&trust),
            audience: terminal_safe(&audience),
        })
    }

    fn audit(&self, log: &Log) -> Result<Option<Vec<AuditEntry>>, EngineRefusal> {
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
                    (terminal_safe(trajectory.as_str()), as_label(snapshot.seed())),
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
                label: self.render_label(&proposed_label.clone().into_label())?,
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
            Fact::Acceptance { narrowing, .. }
            | Fact::ChildReturnAcceptance { narrowing, .. }
            | Fact::CandidateAccepted { narrowing, .. } => AuditEvent::Narrowed {
                from: self.render_label(&narrowing.from.clone().into_label())?,
                to: self.render_label(&narrowing.to.clone().into_label())?,
            },
            Fact::CastApplied { cast, resolved, .. } | Fact::OutputCastApplied { cast, resolved, .. } => {
                AuditEvent::Cast {
                    cast: terminal_safe(cast.as_str()),
                    resolved: self.render_label(&resolved.clone().into_label())?,
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
                label: self.render_label(&value.label)?,
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

    fn handle(
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
        let (pins, memberships) = match (
            self.resolve_dynamics(call, evidence),
            self.resolve_memberships(call, evidence),
        ) {
            (Ok(pins), Ok(memberships)) => (pins, memberships),
            (Err(Resolution::Feedback(text)), _) | (_, Err(Resolution::Feedback(text))) => return Ok(deny(text)),
            (dynamics, memberships) => {
                let requests = [dynamics.err(), memberships.map(|_| ()).err()]
                    .into_iter()
                    .flatten()
                    .flat_map(|missing| match missing {
                        Resolution::Consult(requests) => requests,
                        Resolution::Feedback(_) => Vec::new(),
                    })
                    .collect();
                return Ok(EngineDecision::deliver(Next::ResolveExternal(requests)));
            }
        };
        let proposed = CoreProposedCall {
            tool: ToolName::new(call.tool.clone()),
            arguments: call.arguments.get().as_bytes().to_vec(),
            dynamic_resolutions: pins,
            memberships,
        };
        let expansions = self.membership_evidence(evidence);
        let decided = if spawn {
            match self.decide_proposal(view, trajectory, proposed.clone(), entropy, true, &expansions, evidence) {
                Ok(decision) => Ok(decision),
                Err(TransitionError::SpawnUncontrolled) => {
                    self.decide_proposal(view, trajectory, proposed, entropy, false, &expansions, evidence)
                }
                Err(error) => Err(error),
            }
        } else {
            self.decide_proposal(view, trajectory, proposed, entropy, false, &expansions, evidence)
        };
        let decision = match decided {
            Ok(decision) => decision,
            Err(TransitionError::MembershipNeeded { needed }) => {
                return match self.membership_consult(&expansions, needed)? {
                    MembershipConsult::Requests(requests) => {
                        Ok(EngineDecision::deliver(Next::ResolveExternal(requests)))
                    }
                    MembershipConsult::Unresolved(group) => Ok(deny(unresolved_group(&call.tool, &group))),
                };
            }
            Err(TransitionError::InadmissibleResolution) => {
                return Ok(deny(
                    "[appa] the classifier's answer was not admissible; the call is blocked and may be proposed again"
                        .to_string(),
                ));
            }
            Err(error) => return Err(proposal_refusal(error)),
        };
        let append = decision.append.map(ValidatedFactBatch::into_unsealed);
        let then = self.deliver_proposals(view, trajectory, decision.follow_up, evidence)?;
        Ok(EngineDecision { append, then })
    }

    #[allow(clippy::too_many_arguments)]
    fn decide_proposal(
        &self,
        view: &EngineView,
        trajectory: &TrajectoryId,
        proposed: CoreProposedCall,
        entropy: &OfferNonce,
        spawn: bool,
        expansions: &MembershipEvidence,
        external: &[ExternalEvidence],
    ) -> Result<CoreDecision, TransitionError> {
        let batch = ProposalBatch {
            id: batch_id(entropy),
            trajectory: engine_id(trajectory),
            provider_results: Vec::new(),
            proposals: vec![proposed],
            spawn: spawn.then(|| SpawnMark::at(0)),
            offer_nonce: engine_nonce(entropy),
            evidence: cast_evidence(self.engine.registry().trust_chain(), external),
            expansions: expansions.expansions(),
        };
        self.engine.handle(view, CoreEvent::Proposals(batch))
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
            // source; a cast already asked and unanswered blocks instead of being asked again.
            FollowUp::ProposalsResolve(requests) => {
                let chain = self.engine.registry().trust_chain();
                let mut consults = Vec::new();
                for request in requests {
                    let EvidenceRequest::Cast { casts, value, body } = request else {
                        return Err(EngineRefusal::Invariant {
                            detail: "a proposal batch asked for something other than a cast".to_string(),
                        });
                    };
                    match cast_state(chain, evidence, value) {
                        CastAnswerState::Missing => consults.push(ExternalRequest::Cast { casts, value, body }),
                        CastAnswerState::NoAnswer | CastAnswerState::Resolved => {
                            return Ok(deny_next(
                                "[appa] no registered cast could establish what this call reads; the call is blocked"
                                    .to_string(),
                            ));
                        }
                    }
                }
                Ok(Next::ResolveExternal(consults))
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
        Feedback { text, offers }
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
        let decision = match self.engine.handle(view, CoreEvent::Outcome(report)) {
            Ok(decision) => decision,
            // A classifier's answer the engine will not admit — over its ceiling, out of
            // scope, or disagreeing with a dimension already established. The classifier
            // misbehaved; the log is not in question, so the result is withheld and the
            // report stays repeatable rather than refusing the session.
            Err(TransitionError::InadmissibleResolution) => {
                return Ok(EngineDecision::deliver(Next::PresentToModel(Presentation::Blocked {
                    feedback:
                        "[appa] the classifier's answer was not admissible; the result is withheld and may be retried"
                            .to_string(),
                    offers: Vec::new(),
                })));
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
            FollowUp::Outcome(OutcomeFollowUp::Resolve(request)) => resolve_or_withhold(
                self.engine.registry().trust_chain(),
                request,
                evidence,
                "[appa] no registered cast or sanitizer answered; the result is withheld and may be retried",
            )?,
            FollowUp::Outcome(OutcomeFollowUp::Staged(confined)) => {
                Next::PresentToModel(self.confined_delivery(&confined))
            }
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
            OfferConsult::Sanitizer {
                sanitizer,
                source,
                body,
            } => match sanitizer_derivation(evidence, sanitizer.as_str(), &source) {
                SanitizerAnswer::Missing => {
                    return Ok(EngineDecision::deliver(Next::ResolveExternal(vec![
                        ExternalRequest::Sanitizer {
                            sanitizer: sanitizer.as_str().to_string(),
                            source,
                            body,
                        },
                    ])));
                }
                SanitizerAnswer::NoAnswer => {
                    return Ok(no_answer(format!(
                        "[appa] sanitizer {} gave no answer; the offer stands and may be executed again",
                        sanitizer.as_str()
                    )));
                }
                SanitizerAnswer::Derived(derived) => OfferOutcome::Derived(Evidence::Sanitizer {
                    sanitizer,
                    source,
                    derived,
                }),
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
            FollowUp::Offer(OfferFollowUp::Staged(confined)) => Next::PresentToModel(self.confined_delivery(&confined)),
            FollowUp::Offer(OfferFollowUp::ReturnStaged(stage)) => {
                Next::PresentToModel(self.return_stage_delivery(&stage))
            }
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
        let mut approvals = Vec::new();
        let mut requests = Vec::new();
        for requirement in required {
            let name = requirement.authority.as_str().to_string();
            let review = AuthorityReview {
                tool: call.tool().clone(),
                trajectory_label: views.current_label(),
            };
            match authority_verdict(evidence, &name) {
                None => requests.push(ExternalRequest::Authority {
                    authority: name,
                    payload: authority_payload(
                        &requirement.authority,
                        self.engine.registry(),
                        call,
                        &requirement.covers,
                        views,
                    ),
                    review,
                }),
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

    fn confined_delivery(&self, confined: &Confined) -> Presentation {
        let offers: Vec<OfferId> = confined.offers.iter().map(|(offer, _)| offer_id(offer)).collect();
        let feedback = stage_feedback(
            "[appa] the cleaned result still narrows this session.",
            &confined.residual,
            &offers,
            self.engine.registry().trust_chain(),
        );
        Presentation::Blocked { feedback, offers }
    }

    fn return_stage_delivery(&self, stage: &PendingReturnStage) -> Presentation {
        let offers: Vec<OfferId> = stage.offers.iter().map(|(offer, _)| offer_id(offer)).collect();
        let feedback = stage_feedback(
            "[appa] the child's return still narrows this session.",
            &stage.residual,
            &offers,
            self.engine.registry().trust_chain(),
        );
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
        let report = ChildReport {
            child: engine_id(child),
            fork,
            submission,
            evidence: [
                sanitizer_evidence(evidence),
                cast_evidence(self.engine.registry().trust_chain(), evidence),
            ]
            .concat(),
            offer_nonce: engine_nonce(entropy),
            expansions: expansions.expansions(),
        };
        let decision = match self.engine.handle(view, CoreEvent::ChildReturn(report)) {
            Ok(decision) => decision,
            Err(TransitionError::InadmissibleResolution) => {
                return Ok(EngineDecision::deliver(Next::PresentToModel(Presentation::Blocked {
                    feedback:
                        "[appa] the classifier's answer was not admissible; the return is withheld and may be retried"
                            .to_string(),
                    offers: Vec::new(),
                })));
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
            FollowUp::Child(ChildFollowUp::Pending(stage)) => Next::PresentToModel(self.return_stage_delivery(&stage)),
            FollowUp::Child(ChildFollowUp::Rejected { reason }) => Next::PresentToModel(Presentation::Blocked {
                feedback: format!("[appa] the child's return could not cross: {reason:?}"),
                offers: Vec::new(),
            }),
            FollowUp::Child(ChildFollowUp::Resolve(request)) => resolve_or_withhold(
                self.engine.registry().trust_chain(),
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

    fn resolve_dynamics(
        &self,
        call: &ProposedCall,
        evidence: &[ExternalEvidence],
    ) -> Result<Vec<PinnedDynamicResolution>, Resolution> {
        let tool = ToolName::new(call.tool.clone());
        let Some(contract) = self.engine.registry().tool(&tool) else {
            return Err(Resolution::Feedback(format!(
                "[appa] unknown tool {}: not in this deployment's policy",
                call.tool
            )));
        };
        let bindings = appa_engine::check::dynamic_reads(contract);
        if bindings.is_empty() {
            return Ok(Vec::new());
        }
        let Ok(resolved) = self.engine.resolve_call(tool, call.arguments.get().as_bytes()) else {
            return Ok(Vec::new());
        };
        let mut pins = Vec::new();
        let mut requests = Vec::new();
        for binding in &bindings {
            let Some(argument_value) = resolved.arguments().get(&binding.argument).and_then(|v| v.as_str()) else {
                return Err(Resolution::Feedback(unresolved_recipient(
                    &call.tool,
                    &binding.argument,
                )));
            };
            let answer = evidence.iter().find_map(|entry| match entry {
                ExternalEvidence::Dynamic {
                    resolver,
                    argument,
                    readers,
                } if *resolver == binding.resolver.as_str() && *argument == binding.argument => Some(readers.clone()),
                _ => None,
            });
            match answer {
                None => requests.push(ExternalRequest::Dynamic {
                    resolver: binding.resolver.as_str().to_string(),
                    tool: call.tool.clone(),
                    argument: binding.argument.clone(),
                    value: argument_value.to_string(),
                }),
                Some(None) => {
                    return Err(Resolution::Feedback(unresolved_recipient(
                        &call.tool,
                        &binding.argument,
                    )));
                }
                Some(Some(readers)) => {
                    let audience = Audience::restricted(readers.into_iter().map(ReaderId::new));
                    match PinnedDynamicResolution::from_answer(binding.clone(), audience) {
                        Some(pin) => pins.push(pin),
                        None => {
                            return Err(Resolution::Feedback(unresolved_recipient(
                                &call.tool,
                                &binding.argument,
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
            if named != resolver.as_str()
                || !registry.groups().contains(&group)
                || gathered.answers.contains_key(&group)
            {
                continue;
            }
            let expansion = readers
                .as_ref()
                .and_then(|readers| GroupExpansion::new(group.clone(), readers.iter().map(ReaderId::new)).ok());
            gathered.answers.insert(group, expansion);
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

    fn resolve_memberships(
        &self,
        call: &ProposedCall,
        evidence: &[ExternalEvidence],
    ) -> Result<Vec<PinnedMembership>, Resolution> {
        let tool = ToolName::new(call.tool.clone());
        let Some(contract) = self.engine.registry().tool(&tool) else {
            return Ok(Vec::new());
        };
        // Same fast path as the dynamic pass: no placeholder, nothing to read.
        if !contract.requires.label.audience.iter().any(|requirement| {
            matches!(
                requirement,
                appa_engine::contract::AudienceRequirement::Includes(
                    appa_engine::contract::RecipientSpec::Placeholder(_)
                )
            )
        }) {
            return Ok(Vec::new());
        }
        let Ok(resolved) = self.engine.resolve_call(tool, call.arguments.get().as_bytes()) else {
            return Ok(Vec::new());
        };
        let reads = appa_engine::check::group_reads(contract, &resolved);
        if reads.is_empty() {
            return Ok(Vec::new());
        }
        let Some(resolver) = self.engine.registry().membership() else {
            let read = &reads[0];
            return Err(Resolution::Feedback(format!(
                "[appa] {}: argument {} names {}, but this deployment registers no membership resolver; the call was not checked",
                call.tool, read.argument, read.group
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
                        Err(_) => return Err(Resolution::Feedback(unresolved_group(&call.tool, &read.group))),
                    }
                }
                Some(None) => return Err(Resolution::Feedback(unresolved_group(&call.tool, &read.group))),
            }
        }
        if !requests.is_empty() {
            return Err(Resolution::Consult(requests));
        }
        Ok(pins)
    }
}

fn unresolved_recipient(tool: &str, argument: &str) -> String {
    format!(
        "[appa] {tool}: the recipients of {argument} could not be resolved; the call was not checked — propose it again later"
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

/// One engine dispatch id as the harness carries it (`T31`): the released call
/// quotes it so the harness can name the call it is reporting. Nothing reads it
/// back — an outcome is matched against the dispatches the log shows open.
pub(crate) fn dispatch_wire(dispatch: &EngineDispatchId) -> String {
    serde_json::to_string(dispatch).expect("an engine dispatch id serializes")
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
        dispatch: DispatchId(dispatch_wire(&release.dispatch)),
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

/// Whether the session has an answer for a cast the engine asked about. The three states
/// are what stops a redrive loop: an answer the policy cannot read is `NoAnswer`, never a
/// fresh request, so a classifier naming a rank outside the chain withholds once instead
/// of being asked forever.
enum CastAnswerState {
    Missing,
    NoAnswer,
    Resolved,
}

fn cast_label(chain: &TrustChain, verdict: &CastVerdict) -> Option<EstablishedLabel> {
    match &verdict.label {
        CastLabel::Declared(label) => Some(label.clone()),
        CastLabel::Classified(answer) => {
            let trust = chain.rank_of(&answer.trust)?;
            let audience = match &answer.audience {
                CastAudience::Public => Audience::Public,
                CastAudience::Readers(readers) => Audience::restricted(readers.iter().map(ReaderId::new)),
            };
            Some(EstablishedLabel::new(trust, audience))
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
            } => Some(Evidence::PendingCast {
                cast: appa_engine::names::CastName::new(verdict.cast.clone()),
                source: *source,
                resolved: cast_label(chain, verdict)?,
            }),
            ExternalEvidence::Cast {
                value,
                verdict: Some(verdict),
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
        } = entry
            && reported == source
        {
            return match verdict.as_ref().and_then(|verdict| cast_label(chain, verdict)) {
                Some(_) => CastAnswerState::Resolved,
                None => CastAnswerState::NoAnswer,
            };
        }
    }
    CastAnswerState::Missing
}

fn cast_state(chain: &TrustChain, evidence: &[ExternalEvidence], value: ValueId) -> CastAnswerState {
    for entry in evidence {
        if let ExternalEvidence::Cast {
            value: reported,
            verdict,
        } = entry
            && *reported == value
        {
            return match verdict.as_ref().and_then(|verdict| cast_label(chain, verdict)) {
                Some(_) => CastAnswerState::Resolved,
                None => CastAnswerState::NoAnswer,
            };
        }
    }
    CastAnswerState::Missing
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

fn resolve_or_withhold(
    chain: &TrustChain,
    request: EvidenceRequest,
    evidence: &[ExternalEvidence],
    withheld: &str,
) -> Result<Next, EngineRefusal> {
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
                return Ok(Next::PresentToModel(Presentation::Blocked {
                    feedback: withheld.to_string(),
                    offers: Vec::new(),
                }));
            }
            Ok(Next::ResolveExternal(vec![ExternalRequest::Sanitizer {
                sanitizer: sanitizer.as_str().to_string(),
                source,
                body,
            }]))
        }
        EvidenceRequest::PendingCast { casts, source, body } => match pending_cast_state(chain, evidence, &source) {
            CastAnswerState::Missing => Ok(Next::ResolveExternal(vec![ExternalRequest::PendingCast {
                casts,
                source,
                body,
            }])),
            CastAnswerState::NoAnswer | CastAnswerState::Resolved => Ok(Next::PresentToModel(Presentation::Blocked {
                feedback: withheld.to_string(),
                offers: Vec::new(),
            })),
        },
        EvidenceRequest::Cast { casts, value, body } => match cast_state(chain, evidence, value) {
            CastAnswerState::Missing => Ok(Next::ResolveExternal(vec![ExternalRequest::Cast {
                casts,
                value,
                body,
            }])),
            CastAnswerState::NoAnswer | CastAnswerState::Resolved => Ok(Next::PresentToModel(Presentation::Blocked {
                feedback: withheld.to_string(),
                offers: Vec::new(),
            })),
        },
    }
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

fn authority_payload(
    authority: &appa_engine::names::AuthorityName,
    registry: &appa_engine::registry::Registry,
    resolved: &ResolvedCall,
    gaps: &[appa_engine::check::Gap],
    views: &Views,
) -> serde_json::Value {
    let hint = registry
        .authority(authority)
        .and_then(|registered| registered.hint.as_ref())
        .map(|hint| hint.as_str());
    serde_json::json!({
        "authority": authority.as_str(),
        "hint": hint,
        "tool": resolved.tool().as_str(),
        "digest": hex(resolved.digest().bytes()),
        "trajectory_label": label_wire(&views.current_label(), registry.trust_chain()),
        "arguments": resolved.arguments(),
        "gaps": gaps.iter().map(gap_text).collect::<Vec<_>>(),
    })
}

fn as_label(fold: &PartialLabel) -> Label {
    let bound = fold.bound();
    Label::new(
        match fold.is_established(Dimension::Trust) {
            true => Dim::Known(bound.trust),
            false => Dim::Unknown,
        },
        match fold.is_established(Dimension::Audience) {
            true => Dim::Known(bound.audience.clone()),
            false => Dim::Unknown,
        },
    )
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

fn label_wire(
    label: &appa_engine::label::PartialLabel,
    chain: &appa_engine::registry::TrustChain,
) -> serde_json::Value {
    use appa_engine::label::{Audience, Dimension};

    let bound = label.bound();
    let unresolved = |dim| label.unresolved(dim).map(|value| value.index()).collect::<Vec<_>>();
    let trust = match chain.name_of(bound.trust) {
        Some(rank) => rank.to_string(),
        None => format!("rank {}, which this deployment does not name", bound.trust.rank()),
    };
    serde_json::json!({
        "trust": trust,
        "trust_rank": bound.trust.rank(),
        "audience": match &bound.audience {
            Audience::Public => serde_json::Value::String("public".to_string()),
            Audience::Restricted(readers) => readers.iter().map(|reader| reader.as_str()).collect(),
        },
        "unresolved_trust": unresolved(Dimension::Trust),
        "unresolved_audience": unresolved(Dimension::Audience),
    })
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

/// The tests' seam: a test enqueues the exact decision for each event; the
/// queue is the behavior. Session tests pin orchestration — commit ordering,
/// conflict replay, evidence loops — not engine policy, which the
/// real-engine tests pin against compiled policies.
#[cfg(test)]
pub struct TestSeam {
    queue: Mutex<std::collections::VecDeque<EngineDecision>>,
    seen: Mutex<Vec<EngineEvent>>,
}

#[cfg(test)]
impl TestSeam {
    pub fn new() -> TestSeam {
        TestSeam {
            queue: Mutex::new(std::collections::VecDeque::new()),
            seen: Mutex::new(Vec::new()),
        }
    }

    pub fn enqueue(&self, decision: EngineDecision) {
        self.queue
            .lock()
            .expect("the test queue mutex is never poisoned")
            .push_back(decision);
    }

    pub fn seen(&self) -> Vec<EngineEvent> {
        self.seen.lock().expect("the seen mutex is never poisoned").clone()
    }

    fn next(&self, event: EngineEvent) -> EngineDecision {
        self.seen.lock().expect("the seen mutex is never poisoned").push(event);
        self.queue
            .lock()
            .expect("the test queue mutex is never poisoned")
            .pop_front()
            .expect("a test enqueued a decision for every event it drives")
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ExternalEvidence, ExternalRequest, OfferId, ProposedCall, Resolution, RuntimeEngine, audience_wire,
        remedy_instruction, remedy_lines, terminal_safe,
    };
    use appa_engine::label::{Audience, ReaderId};
    use appa_engine::plan::{ExecutableRemedyPlan, PlanId, PlannedBlock, RemedyPlan, RemedyStep};
    use std::collections::BTreeSet;

    fn dynamic_engine() -> RuntimeEngine {
        let policy = appa_policy::Config::from_toml_str(
            r#"
                version = 1
                [[dynamic_resolver]]
                name = "directory"
                [[tool]]
                name = "send"
                parameters = { type = "object", properties = { to = { type = "string" } }, required = ["to"] }
                delta = { audience = { resolver = "directory", argument = "to" } }
            "#,
        )
        .expect("the fixture policy compiles");
        RuntimeEngine::new(policy.engine().clone())
    }

    fn send(arguments: serde_json::Value) -> ProposedCall {
        ProposedCall {
            tool: "send".to_string(),
            arguments: serde_json::value::RawValue::from_string(arguments.to_string())
                .expect("the fixture arguments serialize"),
        }
    }

    fn answered(readers: Option<Vec<String>>) -> ExternalEvidence {
        ExternalEvidence::Dynamic {
            resolver: "directory".to_string(),
            argument: "to".to_string(),
            readers,
        }
    }

    #[test]
    fn only_a_successful_dynamic_answer_becomes_a_pin() {
        let e = dynamic_engine();
        let call = send(serde_json::json!({ "to": "hr" }));

        match e.resolve_dynamics(&call, &[]) {
            Err(Resolution::Consult(requests)) => assert_eq!(
                requests,
                vec![ExternalRequest::Dynamic {
                    resolver: "directory".to_string(),
                    tool: "send".to_string(),
                    argument: "to".to_string(),
                    value: "hr".to_string(),
                }]
            ),
            _ => panic!("an unanswered binding consults its resolver"),
        }

        let unchecked = |evidence: &[ExternalEvidence], call: &ProposedCall| matches!(e.resolve_dynamics(call, evidence), Err(Resolution::Feedback(text)) if text.contains("send"));
        assert!(unchecked(&[answered(None)], &call), "no answer checks nothing");
        assert!(
            unchecked(&[answered(Some(vec!["@finance".to_string()]))], &call),
            "an answer naming no literal reader set is not evidence either"
        );
        assert!(
            e.resolve_dynamics(&send(serde_json::json!({ "to": 7 })), &[])
                .is_ok_and(|pins| pins.is_empty())
        );

        let pinned = |evidence: &[ExternalEvidence]| match e.resolve_dynamics(&call, evidence) {
            Ok(pins) => pins.iter().map(|pin| pin.audience().clone()).collect::<Vec<_>>(),
            Err(_) => panic!("a successful answer pins"),
        };
        assert_eq!(
            pinned(&[answered(Some(vec!["hr".to_string()]))]),
            vec![Audience::restricted([ReaderId::new("hr")])]
        );
        assert_eq!(
            pinned(&[answered(Some(Vec::new()))]),
            vec![Audience::restricted([])],
            "an empty reader set is a successful answer, not the absence of one"
        );
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
        assert_ne!(
            terminal_safe("tru\u{206A}sted"),
            "trusted",
            "the full Cf range replaces"
        );
    }

    #[test]
    fn the_label_wire_names_every_state_it_can_be_in() {
        use appa_engine::label::{Audience, EstablishedLabel, PartialLabel, ReaderId, Trust};
        use appa_engine::registry::TrustChain;
        use appa_engine::value::ValueId;

        let chain = TrustChain::new(vec!["suspicious".to_string(), "trusted".to_string()]);
        let wire = |label: &PartialLabel| super::label_wire(label, &chain);

        let open = PartialLabel::established(EstablishedLabel::new(Trust::new(1), Audience::Public));
        assert_eq!(
            wire(&open),
            serde_json::json!({
                "trust": "trusted",
                "trust_rank": 1,
                "audience": "public",
                "unresolved_trust": [],
                "unresolved_audience": [],
            }),
        );

        let restricted = PartialLabel::established(EstablishedLabel::new(
            Trust::new(0),
            Audience::restricted([ReaderId::new("private")]),
        ));
        assert_eq!(
            wire(&restricted)["audience"],
            serde_json::json!(["private"]),
            "the bound readers cross by name",
        );
        assert_eq!(wire(&restricted)["trust"], serde_json::json!("suspicious"));

        let nobody = PartialLabel::established(EstablishedLabel::new(Trust::new(0), Audience::restricted([])));
        assert_eq!(
            wire(&nobody)["audience"],
            serde_json::json!([]),
            "an empty reader set is not public",
        );

        let neutral = PartialLabel::established(EstablishedLabel::top());
        let rendered = wire(&neutral);
        assert_eq!(rendered["trust_rank"], serde_json::json!(255));
        assert!(
            !rendered["trust"].is_null(),
            "an unnamed rank still reports itself: {rendered}",
        );
        assert!(
            rendered["trust"].as_str().expect("trust is a string").contains("255"),
            "the rank travels when the chain cannot name it: {rendered}",
        );

        use appa_engine::label::{Dim, Label};
        let mut partial = PartialLabel::established(EstablishedLabel::new(Trust::new(1), Audience::Public));
        partial.fold_value(ValueId::new(7), &Label::new(Dim::Unknown, Dim::Unknown));
        let rendered = wire(&partial);
        assert_eq!(
            rendered["unresolved_trust"],
            serde_json::json!([7]),
            "the unresolved source crosses by its own id: {rendered}",
        );
        assert_eq!(rendered["unresolved_audience"], serde_json::json!([7]));
    }
}
