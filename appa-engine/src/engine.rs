//! The engine: a pure function of the log's views and the immutable registry.

use thiserror::Error;

use crate::admit::{self, AdmitError, ResultAdmission};
use crate::audience::AudienceEvidence;
use crate::branch::{self, BranchError};
use crate::candidate::{CallStage, ConfinedFrom, DerivedCandidate, SanitizerLineage};
use crate::check::{self, CallRole, CheckOutcome, Narrowing, RawBlock};
use crate::contract::ToolAnnotation;
use crate::execute::{self, PlanError};
use crate::fact::{BoundaryKind, Fact, ObservedResult, ReturnDerivation, ReturnPolicy, ReturnSanitizer};
use crate::label::{Expansions, Label, MembershipContext, ReaderId, SymbolicAtom};
use crate::names::{AuthorityName, SanitizerName};
use crate::params::{ArgumentError, CanonicalArguments};
use crate::plan::{self, BlockedCall, PlannedBlock};
use crate::profile::{self, DeploymentName, DeploymentPolicy, DeploymentProfile, PolicyIdentity, PolicyRevision};
use crate::projection::Projection;
use crate::projection::Views;
use crate::registry::{LoadError, Registry, ToolKind};
use crate::transition::{
    Blocked, ChildFollowUp, ChildReport, ChildSubmission, Confined, EngineDecision, EngineEvent, EngineView, Evidence,
    EvidenceRequest, FollowUp, ForkBinding, OfferExecution, OfferFollowUp, OfferOutcome, OutcomeBody, OutcomeFollowUp,
    ProposalBatch, ProposalBatchId, Released, Sequence, Settled, SettledOutcome, SpawnMark, ToolOutcome, ToolReport,
    TransitionError, TransitionRefusal, ValidatedFactBatch,
};
use crate::value::{
    CanonicalDigest, ChildReturnId, DispatchId, ForkId, LabeledValue, Provenance, RawResultDigest, ResolvedCall,
    ToolName, TrajectoryId, ValueBody,
};

mod compose;
mod fork;
mod offers;
mod outcome;
mod proposals;

pub(crate) use compose::{ComposeRefusal, ComposingBatch, compose_batch, membership_context};

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum EngineError {
    #[error("no contract registered for tool {0}")]
    UnknownTool(String),
    #[error(
        "tool {0} is provider-run: it executes inside the inference call, so no executor of this deployment can run a proposed call naming it"
    )]
    ProviderRunTool(String),
    #[error("tool {0} is not provider-run: this deployment releases it, so no exposed result of it can be admitted")]
    NotProviderRun(String),
    #[error("invalid call: {0}")]
    InvalidCall(ArgumentError),
    #[error("invalid file basis evidence: {0}")]
    InvalidFileEvidence(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForkStatus {
    Unprepared,
    Prepared,
    Bound(TrajectoryId),
    /// A fan-out spawn's fork: open for its children to start members under, never bound itself.
    FanOut,
    Failed,
    ParentEnded,
}

/// The pure decision core, owning its static capability: the immutable registry (which carries
/// the validated deployment profile), the policy identity it derives, and where it decides.
///
/// An engine decides **at a revision** of a deployment ([`Engine::at`]), stamping every batch it
/// seals with that revision, so every family it opens follows the deployment and decides at its
/// head. An engine [`Engine::open`] compiled and no revision placed decides nothing: it opens no
/// family, replays no log and judges no view.
#[derive(Clone, Debug)]
pub struct Engine {
    registry: Registry,
    identity: PolicyIdentity,
    /// The deployment and revision this engine decides at; `None` until [`Engine::at`] places it.
    line: Option<Line>,
}

#[derive(Clone, Debug)]
struct Line {
    deployment: DeploymentName,
    revision: PolicyRevision,
}

/// One batch of a family log as the store holds it: the policy revision that decided it, where
/// its first record carries one, and its records. Only a batch holding no engine record carries
/// none.
#[derive(Clone, Copy, Debug)]
pub struct StampedBatch<'a> {
    pub revision: Option<PolicyRevision>,
    pub facts: &'a [Fact],
}

/// Why [`Engine::replay`] could not build a view of a family log.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ReplayRefusal {
    #[error(transparent)]
    Transition(#[from] TransitionRefusal),
    #[error("batch {position} is stamped at policy revision {batch}, earlier than revision {before} before it")]
    RevisionRegressed {
        position: u64,
        before: PolicyRevision,
        batch: PolicyRevision,
    },
    #[error("batch {position} is stamped at policy revision {revision}, and no engine of that revision was supplied")]
    PolicyUnavailable { position: u64, revision: PolicyRevision },
    #[error(
        "batch {position} is stamped at policy revision {batch}, later than revision {engine} the replay decides at"
    )]
    NewerRevision {
        position: u64,
        engine: PolicyRevision,
        batch: PolicyRevision,
    },
    #[error("batch {position} holds engine records and names no policy revision")]
    Unstamped { position: u64 },
}

/// Where a stage is opened: the act that opens it, what that act moves, the nonce its offer ids
/// derive from, and the subject its offers stand on.
#[derive(Clone, Copy)]
struct Opening<'a> {
    act: &'a crate::basis::DecidedAct,
    advance: &'a crate::basis::BasisAdvance,
    nonce: &'a crate::value::OfferNonce,
    subject: &'a crate::basis::SubjectKey,
}

/// What a re-checked call subject stands at now: blocked, or clear with the call it is about.
enum Recheck {
    Blocked(Box<Blocked>),
    Clear(Box<ResolvedCall>),
}

impl Engine {
    /// The one validated constructor: policy and declaration validate together in one
    /// load — the structural registry lints and provider-run split, the profile-exact planner-cap
    /// bound, and the pure policy × profile coverage matrix. No profile-blind path to
    /// a check or a plan exists. The engine decides once [`Engine::at`] places it at a revision of
    /// a deployment.
    pub fn open(policy: DeploymentPolicy) -> Result<Engine, LoadError> {
        let DeploymentPolicy {
            registry: config,
            planner_cap,
            profile: declaration,
        } = policy;
        let profile = DeploymentProfile::declare(declaration.clone())?;
        let registry = Registry::build(config, planner_cap, profile)?;
        profile::validate_coverage(&registry, &declaration)?;
        let identity = PolicyIdentity::of_registry(&registry);
        Ok(Engine {
            registry,
            identity,
            line: None,
        })
    }

    /// This engine deciding at `revision` of `deployment`. The policy is the same: a revision
    /// names where in the deployment's line the policy stands, and the runtime that compiled it
    /// knows that, the policy does not.
    pub fn at(self, deployment: DeploymentName, revision: PolicyRevision) -> Engine {
        Engine {
            line: Some(Line { deployment, revision }),
            ..self
        }
    }

    /// The policy revision this engine decides at, and stamps every batch it seals with; `None`
    /// before [`Engine::at`] places it.
    pub fn revision(&self) -> Option<PolicyRevision> {
        self.line.as_ref().map(|line| line.revision)
    }

    /// The deployment this engine decides for; `None` before [`Engine::at`] places it.
    pub fn deployment(&self) -> Option<&DeploymentName> {
        self.line.as_ref().map(|line| &line.deployment)
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    pub fn profile(&self) -> &DeploymentProfile {
        self.registry.profile()
    }

    pub fn identity(&self) -> PolicyIdentity {
        self.identity
    }

    /// Build the working view over a family log whose every batch this engine's revision decided,
    /// standing at `position` batches: every record passes the one transition validator before
    /// anything reads it, so no caller decides against an untrusted stream. A log whose batches
    /// carry other revisions is built by [`Engine::replay`].
    pub fn view(
        &self,
        family: &TrajectoryId,
        records: Vec<Fact>,
        position: u64,
    ) -> Result<EngineView, TransitionRefusal> {
        let mut sequence = Sequence::empty(self, family, position);
        for fact in &records {
            sequence.admit(fact)?;
        }
        Ok(EngineView::validated(sequence.finish()?, self, family.clone()))
    }

    /// Build the working view over a persisted family log, each batch judged under the engine
    /// that decided it: a batch stamped at this engine's revision by this engine, one stamped
    /// earlier by the engine of its revision in `earlier`. A batch that holds engine records and
    /// names no revision is refused. An earlier engine only validates: replay consults nothing,
    /// so it needs no externals. Where the stamp
    /// changes, the labels folded so far cross into the next revision's trust chain, and the view
    /// this returns stands in this engine's chain, ready for it to decide on. On cache loss the
    /// runtime rebuilds through this same call.
    pub fn replay(
        &self,
        family: &TrajectoryId,
        batches: &[StampedBatch<'_>],
        earlier: &[&Engine],
    ) -> Result<EngineView, ReplayRefusal> {
        self.replay_onto(family, None, batches, earlier)
    }

    /// [`Engine::replay`] resumed where `view` stands: `batches` are the batches its log accepted
    /// after `view`'s position, and the view this returns equals a replay of the whole log. A
    /// runtime holding a view reads only the batches after it this way.
    pub fn extend(
        &self,
        view: EngineView,
        batches: &[StampedBatch<'_>],
        earlier: &[&Engine],
    ) -> Result<EngineView, ReplayRefusal> {
        let family = view.family().clone();
        self.replay_onto(&family, Some(view), batches, earlier)
    }

    fn replay_onto(
        &self,
        family: &TrajectoryId,
        mut view: Option<EngineView>,
        batches: &[StampedBatch<'_>],
        earlier: &[&Engine],
    ) -> Result<EngineView, ReplayRefusal> {
        let offset = view.as_ref().map_or(0, EngineView::position);
        let Some(deciding) = self.revision() else {
            return Err(TransitionRefusal::NoRevision.into());
        };
        // A view an engine at a later revision crossed to stands where this engine never judged.
        if let Some(held) = view.as_ref().and_then(EngineView::stamp)
            && held > deciding
        {
            return Err(ReplayRefusal::NewerRevision {
                position: offset,
                engine: deciding,
                batch: held,
            });
        }
        let mut start = 0;
        while let Some(first) = batches.get(start) {
            let end = start
                + batches[start..]
                    .iter()
                    .take_while(|batch| batch.revision == first.revision)
                    .count();
            let position = offset + start as u64;
            let ended = offset + end as u64;
            let host_only = batches[start..end].iter().all(|batch| batch.facts.is_empty());
            let Some(revision) = first.revision else {
                // Batches that hold no engine record — host observations alone — decide nothing
                // and carry no stamp: no policy judges them, and the family's stamp stays where it
                // was. A batch that holds records and names no revision is refused.
                match view.as_mut() {
                    Some(held) if host_only => {
                        held.pass(None, ended);
                        start = end;
                        continue;
                    }
                    None if host_only => return Err(TransitionRefusal::Unopened.into()),
                    _ => return Err(ReplayRefusal::Unstamped { position }),
                }
            };
            if let Some(before) = view.as_ref().and_then(EngineView::stamp)
                && revision < before
            {
                return Err(ReplayRefusal::RevisionRegressed {
                    position,
                    before,
                    batch: revision,
                });
            }
            if revision > deciding {
                return Err(ReplayRefusal::NewerRevision {
                    position,
                    engine: deciding,
                    batch: revision,
                });
            }
            if host_only && let Some(held) = view.as_mut() {
                held.pass(Some(revision), ended);
                start = end;
                continue;
            }
            let engine = match revision == deciding {
                true => self,
                false => earlier
                    .iter()
                    .copied()
                    .find(|engine| engine.revision() == Some(revision))
                    .ok_or(ReplayRefusal::PolicyUnavailable { position, revision })?,
            };
            let mut sequence = match &view {
                None => Sequence::empty(engine, family, ended),
                Some(view) => Sequence::resuming(engine, &view.crossed_to(engine)),
            };
            for batch in &batches[start..end] {
                for fact in batch.facts {
                    sequence.admit(fact)?;
                }
            }
            let mut projection = sequence.finish()?;
            projection.set_position(ended);
            view = Some(EngineView::validated(projection, engine, family.clone()));
            start = end;
        }
        let view = view.ok_or(TransitionRefusal::Unopened)?;
        Ok(view.crossed_to(self).into_owned())
    }

    /// Advance `view` by one batch its log accepted after the position `view` stands at, decided
    /// by this engine. The batch's records pass the validator a sealed batch passes, resumed where
    /// `view` stands, and fold forward through [`EngineView::advance`]; a view advanced batch by
    /// batch equals the view [`Engine::replay`] builds from the whole log. A runtime holding a view
    /// reads only the batches after it this way; anything refused here it rebuilds through `replay`.
    pub fn admit_batch(
        &self,
        view: &mut EngineView,
        records: Vec<Fact>,
    ) -> Result<(), crate::transition::BatchRefusal> {
        match self.judges(view) {
            Ok(()) => {}
            Err(TransitionError::NewerRevision { view, engine }) => {
                return Err(crate::transition::ViewMismatch::RevisionRegressed { view, batch: engine }.into());
            }
            Err(_) => return Err(crate::transition::ViewMismatch::Unstamped.into()),
        }
        let batch = self.seal(view, records)?;
        view.advance(&batch)?;
        Ok(())
    }

    /// Whether this engine decides on `view`: it decides at a revision, and the view's last batch
    /// is not stamped later than it.
    fn judges(&self, view: &EngineView) -> Result<(), TransitionError> {
        let Some(engine) = self.revision() else {
            return Err(TransitionError::NoRevision);
        };
        match view.stamp() {
            Some(stamp) if stamp > engine => Err(TransitionError::NewerRevision { view: stamp, engine }),
            _ => Ok(()),
        }
    }

    /// `view` as this engine decides on it: in this engine's trust chain. A view stamped at a
    /// later revision than this engine's holds records this policy never judged, so no decision
    /// is made on it; nor does an engine at no revision decide.
    fn current<'v>(&self, view: &'v EngineView) -> Result<std::borrow::Cow<'v, EngineView>, TransitionError> {
        self.judges(view)?;
        Ok(view.crossed_to(self))
    }

    /// Freeze what an independent conversation-root fork of `trajectory` carries over from the
    /// family `view` holds, in this engine's trust chain: the opening of the new root this engine
    /// records. This opens no same-family child and creates no spawn or return contract. `None`
    /// for a trajectory this family never opened or one that has ended.
    pub fn root_fork_origin(
        &self,
        view: &EngineView,
        trajectory: &TrajectoryId,
    ) -> Option<crate::fact::RootForkOrigin> {
        view.crossed_to(self)
            .projection()
            .root_fork_origin(view.family(), trajectory)
    }

    /// The validator over a bare record stream, for tests that pin a refusal without holding a
    /// view. Production reaches it through [`Engine::view`].
    #[cfg(test)]
    pub(crate) fn validate_replay(&self, facts: &[Fact]) -> Result<(), TransitionRefusal> {
        let family = match facts.first() {
            Some(fact) => fact.trajectory().clone(),
            None => return Err(TransitionRefusal::Unopened),
        };
        self.view(&family, facts.to_vec(), facts.len() as u64).map(|_| ())
    }

    /// Seal a candidate batch: the facts an engine operation just built pass the same validator a
    /// persisted log does, resumed where `view` stands in this engine's trust chain, so the sealed
    /// batch is one no replay of it can refuse. The batch is stamped with this engine's revision.
    pub(crate) fn seal(&self, view: &EngineView, facts: Vec<Fact>) -> Result<ValidatedFactBatch, TransitionRefusal> {
        let mut sequence = Sequence::resuming(self, &view.crossed_to(self));
        for fact in &facts {
            sequence.admit(fact)?;
        }
        sequence.finish()?;
        Ok(ValidatedFactBatch::seal(
            facts,
            view.position(),
            self,
            view.family().clone(),
        ))
    }

    /// One act's facts, declared and sealed into the batch a decision appends. The declaration
    /// is derived from the same facts it is prepended to, so the two cannot disagree.
    fn decided(
        &self,
        view: &EngineView,
        act: crate::basis::DecidedAct,
        facts: Vec<Fact>,
    ) -> Result<ValidatedFactBatch, TransitionError> {
        let advance = Sequence::advance_of(self, view, &facts);
        let batch = self.declaring(view.family(), act, advance, facts);
        Ok(self.seal(view, batch)?)
    }

    fn declaring(
        &self,
        family: &TrajectoryId,
        act: crate::basis::DecidedAct,
        advance: crate::basis::BasisAdvance,
        facts: Vec<Fact>,
    ) -> Vec<Fact> {
        // Every act is declared: the declaration carries the revision that decided the batch,
        // which the store fences and a replay judges by. A declaration that moves something names
        // the trajectory its first record moves, and so does one over a record bound to the act
        // it lands under — an offer, an approval, a provider admission — or pinning audience
        // evidence: the declaration delimits the per-act audit bracket at replay. One declared
        // only to carry the revision names the family: its first record may open the trajectory
        // it is about, as a fork binding opens the child.
        let bound = facts.iter().any(|fact| {
            matches!(
                fact,
                Fact::OfferOpened { .. }
                    | Fact::CallApproved { .. }
                    | Fact::ValueAdmitted {
                        provenance: crate::value::Provenance::ProviderRun { .. },
                        ..
                    }
            ) || fact.audience_evidence().is_some_and(|evidence| !evidence.is_empty())
        });
        let trajectory = match facts.first() {
            Some(first) if !advance.is_empty() || bound => first.trajectory().clone(),
            _ => family.clone(),
        };
        let mut declared = vec![Fact::BasisAdvanced {
            trajectory,
            act,
            advance,
            revision: self.revision(),
        }];
        declared.extend(facts);
        declared
    }

    /// The engine's one mutation boundary: decide one event against the view and return
    /// a sealed batch plus the typed follow-up. The engine owns semantic validation and constructs
    /// every fact; it owns no mutable state.
    pub fn handle(&self, view: &EngineView, event: EngineEvent) -> Result<EngineDecision, TransitionError> {
        let view = &*self.current(view)?;
        let acting = match &event {
            EngineEvent::Proposals(batch) => Some(&batch.trajectory),
            EngineEvent::ExecuteOffer(execution) => Some(&execution.trajectory),
            EngineEvent::ChildReturn(report) => Some(&report.child),
            EngineEvent::Outcome(_) | EngineEvent::BindFork(_) => None,
        };
        if acting.is_some_and(|trajectory| !view.projection().is_opened(trajectory)) {
            return Err(TransitionError::UnopenedTrajectory);
        }
        let act = self.event_evidence(view, &event)?;
        let decision = match event {
            EngineEvent::Proposals(batch) => self.decide_proposals(view, &batch, &act),
            EngineEvent::Outcome(report) => self.decide_outcome(view, &report, &act),
            EngineEvent::ChildReturn(report) => self.decide_child_return(view, &report, &act),
            EngineEvent::BindFork(binding) => self.decide_binding(view, &binding),
            EngineEvent::ExecuteOffer(execution) => self.decide_offer(view, &execution, &act),
        }?;
        act.settle(self.registry.audience())?;
        Ok(decision)
    }

    /// The act's audience reading: the event's own pinned primitives over the ones the record
    /// it continues already consumed — an execution starts from its offer's pins, an outcome
    /// from its dispatch's.
    fn event_evidence(&self, view: &EngineView, event: &EngineEvent) -> Result<ActEvidence, TransitionError> {
        let projection = view.projection();
        let (merged, inherited) = match event {
            EngineEvent::Proposals(batch) => (batch.audience.clone(), AudienceEvidence::default()),
            EngineEvent::ExecuteOffer(execution) => {
                let views = projection.view(&execution.trajectory);
                match views.offer(&execution.offer) {
                    Some(offer) => (execution.audience.inheriting(&offer.evidence)?, offer.evidence.clone()),
                    None => (execution.audience.clone(), AudienceEvidence::default()),
                }
            }
            EngineEvent::Outcome(report) => {
                let views = projection.view(report.dispatch.trajectory());
                match views.dispatch_evidence(&report.dispatch) {
                    Some(pinned) => (report.audience.inheriting(pinned)?, pinned.clone()),
                    None => (report.audience.clone(), AudienceEvidence::default()),
                }
            }
            EngineEvent::ChildReturn(report) => (report.audience.clone(), AudienceEvidence::default()),
            EngineEvent::BindFork(_) => (AudienceEvidence::default(), AudienceEvidence::default()),
        };
        self.act_evidence(merged, inherited, projection.principal().cloned())
    }

    /// Validate one act's merged evidence and recompute the answers it carries. Junk or
    /// foreign evidence never enters a decision, whatever answers it would add.
    fn act_evidence(
        &self,
        evidence: AudienceEvidence,
        inherited: AudienceEvidence,
        principal: Option<ReaderId>,
    ) -> Result<ActEvidence, TransitionError> {
        let expansions = self.registry.audience().expansions(&evidence, principal.as_ref())?;
        Ok(ActEvidence::new(evidence, expansions, inherited, principal))
    }

    fn context<'e>(&'e self, act: &'e ActEvidence) -> MembershipContext<'e> {
        membership_context(&self.registry, act)
    }

    /// Every recovery route for a blocked call within `depth` (RMD-20), least mandate power
    /// first: advisory only. Nothing is appended, no offer is minted, and `remedy_plans` stand as
    /// surfaced; `answers` are the pinned primitives the caller can supply now, behind the ones
    /// the log already recorded for this subject. `RouteDepth::ONE` yields exactly the block's
    /// plans. An empty list asserts only that no route exists within this abstraction and `depth`.
    pub fn recovery_routes(
        &self,
        view: &EngineView,
        subject: &crate::basis::SubjectKey,
        answers: &AudienceEvidence,
        depth: crate::route::RouteDepth,
    ) -> Result<Vec<crate::route::RecoveryRoute>, crate::route::RouteError> {
        let view = &*self.current(view).map_err(|error| match error {
            TransitionError::NewerRevision { view, engine } => crate::route::RouteError::NewerRevision { view, engine },
            _ => crate::route::RouteError::NoRevision,
        })?;
        let crate::basis::SubjectKey::Call { trajectory, .. } = subject else {
            return Err(crate::route::RouteError::NotACallSubject);
        };
        let views = view.projection().view(trajectory);
        let context = crate::route::BlockContext::reconstruct(self, &views, subject, answers)?;
        crate::route::search(&self.registry, &views, &context, depth)
    }

    /// Publication label for a file mutation released by this validated dispatch.
    pub fn file_output_label(
        &self,
        view: &EngineView,
        dispatch: &DispatchId,
    ) -> Result<Option<crate::label::Label>, TransitionError> {
        let view = &*self.current(view)?;
        let views = view.projection().view(dispatch.trajectory());
        let call = views.dispatch_call(dispatch).ok_or(TransitionError::UnknownDispatch)?;
        let receiving = views
            .receiving_bound(dispatch)
            .ok_or(TransitionError::UnknownDispatch)?;
        let contract = self.outcome_contract(&views, dispatch, call)?;
        Ok(call.file_output_label_from(&contract.output_label(), receiving))
    }

    /// What the runtime must resolve before it can execute one live offer.
    pub fn offer_consults(
        &self,
        view: &EngineView,
        trajectory: &TrajectoryId,
        offer: &crate::value::OfferId,
    ) -> Result<crate::transition::OfferConsult, TransitionError> {
        use crate::transition::OfferConsult;
        let view = &*self.current(view)?;
        let views = view.projection().view(trajectory);
        let recorded = views.offer(offer).ok_or(TransitionError::UnknownOffer)?;
        if recorded.trajectory != *trajectory {
            return Err(TransitionError::OfferElsewhere);
        }
        if let Some(end) = &recorded.end {
            return Ok(OfferConsult::Replay(replay_outcome(recorded, end)));
        }
        if recorded.basis != views.basis_for(&recorded.subject) {
            return Ok(OfferConsult::Stale);
        }
        match &recorded.subject {
            crate::basis::SubjectKey::Call { .. } => match recorded.plan.hop() {
                Some(sanitizer) => Ok(OfferConsult::Rewrite {
                    sanitizer: sanitizer.clone(),
                    call: self.offer_call(&views, recorded),
                }),
                None if recorded.plan.required.is_empty() && recorded.plan.withholds_output() => {
                    Ok(OfferConsult::Withhold)
                }
                None if recorded.plan.required.is_empty() => Ok(OfferConsult::Accept {
                    sanitizer: recorded.plan.sanitizer().cloned(),
                }),
                None => Ok(OfferConsult::Authorities {
                    call: self.offer_call(&views, recorded),
                    required: recorded.plan.required.clone(),
                    narrowing: recorded.plan.narrowing().cloned(),
                }),
            },
            // No stage stands on a return: it crosses at the child's stop or is refused there.
            crate::basis::SubjectKey::Return(_) => Ok(OfferConsult::Stale),
            crate::basis::SubjectKey::ConfinedResult(dispatch) => match recorded.plan.hop() {
                Some(sanitizer) => {
                    let DerivedCandidate::Result { value, .. } =
                        views.candidate(&recorded.subject).ok_or(TransitionError::StaleOffer)?
                    else {
                        return Err(TransitionError::StaleOffer);
                    };
                    let body = value.body.clone();
                    let source = crate::value::RawResultDigest::of(body.as_str().as_bytes());
                    Ok(OfferConsult::Sanitizer {
                        sanitizer: sanitizer.clone(),
                        source,
                        body,
                        tool: views.dispatch_tool(dispatch).cloned(),
                    })
                }
                None => Ok(OfferConsult::Accept { sanitizer: None }),
            },
            crate::basis::SubjectKey::Approval(_) => Ok(OfferConsult::Stale),
        }
    }

    /// The record a decided proposal batch answers a repeat from, or `None` for an identity
    /// the log has not decided. Read-only: the repeat itself goes through `handle` with the
    /// same identity, which validates the repeated payload against this record before it
    /// answers.
    pub fn recorded_batch(
        &self,
        view: &EngineView,
        batch: &ProposalBatchId,
    ) -> Option<crate::transition::RecordedBatch> {
        let family = view.projection().view(view.family());
        let decided = family.decided_batch(batch)?;
        Some(crate::transition::RecordedBatch {
            trajectory: decided.trajectory.clone(),
            proposals: decided.proposals.clone(),
            spawn: decided.spawn,
            evidence: decided.evidence.clone(),
        })
    }

    /// The batch identity whose decision released this dispatch, if one did: how a repeat
    /// of the released call names the decision it is answered from.
    pub fn batch_of_dispatch(&self, view: &EngineView, dispatch: &DispatchId) -> Option<ProposalBatchId> {
        view.projection()
            .view(dispatch.trajectory())
            .batch_of_dispatch(dispatch)
            .cloned()
    }

    /// Does the block this batch's denial surfaced still stand at its recorded basis? Only
    /// while it does is a repeat of the denied call answered from the record; once the state
    /// has moved — a remedy executed, a sibling admitted — the repeat is a fresh proposal.
    pub fn denial_stands(&self, view: &EngineView, batch: &ProposalBatchId) -> bool {
        let family = view.projection().view(view.family());
        let Some(decided) = family.decided_batch(batch) else {
            return false;
        };
        let views = view.projection().view(&decided.trajectory);
        let composing = ComposingBatch {
            trajectory: &decided.trajectory,
            id: batch,
        };
        (0..decided.proposals.len()).any(|position| views.pending_block(&composing.subject(position)).is_some())
    }

    /// Would `policy` stand as this trajectory's return declaration now? The checks a declaration
    /// at the spawn passes, at the trajectory's current label. A runtime holds a declaration made
    /// ahead of the spawn by them and applies it through the spawn's own declaration offer.
    pub fn check_return_declaration(
        &self,
        view: &EngineView,
        trajectory: &TrajectoryId,
        policy: &ReturnPolicy,
    ) -> Result<(), crate::plan::ReturnPolicyRefusal> {
        let views = view.crossed_to(self);
        match views.views(trajectory) {
            Some(views) => plan::declared_return_policy(&self.registry, &views, &policy.route(), policy),
            None => Err(crate::plan::ReturnPolicyRefusal::RouteMismatch),
        }
    }

    /// The fork one child was bound to, or `None` for a trajectory that never forked.
    pub fn fork_of(&self, view: &EngineView, child: &TrajectoryId) -> Option<crate::value::ForkId> {
        view.views(child)?.fork_of(child).cloned()
    }

    /// The return policy the child's fork recorded — what its parent declared at the spawn —
    /// or `None` for a trajectory that never forked.
    pub fn return_policy_of(&self, view: &EngineView, child: &TrajectoryId) -> Option<ReturnPolicy> {
        view.crossed_to(self).views(child)?.return_policy_of(child).cloned()
    }

    /// The lowest trust rank a return declaration made on this branch may set: its own floor's
    /// trust, or the chain's bottom for a root or under a fork whose sanitizer raises trust.
    /// A declaration below it is refused as [`plan::ReturnPolicyRefusal::FloorBelowOwn`].
    pub fn lowest_return_trust(&self, views: &Views) -> crate::label::Trust {
        plan::floor_of(&self.registry, views).map_or(crate::label::Trust::new(0), |floor| floor.lowest_trust())
    }

    /// The return policy the fork `parent` prepared carries, before a child is bound to it: what
    /// the runtime tells the child at its start, read from the view that binds it.
    pub fn prepared_return_policy(
        &self,
        view: &EngineView,
        parent: &TrajectoryId,
        fork: &crate::value::ForkId,
    ) -> Option<ReturnPolicy> {
        view.crossed_to(self)
            .views(parent)?
            .prepared_fork(fork)
            .map(|prepared| prepared.return_policy.clone())
    }

    /// The value `child` crossed most recently: a stop or a spawn result repeating it replays
    /// that crossing and appends nothing.
    pub fn latest_return_of(&self, view: &EngineView, child: &TrajectoryId) -> Option<ValueBody> {
        view.views(child)?
            .latest_return(child)
            .map(|returned| returned.body.clone())
    }

    /// The plan one recorded offer of `trajectory` carries, for the runtime to read what the
    /// offer's execution must supply.
    pub fn offer_plan(
        &self,
        view: &EngineView,
        trajectory: &TrajectoryId,
        offer: &crate::value::OfferId,
    ) -> Option<plan::ExecutableRemedyPlan> {
        view.views(trajectory)?
            .offer(offer)
            .map(|recorded| recorded.plan.clone())
    }

    /// Where one fork stands: never prepared, prepared and open for binding,
    /// bound to a child, or unbindable because its spawn failed or its parent ended before any
    /// child bound.
    pub fn fork_status(&self, view: &EngineView, fork: &ForkId) -> ForkStatus {
        let projection = view.projection();
        let Some(prepared) = projection.prepared_fork(fork) else {
            return ForkStatus::Unprepared;
        };
        if let Some(child) = projection.bound_child(fork) {
            return ForkStatus::Bound(child.clone());
        }
        let parent = projection.view(&prepared.parent);
        if parent.dispatch_failed(fork.dispatch()) {
            return ForkStatus::Failed;
        }
        if parent.has_ended(&prepared.parent) {
            return ForkStatus::ParentEnded;
        }
        match prepared.kind {
            crate::transition::SpawnKind::Single => ForkStatus::Prepared,
            crate::transition::SpawnKind::FanOut => ForkStatus::FanOut,
        }
    }

    /// Whether the family has nothing in flight or owed — no open dispatch, no unsettled
    /// reservation, no child still on an errand — so a host that clears its context may start
    /// an independent family without any of this one's values reaching it.
    pub fn is_quiescent(&self, view: &EngineView) -> bool {
        view.projection().is_quiescent()
    }

    /// The family's forks in flight: prepared, bound to no child yet, their
    /// spawn dispatch still open, and their parent still live — the spawns whose child the host
    /// may still name. A fork whose parent ended with the spawn dispatch open can
    /// never bind, so it is not in flight, and it does not stand in the way of a later
    /// spawn.
    pub fn forks_in_flight(&self, view: &EngineView) -> Vec<ForkId> {
        let projection = view.projection();
        projection
            .prepared_forks()
            .filter(|fork| {
                matches!(fork, ForkId::Spawn(_))
                    && projection
                        .prepared_fork(fork)
                        .is_some_and(|prepared| prepared.kind == crate::transition::SpawnKind::Single)
            })
            .filter(|fork| projection.bound_child(fork).is_none())
            .filter(|fork| {
                let parent = &projection
                    .prepared_fork(fork)
                    .expect("prepared_forks enumerates only prepared forks")
                    .parent;
                let views = projection.view(parent);
                views.is_open(fork.dispatch()) && !views.has_ended(parent)
            })
            .cloned()
            .collect()
    }

    /// The opening batch of a fresh root trajectory family: one `TrajectoryOpened`
    /// record against the empty log, naming the stored policy file the root opens under. The
    /// runtime appends it before any other family event.
    pub fn open_trajectory(
        &self,
        trajectory: &TrajectoryId,
        policy_file_key: crate::profile::PolicyFileKey,
        principal: Option<ReaderId>,
    ) -> Result<ValidatedFactBatch, TransitionRefusal> {
        self.opening(trajectory, policy_file_key, None, principal)
    }

    /// The opening batch of an independent conversation-root fork. `forked_from` came from
    /// [`Engine::root_fork_origin`] over the parent family's validated view; this creates no
    /// same-family child, spawn, or return contract.
    pub fn open_root_fork(
        &self,
        trajectory: &TrajectoryId,
        policy_file_key: crate::profile::PolicyFileKey,
        forked_from: crate::fact::RootForkOrigin,
    ) -> Result<ValidatedFactBatch, TransitionRefusal> {
        let principal = forked_from.principal.clone();
        self.opening(trajectory, policy_file_key, Some(forked_from), principal)
    }

    fn opening(
        &self,
        trajectory: &TrajectoryId,
        policy_file_key: crate::profile::PolicyFileKey,
        forked_from: Option<crate::fact::RootForkOrigin>,
        principal: Option<ReaderId>,
    ) -> Result<ValidatedFactBatch, TransitionRefusal> {
        let Some(line) = &self.line else {
            return Err(TransitionRefusal::NoRevision);
        };
        let empty = EngineView::validated(Projection::empty(0), self, trajectory.clone());
        self.seal(
            &empty,
            vec![Fact::TrajectoryOpened(crate::fact::TrajectoryOpening {
                trajectory: trajectory.clone(),
                profile: self.profile().clone(),
                policy_file_key,
                deployment: line.deployment.clone(),
                revision: line.revision,
                forked_from,
                principal,
            })],
        )
    }

    /// Convert untrusted provider bytes into the only call representation accepted by this
    /// engine. Tool lookup, strict JSON scanning, schema validation, and RFC 8785 rendering
    /// happen together, so outer runtimes cannot construct a call under a different schema.
    pub fn resolve_call(&self, tool: ToolName, raw_arguments: &[u8]) -> Result<ResolvedCall, EngineError> {
        if self.registry.classify(&tool) == Some(ToolKind::ProviderRun) {
            return Err(EngineError::ProviderRunTool(tool.as_str().to_string()));
        }
        select_call(&self.registry, tool, raw_arguments).map(|(call, _)| call)
    }

    /// Evaluate a proposed call: allow, or block carrying everything that stopped it at once —
    /// the requirement gaps, the narrowing where one fired, and the values whose consumed
    /// dimension no cast has established. Resolution is the runtime's job;
    /// the runtime re-checks after each landed cast, so a surfaced block is the residual.
    #[cfg(test)]
    pub(crate) fn check(&self, views: &Views, call: &ResolvedCall) -> Result<CheckOutcome, EngineError> {
        let contract = self.validated_contract(call)?;
        let audience = self.registry.audience();
        let empty = Expansions::default();
        let context = MembershipContext::new(audience.within_assertions(), audience.providers(), &empty);
        Ok(check::evaluate(
            &contract,
            views,
            call,
            &CallStage::default(),
            CallRole::Ordinary,
            &context,
        )
        .expect("engine test checks read no undecided symbolic audience"))
    }

    /// Attach the sound remedies to a raw block: executable plans and prose recommendations. An empty
    /// result (no plans, no curative recommendation) is a proof the block is unliftable over the
    /// implemented remedy subset — see [`crate::plan`].
    #[cfg(test)]
    pub(crate) fn plan(&self, views: &Views, call: &ResolvedCall, raw: &RawBlock) -> Result<PlannedBlock, EngineError> {
        let contract = self.validated_contract(call)?;
        let audience = self.registry.audience();
        let empty = Expansions::default();
        let context = MembershipContext::new(audience.within_assertions(), audience.providers(), &empty);
        Ok(plan::plan(
            &self.registry,
            views,
            BlockedCall {
                call,
                contract: &contract,
                raw,
                stage: &CallStage::default(),
                role: CallRole::Ordinary,
            },
            &context,
        )
        .expect("engine test plans read no undecided symbolic audience"))
    }

    /// Re-check the call a subject is about against current state, at the subject's stage and
    /// role. A block answers with its plans and the offers already pending on the subject, or
    /// none under the block id `nonce` derives.
    fn recheck(
        &self,
        views: &Views,
        subject: &crate::basis::SubjectKey,
        nonce: &crate::value::OfferNonce,
        call: ResolvedCall,
        act: &ActEvidence,
    ) -> Result<Recheck, TransitionError> {
        let contract = self.validated_contract(&call)?;
        let stage = views.call_stage(subject);
        let role = views.call_role(subject);
        let CheckOutcome::Block(raw) = check::evaluate(&contract, views, &call, &stage, role, &self.context(act))?
        else {
            return Ok(Recheck::Clear(Box::new(call)));
        };
        require_atoms(act, plan::block_atoms(&self.registry, &contract, &raw))?;
        let (block_id, offers) = views
            .pending_block(subject)
            .unwrap_or_else(|| (proposal_block(nonce, subject, &call), Vec::new()));
        let mut block = plan::plan(
            &self.registry,
            views,
            BlockedCall {
                call: &call,
                contract: &contract,
                raw: &raw,
                stage: &stage,
                role,
            },
            &self.context(act),
        )?;
        // A pending offer shows the plan it recorded: a planner that changed since it opened can
        // number another plan the same.
        if !offers.is_empty() {
            let recorded = offers
                .iter()
                .filter_map(|(offer, _)| views.offer(offer))
                .map(|open| plan::RemedyPlan::Executable(open.plan.clone()));
            block.plans = recorded
                .chain(block.plans.into_iter().filter(|plan| plan.executable().is_none()))
                .collect();
        }
        Ok(Recheck::Blocked(Box::new(Blocked {
            block,
            call,
            block_id,
            spawn: role.spawn(),
            offers,
        })))
    }

    /// The contract a dispatch's outcome is judged under: see [`admit::OutcomeContract`].
    fn dispatch_contract<'c>(
        &'c self,
        views: &'c Views<'c>,
        dispatch: &DispatchId,
    ) -> Result<admit::OutcomeContract<'c>, TransitionError> {
        let call = views.dispatch_call(dispatch).ok_or(TransitionError::UnknownDispatch)?;
        self.outcome_contract(views, dispatch, call)
    }

    /// The contract `call`'s outcome is judged under. Its arguments are not checked again: the
    /// release checked them, under the policy that released the call.
    fn outcome_contract<'c>(
        &'c self,
        views: &Views<'_>,
        dispatch: &DispatchId,
        call: &'c ResolvedCall,
    ) -> Result<admit::OutcomeContract<'c>, TransitionError> {
        if self.registry.provider_run_annotation(call.tool()).is_some() {
            return Err(TransitionError::Call(EngineError::ProviderRunTool(
                call.tool().as_str().to_string(),
            )));
        }
        admit::outcome_contract(&self.registry, call, views.release_terms(dispatch))
            .ok_or(TransitionError::UnknownDispatch)
    }

    fn validated_contract<'c>(
        &'c self,
        call: &'c ResolvedCall,
    ) -> Result<std::borrow::Cow<'c, ToolAnnotation>, EngineError> {
        if self.registry.provider_run_annotation(call.tool()).is_some() {
            return Err(EngineError::ProviderRunTool(call.tool().as_str().to_string()));
        }
        let contract = self
            .registry
            .annotation_of(call)
            .ok_or_else(|| EngineError::UnknownTool(call.tool().as_str().to_string()))?;
        contract
            .parameters
            .validate(call.arguments())
            .map_err(EngineError::InvalidCall)?;
        Ok(contract)
    }
}

fn crossed(facts: &[Fact]) -> ValueBody {
    facts
        .iter()
        .find_map(|fact| match fact {
            Fact::ValueAdmitted { value, .. } => Some(value.body.clone()),
            _ => None,
        })
        .expect("a candidate's admission carries the value that crossed")
}

/// Strict JSON scanning, ordered-contract selection on the parsed arguments, schema validation
/// against the selected contract, and RFC 8785 rendering, together: the one way a call is minted
/// from raw argument bytes, whether a provider proposed them or a sanitizer derived them.
fn select_call<'a>(
    registry: &'a Registry,
    tool: ToolName,
    raw_arguments: &[u8],
) -> Result<(ResolvedCall, &'a crate::contract::ToolDeclaration), EngineError> {
    let parsed = CanonicalArguments::parse(raw_arguments).map_err(EngineError::InvalidCall)?;
    let (_, declaration) = registry.select_tool(&tool, parsed.value()).ok_or_else(|| {
        // A name no declaration and no wildcard covers has no contract at all; a declared
        // name whose arguments select no ordered variant is a malformed call under it.
        match registry.classify(&tool) {
            None => EngineError::UnknownTool(tool.as_str().to_string()),
            Some(_) => EngineError::InvalidCall(ArgumentError::NoMatchingContract),
        }
    })?;
    declaration
        .parameters()
        .validate(parsed.value())
        .map_err(EngineError::InvalidCall)?;
    registry
        .placeholders_filled(declaration, parsed.value())
        .map_err(EngineError::InvalidCall)?;
    Ok((ResolvedCall::new(tool, parsed), declaration))
}

/// The call a sanitizer's replacement renders, under the ordered declaration its arguments select.
///
/// Annotation evidence binds the exact canonical call, so a rewrite of an Annotator-declared
/// tool carries the fresh annotation the runtime obtained for the rewritten call, whatever
/// declaration it selects. Audience evidence is operation-level, pinned on the record, so
/// nothing else rides along.
fn substituted_call(
    registry: &Registry,
    call: &ResolvedCall,
    body: &ValueBody,
    annotation: Option<&crate::contract::PinnedAnnotation>,
) -> Result<ResolvedCall, TransitionError> {
    let (rewritten, declaration) =
        select_call(registry, call.tool().clone(), body.as_str().as_bytes()).map_err(TransitionError::Call)?;
    let substituted = rewritten.with_annotation(annotation.cloned());
    match check::validate_annotation(registry, declaration, &substituted) {
        Ok(()) => {}
        Err(check::AnnotationRefusal::Needed(annotator)) => {
            return Err(TransitionError::AnnotationNeeded {
                annotators: vec![annotator],
            });
        }
        Err(_) => return Err(TransitionError::SanitizerUnapplicable),
    }
    Ok(substituted)
}

/// The view's projection with `facts` folded in, for a decision that reads its own records.
fn folded(view: &EngineView, facts: &[Fact]) -> Projection {
    let mut after = view.projection().clone();
    after.fold_all(facts);
    after
}

fn proposal_block(
    nonce: &crate::value::OfferNonce,
    subject: &crate::basis::SubjectKey,
    call: &ResolvedCall,
) -> crate::value::BlockId {
    let crate::basis::SubjectKey::Call {
        trajectory,
        batch,
        position,
    } = subject
    else {
        unreachable!("an opened offer's subject is a call candidate")
    };
    crate::value::BlockId::of_proposal(nonce, trajectory, batch, *position, &call.digest())
}

/// The derivation `evidence` carries from `sanitizer` over `source`, if any.
fn sanitizer_evidence(evidence: &[Evidence], sanitizer: &SanitizerName, source: RawResultDigest) -> Option<ValueBody> {
    evidence.iter().find_map(|item| match item {
        Evidence::Sanitizer {
            sanitizer: named,
            source: from,
            derived,
        } if named == sanitizer && from == &source => Some(derived.clone()),
        Evidence::Sanitizer { .. } | Evidence::Rewrite { .. } | Evidence::File { .. } => None,
    })
}

fn replay_outcome(recorded: &crate::projection::RecordedOffer, end: &crate::projection::OfferEnd) -> OfferOutcome {
    use crate::projection::OfferEnd;
    match end {
        OfferEnd::Denied(authority) => OfferOutcome::Denied {
            authority: authority.clone(),
        },
        OfferEnd::Accepted => match recorded.plan.hop() {
            Some(sanitizer) if matches!(recorded.subject, crate::basis::SubjectKey::Call { .. }) => {
                OfferOutcome::Derived(Evidence::Rewrite {
                    sanitizer: sanitizer.clone(),
                    source: RawResultDigest::of(&[]),
                    derived: ValueBody::new(""),
                    annotation: None,
                })
            }
            Some(sanitizer) if !sanitizer.is_attest_schema() => OfferOutcome::Derived(Evidence::Sanitizer {
                sanitizer: sanitizer.clone(),
                source: RawResultDigest::of(&[]),
                derived: ValueBody::new(""),
            }),
            _ => OfferOutcome::Approved(Vec::new()),
        },
        OfferEnd::Invalidated => OfferOutcome::Approved(Vec::new()),
    }
}

/// One act's audience reading: the membership answers its merged pinned evidence recomputes
/// to, over the act's operation-scope ledger. Built only through validation, so a context
/// over it always reads admissible answers. Interior mutability on the ledger: overlay
/// contexts built mid-decision discover pins and asks the act-building event could not name.
#[derive(Clone, Debug)]
pub(crate) struct ActEvidence {
    expansions: Expansions,
    ledger: std::cell::RefCell<crate::audience::ActLedger>,
    /// The family's session principal: it answers `self` beside the pinned evidence.
    principal: Option<ReaderId>,
}

impl ActEvidence {
    fn new(
        evidence: AudienceEvidence,
        expansions: Expansions,
        inherited: AudienceEvidence,
        principal: Option<ReaderId>,
    ) -> ActEvidence {
        ActEvidence {
            expansions,
            ledger: std::cell::RefCell::new(crate::audience::ActLedger::of(evidence, inherited)),
            principal,
        }
    }

    /// Assemble from parts a caller validated together: the transition validator recomputes
    /// `expansions` from `evidence` before building this. The validator keeps its own
    /// per-act ledger, so no inherited pins are carried here.
    pub(crate) fn validated(
        evidence: AudienceEvidence,
        expansions: Expansions,
        principal: Option<ReaderId>,
    ) -> ActEvidence {
        ActEvidence::new(evidence, expansions, AudienceEvidence::default(), principal)
    }

    /// The evidence a record of this act pins.
    pub(crate) fn pinned(&self) -> std::cell::Ref<'_, AudienceEvidence> {
        std::cell::Ref::map(self.ledger.borrow(), crate::audience::ActLedger::evidence)
    }

    /// The act's pinned answers merged with `fresh`, read without snapshotting them first.
    fn inheriting(&self, fresh: &AudienceEvidence) -> Result<AudienceEvidence, crate::audience::EvidenceRefusal> {
        self.ledger.borrow().evidence().inheriting(fresh)
    }

    /// The context's answers and ask log, for the validator's per-act ledger.
    pub(crate) fn expansions(&self) -> &Expansions {
        &self.expansions
    }

    /// Count `pins` — entries a record this act continues already pinned — as inherited, so
    /// the operation-scope test excuses them.
    fn inherit(&self, pins: &AudienceEvidence) -> Result<(), crate::audience::EvidenceRefusal> {
        self.ledger.borrow_mut().inherit(pins)
    }

    /// The overlay context over this act's answers and `pins`, entries a record this act
    /// continues already pinned: counted as inherited, merged, and validated.
    fn overlay(
        &self,
        pins: &AudienceEvidence,
        audience: &crate::audience::AudienceRegistry,
    ) -> Result<ActEvidence, crate::audience::EvidenceRefusal> {
        self.inherit(pins)?;
        let merged = self.inheriting(pins)?;
        let expansions = audience.expansions(&merged, self.principal.as_ref())?;
        Ok(ActEvidence::validated(merged, expansions, self.principal.clone()))
    }

    /// Count an overlay context's asks as this act's: it read on the act's behalf.
    fn absorb(&self, overlay: &ActEvidence) {
        self.ledger.borrow_mut().read(overlay.expansions.reads());
    }

    /// The operation-scope test, after the decision's reads are complete.
    fn settle(&self, audience: &crate::audience::AudienceRegistry) -> Result<(), crate::audience::EvidenceRefusal> {
        let mut ledger = self.ledger.borrow_mut();
        ledger.read(self.expansions.reads());
        ledger.settle(audience, self.principal.as_ref())
    }
}

/// The gate before a stage is planned: every atom planning may consult is answered, or the
/// missing ones come back as the runtime's ask. Planning under partial answers would silently
/// drop plans; the ask keeps the menu complete and the consultation deterministic.
fn require_atoms(act: &ActEvidence, atoms: impl IntoIterator<Item = SymbolicAtom>) -> Result<(), TransitionError> {
    let mut needed: Vec<SymbolicAtom> = atoms
        .into_iter()
        .filter(|atom| !act.expansions.answered(atom))
        .collect();
    if needed.is_empty() {
        return Ok(());
    }
    needed.sort();
    needed.dedup();
    Err(TransitionError::MembershipNeeded { needed })
}

#[cfg(test)]
mod tests;
