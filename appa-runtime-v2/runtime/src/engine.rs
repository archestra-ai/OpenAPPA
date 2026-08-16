//! The engine boundary: the one module that speaks to `appa-engine`.

#[cfg(test)]
use std::sync::Mutex;

use appa_engine::candidate::DerivedVia;
use appa_engine::check::UnestablishedFact;
use appa_engine::contract::{
    AudienceRequirement, DynamicAudienceBinding, PinnedDynamicResolution, RecipientSpec, ToolContract,
};
use appa_engine::engine::{Engine, EngineError};
use appa_engine::execute::{AuthorityEvidence, AuthorityReview};
use appa_engine::fact::{BoundaryKind, CloseOutcome, EffectSet, Fact, FactBatch, ReturnDerivation, Revision};
use appa_engine::label::{Audience, Dim, Dimension, Label, PartialLabel, ReaderId, Trust};
use appa_engine::plan::{ExecutableRemedyPlan, PlannedBlock, RemedyPlan, RequiredRuling};
use appa_engine::projection::Views;
use appa_engine::registry::TrustChain;
use appa_engine::transition::Blocked as CoreBlocked;
use appa_engine::transition::{
    ChildFollowUp, ChildReport, ChildSubmission, Confined, EngineDecision as CoreDecision, EngineEvent as CoreEvent,
    EngineView as ValidatedView, Evidence, EvidenceRequest, FollowUp, ForkBinding, OfferConsult, OfferExecution,
    OfferFollowUp, OfferOutcome, OutcomeBody as CoreOutcomeBody, OutcomeFollowUp, PendingReturnStage, ProposalBatch,
    ProposalBatchId, ProposedCall as CoreProposedCall, Released, SpawnMark, ToolOutcome as CoreToolOutcome, ToolReport,
    TransitionError, ValidatedFactBatch,
};
use appa_engine::value::{
    DispatchId as EngineDispatchId, ForkId, OfferId as EngineOfferId, OfferNonce as EngineOfferNonce, Provenance,
    RawResultDigest, ResolvedCall, ToolName, ValueBody, ValueId,
};

use crate::api::OutcomeBody;
pub(crate) use crate::api::{DispatchId, OfferId, ProposedCall, SpawnBinding, ToolOutcome, TrajectoryId};

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
    BindFork { fork: ForkId, child: TrajectoryId },
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

/// One engine interaction's outcome, as the session drives it: the unsealed
/// batch to append against its basis revision, the follow-up to
/// deliver, and the child the delivery ends, if any.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineDecision {
    pub append: Option<FactBatch>,
    pub then: Next,
    /// Set when delivering this decision ends a child trajectory — a merge
    /// that crossed, a void return, a pending return whose custody transferred
    /// durably, or a return the mandatory sanitizer rejected.
    pub ends_child: Option<TrajectoryId>,
}

impl EngineDecision {
    fn deliver(then: Next) -> EngineDecision {
        EngineDecision {
            append: None,
            then,
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
    #[error("the trajectory has ended")]
    Ended,
    #[error("the dispatch is no longer open")]
    DispatchClosed,
    #[error("the offer is not one this family carries")]
    UnknownOffer,
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
        if batch.basis().value() != 0 {
            return Err("the opening batch is not at revision zero".to_string());
        }
        let [fact] = batch.facts() else {
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
            batch: serde_json::to_vec(batch.facts()).expect("engine facts serialize"),
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
/// engine compiled at open and the decider. The runtime holds no offer
/// state — offers are the engine's durable facts, routed by id.
pub struct EngineSeam {
    resident: RuntimeEngine,
    decider: Decider,
}

impl EngineSeam {
    pub fn real(resident: RuntimeEngine) -> EngineSeam {
        EngineSeam {
            resident,
            decider: Decider::Real,
        }
    }

    /// The tests' seam: decisions come from the enqueued queue; the real
    /// compiled engine still opens trajectories and gates replays.
    #[cfg(test)]
    pub fn test(resident: RuntimeEngine, seam: TestSeam) -> EngineSeam {
        EngineSeam {
            resident,
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
            Decider::Real => policy.engine().handle(view, event),
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
/// core. It owns every judgment and every fact; the runtime holds
/// no engine state, and offers are the engine's own durable facts.
pub struct RuntimeEngine {
    engine: Engine,
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

    fn handle(&self, view: &EngineView, event: EngineEvent) -> Result<EngineDecision, EngineRefusal> {
        let EngineView { view, trajectory } = view;
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
        view: &ValidatedView,
        trajectory: &TrajectoryId,
        call: &ProposedCall,
        evidence: &[ExternalEvidence],
        entropy: &OfferNonce,
        spawn: bool,
    ) -> Result<EngineDecision, EngineRefusal> {
        let pins = match self.resolve_dynamics(call, evidence) {
            Ok(pins) => pins,
            Err(Resolution::Feedback(text)) => return Ok(deny(text)),
            Err(Resolution::Consult(requests)) => return Ok(EngineDecision::deliver(Next::ResolveExternal(requests))),
        };
        let proposed = CoreProposedCall {
            tool: ToolName::new(call.tool.clone()),
            arguments: call.arguments.get().as_bytes().to_vec(),
            dynamic_resolutions: pins,
        };
        let decision = if spawn {
            match self.decide_proposal(view, trajectory, proposed.clone(), entropy, true) {
                Ok(decision) => decision,
                Err(TransitionError::SpawnUncontrolled) => self
                    .decide_proposal(view, trajectory, proposed, entropy, false)
                    .map_err(proposal_refusal)?,
                Err(error) => return Err(proposal_refusal(error)),
            }
        } else {
            self.decide_proposal(view, trajectory, proposed, entropy, false)
                .map_err(proposal_refusal)?
        };
        let append = decision.append.map(ValidatedFactBatch::into_unsealed);
        let then = self.deliver_proposals(view, trajectory, decision.follow_up)?;
        Ok(EngineDecision {
            append,
            then,
            ends_child: None,
        })
    }

    fn decide_proposal(
        &self,
        view: &ValidatedView,
        trajectory: &TrajectoryId,
        proposed: CoreProposedCall,
        entropy: &OfferNonce,
        spawn: bool,
    ) -> Result<CoreDecision, TransitionError> {
        let batch = ProposalBatch {
            id: batch_id(entropy),
            trajectory: engine_id(trajectory),
            provider_results: Vec::new(),
            proposals: vec![proposed],
            spawn: spawn.then(|| SpawnMark::at(0)),
            offer_nonce: engine_nonce(entropy),
        };
        self.engine.handle(view, CoreEvent::Proposals(batch))
    }

    fn deliver_proposals(
        &self,
        view: &ValidatedView,
        trajectory: &TrajectoryId,
        follow_up: FollowUp,
    ) -> Result<Next, EngineRefusal> {
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

    fn block_delivery(&self, view: &ValidatedView, trajectory: &TrajectoryId, block: &CoreBlocked) -> Feedback {
        let owner = engine_id(trajectory);
        let views = view.views(&owner);
        let offers: Vec<OfferId> = block.offers.iter().map(|(offer, _)| offer_id(offer)).collect();
        let text = block_feedback(&views, &block.block, &offers, self.engine.registry().trust_chain());
        Feedback { text, offers }
    }

    fn tool_outcome(
        &self,
        view: &ValidatedView,
        dispatch: &EngineDispatchId,
        outcome: &ToolOutcome,
        evidence: &[ExternalEvidence],
        entropy: &OfferNonce,
    ) -> Result<EngineDecision, EngineRefusal> {
        let report = ToolReport {
            dispatch: dispatch.clone(),
            outcome: engine_outcome(outcome),
            evidence: sanitizer_evidence(evidence),
            offer_nonce: engine_nonce(entropy),
        };
        let decision = self
            .engine
            .handle(view, CoreEvent::Outcome(report))
            .map_err(outcome_refusal)?;
        let append = decision.append.map(ValidatedFactBatch::into_unsealed);
        let then = match decision.follow_up {
            FollowUp::Outcome(OutcomeFollowUp::Closed { admitted }) => {
                Next::PresentToModel(outcome_presentation(outcome, admitted))
            }
            FollowUp::Outcome(OutcomeFollowUp::Resolve(request)) => resolve_or_withhold(
                request,
                evidence,
                "[appa] the sanitizer gave no answer; the result is withheld and may be retried",
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
        Ok(EngineDecision {
            append,
            then,
            ends_child: None,
        })
    }

    fn execute_offer(
        &self,
        view: &ValidatedView,
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
        let views = view.views(&owner);
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
        let execution = OfferExecution {
            trajectory: engine_id(trajectory),
            offer: engine_offer,
            outcome,
            offer_nonce: engine_nonce(entropy),
        };
        let decision = self
            .engine
            .handle(view, CoreEvent::ExecuteOffer(execution))
            .map_err(offer_refusal)?;
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
                feedback: "[appa] this call already ran; propose a fresh call".to_string(),
            }),
            other => {
                return Err(EngineRefusal::Invariant {
                    detail: format!("an offer produced a non-offer follow-up: {other:?}"),
                });
            }
        };
        Ok(EngineDecision {
            append,
            then,
            ends_child: None,
        })
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
        let offers: Vec<OfferId> = block.offers.iter().map(|(offer, _)| offer_id(offer)).collect();
        let feedback = block_feedback(views, &block.block, &offers, self.engine.registry().trust_chain());
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
        view: &ValidatedView,
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
                ends_child: None,
            }),
            other => Err(EngineRefusal::Invariant {
                detail: format!("a fork binding produced a non-fork follow-up: {other:?}"),
            }),
        }
    }

    fn child_return(
        &self,
        view: &ValidatedView,
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
        let report = ChildReport {
            child: engine_id(child),
            fork,
            submission,
            evidence: sanitizer_evidence(evidence),
            offer_nonce: engine_nonce(entropy),
        };
        let decision = self
            .engine
            .handle(view, CoreEvent::ChildReturn(report))
            .map_err(child_refusal)?;
        let append = decision.append.map(ValidatedFactBatch::into_unsealed);
        let (then, ended) = match decision.follow_up {
            FollowUp::Child(ChildFollowUp::Merged { admitted }) => (
                Next::PresentToModel(Presentation::Value {
                    value: admitted.as_str().to_string(),
                }),
                true,
            ),
            FollowUp::Child(ChildFollowUp::Ended) => (Next::PresentToModel(Presentation::NoValue), true),
            FollowUp::Child(ChildFollowUp::Pending(stage)) => {
                (Next::PresentToModel(self.return_stage_delivery(&stage)), true)
            }
            FollowUp::Child(ChildFollowUp::Rejected { reason }) => (
                Next::PresentToModel(Presentation::Blocked {
                    feedback: format!("[appa] the child's return could not cross: {reason:?}"),
                    offers: Vec::new(),
                }),
                true,
            ),
            FollowUp::Child(ChildFollowUp::Resolve(request)) => (
                resolve_or_withhold(
                    request,
                    evidence,
                    "[appa] the return sanitizer gave no answer; the return is withheld and may be retried",
                )?,
                false,
            ),
            other => {
                return Err(EngineRefusal::Invariant {
                    detail: format!("a child return produced an unexpected follow-up: {other:?}"),
                });
            }
        };
        Ok(EngineDecision {
            append,
            then,
            ends_child: ended.then(|| child.clone()),
        })
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
        let mut bindings = dynamic_bindings(contract).peekable();
        if bindings.peek().is_none() {
            return Ok(Vec::new());
        }
        let Ok(resolved) = self.engine.resolve_call(tool, call.arguments.get().as_bytes()) else {
            return Ok(Vec::new());
        };
        let mut pins = Vec::new();
        let mut requests = Vec::new();
        for binding in bindings {
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
        Ok(pins)
    }
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

pub(crate) fn engine_id(id: &TrajectoryId) -> appa_engine::value::TrajectoryId {
    appa_engine::value::TrajectoryId::new(id.0.clone())
}

fn engine_nonce(entropy: &OfferNonce) -> EngineOfferNonce {
    EngineOfferNonce::new(entropy.0)
}

fn batch_id(entropy: &OfferNonce) -> ProposalBatchId {
    ProposalBatchId::new(hex(&entropy.0))
}

/// One engine dispatch id as the runtime's durable row key (`T31`): serialized
/// structurally so the outcome recovers the exact engine id, never a re-derived
/// one.
pub(crate) fn dispatch_wire(dispatch: &EngineDispatchId) -> String {
    serde_json::to_string(dispatch).expect("an engine dispatch id serializes")
}

/// Recover the engine dispatch id one durable row key carries (`T31`). `None`
/// for a key this runtime did not write — never a guess.
pub(crate) fn parse_dispatch(key: &str) -> Option<EngineDispatchId> {
    serde_json::from_str(key).ok()
}

fn fork_binding(fork: &ForkId) -> SpawnBinding {
    SpawnBinding(serde_json::to_string(fork).expect("a fork id serializes"))
}

/// Recover the fork one spawn binding names. `None` for a binding
/// this runtime did not mint.
pub(crate) fn parse_fork(binding: &SpawnBinding) -> Option<ForkId> {
    serde_json::from_str(&binding.0).ok()
}

fn offer_id(offer: &EngineOfferId) -> OfferId {
    OfferId(offer.to_hex())
}

fn parse_offer(offer: &OfferId) -> Option<EngineOfferId> {
    EngineOfferId::from_hex(&offer.0).ok()
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
        EvidenceRequest::Cast { .. } | EvidenceRequest::PendingCast { .. } => Err(EngineRefusal::Invariant {
            detail: "the engine asked for a cast resolution this runtime does not drive".to_string(),
        }),
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
    EngineRefusal::Invariant {
        detail: format!("binding a fork: {error}"),
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
