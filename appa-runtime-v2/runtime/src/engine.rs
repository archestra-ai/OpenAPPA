//! The engine boundary: the one module that speaks to `appa-engine`.

use std::collections::HashMap;
use std::sync::Mutex;

use appa_engine::admit::{AdmitError, ResultAdmission};
use appa_engine::branch::{BranchError, ReturnBlock, ReturnCheck, ReturnPlan, ReturnSubmission};
use appa_engine::check::{CheckOutcome, UnestablishedFact};
use appa_engine::contract::{
    AudienceRequirement, DynamicAudienceBinding, PinnedDynamicResolution, RecipientSpec, ToolContract,
};
use appa_engine::engine::{Engine, EngineError};
use appa_engine::execute::{AuthorityReview, PlanError, Ruling};
use appa_engine::fact::{
    BoundaryKind, CloseOutcome, EffectSet, Fact, FactBatch, ObservedResult, ReturnDerivation, ReturnPolicy, Revision,
};
use appa_engine::label::{Audience, Dim, Dimension, Label, PartialLabel, ReaderId, Trust};
use appa_engine::plan::{ExecutableRemedyPlan, PlannedBlock, RemedyPlan};
use appa_engine::projection::Views;
use appa_engine::registry::TrustChain;
use appa_engine::transition::EngineView as ValidatedView;
use appa_engine::value::{
    CanonicalDigest, DispatchId as EngineDispatchId, Provenance, RawResultDigest, ResolvedCall, ToolName, ValueBody,
    ValueId,
};

use crate::api::OutcomeBody;
pub(crate) use crate::api::{DispatchId, OfferId, ProposedCall, ToolOutcome, TrajectoryId};

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
        dispatch: EngineDispatchId,
    },
    Sanitizer {
        sanitizer: String,
        payload: serde_json::Value,
    },
    Dynamic {
        resolver: String,
        tool: String,
        argument: String,
        value: String,
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
        dispatch: EngineDispatchId,
    },
    Sanitizer {
        sanitizer: String,
        derived: Option<String>,
    },
    Dynamic {
        resolver: String,
        argument: String,
        readers: Option<Vec<String>>,
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
/// seam maps it onto the engine's composed operations.
#[derive(Debug, Clone)]
pub enum EngineEvent {
    ModelResponse {
        call: ProposedCall,
        evidence: Vec<ExternalEvidence>,
        entropy: OfferNonce,
    },
    SuccessObserved {
        call: ProposedCall,
        observed: ObservedResult,
    },
    ToolOutcome {
        call: ProposedCall,
        outcome: ToolOutcome,
        evidence: Vec<ExternalEvidence>,
    },
    ExecuteOffer {
        offer: OfferId,
        evidence: Vec<ExternalEvidence>,
    },
    ChildStart { child: TrajectoryId },
    ChildReturn {
        parent: TrajectoryId,
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

/// One offered remedy this process remembers between the deny that surfaced
/// it and the `execute_remedy_plan` call that names it. Never trusted at
/// execution: the plan re-derives live and matches by value.
#[derive(Debug, Clone, PartialEq)]
pub enum CachedOffer {
    Call {
        trajectory: TrajectoryId,
        call: ProposedCall,
        plan: ExecutableRemedyPlan,
    },
    ChildReturn {
        trajectory: TrajectoryId,
        child: TrajectoryId,
        raw: String,
        plan: ReturnPlan,
    },
}

/// Offer-cache mutations the session applies only after the decision's
/// transaction committed — a failed append must leave the offers standing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OfferMutations {
    pub stage: Vec<(OfferId, CachedOffer)>,
    pub retire: Vec<OfferId>,
}

/// One engine interaction's outcome: the batch to append against its basis
/// revision, the follow-up to deliver, the offer-cache mutations to
/// apply after the commit, and the child the delivery ends, if any.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineDecision {
    pub append: Option<FactBatch>,
    pub then: Next,
    pub offers: OfferMutations,
    /// Set when delivering this decision ends a child trajectory — a merge
    /// executed through the remedy path ends the child it crossed.
    pub ends_child: Option<TrajectoryId>,
}

impl EngineDecision {
    fn deliver(then: Next) -> EngineDecision {
        EngineDecision {
            append: None,
            then,
            offers: OfferMutations::default(),
            ends_child: None,
        }
    }

    fn append(batch: FactBatch, then: Next) -> EngineDecision {
        EngineDecision {
            append: Some(batch),
            then,
            offers: OfferMutations::default(),
            ends_child: None,
        }
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
    #[error("the child is already forked")]
    ChildAlreadyForked,
    #[error("the trajectory has ended")]
    Ended,
    #[error("the dispatch is no longer open")]
    DispatchClosed,
}

/// The engine's derived working picture of one family log, scoped to the
/// trajectory the event belongs to. Opaque and disposable: rebuilt
/// per event, never stored. The engine's own view is boxed because it carries
/// the whole validated projection.
#[derive(Debug)]
pub struct EngineView {
    view: Box<ValidatedView>,
    trajectory: TrajectoryId,
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

/// One decision the family log recorded, in log order (`docs/runtime.md`, the
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
    Forked { parent: String, seed: AuditLabel },
    Released {
        tool: String,
        label: AuditLabel,
        effects: Vec<String>,
    },
    EffectsCommitted { effects: Vec<String> },
    Closed { outcome: DispatchOutcome },
    Admitted { label: AuditLabel },
    Ruled { authority: String },
    Denied { authority: String },
    Narrowed { from: AuditLabel, to: AuditLabel },
    Cast { cast: String, resolved: AuditLabel },
    CastLapsed { cast: String, resolved: AuditLabel },
    SanitizerBound { sanitizer: String },
    Sanitized { sanitizer: String },
    ChildReturn {
        sanitizer: Option<String>,
        label: AuditLabel,
    },
    Merged,
    VoidReturn,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum DispatchOutcome {
    Ran {
        effects: Vec<String>,
    },
    Failed,
    Unknown,
}

/// A root's validated opening: the engine's opening batch plus
/// the exact-bytes key of the policy file the root opens under. The only
/// constructor derives the trajectory and the identity from the batch's one
/// `TrajectoryOpened` fact, so the durable opening record can never
/// disagree with the fact it stores beside.
#[derive(Debug, Clone)]
pub struct RootOpening {
    trajectory: TrajectoryId,
    policy_key: crate::config::PolicyFileKey,
    policy_identity: String,
    batch: Vec<u8>,
}

impl RootOpening {
    fn new(batch: &FactBatch, policy_key: crate::config::PolicyFileKey) -> Result<RootOpening, String> {
        if batch.basis.value() != 0 {
            return Err("the opening batch is not at revision zero".to_string());
        }
        let [fact] = batch.facts.as_slice() else {
            return Err("the opening batch does not hold exactly one fact".to_string());
        };
        let Fact::TrajectoryOpened {
            trajectory,
            policy_digest,
            ..
        } = fact
        else {
            return Err("the opening batch's fact is not a TrajectoryOpened".to_string());
        };
        Ok(RootOpening {
            trajectory: TrajectoryId(trajectory.as_str().to_string()),
            policy_key,
            policy_identity: hex(policy_digest.bytes()),
            batch: serde_json::to_vec(&batch.facts).expect("engine facts serialize"),
        })
    }

    pub fn trajectory(&self) -> &TrajectoryId {
        &self.trajectory
    }

    /// The store's row for this opening, rendered at the SQLite
    /// encoding boundary. The one way the validated opening reaches
    /// the store: no caller decomposes it into free strings first.
    pub fn into_write(self) -> crate::store::OpeningWrite {
        crate::store::OpeningWrite {
            policy_key: self.policy_key.as_str().to_string(),
            policy_identity: self.policy_identity,
            batch: self.batch,
        }
    }
}

/// The engine deciding one family's events: the resident engine when the
/// root's stored policy file is the current one, or an engine compiled
/// from the root's stored bytes. Per-event and disposable —
/// never a registry of resident engines.
#[expect(clippy::large_enum_variant)]
pub enum PolicyEngine<'a> {
    Resident(&'a RuntimeEngine),
    Stored(RuntimeEngine),
}

impl PolicyEngine<'_> {
    fn engine(&self) -> &RuntimeEngine {
        match self {
            PolicyEngine::Resident(engine) => engine,
            PolicyEngine::Stored(engine) => engine,
        }
    }

    /// The policy identity of the deciding engine, lowercase hex — what
    /// a root's opening record must name.
    pub fn identity_hex(&self) -> String {
        hex(self.engine().engine.identity().bytes())
    }
}

enum Decider {
    Real,
    #[cfg(test)]
    Test(TestSeam),
}

/// The one engine boundary the session drives: the resident
/// engine compiled at open, the process-wide offer cache shared by every
/// resolved engine, and the decider.
pub struct EngineSeam {
    resident: RuntimeEngine,
    offers: Mutex<OfferCache>,
    decider: Decider,
}

impl EngineSeam {
    pub fn real(resident: RuntimeEngine) -> EngineSeam {
        EngineSeam {
            resident,
            offers: Mutex::new(OfferCache::new()),
            decider: Decider::Real,
        }
    }

    /// The tests' seam: decisions come from the enqueued queue; the real
    /// compiled engine still opens trajectories and gates replays.
    #[cfg(test)]
    pub fn test(resident: RuntimeEngine, seam: TestSeam) -> EngineSeam {
        EngineSeam {
            resident,
            offers: Mutex::new(OfferCache::new()),
            decider: Decider::Test(seam),
        }
    }

    pub fn resident(&self) -> PolicyEngine<'_> {
        PolicyEngine::Resident(&self.resident)
    }

    /// The opening of a fresh root under the resident policy: the engine's
    /// opening batch bound to the current policy file's key.
    pub fn root_opening(&self, trajectory: &TrajectoryId, policy_key: &crate::config::PolicyFileKey) -> RootOpening {
        let batch = self.resident.engine.open_trajectory(&engine_id(trajectory));
        RootOpening::new(&batch, policy_key.clone())
            .expect("the engine's opening batch is one TrajectoryOpened at revision zero")
    }

    /// Decode one family log and refuse it before it is trusted,
    /// including the opening gate: the log's first record must be this
    /// family's opening under exactly the deciding engine's policy.
    pub fn rebuild_view(
        &self,
        policy: &PolicyEngine<'_>,
        log: &[Vec<u8>],
        family: &TrajectoryId,
        trajectory: &TrajectoryId,
    ) -> Result<EngineView, EngineRefusal> {
        policy.engine().rebuild_view(log, family, trajectory)
    }

    pub fn handle(
        &self,
        policy: &PolicyEngine<'_>,
        view: &EngineView,
        event: EngineEvent,
    ) -> Result<EngineDecision, EngineRefusal> {
        match &self.decider {
            Decider::Real => policy.engine().handle(view, event, &self.offers),
            #[cfg(test)]
            Decider::Test(seam) => Ok(seam.next(event)),
        }
    }

    /// The canonical bytes of one proposed call, for the byte-exact dispatch
    /// matching of provider-run tools. `None` when the call cannot canonicalize — an
    /// unknown tool or schema-invalid arguments never match a dispatched
    /// call, whose bytes the engine validated.
    pub fn canonical_bytes(&self, policy: &PolicyEngine<'_>, call: &ProposedCall) -> Option<Vec<u8>> {
        match &self.decider {
            Decider::Real => policy.engine().canonical_bytes(call),
            #[cfg(test)]
            Decider::Test(_) => serde_json::to_vec(call).ok(),
        }
    }

    #[cfg(test)]
    pub fn enqueue(&self, decision: EngineDecision) {
        match &self.decider {
            Decider::Test(seam) => seam.enqueue(decision),
            Decider::Real => panic!("only the test seam takes enqueued decisions"),
        }
    }

    #[cfg(test)]
    pub fn seen(&self) -> Vec<EngineEvent> {
        match &self.decider {
            Decider::Test(seam) => seam.seen(),
            Decider::Real => panic!("only the test seam records seen events"),
        }
    }

    pub fn apply_offers(&self, mutations: OfferMutations) {
        let mut cache = self.offers.lock().expect("the offer cache mutex is never poisoned");
        cache.apply(mutations);
    }

    /// Render one trajectory's current label from the rebuilt view, for the
    /// statusline. A projection read: no engine event, no fact, nothing
    /// gated.
    pub fn trajectory_status(&self, policy: &PolicyEngine<'_>, view: &EngineView) -> Option<TrajectoryStatus> {
        match &self.decider {
            Decider::Real => policy.engine().trajectory_status(view),
            #[cfg(test)]
            Decider::Test(_) => Some(TrajectoryStatus {
                trajectory: String::new(),
                trust: String::new(),
                audience: String::new(),
            }),
        }
    }

    /// Render the family's recorded decisions from its persisted log. Like
    /// [`EngineSeam::trajectory_status`], a projection read — and
    /// like the replay gates, it runs on the real compiled engine in both
    /// modes, because what it renders is the log itself.
    pub fn audit(
        &self,
        policy: &PolicyEngine<'_>,
        log: &[Vec<u8>],
        family: &TrajectoryId,
    ) -> Result<Option<Vec<AuditEntry>>, EngineRefusal> {
        policy.engine().audit(log, family)
    }
}

/// The real engine behind the seam: the immutable registry-backed decision
/// core. The deployment's child-return binding is the engine's own
/// validated state. Offer payloads live in the seam, shared by
/// every resolved engine.
pub struct RuntimeEngine {
    engine: Engine,
}

struct OfferCache {
    entries: HashMap<String, (u64, CachedOffer)>,
    staged: u64,
}

const OFFER_CACHE_CAP: usize = 1024;

impl OfferCache {
    fn new() -> OfferCache {
        OfferCache {
            entries: HashMap::new(),
            staged: 0,
        }
    }

    fn apply(&mut self, mutations: OfferMutations) {
        for id in &mutations.retire {
            self.entries.remove(&id.0);
        }
        for (id, offer) in mutations.stage {
            let seq = self.staged;
            self.staged += 1;
            self.entries.insert(id.0, (seq, offer));
        }
        while self.entries.len() > OFFER_CACHE_CAP {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, (seq, _))| *seq)
                .map(|(id, _)| id.clone())
                .expect("a non-empty cache has an oldest entry");
            self.entries.remove(&oldest);
        }
    }
}

impl RuntimeEngine {
    pub fn new(engine: Engine) -> RuntimeEngine {
        RuntimeEngine { engine }
    }

    fn rebuild_view(
        &self,
        log: &[Vec<u8>],
        family: &TrajectoryId,
        trajectory: &TrajectoryId,
    ) -> Result<EngineView, EngineRefusal> {
        let facts = decode_log(log)?;
        let view = self.validated(facts, family, Revision::new(log.len() as u64))?;
        Ok(EngineView {
            view: Box::new(view),
            trajectory: trajectory.clone(),
        })
    }

    fn validated(
        &self,
        facts: Vec<Fact>,
        family: &TrajectoryId,
        revision: Revision,
    ) -> Result<ValidatedView, EngineRefusal> {
        self.engine
            .verify_opening(&facts, &engine_id(family))
            .map_err(|error| EngineRefusal::OpeningMismatch {
                detail: error.to_string(),
            })?;
        self.engine
            .view(&engine_id(family), facts, revision)
            .map_err(|error| EngineRefusal::UntrustedLog {
                detail: error.to_string(),
            })
    }

    fn canonical_bytes(&self, call: &ProposedCall) -> Option<Vec<u8>> {
        let resolved = self
            .engine
            .resolve_call(ToolName::new(call.tool.clone()), call.arguments.get().as_bytes())
            .ok()?;
        Some(resolved.canonical_arguments().canonical_bytes().to_vec())
    }

    fn trajectory_status(&self, view: &EngineView) -> Option<TrajectoryStatus> {
        let EngineView { view, trajectory } = view;
        let current = view.views(&engine_id(trajectory)).current_label();
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

    fn audit(&self, log: &[Vec<u8>], family: &TrajectoryId) -> Result<Option<Vec<AuditEntry>>, EngineRefusal> {
        let facts = decode_log(log)?;
        // The validator takes the records; this read keeps its own copy of
        // them, which is why the audit — and only the audit — clones a log.
        self.validated(facts.clone(), family, Revision::new(log.len() as u64))?;
        let mut entries = Vec::new();
        for fact in &facts {
            let event = match self.audit_event(fact) {
                Some(Some(event)) => event,
                // A record the audit does not show.
                Some(None) => continue,
                // A bound this deployment cannot name.
                None => return Ok(None),
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
            | Fact::OutputCastAccepted { narrowing, .. }
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
            Fact::OutputCastLapsed { cast, resolved, .. } => AuditEvent::CastLapsed {
                cast: terminal_safe(cast.as_str()),
                resolved: self.render_label(&resolved.clone().into_label())?,
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
                BoundaryKind::Fork { parent, snapshot, .. } => AuditEvent::Forked {
                    parent: terminal_safe(parent.as_str()),
                    seed: self.render_label(&as_label(snapshot.seed()))?,
                },
                BoundaryKind::Merge { .. } => AuditEvent::Merged,
                BoundaryKind::VoidReturn => AuditEvent::VoidReturn,
                // A turn's end is the harness's punctuation, not a decision.
                BoundaryKind::TurnEnd => return Some(None),
            },
            Fact::TrajectoryOpened { .. }
            | Fact::ProposalBatchDecided { .. }
            | Fact::AssistantMessage { .. }
            | Fact::BlockFeedback { .. } => return Some(None),
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
        event: EngineEvent,
        offers: &Mutex<OfferCache>,
    ) -> Result<EngineDecision, EngineRefusal> {
        let EngineView { view, trajectory } = view;
        let own = engine_id(trajectory);
        match event {
            EngineEvent::ModelResponse {
                call,
                evidence,
                entropy,
            } => {
                let views = view.views(&own);
                self.model_response(&views, trajectory, &call, &evidence, entropy)
            }
            EngineEvent::SuccessObserved { call, observed } => {
                let views = view.views(&own);
                self.success_observed(&views, &call, observed)
            }
            EngineEvent::ToolOutcome {
                call,
                outcome,
                evidence,
            } => {
                let views = view.views(&own);
                self.tool_outcome(&views, &call, &outcome, &evidence)
            }
            EngineEvent::ExecuteOffer { offer, evidence } => self.execute_offer(view, &offer, &evidence, offers),
            EngineEvent::ChildStart { child } => {
                let views = view.views(&own);
                let child_id = engine_id(&child);
                let batch = self.engine.seed_child(&views, &child_id).map_err(|error| match error {
                    BranchError::AlreadyForked => EngineRefusal::ChildAlreadyForked,
                    BranchError::ParentEnded => EngineRefusal::Ended,
                    error => EngineRefusal::Invariant {
                        detail: format!("seeding child {}: {error}", child.0),
                    },
                })?;
                Ok(EngineDecision::append(batch, Next::Done))
            }
            EngineEvent::ChildReturn {
                parent,
                child,
                value,
                evidence,
                entropy,
            } => {
                let parent_id = engine_id(&parent);
                let views = view.views(&parent_id);
                self.child_return(&views, &parent, &child, value, &evidence, entropy)
            }
        }
    }

    fn model_response(
        &self,
        views: &Views,
        trajectory: &TrajectoryId,
        call: &ProposedCall,
        evidence: &[ExternalEvidence],
        entropy: OfferNonce,
    ) -> Result<EngineDecision, EngineRefusal> {
        let resolved = match self.resolve(call, evidence) {
            Ok(resolved) => resolved,
            Err(Resolution::Feedback(text)) => return Ok(deny(text)),
            Err(Resolution::Consult(requests)) => {
                return Ok(EngineDecision::deliver(Next::ResolveExternal(requests)));
            }
        };
        match self.engine.check(views, &resolved) {
            Ok(CheckOutcome::Allow) => {
                let dispatch = predicted_dispatch(views, &resolved);
                let batch = self
                    .engine
                    .open_dispatch(views, &resolved)
                    .map_err(|error| match error {
                        EngineError::BranchEnded => EngineRefusal::Ended,
                        error => EngineRefusal::Invariant {
                            detail: format!("open after allow: {error}"),
                        },
                    })?;
                Ok(EngineDecision::append(
                    batch,
                    Next::ModelResponse {
                        invocations: vec![released(&dispatch, &resolved)],
                        feedback: Vec::new(),
                    },
                ))
            }
            Ok(CheckOutcome::Block(raw)) => {
                let planned = self
                    .engine
                    .plan(views, &resolved, &raw)
                    .map_err(|error| EngineRefusal::Invariant {
                        detail: format!("planning a checked block: {error}"),
                    })?;
                let mut stage = Vec::new();
                let mut offer_ids = Vec::new();
                for (index, plan) in planned.plans.iter().filter_map(RemedyPlan::executable).enumerate() {
                    let id = OfferId(offer_name(&entropy, index));
                    stage.push((
                        id.clone(),
                        CachedOffer::Call {
                            trajectory: trajectory.clone(),
                            call: call.clone(),
                            plan: plan.clone(),
                        },
                    ));
                    offer_ids.push(id);
                }
                let text = block_feedback(views, &planned, &offer_ids, self.engine.registry().trust_chain());
                let mut decision = EngineDecision::deliver(Next::ModelResponse {
                    invocations: Vec::new(),
                    feedback: vec![Feedback {
                        text,
                        offers: offer_ids,
                    }],
                });
                decision.offers.stage = stage;
                Ok(decision)
            }
            Err(EngineError::UnknownTool(tool)) => Ok(deny(format!(
                "[appa] unknown tool {tool}: not in this deployment's policy"
            ))),
            Err(EngineError::ProviderRunTool(tool)) => Ok(deny(format!(
                "[appa] tool {tool} is provider-run: it executes inside the inference call and cannot be proposed as a tool call"
            ))),
            Err(EngineError::InvalidCall(error)) => Ok(deny(format!("[appa] invalid call: {error}"))),
            Err(EngineError::InvalidReturnSchema(error)) => Ok(deny(format!(
                "[appa] invalid call: return_schema does not compile to a canonical shape: {error}"
            ))),
            Err(EngineError::NotAllowed | EngineError::BranchEnded | EngineError::NotProviderRun(_)) => {
                Err(EngineRefusal::Invariant {
                    detail: "check returned a dispatch-path refusal".to_string(),
                })
            }
        }
    }

    fn success_observed(
        &self,
        views: &Views,
        call: &ProposedCall,
        observed: ObservedResult,
    ) -> Result<EngineDecision, EngineRefusal> {
        let (resolved, dispatch) = self.open_dispatch_for(views, call)?;
        if views.is_succeeded(&dispatch) {
            return Ok(EngineDecision::deliver(Next::Done));
        }
        let batch = self
            .engine
            .observe_success(views, &dispatch, &resolved, observed)
            .map_err(|error| EngineRefusal::Invariant {
                detail: format!("success checkpoint: {error}"),
            })?;
        Ok(EngineDecision::append(batch, Next::Done))
    }

    fn tool_outcome(
        &self,
        views: &Views,
        call: &ProposedCall,
        outcome: &ToolOutcome,
        evidence: &[ExternalEvidence],
    ) -> Result<EngineDecision, EngineRefusal> {
        let (resolved, dispatch) = self.open_dispatch_for(views, call)?;
        let (admission, presentation) = match outcome {
            ToolOutcome::Failure { .. } => (ResultAdmission::Failure, Presentation::KeepOutput),
            ToolOutcome::Indeterminate => (ResultAdmission::Indeterminate, Presentation::KeepOutput),
            ToolOutcome::Success {
                body: OutcomeBody::Unavailable,
            } => (
                ResultAdmission::SuccessNoValue,
                Presentation::ReplaceOutput {
                    placeholder: "[appa] the result was not carried; nothing was admitted".to_string(),
                },
            ),
            ToolOutcome::Success {
                body: OutcomeBody::Available(raw),
            } => match views.bound_sanitizer(&dispatch) {
                None => (
                    ResultAdmission::SuccessRaw {
                        body: ValueBody::new(raw.clone()),
                    },
                    Presentation::KeepOutput,
                ),
                Some(sanitizer) => {
                    let name = sanitizer.as_str().to_string();
                    let derived = evidence.iter().find_map(|entry| match entry {
                        ExternalEvidence::Sanitizer { sanitizer, derived } if *sanitizer == name => {
                            Some(derived.clone())
                        }
                        _ => None,
                    });
                    match derived {
                        None => {
                            return Ok(EngineDecision::deliver(Next::ResolveExternal(vec![
                                ExternalRequest::Sanitizer {
                                    sanitizer: name,
                                    payload: serde_json::json!({ "body": raw }),
                                },
                            ])));
                        }
                        Some(Some(derived)) => {
                            let placeholder = derived.clone();
                            (
                                ResultAdmission::SuccessSanitized {
                                    body: ValueBody::new(derived),
                                    sanitizer: sanitizer.clone(),
                                    raw_digest: RawResultDigest::of(raw.as_bytes()),
                                },
                                Presentation::ReplaceOutput { placeholder },
                            )
                        }
                        Some(None) => (
                            ResultAdmission::SuccessNoValue,
                            Presentation::ReplaceOutput {
                                placeholder: "[appa] the sanitizer gave no answer; the result is withheld".to_string(),
                            },
                        ),
                    }
                }
            },
        };
        match self.engine.admit_result(views, &dispatch, &resolved, admission) {
            Ok(batch) => Ok(EngineDecision::append(batch, Next::PresentToModel(presentation))),
            Err(
                error @ (AdmitError::UnknownTool(_)
                | AdmitError::DigestMismatch
                | AdmitError::ForeignDispatch
                | AdmitError::NotOpen
                | AdmitError::ObservationMismatch
                | AdmitError::SuccessContradicted),
            ) => Err(EngineRefusal::Invariant {
                detail: format!("result admission identity: {error}"),
            }),
            Err(error) => Ok(EngineDecision::admission_refused(format!(
                "[appa] the result was not admitted: {error}"
            ))),
        }
    }

    fn execute_offer(
        &self,
        view: &ValidatedView,
        offer: &OfferId,
        evidence: &[ExternalEvidence],
        offers: &Mutex<OfferCache>,
    ) -> Result<EngineDecision, EngineRefusal> {
        let cached = {
            let cache = offers.lock().expect("the offer cache mutex is never poisoned");
            cache.entries.get(&offer.0).map(|(_, cached)| cached.clone())
        };
        let Some(cached) = cached else {
            return Ok(retire_declined(
                offer,
                "[appa] this offer no longer stands; re-propose the call".to_string(),
            ));
        };
        match cached {
            CachedOffer::Call { trajectory, call, plan } => {
                let owner = engine_id(&trajectory);
                let views = view.views(&owner);
                self.execute_call_offer(&views, &trajectory, offer, &call, &plan, evidence, offers)
            }
            CachedOffer::ChildReturn {
                trajectory,
                child,
                raw,
                plan,
                ..
            } => {
                let owner = engine_id(&trajectory);
                let views = view.views(&owner);
                self.execute_return_offer(&views, offer, &child, &raw, &plan, evidence)
            }
        }
    }

    #[expect(clippy::too_many_arguments)]
    fn execute_call_offer(
        &self,
        views: &Views,
        trajectory: &TrajectoryId,
        offer: &OfferId,
        call: &ProposedCall,
        chosen: &ExecutableRemedyPlan,
        evidence: &[ExternalEvidence],
        offers: &Mutex<OfferCache>,
    ) -> Result<EngineDecision, EngineRefusal> {
        let resolved = match self.resolve(call, evidence) {
            Ok(resolved) => resolved,
            Err(Resolution::Feedback(text)) => return Ok(retire_declined(offer, text)),
            Err(Resolution::Consult(requests)) => {
                return Ok(EngineDecision::deliver(Next::ResolveExternal(requests)));
            }
        };
        let raw = match self.engine.check(views, &resolved) {
            Ok(CheckOutcome::Block(raw)) => raw,
            Ok(CheckOutcome::Allow) => {
                return Ok(retire_declined(
                    offer,
                    "[appa] the state changed and this offer no longer applies; re-propose the call".to_string(),
                ));
            }
            Err(error) => {
                return Ok(retire_declined(
                    offer,
                    format!("[appa] the offer's call is no longer valid: {error}"),
                ));
            }
        };
        if !raw.unestablished.is_empty() {
            return Ok(EngineDecision::deliver(Next::PresentToModel(Presentation::Declined {
                feedback: unestablished_feedback(views, &raw.unestablished),
            })));
        }
        let planned = self
            .engine
            .plan(views, &resolved, &raw)
            .map_err(|error| EngineRefusal::Invariant {
                detail: format!("re-planning a checked block: {error}"),
            })?;
        if !planned
            .plans
            .iter()
            .filter_map(RemedyPlan::executable)
            .any(|offered| offered == chosen)
        {
            return Ok(retire_declined(
                offer,
                "[appa] the state changed and this offer no longer applies; re-propose the call".to_string(),
            ));
        }
        let dispatch = predicted_dispatch(views, &resolved);
        let mut rulings = Vec::new();
        let mut requests = Vec::new();
        for requirement in &chosen.required {
            let name = requirement.authority.as_str().to_string();
            let answer = evidence.iter().find_map(|entry| match entry {
                ExternalEvidence::Authority {
                    authority,
                    verdict,
                    review,
                    dispatch: reviewed_dispatch,
                } if *authority == name && *reviewed_dispatch == dispatch => Some((*verdict, review.clone())),
                _ => None,
            });
            match answer {
                None => requests.push(ExternalRequest::Authority {
                    authority: name,
                    payload: authority_payload(
                        &requirement.authority,
                        self.engine.registry(),
                        &resolved,
                        &requirement.covers,
                        views,
                    ),
                    review: AuthorityReview {
                        tool: resolved.tool().clone(),
                        trajectory_label: views.current_label(),
                    },
                    dispatch: dispatch.clone(),
                }),
                Some((AuthorityVerdict::Approve, review)) => rulings.push(Ruling {
                    dispatch: dispatch.clone(),
                    authority: requirement.authority.clone(),
                    covers: requirement.covers.clone(),
                    reviewed: review,
                }),
                Some((AuthorityVerdict::Deny, _)) => {
                    let denier = requirement.authority.clone();
                    let digest = resolved.digest();
                    let batch = FactBatch::new(
                        views.revision(),
                        vec![Fact::Denial {
                            trajectory: views.trajectory().clone(),
                            digest,
                            authority: denier.clone(),
                        }],
                    );
                    let retire = self.offers_naming(trajectory, &denier, &digest, offers);
                    let mut decision = EngineDecision::append(
                        batch,
                        Next::PresentToModel(Presentation::Declined {
                            feedback: format!(
                                "[appa] authority {} denied this call; the offers naming it are withdrawn",
                                denier.as_str()
                            ),
                        }),
                    );
                    decision.offers.retire = retire;
                    return Ok(decision);
                }
                Some((AuthorityVerdict::Abstain, _)) => {
                    return Ok(EngineDecision::deliver(Next::PresentToModel(Presentation::NoAnswer {
                        feedback: format!(
                            "[appa] authority {name} gave no answer; the offer stands and may be executed again"
                        ),
                    })));
                }
            }
        }
        if !requests.is_empty() {
            return Ok(EngineDecision::deliver(Next::ResolveExternal(requests)));
        }
        match self.engine.execute_remedy_plan(views, chosen, &resolved, &rulings) {
            Ok(batch) => {
                let mut decision = EngineDecision::append(batch, Next::InvokeTool(released(&dispatch, &resolved)));
                decision.offers.retire = vec![offer.clone()];
                Ok(decision)
            }
            Err(PlanError::Unestablished(facts)) => {
                Ok(EngineDecision::deliver(Next::PresentToModel(Presentation::Declined {
                    feedback: unestablished_feedback(views, &facts),
                })))
            }
            Err(error) => Ok(retire_declined(
                offer,
                format!("[appa] the remedy plan could not be executed on the current state: {error}"),
            )),
        }
    }

    fn execute_return_offer(
        &self,
        parent_views: &Views,
        offer: &OfferId,
        child: &TrajectoryId,
        raw: &str,
        chosen: &ReturnPlan,
        evidence: &[ExternalEvidence],
    ) -> Result<EngineDecision, EngineRefusal> {
        let check = match self.engine.check_child_return(parent_views, &engine_id(child)) {
            Ok(check) => check,
            Err(BranchError::AlreadyEnded) => {
                return Ok(retire_declined(
                    offer,
                    "[appa] the child has already ended; this return offer no longer stands".to_string(),
                ));
            }
            Err(error) => {
                return Err(EngineRefusal::Invariant {
                    detail: format!("re-checking a child return: {error}"),
                });
            }
        };
        let plans = match &check {
            ReturnCheck::Block(block) => block.plans.clone(),
            ReturnCheck::Allow => Vec::new(),
        };
        if !plans.contains(chosen) {
            return Ok(retire_declined(
                offer,
                "[appa] the state changed and this return offer no longer applies".to_string(),
            ));
        }
        let submission = match chosen {
            ReturnPlan::Accept(_) => ReturnSubmission::Raw {
                body: ValueBody::new(raw.to_string()),
            },
            ReturnPlan::Sanitize { sanitizer, .. } => {
                let name = sanitizer.as_str().to_string();
                let derived = evidence.iter().find_map(|entry| match entry {
                    ExternalEvidence::Sanitizer { sanitizer, derived } if *sanitizer == name => Some(derived.clone()),
                    _ => None,
                });
                match derived {
                    None => {
                        return Ok(EngineDecision::deliver(Next::ResolveExternal(vec![
                            ExternalRequest::Sanitizer {
                                sanitizer: name,
                                payload: serde_json::json!({ "body": raw }),
                            },
                        ])));
                    }
                    Some(Some(derived)) => ReturnSubmission::Derived {
                        body: ValueBody::new(derived),
                        raw_digest: RawResultDigest::of(raw.as_bytes()),
                    },
                    Some(None) => {
                        return Ok(EngineDecision::deliver(Next::PresentToModel(Presentation::NoAnswer {
                            feedback: format!(
                                "[appa] sanitizer {name} gave no answer; the offer stands and may be executed again"
                            ),
                        })));
                    }
                }
            }
        };
        let value = match &submission {
            ReturnSubmission::Raw { body } | ReturnSubmission::Derived { body, .. } => body.as_str().to_string(),
        };
        match self
            .engine
            .execute_child_return_plan(parent_views, &engine_id(child), chosen.clone(), submission)
        {
            Ok(batch) => {
                let mut decision = EngineDecision::append(batch, Next::PresentToModel(Presentation::Value { value }));
                decision.offers.retire = vec![offer.clone()];
                decision.ends_child = Some(child.clone());
                Ok(decision)
            }
            Err(error) => Ok(retire_declined(
                offer,
                format!("[appa] the return plan could not be executed on the current state: {error}"),
            )),
        }
    }

    fn child_return(
        &self,
        parent_views: &Views,
        parent: &TrajectoryId,
        child: &TrajectoryId,
        value: Option<String>,
        evidence: &[ExternalEvidence],
        entropy: OfferNonce,
    ) -> Result<EngineDecision, EngineRefusal> {
        let child_engine = engine_id(child);
        let Some(value) = value else {
            let batch = self
                .engine
                .submit_void_return(parent_views, &child_engine)
                .map_err(|error| match error {
                    BranchError::AlreadyEnded => EngineRefusal::Ended,
                    error => EngineRefusal::Invariant {
                        detail: format!("void return: {error}"),
                    },
                })?;
            return Ok(EngineDecision::append(
                batch,
                Next::PresentToModel(Presentation::NoValue),
            ));
        };
        if let Some(ReturnPolicy::Sanitized(sanitizer)) = parent_views.return_policy_of(&child_engine) {
            let name = sanitizer.as_str().to_string();
            let derived = evidence.iter().find_map(|entry| match entry {
                ExternalEvidence::Sanitizer { sanitizer, derived } if *sanitizer == name => Some(derived.clone()),
                _ => None,
            });
            return match derived {
                None => Ok(EngineDecision::deliver(Next::ResolveExternal(vec![
                    ExternalRequest::Sanitizer {
                        sanitizer: name,
                        payload: serde_json::json!({ "body": value }),
                    },
                ]))),
                Some(None) => Ok(EngineDecision::deliver(Next::PresentToModel(Presentation::Blocked {
                    feedback: format!(
                        "[appa] sanitizer {name} gave no answer; the return is withheld and may be retried"
                    ),
                    offers: Vec::new(),
                }))),
                Some(Some(derived)) => {
                    match self.engine.submit_child_return(
                        parent_views,
                        &child_engine,
                        ReturnSubmission::Derived {
                            body: ValueBody::new(derived.clone()),
                            raw_digest: RawResultDigest::of(value.as_bytes()),
                        },
                    ) {
                        Ok(batch) => Ok(EngineDecision::append(
                            batch,
                            Next::PresentToModel(Presentation::Value { value: derived }),
                        )),
                        Err(BranchError::AlreadyEnded) => Err(EngineRefusal::Ended),
                        Err(error) => Ok(EngineDecision::deliver(Next::PresentToModel(Presentation::Blocked {
                            feedback: format!("[appa] the sanitized return may not cross: {error}"),
                            offers: Vec::new(),
                        }))),
                    }
                }
            };
        }
        let check = self
            .engine
            .check_child_return(parent_views, &child_engine)
            .map_err(|error| match error {
                BranchError::AlreadyEnded => EngineRefusal::Ended,
                error => EngineRefusal::Invariant {
                    detail: format!("checking a child return: {error}"),
                },
            })?;
        match check {
            ReturnCheck::Allow => {
                let batch = self
                    .engine
                    .submit_child_return(
                        parent_views,
                        &child_engine,
                        ReturnSubmission::Raw {
                            body: ValueBody::new(value.clone()),
                        },
                    )
                    .map_err(|error| EngineRefusal::Invariant {
                        detail: format!("submitting an allowed return: {error}"),
                    })?;
                Ok(EngineDecision::append(
                    batch,
                    Next::PresentToModel(Presentation::Value { value }),
                ))
            }
            ReturnCheck::Block(ReturnBlock { narrowing, plans }) => {
                let mut stage = Vec::new();
                let mut offer_ids = Vec::new();
                for (index, plan) in plans.iter().enumerate() {
                    let id = OfferId(offer_name(&entropy, index));
                    stage.push((
                        id.clone(),
                        CachedOffer::ChildReturn {
                            trajectory: parent.clone(),
                            child: child.clone(),
                            raw: value.clone(),
                            plan: plan.clone(),
                        },
                    ));
                    offer_ids.push(id);
                }
                let feedback = return_block_feedback(&narrowing, &plans, &offer_ids);
                let mut decision = EngineDecision::deliver(Next::PresentToModel(Presentation::Blocked {
                    feedback,
                    offers: offer_ids,
                }));
                decision.offers.stage = stage;
                Ok(decision)
            }
        }
    }

    fn resolve(&self, call: &ProposedCall, evidence: &[ExternalEvidence]) -> Result<ResolvedCall, Resolution> {
        let tool = ToolName::new(call.tool.clone());
        let Some(contract) = self.engine.registry().tool(&tool) else {
            return Err(Resolution::Feedback(format!(
                "[appa] unknown tool {}: not in this deployment's policy",
                call.tool
            )));
        };
        let resolved = self
            .engine
            .resolve_call(tool, call.arguments.get().as_bytes())
            .map_err(|error| Resolution::Feedback(format!("[appa] {error}")))?;
        let mut pins = Vec::new();
        let mut requests = Vec::new();
        for binding in dynamic_bindings(contract) {
            let Some(argument_value) = resolved.arguments().get(&binding.argument).and_then(|v| v.as_str()) else {
                continue;
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
                Some(readers) => {
                    let audience = readers.map(|readers| Audience::restricted(readers.into_iter().map(ReaderId::new)));
                    pins.push(PinnedDynamicResolution::from_answer(binding.clone(), audience));
                }
            }
        }
        if !requests.is_empty() {
            return Err(Resolution::Consult(requests));
        }
        Ok(resolved.with_dynamic_resolutions(pins))
    }

    fn open_dispatch_for(
        &self,
        views: &Views,
        call: &ProposedCall,
    ) -> Result<(ResolvedCall, EngineDispatchId), EngineRefusal> {
        let tool = ToolName::new(call.tool.clone());
        let resolved = self
            .engine
            .resolve_call(tool, call.arguments.get().as_bytes())
            .map_err(|_| EngineRefusal::Invariant {
                detail: "a matched outcome's call no longer canonicalizes".to_string(),
            })?;
        let digest = resolved.digest();
        let dispatch = (0..views.dispatch_count(&digest))
            .map(|occurrence| EngineDispatchId::new(views.trajectory().clone(), digest, occurrence))
            .find(|dispatch| views.is_open(dispatch))
            .ok_or(EngineRefusal::DispatchClosed)?;
        Ok((resolved, dispatch))
    }

    fn offers_naming(
        &self,
        trajectory: &TrajectoryId,
        denier: &appa_engine::names::AuthorityName,
        digest: &CanonicalDigest,
        offers: &Mutex<OfferCache>,
    ) -> Vec<OfferId> {
        let cache = offers.lock().expect("the offer cache mutex is never poisoned");
        cache
            .entries
            .iter()
            .filter(|(_, (_, cached))| match cached {
                CachedOffer::Call {
                    trajectory: owner,
                    call,
                    plan,
                } => {
                    owner == trajectory
                        && plan.names_authority(denier)
                        && self.digest_of(call).is_some_and(|d| d == *digest)
                }
                CachedOffer::ChildReturn { .. } => false,
            })
            .map(|(id, _)| OfferId(id.clone()))
            .collect()
    }

    fn digest_of(&self, call: &ProposedCall) -> Option<CanonicalDigest> {
        self.engine
            .resolve_call(ToolName::new(call.tool.clone()), call.arguments.get().as_bytes())
            .ok()
            .map(|call| call.digest())
    }
}

enum Resolution {
    Feedback(String),
    Consult(Vec<ExternalRequest>),
}

impl EngineDecision {
    fn admission_refused(feedback: String) -> EngineDecision {
        EngineDecision::deliver(Next::PresentToModel(Presentation::Blocked {
            feedback,
            offers: Vec::new(),
        }))
    }
}

fn deny(text: String) -> EngineDecision {
    EngineDecision::deliver(Next::ModelResponse {
        invocations: Vec::new(),
        feedback: vec![Feedback {
            text,
            offers: Vec::new(),
        }],
    })
}

fn retire_declined(offer: &OfferId, feedback: String) -> EngineDecision {
    let mut decision = EngineDecision::deliver(Next::PresentToModel(Presentation::Declined { feedback }));
    decision.offers.retire = vec![offer.clone()];
    decision
}

fn engine_id(id: &TrajectoryId) -> appa_engine::value::TrajectoryId {
    appa_engine::value::TrajectoryId::new(id.0.clone())
}

fn decode_log(log: &[Vec<u8>]) -> Result<Vec<Fact>, EngineRefusal> {
    let mut facts = Vec::new();
    for (seq, row) in log.iter().enumerate() {
        let batch: Vec<Fact> = serde_json::from_slice(row).map_err(|error| EngineRefusal::UntrustedLog {
            detail: format!("batch {seq} does not decode: {error}"),
        })?;
        facts.extend(batch);
    }
    Ok(facts)
}

fn predicted_dispatch(views: &Views, resolved: &ResolvedCall) -> EngineDispatchId {
    let digest = resolved.digest();
    EngineDispatchId::new(views.trajectory().clone(), digest, views.dispatch_count(&digest))
}

fn released(dispatch: &EngineDispatchId, resolved: &ResolvedCall) -> ReleasedCall {
    ReleasedCall {
        dispatch: DispatchId(render_dispatch(dispatch)),
        tool: resolved.tool().as_str().to_string(),
        bytes: resolved.canonical_arguments().canonical_bytes().to_vec(),
    }
}

fn render_dispatch(dispatch: &EngineDispatchId) -> String {
    format!(
        "{}:{}:{}",
        dispatch.trajectory().as_str(),
        hex(dispatch.digest().bytes()),
        dispatch.occurrence()
    )
}

fn offer_name(entropy: &OfferNonce, index: usize) -> String {
    format!("offer-{}-{index}", hex(&entropy.0[..16]))
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

/// The dynamic recipient bindings a contract declares, at
/// both declaration sites: `requires.audience.includes` and
/// `delta.audience`.
pub(crate) fn dynamic_bindings(contract: &ToolContract) -> impl Iterator<Item = &DynamicAudienceBinding> {
    let from_requires = contract.requires.label.audience.iter().filter_map(|req| match req {
        AudienceRequirement::Includes(RecipientSpec::Dynamic(binding)) => Some(binding),
        _ => None,
    });
    let from_delta = contract
        .delta
        .as_ref()
        .and_then(|delta| delta.audience.as_ref())
        .and_then(|audience| match audience {
            appa_engine::contract::AudienceDelta::Dynamic(binding) => Some(binding),
            _ => None,
        });
    from_requires.chain(from_delta)
}

fn gap_text(gap: &appa_engine::check::Gap) -> String {
    use appa_engine::check::Gap;
    match gap {
        Gap::TrustFloor { required, actual } => {
            format!("trust is {actual:?}, below the required floor {required:?}")
        }
        Gap::Includes { recipients } => format!("the readers do not include {recipients:?}"),
        Gap::UnresolvedDynamicRecipient { resolver, argument } => {
            format!(
                "recipient argument {argument} did not resolve via {}",
                resolver.as_str()
            )
        }
        Gap::Cap { cap } => format!("the committed readers exceed the cap {cap:?}"),
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
        Some(Provenance::UserInput) => Some("your prompt".to_string()),
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
            terminal_safe(&audience_wire(&narrowing.from.audience)),
            terminal_safe(&audience_wire(&narrowing.to.audience)),
        ));
    }
    changes
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

fn block_feedback(views: &Views, planned: &PlannedBlock, offers: &[OfferId], chain: &TrustChain) -> String {
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

    let mut remedies = Vec::new();
    let mut offer_iter = offers.iter();
    for plan in &planned.plans {
        match plan {
            RemedyPlan::Executable(plan) => {
                if let Some(id) = offer_iter.next() {
                    remedies.push(remedy_instruction(plan, id));
                }
            }
            RemedyPlan::Redispatch(redispatch) => {
                remedies.push(format!(
                    "  - Run {} first; it clears: {}.",
                    terminal_safe(redispatch.tool().as_str()),
                    terminal_safe(&redispatch.clears().iter().map(gap_text).collect::<Vec<_>>().join("; ")),
                ));
            }
        }
    }
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

fn return_block_feedback(
    narrowing: &appa_engine::check::Narrowing,
    plans: &[ReturnPlan],
    offers: &[OfferId],
) -> String {
    let mut lines = vec![format!(
        "[appa] the child's return is blocked: merging it would narrow the parent from {:?} to {:?}.",
        narrowing.from, narrowing.to
    )];
    for (plan, id) in plans.iter().zip(offers) {
        match plan {
            ReturnPlan::Accept(_) => lines.push(format!(
                "Option: call execute_remedy_plan with offer id {} to accept the narrowing and merge the raw return.",
                id.0
            )),
            ReturnPlan::Sanitize { sanitizer, .. } => lines.push(format!(
                "Option: call execute_remedy_plan with offer id {} to merge sanitizer {}'s derivation instead.",
                id.0,
                sanitizer.as_str()
            )),
        }
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
    use super::{audience_wire, terminal_safe};
    use appa_engine::label::{Audience, ReaderId};
    use std::collections::BTreeSet;

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

    #[test]
    fn only_the_api_module_calls_the_boundary() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        check_dir(&src, &src, &mut offenders);
        assert!(
            offenders.is_empty(),
            "engine-boundary references outside src/api and src/lib.rs: {offenders:?}",
        );
    }

    fn check_dir(root: &std::path::Path, dir: &std::path::Path, offenders: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).expect("crate source directory is readable") {
            let path = entry.expect("crate source entry is readable").path();
            if path.is_dir() {
                check_dir(root, &path, offenders);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .expect("entry sits under the crate source root")
                .to_string_lossy()
                .into_owned();
            if relative == "engine.rs" || relative == "lib.rs" || relative.starts_with("api/") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("crate source file is readable");
            if text.contains("crate::engine") || text.contains("super::engine") || text.contains("appa_engine") {
                offenders.push(relative);
            }
        }
    }
}
