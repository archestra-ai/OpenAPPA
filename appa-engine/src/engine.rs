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
use crate::profile::{self, DeploymentPolicy, DeploymentProfile, OpenVector, PolicyDialectVersion, PolicyIdentity};
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
/// the validated deployment profile) and the policy identity the durable opening binds.
#[derive(Clone, Debug)]
pub struct Engine {
    registry: Registry,
    identity: PolicyIdentity,
    /// What earlier identity algorithms give this policy: the digest an opening an earlier engine
    /// recorded names it by.
    earlier_identities: Vec<PolicyIdentity>,
    dialect: PolicyDialectVersion,
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

impl Engine {
    /// The one validated constructor: policy and declaration validate together in one
    /// load — the structural registry lints and provider-run split, the profile-exact planner-cap
    /// bound, and the pure policy × profile coverage matrix. No profile-blind path to
    /// a check or a plan exists.
    pub fn open(policy: DeploymentPolicy) -> Result<Engine, LoadError> {
        let DeploymentPolicy {
            registry: config,
            planner_cap,
            dialect,
            profile: declaration,
        } = policy;
        let profile = DeploymentProfile::declare(declaration.clone())?;
        let registry = Registry::build(config, planner_cap, profile)?;
        profile::validate_coverage(&registry, &declaration)?;
        let identity = PolicyIdentity::of_registry(&registry);
        let earlier_identities = PolicyIdentity::earlier_of_registry(&registry);
        Ok(Engine {
            registry,
            identity,
            earlier_identities,
            dialect,
        })
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

    /// Whether a durable opening's `policy_digest` names this engine's policy: its identity, or
    /// the identity an earlier identity algorithm gives the same policy. Every opening this engine
    /// records carries [`Engine::identity`].
    pub fn recognizes(&self, digest: &PolicyIdentity) -> bool {
        *digest == self.identity || self.earlier_identities.contains(digest)
    }

    /// What earlier identity algorithms give this policy, newest first. Empty for a policy no
    /// earlier algorithm could express.
    pub fn earlier_identities(&self) -> &[PolicyIdentity] {
        &self.earlier_identities
    }

    /// The open vectors derived from the validated declaration and the registered tool set —
    /// recomputed, never stored, so they cannot drift from the profile.
    pub fn open_vectors(&self) -> Vec<OpenVector> {
        let tools = self
            .registry
            .tools()
            .map(|declaration| declaration.name())
            .chain(self.registry.provider_run_annotations().map(|tool| &tool.name));
        profile::derive_open_vectors(self.profile(), tools)
    }

    /// The policy dialect version this engine reads, which every opening it judges must carry.
    pub(crate) fn dialect(&self) -> PolicyDialectVersion {
        self.dialect
    }

    /// Build the working view over a persisted family log: every record passes the one
    /// transition validator before anything reads it, so no caller decides against an untrusted
    /// stream. On cache loss the runtime rebuilds through this same call.
    pub fn view(
        &self,
        family: &TrajectoryId,
        records: Vec<Fact>,
        revision: u64,
    ) -> Result<EngineView, TransitionRefusal> {
        let projection = self.replay(family, &records, revision)?;
        Ok(EngineView::validated(projection, self.identity, family.clone()))
    }

    /// Advance `view` by one batch its log accepted after the position `view` stands at. The
    /// batch's records pass the validator a sealed batch passes, resumed where `view` stands,
    /// and fold forward through [`EngineView::advance`]; a view advanced batch by batch equals
    /// the view [`Engine::view`] builds from the whole log. A runtime holding a view reads only
    /// the batches after it this way; anything refused here it rebuilds through `view`.
    pub fn admit_batch(
        &self,
        view: &mut EngineView,
        records: Vec<Fact>,
    ) -> Result<(), crate::transition::BatchRefusal> {
        if view.policy() != self.identity {
            return Err(crate::transition::ViewMismatch::ForeignPolicy.into());
        }
        let batch = self.seal(view, records)?;
        view.advance(&batch)?;
        Ok(())
    }

    /// The validator over a bare record stream, for tests that pin a refusal without holding a
    /// view. Production reaches it through [`Engine::view`].
    #[cfg(test)]
    pub(crate) fn validate_replay(&self, facts: &[Fact]) -> Result<(), TransitionRefusal> {
        let family = match facts.first() {
            Some(fact) => fact.trajectory().clone(),
            None => return Err(TransitionRefusal::Unopened),
        };
        self.replay(&family, facts, facts.len() as u64).map(|_| ())
    }

    fn replay(&self, family: &TrajectoryId, records: &[Fact], revision: u64) -> Result<Projection, TransitionRefusal> {
        let mut sequence = Sequence::empty(self, family, revision);
        for fact in records {
            sequence.admit(fact)?;
        }
        sequence.finish()
    }

    /// Seal a candidate batch: the facts an engine operation just built pass the same validator a
    /// persisted log does, so the sealed batch is one no replay of it can refuse.
    pub(crate) fn seal(&self, view: &EngineView, facts: Vec<Fact>) -> Result<ValidatedFactBatch, TransitionRefusal> {
        let mut sequence = Sequence::resuming(self, view);
        for fact in &facts {
            sequence.admit(fact)?;
        }
        sequence.finish()?;
        Ok(ValidatedFactBatch::seal(
            facts,
            view.revision(),
            self.identity,
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
        let batch = self.declaring(act, advance, facts);
        Ok(self.seal(view, batch)?)
    }

    fn declaring(
        &self,
        act: crate::basis::DecidedAct,
        advance: crate::basis::BasisAdvance,
        facts: Vec<Fact>,
    ) -> Vec<Fact> {
        // A record bound to the act it lands under — an offer, an approval, a provider
        // admission — needs the act declared over it even when nothing moves. So does a
        // record pinning audience evidence: the declaration delimits the per-act audit
        // bracket at replay, and evidence justified by a neighboring act's asks would
        // otherwise pass a full-log replay that the live seal refuses.
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
        if advance.is_empty() && !bound {
            return facts;
        }
        let trajectory = facts
            .first()
            .expect("a batch that advances a basis carries the record that advanced it")
            .trajectory()
            .clone();
        let mut declared = vec![Fact::BasisAdvanced {
            trajectory,
            act,
            advance,
        }];
        declared.extend(facts);
        declared
    }

    /// The engine's one mutation boundary: decide one event against the view and return
    /// a sealed batch plus the typed follow-up. The engine owns semantic validation and constructs
    /// every fact; it owns no mutable state.
    pub fn handle(&self, view: &EngineView, event: EngineEvent) -> Result<EngineDecision, TransitionError> {
        if view.policy() != self.identity {
            return Err(TransitionError::ForeignView);
        }
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
        if view.policy() != self.identity {
            return Err(crate::route::RouteError::ForeignView);
        }
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
        if view.policy() != self.identity {
            return Err(TransitionError::ForeignView);
        }
        let views = view.projection().view(dispatch.trajectory());
        let call = views.dispatch_call(dispatch).ok_or(TransitionError::UnknownDispatch)?;
        let receiving = views
            .receiving_bound(dispatch)
            .ok_or(TransitionError::UnknownDispatch)?;
        let contract = self.validated_contract(call)?;
        Ok(call.file_output_label(&contract, receiving))
    }

    /// What the runtime must resolve before it can execute one live offer.
    pub fn offer_consults(
        &self,
        view: &EngineView,
        trajectory: &TrajectoryId,
        offer: &crate::value::OfferId,
    ) -> Result<crate::transition::OfferConsult, TransitionError> {
        use crate::transition::OfferConsult;
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
        (0..decided.proposals.len() as u32).any(|position| {
            views
                .pending_block(&crate::basis::SubjectKey::Call {
                    trajectory: decided.trajectory.clone(),
                    batch: batch.clone(),
                    position,
                })
                .is_some()
        })
    }

    /// The fork one child was bound to, or `None` for a trajectory that never forked.
    pub fn fork_of(&self, view: &EngineView, child: &TrajectoryId) -> Option<crate::value::ForkId> {
        view.views(child)?.fork_of(child).cloned()
    }

    /// The return policy the child's fork recorded — what its parent declared at the spawn —
    /// or `None` for a trajectory that never forked.
    pub fn return_policy_of(&self, view: &EngineView, child: &TrajectoryId) -> Option<ReturnPolicy> {
        view.views(child)?.return_policy_of(child).cloned()
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
        view.views(parent)?.prepared_return_policy(fork).cloned()
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
            .filter(|fork| projection.bound_child(fork).is_none() && projection.is_dispatch_open(fork.dispatch()))
            .filter(|fork| {
                let parent = &projection
                    .prepared_fork(fork)
                    .expect("prepared_forks enumerates only prepared forks")
                    .parent;
                !projection.view(parent).has_ended(parent)
            })
            .cloned()
            .collect()
    }

    fn decide_binding(&self, view: &EngineView, binding: &ForkBinding) -> Result<EngineDecision, TransitionError> {
        let prepared = view
            .projection()
            .prepared_fork(&binding.fork)
            .ok_or(TransitionError::UnbindableFork)?;
        if let (crate::transition::SpawnKind::FanOut, ForkId::Spawn(spawn)) = (prepared.kind, &binding.fork) {
            return self.decide_member_binding(view, spawn, &binding.child);
        }
        let parent = prepared.parent.clone();
        let views = view.projection().view(&parent);
        if let Some(bound) = view.projection().bound_child(&binding.fork) {
            if bound != &binding.child {
                return Err(TransitionError::UnbindableFork);
            }
            // The same pair again is the parent addressing its child once more: the parent's
            // label as it stands now flows into the child, and a derivation the child never
            // echoed is dropped with the errand it belonged to.
            if views.has_ended(&binding.child) {
                return Err(TransitionError::BranchEnded);
            }
            let batch = vec![Fact::Boundary {
                trajectory: binding.child.clone(),
                kind: BoundaryKind::Resume {
                    seed: views.current_label(),
                },
            }];
            return Ok(EngineDecision {
                append: Some(self.decided(view, crate::basis::DecidedAct::Binding(binding.fork.clone()), batch)?),
                follow_up: FollowUp::Fork {
                    child: binding.child.clone(),
                },
            });
        }
        if view.projection().is_opened(&binding.child) {
            return Err(TransitionError::ChildAlreadyUsed);
        }
        // A recorded spawn failure makes the preparation unbindable: no child ran.
        if views.dispatch_failed(binding.fork.dispatch()) {
            return Err(TransitionError::UnbindableFork);
        }
        let batch = vec![Fact::ForkOpened {
            trajectory: binding.child.clone(),
            fork: binding.fork.clone(),
        }];
        Ok(EngineDecision {
            append: Some(self.decided(view, crate::basis::DecidedAct::Binding(binding.fork.clone()), batch)?),
            follow_up: FollowUp::Fork {
                child: binding.child.clone(),
            },
        })
    }

    /// A fan-out spawn's child starting: its own member fork, prepared now under the fan-out's
    /// return policy and seeded at the parent's label as it stands now, so a child started after
    /// a sibling's return crossed inherits what that return carried. A child already bound to its
    /// member is addressed again, as a single spawn's child is.
    fn decide_member_binding(
        &self,
        view: &EngineView,
        spawn: &DispatchId,
        child: &TrajectoryId,
    ) -> Result<EngineDecision, TransitionError> {
        let member = ForkId::member(spawn, child);
        if view.projection().prepared_fork(&member).is_some() {
            return self.decide_binding(
                view,
                &ForkBinding {
                    fork: member,
                    child: child.clone(),
                },
            );
        }
        let fan_out = view
            .projection()
            .prepared_fork(&ForkId::of(spawn))
            .expect("the caller found this fan-out fork prepared");
        let views = view.projection().view(&fan_out.parent);
        if view.projection().is_opened(child) {
            return Err(TransitionError::ChildAlreadyUsed);
        }
        if views.has_ended(&fan_out.parent) {
            return Err(TransitionError::BranchEnded);
        }
        if views.dispatch_failed(spawn) {
            return Err(TransitionError::UnbindableFork);
        }
        let batch = vec![
            Fact::ForkPrepared {
                trajectory: fan_out.parent.clone(),
                fork: member.clone(),
                snapshot: views.freeze_basis(),
                return_policy: fan_out.return_policy.clone(),
                kind: crate::transition::SpawnKind::Single,
            },
            Fact::ForkOpened {
                trajectory: child.clone(),
                fork: member.clone(),
            },
        ];
        Ok(EngineDecision {
            append: Some(self.decided(view, crate::basis::DecidedAct::Binding(member), batch)?),
            follow_up: FollowUp::Fork { child: child.clone() },
        })
    }

    /// One child's stop, judged against the policy its fork recorded. A void submission ends
    /// the branch. A value crosses at the child's fold, or through the fork's sanitizer: an
    /// attested body crosses in canonical form; a named sanitizer derives the submission once,
    /// stages the derivation, and crosses it when the child returns exactly it. A stop repeating
    /// the latest crossing is a replay and appends nothing. A return below the floor the parent
    /// set is refused, and the child keeps it.
    fn decide_child_return(
        &self,
        view: &EngineView,
        report: &ChildReport,
        act: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        let child = &report.child;
        let projection = view.projection();
        let parent = projection
            .view(child)
            .parent_of(child)
            .ok_or(TransitionError::NotForked)?
            .clone();
        let views = projection.view(&parent);
        // Every return addresses the exact fork that opened its child.
        if views.fork_of(child) != Some(&report.fork) {
            return Err(TransitionError::ReturnForkMismatch);
        }
        if views.has_ended(child) {
            return Err(TransitionError::BranchEnded);
        }
        let body = match &report.submission {
            ChildSubmission::Void => {
                let batch = branch::submit_void_return(&views, child).map_err(branch_refusal)?;
                return Ok(EngineDecision {
                    append: Some(self.decided(view, return_act(&views, child), batch)?),
                    follow_up: FollowUp::Child(ChildFollowUp::Ended),
                });
            }
            ChildSubmission::Value { body } => body.clone(),
        };
        let ReturnPolicy::Crossing(policy) = views.return_policy_of(child).ok_or(TransitionError::NotForked)?.clone()
        else {
            return Err(TransitionError::ReturnVoid);
        };
        let floor = plan::floor_of(&self.registry, &projection.view(child))
            .expect("a forked child stands under its fork's floor");
        let fold = views.branch_label(child);
        let replay = |admitted: &LabeledValue| EngineDecision {
            append: None,
            follow_up: FollowUp::Child(ChildFollowUp::Merged {
                admitted: admitted.body.clone(),
            }),
        };
        match &policy.sanitizer {
            None => {
                if let Some(latest) = views.latest_return(child).filter(|latest| latest.body == body) {
                    return Ok(replay(latest));
                }
                if !floor.holds(&fold) {
                    return Err(TransitionError::ReturnBelowFloor {
                        floor: floor.label().clone(),
                    });
                }
                let facts = branch::crossing_facts(
                    &views,
                    child,
                    LabeledValue::new(body.clone(), fold),
                    ReturnDerivation::Raw,
                    act.pinned().clone(),
                );
                Ok(EngineDecision {
                    append: Some(self.decided(view, return_act(&views, child), facts)?),
                    follow_up: FollowUp::Child(ChildFollowUp::Merged { admitted: body }),
                })
            }
            Some(ReturnSanitizer::Attest(shape)) => {
                let canonical = ValueBody::new(
                    shape
                        .validate(body.as_str())
                        .map_err(TransitionError::ReturnShapeMismatch)?,
                );
                if let Some(latest) = views.latest_return(child).filter(|latest| latest.body == canonical) {
                    return Ok(replay(latest));
                }
                let name = SanitizerName::new(SanitizerName::ATTEST_SCHEMA);
                let registered = self
                    .registry
                    .sanitizer(&name)
                    .ok_or(TransitionError::SanitizerUnapplicable)?;
                let ceiling =
                    plan::attest_ceiling(&registered.transition).ok_or(TransitionError::SanitizerUnapplicable)?;
                if !views.fork_seed(child).is_some_and(|seed| seed.meets_floor(ceiling)) {
                    return Err(TransitionError::SanitizerUnapplicable);
                }
                let label = registered
                    .derive_output(&fold, &[], &self.context(act))?
                    .ok_or(TransitionError::SanitizerUnapplicable)?;
                if !floor.holds(&label) {
                    return Err(TransitionError::ReturnBelowFloor {
                        floor: floor.label().clone(),
                    });
                }
                let facts = branch::crossing_facts(
                    &views,
                    child,
                    LabeledValue::new(canonical.clone(), label),
                    ReturnDerivation::Sanitized {
                        sanitizer: name,
                        raw_digest: RawResultDigest::of(body.as_str().as_bytes()),
                    },
                    act.pinned().clone(),
                );
                Ok(EngineDecision {
                    append: Some(self.decided(view, return_act(&views, child), facts)?),
                    follow_up: FollowUp::Child(ChildFollowUp::Merged { admitted: canonical }),
                })
            }
            Some(ReturnSanitizer::Named(name)) => {
                let registered = self
                    .registry
                    .sanitizer(name)
                    .ok_or(TransitionError::SanitizerUnapplicable)?;
                let id = ChildReturnId::new(child.clone(), views.returns_by(child));
                let subject = crate::basis::SubjectKey::Return(id);
                if let Some(DerivedCandidate::Return { source, value }) = views.candidate(&subject) {
                    // A derivation stands: it crosses when the child says exactly it, and the
                    // sanitizer never runs again for this errand.
                    if value.body != body {
                        return Ok(EngineDecision {
                            append: None,
                            follow_up: FollowUp::Child(ChildFollowUp::Staged {
                                derived: value.body.clone(),
                            }),
                        });
                    }
                    let facts = branch::crossing_facts(
                        &views,
                        child,
                        value.clone(),
                        ReturnDerivation::Sanitized {
                            sanitizer: name.clone(),
                            raw_digest: *source,
                        },
                        act.pinned().clone(),
                    );
                    return Ok(EngineDecision {
                        append: Some(self.decided(view, return_act(&views, child), facts)?),
                        follow_up: FollowUp::Child(ChildFollowUp::Merged { admitted: body }),
                    });
                }
                if let Some(latest) = views.latest_return(child).filter(|latest| latest.body == body) {
                    return Ok(replay(latest));
                }
                let digest = RawResultDigest::of(body.as_str().as_bytes());
                let derived = report.evidence.iter().find_map(|item| match item {
                    Evidence::Sanitizer {
                        sanitizer,
                        source,
                        derived,
                    } if sanitizer == name && source == &digest => Some(derived.clone()),
                    _ => None,
                });
                let Some(derived) = derived else {
                    return Ok(EngineDecision {
                        append: None,
                        follow_up: FollowUp::Child(ChildFollowUp::Resolve(EvidenceRequest::Sanitizer {
                            sanitizer: name.clone(),
                            source: digest,
                            body,
                        })),
                    });
                };
                // An undecided mandate is the runtime's ask, never a refusal.
                let label = registered
                    .derive_output(&fold, &[], &self.context(act))?
                    .ok_or(TransitionError::SanitizerUnapplicable)?;
                if !floor.holds(&label) {
                    return Err(TransitionError::ReturnBelowFloor {
                        floor: floor.label().clone(),
                    });
                }
                let value = LabeledValue::new(derived.clone(), label);
                let mut facts = vec![Fact::CandidateDerived {
                    trajectory: parent.clone(),
                    subject,
                    sanitizer: name.clone(),
                    derived: DerivedCandidate::Return {
                        source: digest,
                        value: value.clone(),
                    },
                    lineage: SanitizerLineage::default()
                        .extend(name.clone())
                        .expect("an empty lineage spends no sanitizer yet"),
                    evidence: act.pinned().clone(),
                }];
                // A derivation equal to the submission needs no echo: it crosses now.
                if derived == body {
                    facts.extend(branch::crossing_facts(
                        &views,
                        child,
                        value,
                        ReturnDerivation::Sanitized {
                            sanitizer: name.clone(),
                            raw_digest: digest,
                        },
                        act.pinned().clone(),
                    ));
                    return Ok(EngineDecision {
                        append: Some(self.decided(view, return_act(&views, child), facts)?),
                        follow_up: FollowUp::Child(ChildFollowUp::Merged { admitted: derived }),
                    });
                }
                Ok(EngineDecision {
                    append: Some(self.decided(view, return_act(&views, child), facts)?),
                    follow_up: FollowUp::Child(ChildFollowUp::Staged { derived }),
                })
            }
        }
    }

    fn decide_outcome(
        &self,
        view: &EngineView,
        report: &ToolReport,
        act: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        let dispatch = &report.dispatch;
        let views = view.projection().view(dispatch.trajectory());
        let call = views
            .dispatch_call(dispatch)
            .ok_or(TransitionError::UnknownDispatch)?
            .clone();
        if matches!(report.outcome, ToolOutcome::FailureWithBody { .. }) && call.file_basis().is_none() {
            return Err(TransitionError::FailureBodyOutsideFile);
        }
        let withholds_output = views.withholds_output(dispatch);
        let observed = match (&report.outcome, withholds_output) {
            (ToolOutcome::Success { .. }, true) => Some(ObservedResult::Unavailable),
            (
                ToolOutcome::Success {
                    body: OutcomeBody::Available(raw),
                },
                false,
            ) => Some(ObservedResult::Available(RawResultDigest::of(raw.as_str().as_bytes()))),
            (
                ToolOutcome::Success {
                    body: OutcomeBody::Unavailable,
                },
                false,
            ) => Some(ObservedResult::Unavailable),
            (ToolOutcome::Failure | ToolOutcome::Indeterminate, _) => None,
            (ToolOutcome::FailureWithBody { body }, _) => {
                Some(ObservedResult::Available(RawResultDigest::of(body.as_str().as_bytes())))
            }
        };
        if let ToolOutcome::FailureWithBody { body } = &report.outcome
            && views.failed_with_body(dispatch)
        {
            let digest = RawResultDigest::of(body.as_str().as_bytes());
            if views.failure_body_digest(dispatch) != Some(digest) {
                return Err(TransitionError::ObservationMismatch);
            }
            return Ok(EngineDecision {
                append: None,
                follow_up: FollowUp::Outcome(OutcomeFollowUp::Closed {
                    admitted: views.admitted_body(dispatch).cloned(),
                    withheld: withholds_output,
                }),
            });
        }
        if !views.is_open(dispatch) {
            match (views.closed_successfully(dispatch), &observed) {
                (true, None) => return Err(TransitionError::ContradictedSuccess),
                (false, Some(_)) if views.closed_unobserved(dispatch) => {
                    return Err(TransitionError::ClosedUnobserved);
                }
                (false, Some(_)) => return Err(TransitionError::ObservationMismatch),
                _ => {}
            }
            if let (Some(recorded), Some(reported)) = (fixed_observation(&views, dispatch), &observed)
                && &recorded != reported
            {
                return Err(TransitionError::ObservationMismatch);
            }
            return Ok(EngineDecision {
                append: None,
                follow_up: FollowUp::Outcome(OutcomeFollowUp::Closed {
                    admitted: views.admitted_body(dispatch).cloned(),
                    withheld: withholds_output,
                }),
            });
        }
        let checkpointed = views.observed_result(dispatch).cloned();
        match (&checkpointed, &observed) {
            // A recorded success cannot be withdrawn: its effects are committed.
            (Some(_), None) => return Err(TransitionError::ContradictedSuccess),
            (Some(recorded), Some(reported)) if recorded != reported => {
                return Err(TransitionError::ObservationMismatch);
            }
            _ => {}
        }
        if views
            .candidate(&crate::basis::SubjectKey::ConfinedResult(dispatch.clone()))
            .is_some()
        {
            return self.restage(view, &views, dispatch, report.offer_nonce, act);
        }

        let admission = match (&report.outcome, withholds_output) {
            (ToolOutcome::Success { .. }, true) => ResultAdmission::SuccessNoValue,
            (ToolOutcome::Failure, _) => ResultAdmission::Failure,
            (ToolOutcome::FailureWithBody { .. }, true) => {
                let Some(ObservedResult::Available(observed)) = observed else {
                    unreachable!("a failure with a body observed its digest above")
                };
                ResultAdmission::FailureNoValue { observed }
            }
            (ToolOutcome::FailureWithBody { body }, false) => ResultAdmission::FailureWithBody { body: body.clone() },
            (ToolOutcome::Indeterminate, _) => ResultAdmission::Indeterminate,
            (
                ToolOutcome::Success {
                    body: OutcomeBody::Unavailable,
                },
                false,
            ) => ResultAdmission::SuccessNoValue,
            (
                ToolOutcome::Success {
                    body: OutcomeBody::Available(raw),
                },
                false,
            ) => {
                let Some(ObservedResult::Available(raw_digest)) = observed else {
                    unreachable!("an available success observed its body digest above")
                };
                match views.bound_sanitizer(dispatch) {
                    None => {
                        self.validated_contract(&call)?;
                        ResultAdmission::SuccessRaw { body: raw.clone() }
                    }
                    Some(sanitizer) => {
                        let sanitizer = sanitizer.clone();
                        let derived = report.evidence.iter().find_map(|evidence| match evidence {
                            Evidence::Sanitizer {
                                sanitizer: named,
                                source,
                                derived,
                            } if named == &sanitizer && source == &raw_digest => Some(derived.clone()),
                            Evidence::Sanitizer { .. } | Evidence::Rewrite { .. } | Evidence::File { .. } => None,
                        });
                        let Some(derived) = derived else {
                            let append = self.checkpoint_batch(view, &views, dispatch, &call, raw_digest)?;
                            return Ok(EngineDecision {
                                append,
                                follow_up: FollowUp::Outcome(OutcomeFollowUp::Resolve(EvidenceRequest::Sanitizer {
                                    sanitizer,
                                    source: raw_digest,
                                    body: raw.clone(),
                                })),
                            });
                        };
                        let contract = self.validated_contract(&call)?;
                        let (candidate, lineage) = crate::admit::bound_candidate(
                            &self.registry,
                            &views,
                            dispatch,
                            &call,
                            &contract,
                            &sanitizer,
                            raw_digest,
                            derived.clone(),
                            &self.context(act),
                        )
                        .map_err(|error| match error {
                            AdmitError::SanitizerTransitionUnmet | AdmitError::SanitizerBindingMismatch => {
                                TransitionError::SanitizerUnapplicable
                            }
                            AdmitError::MembershipNeeded(needed) => TransitionError::from(needed),
                            other => unreachable!("the outcome path derives what the log already proved: {other}"),
                        })?;
                        let DerivedCandidate::Result { residual: Some(_), .. } = &candidate else {
                            return self.admitting_outcome(
                                view,
                                &views,
                                dispatch,
                                &call,
                                ResultAdmission::SuccessSanitized {
                                    body: derived,
                                    sanitizer,
                                    raw_digest,
                                },
                                act,
                            );
                        };
                        return self.stage_candidate(
                            view,
                            &views,
                            dispatch,
                            &call,
                            report.offer_nonce,
                            Fact::CandidateDerived {
                                trajectory: views.trajectory().clone(),
                                subject: crate::basis::SubjectKey::ConfinedResult(dispatch.clone()),
                                sanitizer,
                                derived: candidate,
                                lineage,
                                evidence: act.pinned().clone(),
                            },
                            act,
                        );
                    }
                }
            }
        };
        self.admitting_outcome(view, &views, dispatch, &call, admission, act)
    }

    fn admitting_outcome(
        &self,
        view: &EngineView,
        views: &Views,
        dispatch: &DispatchId,
        call: &ResolvedCall,
        admission: ResultAdmission,
        act: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        let batch = admit::admit_result(
            &self.registry,
            views,
            dispatch,
            call,
            admission,
            &self.context(act),
            &act.pinned(),
        )
        .map_err(|error| match error {
            AdmitError::SanitizerTransitionUnmet | AdmitError::SanitizerBindingMismatch => {
                TransitionError::SanitizerUnapplicable
            }
            AdmitError::MembershipNeeded(needed) => TransitionError::from(needed),
            other => unreachable!("the outcome path admits what the log already proved: {other}"),
        })?;
        let admitted = batch.iter().find_map(|fact| match fact {
            Fact::ValueAdmitted { value, .. } => Some(value.body.clone()),
            _ => None,
        });
        let withheld = views.withholds_output(dispatch);
        Ok(EngineDecision {
            append: Some(self.decided(view, crate::basis::DecidedAct::Outcome(dispatch.clone()), batch)?),
            follow_up: FollowUp::Outcome(OutcomeFollowUp::Closed { admitted, withheld }),
        })
    }

    /// The remedy menu a confined result's stage offers, with the group requirement raised before
    /// the menu reads anything.
    #[allow(clippy::too_many_arguments)]
    fn confined_menu(
        &self,
        views: &Views,
        contract: &ToolAnnotation,
        receiving: &Label,
        label: &Label,
        residual: &Narrowing,
        lineage: &SanitizerLineage,
        act: &ActEvidence,
    ) -> Result<Vec<plan::ExecutableRemedyPlan>, TransitionError> {
        let floor = plan::floor_of(&self.registry, views);
        Ok(plan::confined_stage(
            &self.registry,
            contract,
            receiving,
            label,
            residual,
            lineage,
            floor.as_ref(),
            &self.context(act),
        )?)
    }

    /// The success checkpoint a still-open dispatch owes before any external step runs: its
    /// declared effects commit now, while value finalization — an output sanitizer derivation —
    /// is still in flight. Empty where the log already records the observation.
    fn observed_checkpoint(
        &self,
        views: &Views,
        dispatch: &DispatchId,
        call: &ResolvedCall,
        source: RawResultDigest,
    ) -> Vec<Fact> {
        if views.observed_result(dispatch).is_some() {
            return Vec::new();
        }
        admit::observe_success(&self.registry, views, dispatch, call, ObservedResult::Available(source))
            .expect("an open, unreported dispatch checkpoints its observed success")
    }

    /// [`Self::observed_checkpoint`] sealed as its own batch, for the paths that append it and
    /// then hand the external step back to the runtime.
    fn checkpoint_batch(
        &self,
        view: &EngineView,
        views: &Views,
        dispatch: &DispatchId,
        call: &ResolvedCall,
        source: RawResultDigest,
    ) -> Result<Option<ValidatedFactBatch>, TransitionError> {
        let checkpoint = self.observed_checkpoint(views, dispatch, call, source);
        if checkpoint.is_empty() {
            return Ok(None);
        }
        Ok(Some(self.decided(
            view,
            crate::basis::DecidedAct::Outcome(dispatch.clone()),
            checkpoint,
        )?))
    }

    #[allow(clippy::too_many_arguments)]
    fn stage_candidate(
        &self,
        view: &EngineView,
        views: &Views,
        dispatch: &DispatchId,
        call: &ResolvedCall,
        nonce: crate::value::OfferNonce,
        derived: Fact,
        act: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        let source = match &derived {
            Fact::CandidateDerived {
                derived: DerivedCandidate::Result { source, .. },
                ..
            } => *source,
            _ => unreachable!("a staged record is a derived candidate"),
        };
        let mut facts = self.observed_checkpoint(views, dispatch, call, source);
        facts.push(derived);
        let (batch, confined) = self.staged(
            view,
            views,
            crate::basis::DecidedAct::Outcome(dispatch.clone()),
            nonce,
            dispatch,
            facts,
            act,
        )?;
        Ok(EngineDecision {
            append: Some(batch),
            follow_up: FollowUp::Outcome(OutcomeFollowUp::Staged(Box::new(confined))),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn staged(
        &self,
        view: &EngineView,
        views: &Views,
        act: crate::basis::DecidedAct,
        nonce: crate::value::OfferNonce,
        dispatch: &DispatchId,
        mut facts: Vec<Fact>,
        evidence: &ActEvidence,
    ) -> Result<(crate::transition::ValidatedFactBatch, Confined), TransitionError> {
        let Some(Fact::CandidateDerived {
            subject,
            derived:
                DerivedCandidate::Result {
                    value,
                    residual: Some(residual),
                    ..
                },
            lineage,
            ..
        }) = facts.last()
        else {
            unreachable!("a staged act ends on the candidate it derived")
        };
        let (subject, value, residual, lineage) = (subject.clone(), value.clone(), residual.clone(), lineage.clone());
        let receiving = views
            .receiving_bound(dispatch)
            .ok_or(TransitionError::UnknownDispatch)?
            .clone();
        let contract = self.dispatch_contract(views, dispatch)?;
        let stage = self.confined_menu(
            views,
            &contract,
            &receiving,
            &value.label,
            &residual,
            &lineage,
            evidence,
        )?;
        let advance = Sequence::advance_of(self, view, &facts);
        let (_, offers, opened) = self.open_offers(
            views,
            Opening {
                act: &act,
                advance: &advance,
                nonce: &nonce,
                subject: &subject,
            },
            dispatch.digest(),
            &stage,
            evidence,
        );
        facts.extend(opened);
        let batch = self.declaring(act, advance, facts);
        Ok((
            self.seal(view, batch)?,
            Confined {
                dispatch: dispatch.clone(),
                candidate: value,
                residual,
                offers,
            },
        ))
    }

    fn restage(
        &self,
        view: &EngineView,
        views: &Views,
        dispatch: &DispatchId,
        nonce: crate::value::OfferNonce,
        evidence: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        let subject = crate::basis::SubjectKey::ConfinedResult(dispatch.clone());
        let Some(DerivedCandidate::Result {
            value,
            residual: Some(residual),
            ..
        }) = views.candidate(&subject).cloned()
        else {
            unreachable!("a standing candidate owes a residual")
        };
        if let Some((_, offers)) = views.pending_block(&subject) {
            return Ok(EngineDecision {
                append: None,
                follow_up: FollowUp::Outcome(OutcomeFollowUp::Staged(Box::new(Confined {
                    dispatch: dispatch.clone(),
                    candidate: value,
                    residual,
                    offers,
                }))),
            });
        }
        let receiving = views
            .receiving_bound(dispatch)
            .ok_or(TransitionError::UnknownDispatch)?
            .clone();
        let lineage = views.lineage(&subject);
        let contract = self.dispatch_contract(views, dispatch)?;
        let stage = self.confined_menu(
            views,
            &contract,
            &receiving,
            &value.label,
            &residual,
            &lineage,
            evidence,
        )?;
        let act = crate::basis::DecidedAct::Outcome(dispatch.clone());
        let advance = crate::basis::BasisAdvance::default();
        let (_, offers, opened) = self.open_offers(
            views,
            Opening {
                act: &act,
                advance: &advance,
                nonce: &nonce,
                subject: &subject,
            },
            dispatch.digest(),
            &stage,
            evidence,
        );
        let batch = self.declaring(act, advance, opened);
        Ok(EngineDecision {
            append: Some(self.seal(view, batch)?),
            follow_up: FollowUp::Outcome(OutcomeFollowUp::Staged(Box::new(Confined {
                dispatch: dispatch.clone(),
                candidate: value,
                residual,
                offers,
            }))),
        })
    }

    /// The guards a proposal batch passes before anything is read for a decision, in the order the
    /// boundary applies them. Returns how many provider results the log already admitted for this
    /// batch identity.
    fn admissible_batch(&self, views: &Views, batch: &ProposalBatch) -> Result<usize, TransitionError> {
        if let Some(mark) = batch.spawn {
            if mark.index() >= batch.proposals.len() {
                return Err(TransitionError::SpawnMarkOutOfRange);
            }
            if !self.profile().context_control() {
                return Err(TransitionError::SpawnUncontrolled);
            }
        }
        if batch.proposals.is_empty() && batch.provider_results.is_empty() {
            return Err(TransitionError::EmptyBatch);
        }
        // An ended branch is closed to new work, releases and admissions included.
        if views.has_ended(&batch.trajectory) {
            return Err(TransitionError::BranchEnded);
        }
        let admitted = views.provider_admissions(&batch.id).len();
        let known = admitted > 0 || views.decided_batch(&batch.id).is_some();
        if known
            && !views.provider_admissions(&batch.id).eq(batch
                .provider_results
                .iter()
                .map(|result| (&batch.trajectory, &result.tool, &result.body)))
        {
            return Err(TransitionError::BatchIdentityConflict);
        }
        for result in &batch.provider_results {
            if self.registry.provider_run_annotation(&result.tool).is_none() {
                return Err(TransitionError::Call(match self.registry.declared(&result.tool) {
                    true => EngineError::NotProviderRun(result.tool.as_str().to_string()),
                    false => EngineError::UnknownTool(result.tool.as_str().to_string()),
                }));
            }
        }
        Ok(admitted)
    }

    /// A batch identity the log already decided answers from the record and appends nothing. The
    /// repeat must carry the same trajectory and the same canonical payload, or it is a different
    /// batch wearing a spent identity.
    fn replayed_batch(
        &self,
        views: &Views,
        batch: &ProposalBatch,
        act: &ActEvidence,
    ) -> Result<Option<EngineDecision>, TransitionError> {
        let Some(decided) = views.decided_batch(&batch.id) else {
            return Ok(None);
        };
        if decided.trajectory != batch.trajectory {
            return Err(TransitionError::BatchIdentityConflict);
        }
        let recorded = decided.clone();
        let Ok(proposals) = self.resolve_proposals(batch) else {
            return Err(TransitionError::BatchIdentityConflict);
        };
        if recorded.payload != CanonicalDigest::of_batch(&proposals, batch.spawn) {
            return Err(TransitionError::BatchIdentityConflict);
        }
        // The pinned audience answers are act payload: a repeat under other answers is a
        // different act wearing a spent identity.
        if recorded.evidence != batch.audience {
            return Err(TransitionError::BatchIdentityConflict);
        }
        act.inherit(&recorded.evidence)?;
        let under = self.act_evidence(
            act.inheriting(&recorded.evidence)?,
            AudienceEvidence::default(),
            act.principal.clone(),
        )?;
        let follow_up = self.decided_follow_up(views, batch, &proposals, &recorded.released, &under)?;
        act.absorb(&under);
        Ok(Some(EngineDecision {
            append: None,
            follow_up,
        }))
    }

    /// Every proposal carries the annotation its declaration requires. The ask comes back
    /// before the batch composes; a foreign or out-of-policy claim is a refusal.
    fn answered_proposals(&self, proposals: &[ResolvedCall]) -> Result<(), TransitionError> {
        let mut unresolved = Vec::new();
        for call in proposals {
            let declaration = self
                .registry
                .declaration(call)
                .expect("a resolved call names a checkable tool");
            if let Some(annotator) = unanswered(&self.registry, declaration, call)?
                && !unresolved.contains(&annotator)
            {
                unresolved.push(annotator);
            }
        }
        if !unresolved.is_empty() {
            return Err(TransitionError::AnnotationNeeded { annotators: unresolved });
        }
        Ok(())
    }

    fn decide_proposals(
        &self,
        view: &EngineView,
        batch: &ProposalBatch,
        act: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        let views = view.projection().view(&batch.trajectory);
        let admitted = self.admissible_batch(&views, batch)?;
        if let Some(answer) = self.replayed_batch(&views, batch, act)? {
            return Ok(answer);
        }

        let mut facts: Vec<Fact> = Vec::new();
        facts.extend(match admitted {
            0 => batch
                .provider_results
                .iter()
                .enumerate()
                .map(|(position, result)| {
                    let contract = self
                        .registry
                        .provider_run_annotation(&result.tool)
                        .expect("every exposed result was classified above");
                    Fact::ValueAdmitted {
                        trajectory: batch.trajectory.clone(),
                        value: LabeledValue::new(result.body.clone(), contract.output_label()),
                        provenance: Provenance::ProviderRun {
                            tool: result.tool.clone(),
                            batch: batch.id.clone(),
                            position: position as u32,
                            effects: contract.emits.clone(),
                            evidence: act.pinned().clone(),
                        },
                    }
                })
                .collect(),
            _ => Vec::new(),
        });

        let proposals = match self.resolve_proposals(batch) {
            Ok(proposals) => proposals,
            Err((position, error)) => {
                return Ok(EngineDecision {
                    append: self.seal_admissions(view, &batch.id, facts)?,
                    follow_up: FollowUp::Malformed { position, error },
                });
            }
        };
        self.answered_proposals(&proposals)?;

        let mut working = std::borrow::Cow::Borrowed(view.projection());
        for fact in &facts {
            working.to_mut().fold(fact);
        }
        let admissions = Sequence::advance_of(self, view, &facts);
        let composed = compose_batch(
            &self.registry,
            &mut working,
            ComposingBatch {
                trajectory: &batch.trajectory,
                id: &batch.id,
            },
            &proposals,
            batch.spawn,
            &|views, call| {
                views
                    .approvals_for(call)
                    .find(|(offer, approval)| {
                        approval.basis == views.basis_after(&admissions, &crate::basis::SubjectKey::Approval(*offer))
                    })
                    .map(|(offer, _)| offer)
            },
            &|views, call, taken| {
                views
                    .call_candidates_for(call)
                    .filter(|(subject, recorded)| {
                        !taken.contains(*subject)
                            && recorded.generation == views.basis_after(&admissions, subject).subject
                    })
                    .map(|(subject, _)| subject.clone())
                    .collect()
            },
            act,
        )
        .map_err(|refusal| match refusal {
            ComposeRefusal::Malformed(error) => TransitionError::Call(error),
            ComposeRefusal::MembershipNeeded(needed) => TransitionError::from(needed),
            ComposeRefusal::Evidence(refusal) => TransitionError::ForeignEvidence(refusal),
        })?;

        let released: Vec<Released> = composed
            .iter()
            .zip(&proposals)
            .filter_map(|(release, call)| {
                release.as_ref().map(|release| Released {
                    dispatch: release.dispatch.clone(),
                    call: call.clone(),
                    fork: release.prepares_fork.map(|_| ForkId::of(&release.dispatch)),
                })
            })
            .collect();
        facts.push(Fact::ProposalBatchDecided {
            trajectory: batch.trajectory.clone(),
            batch: batch.id.clone(),
            proposals: proposals.clone(),
            spawn: batch.spawn,
            released: released.iter().map(|release| release.dispatch.clone()).collect(),
            // The act's own pinned answers: batch payload, compared on repeat. A release that
            // spends an approval reads under the approval's pins too, but those are the
            // approval record's — replay re-merges them from it.
            evidence: act.pinned().clone(),
        });
        facts.extend(composed.iter().flatten().flat_map(|release| release.facts.clone()));
        // What this decision moves, derived before the offers that have to record where it lands.
        // The declaration prepended below re-derives it over the whole batch; an offer record
        // moves nothing, so the two agree by construction.
        let advance = Sequence::advance_of(self, view, &facts);
        let final_views = working.view(&batch.trajectory);
        let mut refused: Vec<(usize, &ResolvedCall, ToolAnnotation, RawBlock, CallRole)> = Vec::new();
        for (position, call) in composed
            .iter()
            .enumerate()
            .filter(|(_, release)| release.is_none())
            .map(|(position, _)| (position, &proposals[position]))
        {
            let contract = self.validated_contract(call)?.into_owned();
            let role = CallRole::of(batch.spawn, position);
            let raw = match check::evaluate(
                &contract,
                &final_views,
                call,
                &CallStage::default(),
                role,
                &self.context(act),
            )? {
                CheckOutcome::Block(raw) => raw,
                CheckOutcome::Allow => {
                    unreachable!("an in-batch release only ever adds gaps to a refused sibling's block")
                }
            };
            refused.push((position, call, contract, raw, role));
        }
        let block_stage: Vec<SymbolicAtom> = refused
            .iter()
            .flat_map(|(_, _, contract, raw, _)| plan::block_atoms(&self.registry, contract, raw))
            .collect();
        require_atoms(act, block_stage)?;
        let mut blocked = Vec::new();
        for (position, call, contract, raw, role) in refused {
            let subject = crate::basis::SubjectKey::Call {
                trajectory: batch.trajectory.clone(),
                batch: batch.id.clone(),
                position: position as u32,
            };
            let (block, opened_offers) = self.surface_call_block(
                &final_views,
                Opening {
                    act: &crate::basis::DecidedAct::Proposals(batch.id.clone()),
                    advance: &advance,
                    nonce: &batch.offer_nonce,
                    subject: &subject,
                },
                BlockedCall {
                    call,
                    contract: &contract,
                    raw: &raw,
                    stage: &CallStage::default(),
                    role,
                },
                act,
            )?;
            facts.extend(opened_offers);
            blocked.push(block);
        }
        let decided = self.declaring(crate::basis::DecidedAct::Proposals(batch.id.clone()), advance, facts);
        let append = self.seal(view, decided)?;
        Ok(EngineDecision {
            append: Some(append),
            follow_up: FollowUp::Proposals {
                released,
                blocked,
                spent: Vec::new(),
                settled: Vec::new(),
            },
        })
    }

    fn resolve_proposals(&self, batch: &ProposalBatch) -> Result<Vec<ResolvedCall>, (usize, EngineError)> {
        let mut file_bases = vec![None; batch.proposals.len()];
        for evidence in &batch.evidence {
            if let Evidence::File { position, basis } = evidence {
                let Some(slot) = file_bases.get_mut(*position) else {
                    return Err((
                        *position,
                        EngineError::InvalidFileEvidence("position is outside the proposal batch".into()),
                    ));
                };
                if slot.replace(basis.clone()).is_some() {
                    return Err((*position, EngineError::InvalidFileEvidence("duplicate position".into())));
                }
            }
        }
        let proposals: Vec<ResolvedCall> = batch
            .proposals
            .iter()
            .enumerate()
            .map(|(position, proposed)| {
                self.resolve_call(proposed.tool.clone(), &proposed.arguments)
                    .map(|call| {
                        call.with_annotation(proposed.annotation.clone())
                            .with_file_basis(file_bases[position].clone())
                    })
                    .map_err(|error| (position, error))
            })
            .collect::<Result<_, _>>()?;
        Ok(proposals)
    }

    fn seal_admissions(
        &self,
        view: &EngineView,
        id: &crate::transition::ProposalBatchId,
        facts: Vec<Fact>,
    ) -> Result<Option<ValidatedFactBatch>, TransitionError> {
        if facts.is_empty() {
            return Ok(None);
        }
        let advance = Sequence::advance_of(self, view, &facts);
        let batch = self.declaring(crate::basis::DecidedAct::Proposals(id.clone()), advance, facts);
        Ok(Some(self.seal(view, batch)?))
    }

    fn surface_call_block(
        &self,
        views: &Views,
        opening: Opening<'_>,
        blocked: BlockedCall<'_>,
        act: &ActEvidence,
    ) -> Result<(Blocked, Vec<Fact>), TransitionError> {
        let call = blocked.call;
        let planned = plan::plan(&self.registry, views, blocked, &self.context(act))?;
        let (block_id, offers, opened) =
            self.open_offers(views, opening, &call.digest(), &Engine::executable(&planned), act);
        Ok((
            Blocked {
                call: call.clone(),
                block: planned,
                block_id,
                offers,
            },
            opened,
        ))
    }

    fn open_offers(
        &self,
        views: &Views,
        opening: Opening<'_>,
        call: &crate::value::CanonicalDigest,
        plans: &[plan::ExecutableRemedyPlan],
        under: &ActEvidence,
    ) -> (
        crate::value::BlockId,
        Vec<(crate::value::OfferId, plan::PlanId)>,
        Vec<Fact>,
    ) {
        let Opening {
            act,
            advance,
            nonce,
            subject,
        } = opening;
        let basis = views.basis_after(advance, subject);
        let (trajectory, block_id) = match subject {
            crate::basis::SubjectKey::Call {
                trajectory,
                batch,
                position,
            } => (
                trajectory,
                crate::value::BlockId::of_proposal(nonce, trajectory, batch, *position, call),
            ),
            crate::basis::SubjectKey::ConfinedResult(dispatch) => (
                dispatch.trajectory(),
                crate::value::BlockId::of_candidate(nonce, dispatch, basis.subject),
            ),
            // A return crosses at the child's stop or is refused there; a prepared approval is
            // spent by releasing its call. Neither carries a stage of its own.
            crate::basis::SubjectKey::Return(_) => unreachable!("no stage stands on a return"),
            crate::basis::SubjectKey::Approval(_) => unreachable!("no stage stands on an approval"),
        };
        let mut ids = Vec::new();
        let mut facts = Vec::new();
        for (index, executable) in plans.iter().enumerate() {
            let offer = crate::value::OfferId::of_plan(
                &block_id,
                index as u32,
                &serde_json_canonicalizer::to_vec(executable).expect("a derived plan canonicalizes"),
            );
            ids.push((offer, executable.id));
            facts.push(Fact::OfferOpened {
                trajectory: trajectory.clone(),
                offer,
                block: block_id,
                act: act.clone(),
                call: *call,
                subject: subject.clone(),
                plan: executable.clone(),
                basis,
                evidence: under.pinned().clone(),
            });
        }
        (block_id, ids, facts)
    }

    fn executable(planned: &PlannedBlock) -> Vec<plan::ExecutableRemedyPlan> {
        planned
            .plans
            .iter()
            .filter_map(plan::RemedyPlan::executable)
            .cloned()
            .collect()
    }

    fn decided_follow_up(
        &self,
        views: &Views,
        batch: &ProposalBatch,
        proposals: &[ResolvedCall],
        recorded: &[DispatchId],
        act: &ActEvidence,
    ) -> Result<FollowUp, TransitionError> {
        let subject_at = |position: usize| crate::basis::SubjectKey::Call {
            trajectory: batch.trajectory.clone(),
            batch: batch.id.clone(),
            position: position as u32,
        };
        // The call each position is about now: the candidate an input hop derived, under the
        // contract its own arguments select and with the pinned evidence that hop consumed,
        // or the proposal.
        let standing: Vec<(&ResolvedCall, ActEvidence)> = proposals
            .iter()
            .enumerate()
            .map(|(position, call)| {
                let subject = subject_at(position);
                let pinned = views.candidate_evidence(&subject);
                act.inherit(pinned)?;
                let under = self.act_evidence(
                    act.inheriting(pinned)?,
                    AudienceEvidence::default(),
                    act.principal.clone(),
                )?;
                Ok((views.standing_call(&subject).unwrap_or(call), under))
            })
            .collect::<Result<_, TransitionError>>()?;
        let mut released = Vec::new();
        let mut blocked = Vec::new();
        let mut spent = Vec::new();
        let mut settled = Vec::new();
        let mut next = recorded.iter().peekable();
        for (position, call) in proposals.iter().enumerate() {
            match next.next_if(|dispatch| views.dispatch_call(dispatch) == Some(call)) {
                // Only a dispatch still awaiting its result may be handed back for invocation.
                Some(dispatch) if views.is_open(dispatch) && !views.is_succeeded(dispatch) => released.push(Released {
                    dispatch: dispatch.clone(),
                    call: call.clone(),
                    fork: prepared_fork(views, dispatch),
                }),
                Some(dispatch) => {
                    settled.push(Settled {
                        dispatch: dispatch.clone(),
                        call: call.clone(),
                        outcome: settled_outcome(views, dispatch),
                    });
                }
                None => {
                    let subject = subject_at(position);
                    // An input hop the agent has since run replaced this proposal, so the call
                    // this position is about now is the candidate, and the check reads its
                    // substitution. Its offers are the ones already pending on
                    // the same subject, which is why the two must be reported together.
                    let (candidate, under) = &standing[position];
                    let candidate = (*candidate).clone();
                    let contract = self.validated_contract(&candidate)?;
                    let stage = views.call_stage(&subject);
                    let role = views.call_role(&subject);
                    match check::evaluate(&contract, views, &candidate, &stage, role, &self.context(under))? {
                        CheckOutcome::Block(raw) => {
                            require_atoms(under, plan::block_atoms(&self.registry, &contract, &raw))?;
                            let (block_id, offers) = views.pending_block(&subject).unwrap_or_else(|| {
                                let block_id = crate::value::BlockId::of_proposal(
                                    &batch.offer_nonce,
                                    &batch.trajectory,
                                    &batch.id,
                                    position as u32,
                                    &candidate.digest(),
                                );
                                (block_id, Vec::new())
                            });
                            blocked.push(Blocked {
                                block: plan::plan(
                                    &self.registry,
                                    views,
                                    BlockedCall {
                                        call: &candidate,
                                        contract: &contract,
                                        raw: &raw,
                                        stage: &stage,
                                        role,
                                    },
                                    &self.context(under),
                                )?,
                                call: candidate,
                                block_id,
                                offers,
                            });
                        }
                        CheckOutcome::Allow => match views.subject_dispatch(&subject).cloned() {
                            Some(dispatch) if views.is_open(&dispatch) && !views.is_succeeded(&dispatch) => {
                                released.push(Released {
                                    dispatch,
                                    call: candidate,
                                    fork: None,
                                });
                            }
                            Some(dispatch) => settled.push(Settled {
                                outcome: settled_outcome(views, &dispatch),
                                dispatch,
                                call: candidate,
                            }),
                            None => spent.push(candidate),
                        },
                    }
                }
            }
        }
        for (_, under) in &standing {
            act.absorb(under);
        }
        Ok(FollowUp::Proposals {
            released,
            blocked,
            spent,
            settled,
        })
    }

    fn decide_offer(
        &self,
        view: &EngineView,
        execution: &OfferExecution,
        act: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        let views = view.projection().view(&execution.trajectory);
        let recorded = views
            .offer(&execution.offer)
            .ok_or(TransitionError::UnknownOffer)?
            .clone();
        if recorded.trajectory != execution.trajectory {
            return Err(TransitionError::OfferElsewhere);
        }
        if let Some(end) = recorded.end.clone() {
            return self.ended_offer(&views, &recorded, &end, execution, act);
        }
        if recorded.basis != views.basis_for(&recorded.subject) {
            return Err(TransitionError::StaleOffer);
        }
        if let crate::basis::SubjectKey::ConfinedResult(dispatch) = &recorded.subject {
            let dispatch = dispatch.clone();
            return self.decide_confined(view, &views, execution, &recorded, &dispatch, act);
        }
        if let crate::basis::SubjectKey::Return(_) = &recorded.subject {
            return self.invalidated(view, execution, &recorded);
        }
        let call = self.offer_call(&views, &recorded);
        let contract = self.validated_contract(&call)?;
        let stage = views.call_stage(&recorded.subject);
        let role = views.call_role(&recorded.subject);
        let live = match check::evaluate(&contract, &views, &call, &stage, role, &self.context(act))? {
            CheckOutcome::Block(raw) => {
                require_atoms(act, plan::block_atoms(&self.registry, &contract, &raw))?;
                require_atoms(act, plan::plan_atoms(&self.registry, &recorded.plan))?;
                plan::plan(
                    &self.registry,
                    &views,
                    BlockedCall {
                        call: &call,
                        contract: &contract,
                        raw: &raw,
                        stage: &stage,
                        role,
                    },
                    &self.context(act),
                )?
                .plans
                .iter()
                .filter_map(plan::RemedyPlan::executable)
                .any(|offered| offered == &recorded.plan)
                .then_some(raw)
            }
            // The block is gone: whatever the agent would have remedied, nothing needs it now.
            CheckOutcome::Allow => None,
        };
        let Some(raw) = live else {
            return self.invalidated(view, execution, &recorded);
        };
        match (&execution.outcome, recorded.plan.hop()) {
            (OfferOutcome::Derived(evidence), Some(sanitizer)) => self.hop_call(
                view, &views, execution, &recorded, &contract, &raw, &call, &stage, sanitizer, evidence, act,
            ),
            (OfferOutcome::Approved(evidence), None) => self.approve_offer(
                view, &views, execution, &recorded, &contract, &raw, &call, evidence, act,
            ),
            (OfferOutcome::Denied { authority }, None) => self.deny_offer(
                view, &views, execution, &recorded, &contract, &call, &raw, &stage, authority, act,
            ),
            _ => Err(TransitionError::PlanOutcomeMismatch),
        }
    }

    fn invalidated(
        &self,
        view: &EngineView,
        execution: &OfferExecution,
        recorded: &crate::projection::RecordedOffer,
    ) -> Result<EngineDecision, TransitionError> {
        let batch = vec![Fact::OfferInvalidated {
            trajectory: recorded.trajectory.clone(),
            offer: execution.offer,
        }];
        Ok(EngineDecision {
            append: Some(self.seal(view, batch)?),
            follow_up: FollowUp::Offer(OfferFollowUp::Invalidated),
        })
    }

    fn decide_confined(
        &self,
        view: &EngineView,
        views: &Views,
        execution: &OfferExecution,
        recorded: &crate::projection::RecordedOffer,
        dispatch: &DispatchId,
        act: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        let subject = crate::basis::SubjectKey::ConfinedResult(dispatch.clone());
        let call = views
            .dispatch_call(dispatch)
            .ok_or(TransitionError::UnknownDispatch)?
            .clone();
        let receiving = views
            .receiving_bound(dispatch)
            .ok_or(TransitionError::UnknownDispatch)?
            .clone();
        let Some(DerivedCandidate::Result {
            value,
            residual: Some(residual),
            ..
        }) = views.candidate(&subject).cloned()
        else {
            return self.invalidated(view, execution, recorded);
        };
        let lineage = views.lineage(&subject);
        let contract = self.dispatch_contract(views, dispatch)?;
        let stage = self.confined_menu(views, &contract, &receiving, &value.label, &residual, &lineage, act)?;
        if !stage.contains(&recorded.plan) {
            return self.invalidated(view, execution, recorded);
        }
        let mut facts = vec![Fact::OfferAccepted {
            trajectory: recorded.trajectory.clone(),
            offer: execution.offer,
        }];
        facts.extend(invalidated_siblings(
            views,
            &recorded.trajectory,
            &subject,
            execution.offer,
        ));
        match (&execution.outcome, recorded.plan.hop()) {
            (OfferOutcome::Derived(evidence), Some(sanitizer)) => self.hop_candidate(
                view, views, execution, dispatch, &call, &receiving, &value, &lineage, sanitizer, evidence, facts, act,
            ),
            (OfferOutcome::Approved(evidence), None) if evidence.is_empty() => {
                self.accept_candidate(view, views, execution, dispatch, &call, facts, act)
            }
            _ => Err(TransitionError::PlanOutcomeMismatch),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn accept_candidate(
        &self,
        view: &EngineView,
        views: &Views,
        execution: &OfferExecution,
        dispatch: &DispatchId,
        call: &ResolvedCall,
        mut facts: Vec<Fact>,
        act: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        let admitted = admit::admit_result(
            &self.registry,
            views,
            dispatch,
            call,
            ResultAdmission::CandidateAccepted { offer: execution.offer },
            &self.context(act),
            &act.pinned(),
        )
        .unwrap_or_else(|error| unreachable!("the confined stage admits what the log already proved: {error}"));
        facts.extend(admitted);
        let value = crossed(&facts);
        Ok(EngineDecision {
            append: Some(self.decided(view, crate::basis::DecidedAct::Offer(execution.offer), facts)?),
            follow_up: FollowUp::Offer(OfferFollowUp::Admitted { value }),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn hop_candidate(
        &self,
        view: &EngineView,
        views: &Views,
        execution: &OfferExecution,
        dispatch: &DispatchId,
        call: &ResolvedCall,
        receiving: &Label,
        predecessor: &crate::value::LabeledValue,
        lineage: &SanitizerLineage,
        sanitizer: &SanitizerName,
        evidence: &Evidence,
        mut facts: Vec<Fact>,
        act: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        let Evidence::Sanitizer {
            sanitizer: named,
            source,
            derived: body,
        } = evidence
        else {
            return Err(TransitionError::EvidenceMismatch);
        };
        let source_digest = RawResultDigest::of(predecessor.body.as_str().as_bytes());
        if named != sanitizer || source != &source_digest {
            return Err(TransitionError::EvidenceMismatch);
        }
        let registered = self
            .registry
            .sanitizer(sanitizer)
            .ok_or(TransitionError::SanitizerUnapplicable)?;
        let label = registered
            .derive_output(
                &predecessor.label,
                &self.validated_contract(call)?.tags,
                &self.context(act),
            )?
            .ok_or(TransitionError::SanitizerUnapplicable)?;
        if !plan::confined_hop_helps(receiving, &predecessor.label, &label) {
            return Err(TransitionError::SanitizerUnapplicable);
        }
        let lineage = lineage
            .extend(sanitizer.clone())
            .ok_or(TransitionError::SanitizerUnapplicable)?;
        let residual = crate::admit::confined_residual(receiving, &label);
        let staged = residual.is_some();
        facts.push(Fact::CandidateDerived {
            trajectory: views.trajectory().clone(),
            subject: crate::basis::SubjectKey::ConfinedResult(dispatch.clone()),
            sanitizer: sanitizer.clone(),
            derived: DerivedCandidate::Result {
                dispatch: dispatch.clone(),
                source: source_digest,
                from: ConfinedFrom::Offer(execution.offer),
                value: crate::value::LabeledValue::new(body.clone(), label),
                residual,
            },
            lineage,
            evidence: act.pinned().clone(),
        });
        if staged {
            let (batch, confined) = self.staged(
                view,
                views,
                crate::basis::DecidedAct::Offer(execution.offer),
                execution.offer_nonce,
                dispatch,
                facts,
                act,
            )?;
            return Ok(EngineDecision {
                append: Some(batch),
                follow_up: FollowUp::Offer(OfferFollowUp::Staged(Box::new(confined))),
            });
        }
        // The successor owes nothing, so it crosses here. The admission re-derives from the record
        // just written, which is why the candidate is folded first: the one admission choke point
        // reads state, never a caller's claim.
        let mut after = view.projection().clone();
        for fact in &facts {
            after.fold(fact);
        }
        let admitted = admit::admit_result(
            &self.registry,
            &after.view(views.trajectory()),
            dispatch,
            call,
            ResultAdmission::CandidateAdmissible,
            &self.context(act),
            &act.pinned(),
        )
        .unwrap_or_else(|error| unreachable!("the confined stage admits what this act just derived: {error}"));
        facts.extend(admitted);
        let value = crossed(&facts);
        Ok(EngineDecision {
            append: Some(self.decided(view, crate::basis::DecidedAct::Offer(execution.offer), facts)?),
            follow_up: FollowUp::Offer(OfferFollowUp::Admitted { value }),
        })
    }

    /// One input-substitution progress hop.
    ///
    /// The sanitizer read the engine's own canonical argument bytes and returned one complete
    /// replacement object. Its bytes are untrusted: the engine strictly parses them, selects the
    /// ordered contract their arguments name, schema-checks them against that contract's
    /// parameters, constructs the canonical arguments itself, and only then has a call to measure.
    /// Nothing about the replacement is taken on the runtime's word, and the tool is never
    /// replaced. The sanitizer's scope and the block it improves are judged on the contract the
    /// offer was planned on; the rewritten call is judged on the contract it selects.
    ///
    /// A valid strictly helpful replacement commits as the next candidate on this subject and ends
    /// every sibling offer standing on its predecessor. The engine then re-checks it: where nothing
    /// is left to remedy it is released in this same batch — the immediate-admissibility exception
    /// — and otherwise the stage it leaves is the agent's next choice. An invalid
    /// derivation produces no record and no dispatch, and leaves the offer pending for a deliberate
    /// retry.
    #[allow(clippy::too_many_arguments)]
    fn hop_call(
        &self,
        view: &EngineView,
        views: &Views,
        execution: &OfferExecution,
        recorded: &crate::projection::RecordedOffer,
        contract: &ToolAnnotation,
        raw: &crate::check::RawBlock,
        call: &ResolvedCall,
        stage: &CallStage,
        sanitizer: &SanitizerName,
        evidence: &Evidence,
        under: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        let Evidence::Rewrite {
            sanitizer: named,
            source,
            derived: body,
            annotation,
        } = evidence
        else {
            return Err(TransitionError::EvidenceMismatch);
        };
        if named != sanitizer || source != &RawResultDigest::of(call.canonical_arguments().canonical_bytes()) {
            return Err(TransitionError::EvidenceMismatch);
        }
        let registered = self
            .registry
            .sanitizer(sanitizer)
            .ok_or(TransitionError::SanitizerUnapplicable)?;
        let label = registered
            .derive_input(
                &stage.released(&views.current_label()),
                &contract.tags,
                &self.context(under),
            )?
            .ok_or(TransitionError::SanitizerUnapplicable)?;
        let lineage = stage
            .lineage()
            .extend(sanitizer.clone())
            .ok_or(TransitionError::SanitizerUnapplicable)?;
        let substituted = substituted_call(&self.registry, call, body, annotation.as_ref())?;
        let contract = self
            .registry
            .annotation_of(&substituted)
            .expect("a validated call resolves its annotation");
        // The sanitizer's jurisdiction reaches the contract the rewrite selects as well as the
        // one the offer was planned on: a rewrite is no way past a tag that keeps sanitizers off
        // a contract.
        if !registered.applies_to(&contract.tags) {
            return Err(TransitionError::SanitizerUnapplicable);
        }

        let next = CallStage::substituting(label.clone(), lineage.clone());
        let role = views.call_role(&recorded.subject);
        let after = check::evaluate(&contract, views, &substituted, &next, role, &self.context(under))?;
        if !plan::substitution_helps(raw, &after) {
            return Err(TransitionError::SanitizerUnapplicable);
        }
        let derived = DerivedCandidate::Call {
            source: *source,
            from: execution.offer,
            call: substituted.clone(),
            label,
        };
        let trajectory = &recorded.trajectory;
        let mut facts = vec![Fact::OfferAccepted {
            trajectory: trajectory.clone(),
            offer: execution.offer,
        }];
        facts.extend(invalidated_siblings(
            views,
            trajectory,
            &recorded.subject,
            execution.offer,
        ));
        facts.push(Fact::CandidateDerived {
            trajectory: trajectory.clone(),
            subject: recorded.subject.clone(),
            sanitizer: sanitizer.clone(),
            derived,
            lineage,
            evidence: under.pinned().clone(),
        });
        let staged = Sequence::advance_of(self, view, &facts);
        let act = crate::basis::DecidedAct::Offer(execution.offer);
        let follow_up = match after {
            // The rewrite clears the block, and the derivation it staged is what says so. No
            // dispatch opens here: the model proposes the substituted call, and that proposal
            // takes the derivation and is judged against the state it arrives at. Releasing the
            // call here instead would decide it once, in advance, against state that can have
            // moved by the time the harness delivers it.
            CheckOutcome::Allow => OfferFollowUp::Approved {
                call: Box::new(substituted),
            },
            CheckOutcome::Block(raw) => {
                let role = views.call_role(&recorded.subject);
                require_atoms(under, plan::block_atoms(&self.registry, &contract, &raw))?;
                let (block, opened) = self.surface_call_block(
                    views,
                    Opening {
                        act: &act,
                        advance: &staged,
                        nonce: &execution.offer_nonce,
                        subject: &recorded.subject,
                    },
                    BlockedCall {
                        call: &substituted,
                        contract: &contract,
                        raw: &raw,
                        stage: &next,
                        role,
                    },
                    under,
                )?;
                facts.extend(opened);
                OfferFollowUp::Substituted { block: Box::new(block) }
            }
        };
        Ok(EngineDecision {
            append: Some(self.decided(view, act, facts)?),
            follow_up: FollowUp::Offer(follow_up),
        })
    }

    fn substituted_repeat(
        &self,
        views: &Views,
        recorded: &crate::projection::RecordedOffer,
        execution: &OfferExecution,
        act: &ActEvidence,
    ) -> Result<OfferFollowUp, TransitionError> {
        let Some(candidate) = views.call_candidate(&recorded.subject).cloned() else {
            return Ok(OfferFollowUp::Invalidated);
        };
        if views.pending_block(&recorded.subject).is_some() {
            // The candidate may stand under another contract than the offer was planned on; the
            // atoms that contract reads were pinned by the hop that derived it.
            let pinned = views.candidate_evidence(&recorded.subject);
            act.inherit(pinned)?;
            let under = self.act_evidence(
                act.inheriting(pinned)?,
                AudienceEvidence::default(),
                act.principal.clone(),
            )?;
            let reblocked = self.reblocked(views, recorded, execution, &under)?;
            act.absorb(&under);
            return Ok(match reblocked {
                Some(block) => OfferFollowUp::Substituted { block: Box::new(block) },
                None => OfferFollowUp::Invalidated,
            });
        }
        // A hop opens no dispatch, so its subject never has one: the derivation it staged is
        // the whole of what it did. While that derivation is current the repeat answers with the
        // same approved call — executing an offer twice must not change what the model is told
        // to do — and once something has taken or replaced it the offer no longer stands.
        Ok(match views.recorded_candidate(&recorded.subject) {
            Some(held) if held.generation == views.basis_for(&recorded.subject).subject => OfferFollowUp::Approved {
                call: Box::new(candidate),
            },
            Some(_) | None => OfferFollowUp::Invalidated,
        })
    }

    fn offer_call(&self, views: &Views, recorded: &crate::projection::RecordedOffer) -> ResolvedCall {
        views
            .standing_call(&recorded.subject)
            .expect("an opened offer names a proposal of a decided batch, or the candidate that replaced it")
            .clone()
    }

    fn ended_offer(
        &self,
        views: &Views,
        recorded: &crate::projection::RecordedOffer,
        end: &crate::projection::OfferEnd,
        execution: &OfferExecution,
        act: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        use crate::projection::OfferEnd;
        if let crate::basis::SubjectKey::ConfinedResult(dispatch) = &recorded.subject {
            return match (end, &execution.outcome, recorded.plan.hop()) {
                (OfferEnd::Accepted, OfferOutcome::Derived(Evidence::Sanitizer { .. }), Some(_)) => {
                    Ok(EngineDecision {
                        append: None,
                        follow_up: FollowUp::Offer(self.confined_repeat(views, dispatch)),
                    })
                }
                (OfferEnd::Accepted, OfferOutcome::Approved(evidence), None) if evidence.is_empty() => {
                    Ok(EngineDecision {
                        append: None,
                        follow_up: FollowUp::Offer(self.confined_repeat(views, dispatch)),
                    })
                }
                (OfferEnd::Invalidated, _, _) => Ok(EngineDecision {
                    append: None,
                    follow_up: FollowUp::Offer(OfferFollowUp::Invalidated),
                }),
                (OfferEnd::Accepted, OfferOutcome::Derived(_), Some(_))
                | (_, OfferOutcome::Derived(_), None)
                | (_, OfferOutcome::Approved(_), Some(_))
                | (OfferEnd::Accepted, OfferOutcome::Approved(_), None) => Err(TransitionError::PlanOutcomeMismatch),
                _ => Err(TransitionError::TerminalOffer),
            };
        }
        if recorded.plan.hop().is_some() {
            return match (end, &execution.outcome) {
                (OfferEnd::Accepted, OfferOutcome::Derived(Evidence::Rewrite { .. })) => Ok(EngineDecision {
                    append: None,
                    follow_up: FollowUp::Offer(self.substituted_repeat(views, recorded, execution, act)?),
                }),
                (OfferEnd::Accepted, OfferOutcome::Derived(_)) => Err(TransitionError::PlanOutcomeMismatch),
                (OfferEnd::Invalidated, _) => Ok(EngineDecision {
                    append: None,
                    follow_up: FollowUp::Offer(OfferFollowUp::Invalidated),
                }),
                (_, OfferOutcome::Approved(_)) => Err(TransitionError::PlanOutcomeMismatch),
                // A hop assigns no authority, so none of its offers can have been denied.
                _ => Err(TransitionError::TerminalOffer),
            };
        }
        let follow_up = match (end, &execution.outcome) {
            (OfferEnd::Accepted, OfferOutcome::Approved(_)) => OfferFollowUp::Approved {
                call: Box::new(
                    views
                        .approval(&execution.offer)
                        .ok_or(TransitionRefusal::UndischargedAcceptance)?
                        .call
                        .clone(),
                ),
            },
            (OfferEnd::Denied(recorded_authority), OfferOutcome::Denied { authority })
                if recorded_authority == authority =>
            {
                match self.reblocked(views, recorded, execution, act)? {
                    Some(block) => OfferFollowUp::Denied { block: Box::new(block) },
                    None => OfferFollowUp::Invalidated,
                }
            }
            (OfferEnd::Invalidated, _) => OfferFollowUp::Invalidated,
            // A terminal plan never took a sanitizer's derivation, spent or not.
            (_, OfferOutcome::Derived(_)) => return Err(TransitionError::PlanOutcomeMismatch),
            _ => return Err(TransitionError::TerminalOffer),
        };
        Ok(EngineDecision {
            append: None,
            follow_up: FollowUp::Offer(follow_up),
        })
    }

    fn confined_repeat(&self, views: &Views, dispatch: &DispatchId) -> OfferFollowUp {
        if let Some(value) = views.admitted_body(dispatch) {
            return OfferFollowUp::Admitted { value: value.clone() };
        }
        let subject = crate::basis::SubjectKey::ConfinedResult(dispatch.clone());
        match (views.candidate(&subject), views.pending_block(&subject)) {
            (
                Some(DerivedCandidate::Result {
                    value,
                    residual: Some(residual),
                    ..
                }),
                Some((_, offers)),
            ) => OfferFollowUp::Staged(Box::new(Confined {
                dispatch: dispatch.clone(),
                candidate: value.clone(),
                residual: residual.clone(),
                offers,
            })),
            _ => OfferFollowUp::Invalidated,
        }
    }

    fn reblocked(
        &self,
        views: &Views,
        recorded: &crate::projection::RecordedOffer,
        execution: &OfferExecution,
        act: &ActEvidence,
    ) -> Result<Option<Blocked>, TransitionError> {
        let call = self.offer_call(views, recorded);
        let contract = self.validated_contract(&call)?;
        let stage = views.call_stage(&recorded.subject);
        let role = views.call_role(&recorded.subject);
        let CheckOutcome::Block(raw) = check::evaluate(&contract, views, &call, &stage, role, &self.context(act))?
        else {
            return Ok(None);
        };
        require_atoms(act, plan::block_atoms(&self.registry, &contract, &raw))?;
        let (block_id, offers) = views
            .pending_block(&recorded.subject)
            .unwrap_or((offer_block(recorded, execution, &call), Vec::new()));
        Ok(Some(Blocked {
            block: plan::plan(
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
            )?,
            call,
            block_id,
            offers,
        }))
    }

    #[allow(clippy::too_many_arguments)]
    fn approve_offer(
        &self,
        view: &EngineView,
        views: &Views,
        execution: &OfferExecution,
        recorded: &crate::projection::RecordedOffer,
        contract: &ToolAnnotation,
        raw: &crate::check::RawBlock,
        call: &ResolvedCall,
        evidence: &[execute::AuthorityEvidence],
        act: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        if evidence.len() != recorded.plan.required.len() {
            return Err(PlanError::RulingAssignmentMismatch.into());
        }
        for required in &recorded.plan.required {
            let matched = evidence
                .iter()
                .filter(|given| given.authority == required.authority && given.covers == required.covers)
                .count();
            if matched != 1 {
                return Err(PlanError::RulingAssignmentMismatch.into());
            }
        }
        execute::rulings_cover(
            &self.registry,
            contract,
            raw,
            evidence.iter().map(|given| (&given.authority, given.covers.as_slice())),
            &self.context(act),
        )
        .map_err(|error| match error {
            // The undecided atoms surface as the act's ask, exactly as the gates raise them.
            PlanError::MembershipNeeded(needed) => TransitionError::from(needed),
            other => TransitionError::Plan(other),
        })?;
        if evidence.iter().any(|given| given.offer != execution.offer) {
            return Err(PlanError::EvidenceOfferMismatch.into());
        }
        // And each reviewed exactly this call at the fold the release will run against.
        let live = views.current_label();
        if evidence
            .iter()
            .any(|given| given.reviewed.tool != *call.tool() || given.reviewed.trajectory_label != live)
        {
            return Err(PlanError::ReviewMismatch.into());
        }
        // One current approval per call. A second would leave the release picking between two
        // plans the agent selected separately, and only one of them is the choice it made;
        // the approval that stands is released by proposing its call.
        if views.current_approval(call).is_some() {
            return Err(TransitionError::ApprovalPending);
        }
        // A marked spawn's plan declares the child's return policy the parent now supplies.
        let return_policy = match (recorded.plan.return_step(), &execution.return_policy) {
            (None, None) => None,
            (Some(step), Some(policy)) => {
                plan::declared_return_policy(&self.registry, views, step, policy)?;
                Some(policy.clone())
            }
            _ => return Err(TransitionError::PlanOutcomeMismatch),
        };
        let trajectory = &recorded.trajectory;
        let mut facts = vec![Fact::OfferAccepted {
            trajectory: trajectory.clone(),
            offer: execution.offer,
        }];
        facts.extend(invalidated_siblings(
            views,
            trajectory,
            &recorded.subject,
            execution.offer,
        ));
        let advance = Sequence::advance_of(self, view, &facts);
        let subject = crate::basis::SubjectKey::Approval(execution.offer);
        facts.push(Fact::CallApproved {
            trajectory: trajectory.clone(),
            offer: execution.offer,
            call: call.clone(),
            plan: recorded.plan.id,
            acceptance: recorded.plan.narrowing().cloned(),
            rulings: recorded
                .plan
                .required
                .iter()
                .map(|required| {
                    evidence
                        .iter()
                        .find(|given| given.authority == required.authority && given.covers == required.covers)
                        .expect("the assignment check matched each required entry to exactly one response")
                        .clone()
                })
                .collect(),
            output: recorded.plan.output_remedy(),
            return_policy,
            basis: views.basis_after(&advance, &subject),
            evidence: act.pinned().clone(),
        });
        let batch = self.declaring(crate::basis::DecidedAct::Offer(execution.offer), advance, facts);
        Ok(EngineDecision {
            append: Some(self.seal(view, batch)?),
            follow_up: FollowUp::Offer(OfferFollowUp::Approved {
                call: Box::new(call.clone()),
            }),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn deny_offer(
        &self,
        view: &EngineView,
        views: &Views,
        execution: &OfferExecution,
        recorded: &crate::projection::RecordedOffer,
        contract: &ToolAnnotation,
        call: &ResolvedCall,
        raw: &crate::check::RawBlock,
        stage: &CallStage,
        authority: &AuthorityName,
        act: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        if !recorded.plan.names_authority(authority) {
            return Err(TransitionError::UnassignedAuthority);
        }
        if self.registry.authority(authority).is_none() {
            return Err(PlanError::UnknownAuthority(authority.as_str().to_string()).into());
        }
        let trajectory = &recorded.trajectory;
        let mut facts = vec![Fact::Denial {
            trajectory: trajectory.clone(),
            digest: recorded.call,
            authority: authority.clone(),
        }];
        facts.extend(
            views
                .offers_naming(&recorded.call, authority)
                .into_iter()
                .map(|offer| Fact::OfferDenied {
                    trajectory: trajectory.clone(),
                    offer,
                    authority: authority.clone(),
                }),
        );
        let mut after = view.projection().clone();
        for fact in &facts {
            after.fold(fact);
        }
        let after = after.view(trajectory);
        let advance = Sequence::advance_of(self, view, &facts);
        let role = after.call_role(&recorded.subject);
        let (block, opened) = self.surface_call_block(
            &after,
            Opening {
                act: &crate::basis::DecidedAct::Offer(execution.offer),
                advance: &advance,
                nonce: &execution.offer_nonce,
                subject: &recorded.subject,
            },
            BlockedCall {
                call,
                contract,
                raw,
                stage,
                role,
            },
            act,
        )?;
        facts.extend(opened);
        let batch = self.declaring(crate::basis::DecidedAct::Offer(execution.offer), advance, facts);
        Ok(EngineDecision {
            append: Some(self.seal(view, batch)?),
            follow_up: FollowUp::Offer(OfferFollowUp::Denied { block: Box::new(block) }),
        })
    }

    /// The opening batch of a fresh root trajectory family: one `TrajectoryOpened`
    /// record against the empty log. The runtime appends it before any other family event.
    pub fn open_trajectory(
        &self,
        trajectory: &TrajectoryId,
        policy_file_key: crate::profile::PolicyFileKey,
        principal: Option<ReaderId>,
    ) -> Result<ValidatedFactBatch, TransitionRefusal> {
        self.opening(trajectory, policy_file_key, None, principal)
    }

    /// The opening batch of an independent conversation-root fork. `forked_from` came from the
    /// parent family's validated view; this creates no same-family child, spawn, or return
    /// contract.
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
        let empty = EngineView::validated(Projection::empty(0), self.identity, trajectory.clone());
        self.seal(
            &empty,
            vec![Fact::TrajectoryOpened(crate::fact::TrajectoryOpening {
                trajectory: trajectory.clone(),
                dialect: self.dialect,
                profile: self.profile().clone(),
                policy_digest: self.identity,
                policy_file_key,
                open_vectors: self.open_vectors(),
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

    fn dispatch_contract<'c>(
        &'c self,
        views: &'c Views<'c>,
        dispatch: &DispatchId,
    ) -> Result<std::borrow::Cow<'c, ToolAnnotation>, TransitionError> {
        let call = views.dispatch_call(dispatch).ok_or(TransitionError::UnknownDispatch)?;
        self.validated_contract(call).map_err(TransitionError::Call)
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
    let (id, declaration) = registry.select_tool(&tool, parsed.value()).ok_or_else(|| {
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
    Ok((ResolvedCall::new_keyed(tool, id, parsed), declaration))
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
    let same = rewritten.declaration_id() == call.declaration_id();
    let substituted = if same {
        call.substituting(rewritten.into_canonical_arguments())
            .with_annotation(annotation.cloned())
    } else {
        rewritten.with_annotation(annotation.cloned())
    };
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

/// The Annotator whose annotation a call still owes, if any. A foreign or
/// out-of-policy answer is a refusal.
fn unanswered(
    registry: &Registry,
    declaration: &crate::contract::ToolDeclaration,
    call: &ResolvedCall,
) -> Result<Option<crate::names::AnnotatorName>, TransitionError> {
    match check::validate_annotation(registry, declaration, call) {
        Ok(()) => Ok(None),
        Err(check::AnnotationRefusal::Needed(annotator)) => Ok(Some(annotator)),
        Err(check::AnnotationRefusal::Foreign(reason)) => Err(TransitionError::ForeignAnnotation { reason }),
        Err(check::AnnotationRefusal::OutsidePolicy(reason)) => Err(TransitionError::InvalidAnnotation { reason }),
    }
}

fn invalidated_siblings(
    views: &Views,
    trajectory: &TrajectoryId,
    subject: &crate::basis::SubjectKey,
    spent: crate::value::OfferId,
) -> Vec<Fact> {
    views
        .pending_block(subject)
        .map(|(_, offers)| offers)
        .unwrap_or_default()
        .into_iter()
        .map(|(offer, _)| offer)
        .filter(|offer| offer != &spent)
        .map(|offer| Fact::OfferInvalidated {
            trajectory: trajectory.clone(),
            offer,
        })
        .collect()
}

fn settled_outcome(views: &Views, dispatch: &DispatchId) -> SettledOutcome {
    match views.is_open(dispatch) {
        true => SettledOutcome::Confined,
        false => SettledOutcome::Closed {
            admitted: views.admitted_body(dispatch).cloned(),
        },
    }
}

fn approved_release(
    registry: &Registry,
    contract: &ToolAnnotation,
    output: &Label,
    trajectory: &TrajectoryId,
    dispatch: &DispatchId,
    approval: &crate::projection::PreparedApproval,
    act: &ActEvidence,
) -> Vec<Fact> {
    let context = membership_context(registry, act);
    let mut facts = Vec::new();
    if let Some(narrowing) = &approval.acceptance {
        facts.push(Fact::Acceptance {
            trajectory: trajectory.clone(),
            dispatch: dispatch.clone(),
            plan: approval.plan,
            narrowing: narrowing.clone(),
        });
    }
    facts.extend(approval.rulings.iter().map(|given| Fact::Ruling {
        trajectory: trajectory.clone(),
        dispatch: dispatch.clone(),
        plan: approval.plan,
        authority: given.authority.clone(),
        covers: given.covers.clone(),
        reviewed: given.reviewed.clone(),
        evidence: act.pinned().clone(),
    }));
    match &approval.output {
        Some(crate::plan::OutputRemedy::Withhold) => facts.push(Fact::OutputWithheld {
            trajectory: trajectory.clone(),
            dispatch: dispatch.clone(),
            plan: approval.plan,
        }),
        Some(crate::plan::OutputRemedy::Sanitize(sanitizer)) => {
            facts.push(Fact::OutputSanitizerBound {
                trajectory: trajectory.clone(),
                dispatch: dispatch.clone(),
                plan: approval.plan,
                sanitizer: sanitizer.clone(),
                contribution: crate::plan::bound_contribution(registry, contract, output, sanitizer, &context)
                    .expect("the compose gate answers a spent approval's sanitizer atoms")
                    .expect("a prepared approval binds an output sanitizer enumeration found applicable"),
                evidence: act.pinned().clone(),
            });
        }
        None => {}
    }
    facts
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

fn offer_block(
    recorded: &crate::projection::RecordedOffer,
    execution: &OfferExecution,
    call: &ResolvedCall,
) -> crate::value::BlockId {
    let crate::basis::SubjectKey::Call {
        trajectory,
        batch,
        position,
    } = &recorded.subject
    else {
        unreachable!("an opened offer's subject is a call candidate")
    };
    crate::value::BlockId::of_proposal(&execution.offer_nonce, trajectory, batch, *position, &call.digest())
}

/// The act one child's stop lands under: its next crossing's identity.
fn return_act(views: &Views, child: &TrajectoryId) -> crate::basis::DecidedAct {
    crate::basis::DecidedAct::ChildReturn(ChildReturnId::new(child.clone(), views.returns_by(child)))
}

fn prepared_fork(views: &Views, dispatch: &DispatchId) -> Option<ForkId> {
    let fork = ForkId::of(dispatch);
    (views.is_prepared(&fork) && views.bound_child_of(&fork).is_none() && !views.dispatch_failed(dispatch))
        .then_some(fork)
}

fn fixed_observation(views: &Views, dispatch: &DispatchId) -> Option<ObservedResult> {
    if let Some(observed) = views.observed_result(dispatch) {
        return Some(observed.clone());
    }
    if !views.closed_successfully(dispatch) || views.bound_sanitizer(dispatch).is_some() {
        return None;
    }
    Some(match views.admitted_body(dispatch) {
        Some(body) => ObservedResult::Available(RawResultDigest::of(body.as_str().as_bytes())),
        None => ObservedResult::Unavailable,
    })
}

fn branch_refusal(error: BranchError) -> TransitionError {
    match error {
        BranchError::NotDirectParent | BranchError::NotForked => TransitionError::NotForked,
        BranchError::AlreadyEnded => TransitionError::BranchEnded,
    }
}

/// Build the `DispatchOpened` fact for a call: its proposed committed label, the effects it would
/// commit on success, its occurrence (a repeat identical call is a new dispatch), and the subject
/// whose decision released it.
pub(crate) fn opened_dispatch(
    contract: &ToolAnnotation,
    views: &Views,
    call: &ResolvedCall,
    subject: crate::basis::SubjectKey,
    act: &ActEvidence,
) -> (DispatchId, Fact) {
    let digest = call.digest();
    let occurrence = views.dispatch_count(&digest);
    let dispatch = DispatchId::new(views.trajectory().clone(), digest, occurrence);
    let current = views.current_label();
    let fact = Fact::DispatchOpened {
        trajectory: views.trajectory().clone(),
        dispatch: dispatch.clone(),
        tool: call.tool().clone(),
        declaration: call.declaration_id(),
        arguments: call.canonical_arguments().clone(),
        proposed_label: current.combine(&call.output_label(contract, &current)),
        receiving: current.clone(),
        proposed_effects: contract.emits.clone(),
        annotation: call.annotation().cloned(),
        file_basis: call.file_basis().cloned(),
        evidence: act.pinned().clone(),
        subject,
    };
    (dispatch, fact)
}

pub(crate) struct SiblingRelease {
    pub(crate) dispatch: DispatchId,
    pub(crate) consumes: Option<crate::value::OfferId>,
    /// The staged derivation this release took, by the subject that staged it.
    pub(crate) takes: Option<crate::basis::SubjectKey>,
    /// The kind of fork a marked spawn's release prepares, at `ForkId::of(&dispatch)`.
    pub(crate) prepares_fork: Option<crate::transition::SpawnKind>,
    pub(crate) facts: Vec<Fact>,
    /// The pinned audience evidence this release's check read under: the act's, behind the
    /// spent approval's where one is consumed.
    pub(crate) evidence: AudienceEvidence,
}

/// Which batch a composition is running: the trajectory it belongs to, and the batch's own id.
/// Together they name each position's subject — what the openings record, so a repeat
/// answers with the dispatch its own position opened.
#[derive(Clone, Copy)]
pub(crate) struct ComposingBatch<'a> {
    pub(crate) trajectory: &'a TrajectoryId,
    pub(crate) id: &'a crate::transition::ProposalBatchId,
}

impl ComposingBatch<'_> {
    pub(crate) fn subject(&self, position: usize) -> crate::basis::SubjectKey {
        crate::basis::SubjectKey::Call {
            trajectory: self.trajectory.clone(),
            batch: self.id.clone(),
            position: position as u32,
        }
    }
}

pub(crate) enum ComposeRefusal {
    Malformed(EngineError),
    MembershipNeeded(crate::label::MembershipNeeded),
    Evidence(crate::audience::EvidenceRefusal),
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

pub(crate) fn membership_context<'e>(registry: &'e Registry, act: &'e ActEvidence) -> MembershipContext<'e> {
    let audience = registry.audience();
    MembershipContext::new(audience.within_assertions(), audience.providers(), &act.expansions)
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

/// The atoms a spent approval's consumption reads: each ruling's mandate over its covered
/// gaps. The bound output sanitizer reads its transition against the result when it applies.
fn approval_atoms(registry: &Registry, approval: &crate::projection::PreparedApproval) -> Vec<SymbolicAtom> {
    let providers = registry.audience().providers();
    approval
        .rulings
        .iter()
        .filter_map(|given| registry.authority(&given.authority).map(|authority| (authority, given)))
        .flat_map(|(authority, given)| authority.mandate.reads(&given.covers, providers))
        .collect()
}

/// The ordered in-batch composition, position by position: what each proposed sibling
/// does, and the records that say so. `None` at a position is a refusal.
#[allow(clippy::too_many_arguments)]
pub(crate) fn compose_batch<'a>(
    registry: &Registry,
    working: &mut std::borrow::Cow<'a, Projection>,
    batch: ComposingBatch<'_>,
    proposals: &[ResolvedCall],
    spawn: Option<SpawnMark>,
    approval: &impl Fn(&Views, &ResolvedCall) -> Option<crate::value::OfferId>,
    staged: &impl Fn(
        &Views,
        &ResolvedCall,
        &std::collections::BTreeSet<crate::basis::SubjectKey>,
    ) -> Vec<crate::basis::SubjectKey>,
    act: &ActEvidence,
) -> Result<Vec<Option<SiblingRelease>>, ComposeRefusal> {
    let trajectory = batch.trajectory;
    let singleton = proposals.len() == 1;
    let mut needed: Vec<SymbolicAtom> = Vec::new();
    // Owned only where a spent approval overlays its pins; every other sibling reads the act's.
    let mut per_call: Vec<(std::borrow::Cow<'_, ActEvidence>, Option<crate::value::OfferId>)> =
        Vec::with_capacity(proposals.len());
    {
        let views = working.view(trajectory);
        for (position, call) in proposals.iter().enumerate() {
            if contract_for_call(registry, call).is_err() {
                // Reported as malformed by the composition below, at its position.
                per_call.push((std::borrow::Cow::Borrowed(act), None));
                continue;
            }
            // An approval declaring a child's return policy spends only at the marked spawn's
            // position, and one declaring none only at an ordinary one.
            let marked = SpawnMark::marks(spawn, position);
            let spends = if singleton { approval(&views, call) } else { None }.filter(|offer| {
                views
                    .approval(offer)
                    .is_some_and(|prepared| prepared.return_policy.is_some() == marked)
            });
            let under = match spends.and_then(|offer| views.approval(&offer)) {
                Some(prepared) => {
                    act.inherit(&prepared.evidence).map_err(ComposeRefusal::Evidence)?;
                    let merged = act
                        .pinned()
                        .inheriting(&prepared.evidence)
                        .map_err(ComposeRefusal::Evidence)?;
                    let expansions = registry
                        .audience()
                        .expansions(&merged, act.principal.as_ref())
                        .map_err(ComposeRefusal::Evidence)?;
                    std::borrow::Cow::Owned(ActEvidence::validated(merged, expansions, act.principal.clone()))
                }
                None => std::borrow::Cow::Borrowed(act),
            };
            // The check's own reads surface from its three-valued evaluation below — asking
            // by the label's actual state, not the contract's whole vocabulary. Only a spent
            // approval's consumption reads atoms the evaluation never touches.
            if let Some(prepared) = spends.and_then(|offer| views.approval(&offer)) {
                needed.extend(
                    approval_atoms(registry, prepared)
                        .into_iter()
                        .filter(|atom| !under.expansions.answered(atom)),
                );
            }
            per_call.push((under, spends));
        }
    }
    if !needed.is_empty() {
        needed.sort();
        needed.dedup();
        return Err(ComposeRefusal::MembershipNeeded(crate::label::MembershipNeeded {
            needed,
        }));
    }
    let mut composed = Vec::with_capacity(proposals.len());
    // Whether any earlier sibling was refused, and so will be re-planned against the final state.
    let mut refused = false;
    // What earlier siblings of this batch have already taken. One derivation releases one call,
    // and a sibling's take does not reach `working` — only the batch's own `BasisAdvanced` does —
    // so two identical proposals would otherwise both stand on it.
    let mut taken: std::collections::BTreeSet<crate::basis::SubjectKey> = std::collections::BTreeSet::new();
    for (position, call) in proposals.iter().enumerate() {
        let (under, spends) = &per_call[position];
        let release = {
            let views = working.view(trajectory);
            let malformed = ComposeRefusal::Malformed;
            let contract = contract_for_call(registry, call).map_err(malformed)?;
            contract
                .parameters
                .validate(call.arguments())
                .map_err(|error| malformed(EngineError::InvalidCall(error)))?;
            let role = CallRole::of(spawn, position);
            // The derivations earlier blocks' remedies staged for exactly these bytes. Taking one
            // puts the call at that sanitizer's stage — its derived label and spent lineage —
            // while every other term is judged against current state, so a trajectory that
            // narrowed since the derivation landed re-gates the call rather than releasing it on
            // the terms it was staged under. A marked spawn takes none: its fork's return policy
            // rides on an approval, which a derivation does not carry.
            //
            // A derivation's label is a claim about the bytes, so where several stand for the same
            // bytes the call clears if any of them covers it: it is judged under each in subject
            // order and takes the first that clears, else it blocks under the first.
            let stages: Vec<(crate::basis::SubjectKey, CallStage)> = match role {
                CallRole::MarkedSpawn(_) => Vec::new(),
                CallRole::Ordinary => staged(&views, call, &taken)
                    .into_iter()
                    .filter_map(|subject| {
                        let recorded = views.recorded_candidate(&subject)?;
                        Some((
                            subject,
                            CallStage::of(Some(&recorded.derived), recorded.lineage.clone()),
                        ))
                    })
                    .collect(),
            };
            let context = membership_context(registry, under);
            let mut judged: Option<(crate::basis::SubjectKey, CheckOutcome)> = None;
            for (subject, stage) in stages {
                let outcome = check::evaluate(&contract, &views, call, &stage, role, &context)
                    .map_err(ComposeRefusal::MembershipNeeded)?;
                let clears = matches!(outcome, CheckOutcome::Allow);
                if clears || judged.is_none() {
                    judged = Some((subject, outcome));
                }
                if clears {
                    break;
                }
            }
            let (takes, outcome) = match judged {
                Some((subject, outcome)) => (Some(subject), outcome),
                None => (
                    None,
                    check::evaluate(&contract, &views, call, &CallStage::default(), role, &context)
                        .map_err(ComposeRefusal::MembershipNeeded)?,
                ),
            };
            let consumes = match outcome {
                CheckOutcome::Allow => None,
                CheckOutcome::Block(_) => match spends {
                    Some(offer) => Some(*offer),
                    None => {
                        refused = true;
                        composed.push(None);
                        continue;
                    }
                },
            };
            let subject = batch.subject(position);
            let (dispatch, opening) = opened_dispatch(&contract, &views, call, subject, under);
            let mut facts = Vec::new();
            if let Some(staged) = &takes {
                facts.push(Fact::CandidateConsumed {
                    trajectory: trajectory.clone(),
                    subject: staged.clone(),
                    dispatch: dispatch.clone(),
                });
            }
            if let Some(offer) = consumes {
                let prepared = views
                    .approval(&offer)
                    .expect("the currency test answered with an approval this state records")
                    .clone();
                facts.push(Fact::CallApprovalConsumed {
                    trajectory: trajectory.clone(),
                    offer,
                    dispatch: dispatch.clone(),
                });
                facts.extend(approved_release(
                    registry,
                    &contract,
                    &call.output_label(&contract, &views.current_label()),
                    trajectory,
                    &dispatch,
                    &prepared,
                    under,
                ));
            }
            facts.push(opening);
            let prepares_fork = if matches!(role, CallRole::MarkedSpawn(_)) {
                let return_policy = consumes
                    .and_then(|offer| views.approval(&offer))
                    .and_then(|prepared| prepared.return_policy.clone())
                    .expect(
                        "a marked spawn releases only by spending an approval that declares its child's return policy",
                    );
                let kind = spawn
                    .map(SpawnMark::kind)
                    .expect("a marked spawn's role comes from the batch's spawn mark");
                facts.push(Fact::ForkPrepared {
                    trajectory: trajectory.clone(),
                    fork: ForkId::of(&dispatch),
                    snapshot: views.freeze_basis(),
                    return_policy,
                    kind,
                });
                Some(kind)
            } else {
                None
            };
            if let Some(subject) = &takes {
                taken.insert(subject.clone());
            }
            SiblingRelease {
                dispatch,
                consumes,
                takes: takes.clone(),
                prepares_fork,
                facts,
                evidence: under.pinned().clone(),
            }
        };
        if position + 1 < proposals.len() || refused {
            for fact in &release.facts {
                working.to_mut().fold(fact);
            }
        }
        composed.push(Some(release));
    }
    // A spent approval's overlay context read on behalf of this act; the act's
    // operation-scope justification must count those asks.
    for (under, _) in &per_call {
        if let std::borrow::Cow::Owned(under) = under {
            act.absorb(under);
        }
    }
    Ok(composed)
}

fn contract_for_call<'a>(
    registry: &'a Registry,
    call: &'a ResolvedCall,
) -> Result<std::borrow::Cow<'a, ToolAnnotation>, EngineError> {
    registry.annotation_of(call).ok_or_else(|| {
        if registry.provider_run_annotation(call.tool()).is_some() {
            EngineError::ProviderRunTool(call.tool().as_str().to_string())
        } else {
            EngineError::UnknownTool(call.tool().as_str().to_string())
        }
    })
}

#[cfg(test)]
mod tests;
