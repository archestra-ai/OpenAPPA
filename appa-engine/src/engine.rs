//! The engine: a pure function of the log's views and the immutable registry.

use thiserror::Error;

use crate::admit::{self, AdmitError, ResultAdmission};
use crate::audience::AudienceEvidence;
use crate::branch::{self, BranchError};
use crate::candidate::{CallStage, ConfinedFrom, DerivedCandidate, DerivedVia, SanitizerLineage};
use crate::check::{self, CheckOutcome, Narrowing, RawBlock};
use crate::contract::ToolAnnotation;
use crate::execute::{self, PlanError};
use crate::fact::{Fact, ObservedResult, ReturnDerivation, ReturnPolicy, ReturnRejection};
use crate::label::{Expansions, Label, MembershipContext, SymbolicAtom};
use crate::names::{AuthorityName, SanitizerName};
use crate::params::{ArgumentError, CanonicalArguments};
use crate::plan::{self, BlockedCall, PlannedBlock};
use crate::profile::{self, DeploymentPolicy, DeploymentProfile, OpenVector, PolicyDialectVersion, PolicyIdentityV1};
use crate::projection::Projection;
use crate::projection::Views;
use crate::registry::{LoadError, Registry, ToolKind};
use crate::transition::{
    Blocked, ChildFollowUp, ChildReport, ChildSubmission, Confined, EngineDecision, EngineEvent, EngineView, Evidence,
    EvidenceRequest, FollowUp, ForkBinding, OfferExecution, OfferFollowUp, OfferOutcome, OutcomeBody, OutcomeFollowUp,
    PendingReturnStage, ProposalBatch, Released, Sequence, Settled, SettledOutcome, SpawnMark, ToolOutcome, ToolReport,
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
    #[error("invalid call: the marked spawn's return_schema does not compile: {0}")]
    InvalidReturnSchema(crate::shape::ShapeError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForkStatus {
    Unprepared,
    Prepared,
    Bound(TrajectoryId),
    Failed,
    ParentEnded,
}

/// The pure decision core, owning its static capability: the immutable registry (which carries
/// the validated deployment profile), the deployment's immutable child-return binding, and the
/// policy identity the durable opening binds.
#[derive(Clone, Debug)]
pub struct Engine {
    registry: Registry,
    identity: PolicyIdentityV1,
    dialect: PolicyDialectVersion,
    child_return: ReturnPolicy,
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

/// A refused call and the block the check produced for it — what a remedy menu is planned over.
#[derive(Clone, Copy)]
/// What a staged return's remedy menu is computed over: the crossing the child owes, at the
/// label and body the stage stands on, and the sanitizer lineage that reached it.
struct ReturnStageInput<'a> {
    child: &'a TrajectoryId,
    label: &'a Label,
    body: &'a ValueBody,
    residual: &'a Narrowing,
    lineage: &'a SanitizerLineage,
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
            child_return,
            profile: declaration,
        } = policy;
        let profile = DeploymentProfile::declare(declaration.clone())?;
        let registry = Registry::build(config, planner_cap, profile)?;
        profile::validate_coverage(&registry, &declaration, &child_return)?;
        let identity = PolicyIdentityV1::of_registry(&registry, &child_return);
        Ok(Engine {
            registry,
            identity,
            dialect,
            child_return,
        })
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    pub fn profile(&self) -> &DeploymentProfile {
        self.registry.profile()
    }

    pub fn child_return(&self) -> &ReturnPolicy {
        &self.child_return
    }

    pub fn identity(&self) -> PolicyIdentityV1 {
        self.identity
    }

    /// The open vectors derived from the validated declaration and the registered tool set —
    /// recomputed, never stored, so they cannot drift from the profile.
    pub fn open_vectors(&self) -> Vec<OpenVector> {
        let tools = self
            .registry
            .tool_names()
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
        // The operation-scope test, after the decision's reads are complete: every pinned
        // entry is inherited or answers an ask this act actually made.
        self.registry
            .audience()
            .only_requested(&act.evidence, &act.inherited.borrow(), &act.expansions.reads())?;
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
        self.act_evidence(merged, inherited)
    }

    /// Validate one act's merged evidence and recompute the answers it carries. Junk or
    /// foreign evidence never enters a decision, whatever answers it would add.
    fn act_evidence(
        &self,
        evidence: AudienceEvidence,
        inherited: AudienceEvidence,
    ) -> Result<ActEvidence, TransitionError> {
        let expansions = self.registry.audience().expansions(&evidence)?;
        Ok(ActEvidence {
            evidence,
            expansions,
            inherited: std::cell::RefCell::new(inherited),
        })
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
                None if recorded.plan.required.is_empty() => Ok(OfferConsult::Accept),
                None => Ok(OfferConsult::Authorities {
                    call: self.offer_call(&views, recorded),
                    required: recorded.plan.required.clone(),
                }),
            },
            crate::basis::SubjectKey::Return(id) => match recorded.plan.hop() {
                Some(sanitizer) if sanitizer.is_attest_schema() => Ok(OfferConsult::Accept),
                Some(sanitizer) => {
                    let (source, body) = match views.candidate(&recorded.subject) {
                        Some(DerivedCandidate::Return { value, .. }) => (
                            crate::value::RawResultDigest::of(value.body.as_str().as_bytes()),
                            value.body.clone(),
                        ),
                        _ => {
                            let body = views
                                .pending_return(id)
                                .ok_or(TransitionError::StaleOffer)?
                                .body()
                                .clone();
                            (crate::value::RawResultDigest::of(body.as_str().as_bytes()), body)
                        }
                    };
                    Ok(OfferConsult::Sanitizer {
                        sanitizer: sanitizer.clone(),
                        source,
                        body,
                        tool: None,
                    })
                }
                None => Ok(OfferConsult::Accept),
            },
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
                None => Ok(OfferConsult::Accept),
            },
            crate::basis::SubjectKey::Approval(_) => Ok(OfferConsult::Stale),
        }
    }

    /// The fork one child was bound to, or `None` for a trajectory that never forked.
    pub fn fork_of(&self, view: &EngineView, child: &TrajectoryId) -> Option<crate::value::ForkId> {
        view.views(child)?.fork_of(child).cloned()
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
        ForkStatus::Prepared
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
        let parent = view
            .projection()
            .prepared_fork(&binding.fork)
            .ok_or(TransitionError::UnbindableFork)?
            .parent
            .clone();
        let views = view.projection().view(&parent);
        if let Some(bound) = view.projection().bound_child(&binding.fork) {
            return if bound == &binding.child {
                Ok(EngineDecision {
                    append: None,
                    follow_up: FollowUp::Fork {
                        child: binding.child.clone(),
                    },
                })
            } else {
                Err(TransitionError::UnbindableFork)
            };
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
            return self.ended_return(view, &views, report, act);
        }
        let body = match &report.submission {
            ChildSubmission::Void => {
                let batch = branch::submit_void_return(&views, child).map_err(branch_refusal)?;
                return Ok(EngineDecision {
                    append: Some(self.decided(view, return_act(child), batch)?),
                    follow_up: FollowUp::Child(ChildFollowUp::Ended),
                });
            }
            ChildSubmission::Value { body } => match views.return_shape_of(child) {
                Some(shape) => match shape.validate(body.as_str()) {
                    Ok(canonical) => ValueBody::new(canonical),
                    Err(mismatch) => {
                        if let Some(ReturnPolicy::Sanitized(name)) = views.return_policy_of(child)
                            && name.is_attest_schema()
                        {
                            return self.rejecting(
                                view,
                                child,
                                &ChildReturnId::new(child.clone(), 0),
                                &report.fork,
                                RawResultDigest::of(body.as_str().as_bytes()),
                                ReturnRejection::PreconditionUnmet,
                                Vec::new(),
                                act,
                            );
                        }
                        return Err(TransitionError::ReturnShapeMismatch(mismatch));
                    }
                },
                None => body.clone(),
            },
        };
        let fork = report.fork.clone();
        let working = std::borrow::Cow::Borrowed(projection);
        let cast_facts: Vec<Fact> = Vec::new();
        let views = working.view(&parent);
        let id = ChildReturnId::new(child.clone(), 0);
        let policy = views.return_policy_of(child).ok_or(TransitionError::NotForked)?.clone();
        match policy {
            ReturnPolicy::Raw => self.raw_return(
                view,
                &views,
                child,
                &id,
                &fork,
                body,
                cast_facts,
                report.offer_nonce,
                act,
            ),
            ReturnPolicy::Sanitized(name) => self.sanitized_return(
                view,
                &views,
                child,
                &id,
                &fork,
                &name,
                body,
                cast_facts,
                &report.evidence,
                report.offer_nonce,
                act,
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn raw_return(
        &self,
        view: &EngineView,
        views: &Views,
        child: &TrajectoryId,
        id: &ChildReturnId,
        fork: &ForkId,
        body: ValueBody,
        mut facts: Vec<Fact>,
        nonce: crate::value::OfferNonce,
        act: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        match branch::submit_child_return(views, child, &body, &act.evidence).map_err(branch_refusal)? {
            branch::RawCrossing::Merged(crossing) => {
                facts.extend(crossing);
                Ok(EngineDecision {
                    append: Some(self.decided(view, return_act(child), facts)?),
                    follow_up: FollowUp::Child(ChildFollowUp::Merged { admitted: body }),
                })
            }
            branch::RawCrossing::Narrows(narrowing) => {
                let fold = views.branch_label(child);
                let candidate = fold.clone();
                let lineage = SanitizerLineage::default();
                let menu = self.return_menu(
                    views,
                    ReturnStageInput {
                        child,
                        label: &candidate,
                        body: &body,
                        residual: &narrowing,
                        lineage: &lineage,
                    },
                    act,
                )?;
                let stage = menu;
                facts.push(Fact::ReturnSubmitted {
                    trajectory: child.clone(),
                    id: id.clone(),
                    fork: fork.clone(),
                    parent: views.trajectory().clone(),
                    label: fold,
                    digest: RawResultDigest::of(body.as_str().as_bytes()),
                    body,
                    policy: ReturnPolicy::Raw,
                    evidence: act.pinned(),
                });
                let (batch, staged) = self.pending_stage(
                    view,
                    views,
                    return_act(child),
                    nonce,
                    id,
                    fork,
                    candidate,
                    narrowing,
                    stage,
                    facts,
                    act,
                )?;
                Ok(EngineDecision {
                    append: Some(batch),
                    follow_up: FollowUp::Child(ChildFollowUp::Pending(Box::new(staged))),
                })
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn sanitized_return(
        &self,
        view: &EngineView,
        views: &Views,
        child: &TrajectoryId,
        id: &ChildReturnId,
        fork: &ForkId,
        name: &SanitizerName,
        body: ValueBody,
        mut facts: Vec<Fact>,
        evidence: &[Evidence],
        nonce: crate::value::OfferNonce,
        act: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        let registered = self
            .registry
            .sanitizer(name)
            .ok_or(TransitionError::SanitizerUnapplicable)?;
        let fold = views.branch_label(child);
        let digest = RawResultDigest::of(body.as_str().as_bytes());
        if name.is_attest_schema() && !plan::attest_applicable(views, child, &body, &registered.transition) {
            return self.rejecting(
                view,
                child,
                id,
                fork,
                digest,
                ReturnRejection::PreconditionUnmet,
                facts,
                act,
            );
        }
        // An undecided mandate is the runtime's ask, never a rejection.
        if registered.derive_output(&fold, &[], &self.context(act))?.is_none() {
            return self.rejecting(view, child, id, fork, digest, ReturnRejection::MandateUnmet, facts, act);
        }
        // Applicability holds: custody transfers, and the branch ends.
        facts.push(Fact::ReturnSubmitted {
            trajectory: child.clone(),
            id: id.clone(),
            fork: fork.clone(),
            parent: views.trajectory().clone(),
            label: fold.clone(),
            digest,
            body: body.clone(),
            policy: ReturnPolicy::Sanitized(name.clone()),
            evidence: act.pinned(),
        });
        if name.is_attest_schema() {
            let receiving = views.current_label().clone();
            return self.mandatory_derivation(
                view, views, id, fork, name, &fold, receiving, digest, body, facts, nonce, act,
            );
        }
        let derived = evidence.iter().find_map(|item| match item {
            Evidence::Sanitizer {
                sanitizer,
                source,
                derived,
            } if sanitizer == name && source == &digest => Some(derived.clone()),
            _ => None,
        });
        let Some(derived) = derived else {
            return Ok(EngineDecision {
                append: Some(self.decided(view, return_act(child), facts)?),
                follow_up: FollowUp::Child(ChildFollowUp::Resolve(EvidenceRequest::Sanitizer {
                    sanitizer: name.clone(),
                    source: digest,
                    body,
                })),
            });
        };
        // The receiving bound the submission pins at this same fold step.
        let receiving = views.current_label().clone();
        self.mandatory_derivation(
            view, views, id, fork, name, &fold, receiving, digest, derived, facts, nonce, act,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn mandatory_derivation(
        &self,
        view: &EngineView,
        views: &Views,
        id: &ChildReturnId,
        fork: &ForkId,
        name: &SanitizerName,
        fold: &Label,
        receiving: Label,
        digest: RawResultDigest,
        derived: ValueBody,
        mut facts: Vec<Fact>,
        nonce: crate::value::OfferNonce,
        act: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        let child = id.child();
        let registered = self
            .registry
            .sanitizer(name)
            .ok_or(TransitionError::SanitizerUnapplicable)?;
        let label = registered
            .derive_output(fold, &[], &self.context(act))?
            .ok_or(TransitionError::SanitizerUnapplicable)?;
        let residual = admit::confined_residual(&receiving, &label);
        let lineage = SanitizerLineage::default()
            .extend(name.clone())
            .expect("an empty lineage spends no sanitizer yet");
        facts.push(Fact::CandidateDerived {
            trajectory: views.trajectory().clone(),
            subject: crate::basis::SubjectKey::Return(id.clone()),
            via: DerivedVia {
                name: name.clone(),
                transition: registered.transition.applied(),
            },
            derived: DerivedCandidate::Return {
                source: digest,
                from: ConfinedFrom::Bound,
                value: LabeledValue::new(derived.clone(), label.clone()),
                residual: residual.clone(),
            },
            lineage: lineage.clone(),
            evidence: act.pinned(),
        });
        let Some(residual) = residual else {
            // The derivation narrows nothing: candidate and merge land atomically.
            facts.extend(branch::crossing_facts(
                views,
                child,
                LabeledValue::new(derived.clone(), label),
                ReturnDerivation::Sanitized {
                    sanitizer: name.clone(),
                    raw_digest: digest,
                    transition: registered.transition.applied(),
                },
                None,
                act.pinned(),
            ));
            return Ok(EngineDecision {
                append: Some(self.decided(view, return_act(child), facts)?),
                follow_up: FollowUp::Child(ChildFollowUp::Merged { admitted: derived }),
            });
        };
        let menu = self.return_menu(
            views,
            ReturnStageInput {
                child,
                label: &label,
                body: &derived,
                residual: &residual,
                lineage: &lineage,
            },
            act,
        )?;
        let stage = menu;
        let (batch, staged) = self.pending_stage(
            view,
            views,
            return_act(child),
            nonce,
            id,
            fork,
            label,
            residual,
            stage,
            facts,
            act,
        )?;
        Ok(EngineDecision {
            append: Some(batch),
            follow_up: FollowUp::Child(ChildFollowUp::Pending(Box::new(staged))),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn rejecting(
        &self,
        view: &EngineView,
        child: &TrajectoryId,
        id: &ChildReturnId,
        fork: &ForkId,
        digest: RawResultDigest,
        reason: ReturnRejection,
        mut facts: Vec<Fact>,
        act: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        facts.push(Fact::ReturnRejected {
            trajectory: child.clone(),
            id: id.clone(),
            fork: fork.clone(),
            digest,
            reason: reason.clone(),
            evidence: act.pinned(),
        });
        Ok(EngineDecision {
            append: Some(self.decided(view, return_act(child), facts)?),
            follow_up: FollowUp::Child(ChildFollowUp::Rejected { reason }),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn pending_stage(
        &self,
        view: &EngineView,
        views: &Views,
        act: crate::basis::DecidedAct,
        nonce: crate::value::OfferNonce,
        id: &ChildReturnId,
        fork: &ForkId,
        label: Label,
        residual: Narrowing,
        stage: Vec<plan::ExecutableRemedyPlan>,
        mut facts: Vec<Fact>,
        evidence: &ActEvidence,
    ) -> Result<(ValidatedFactBatch, PendingReturnStage), TransitionError> {
        let subject = crate::basis::SubjectKey::Return(id.clone());
        let call = views
            .dispatch_call(fork.dispatch())
            .ok_or(TransitionError::UnknownDispatch)?
            .digest();
        let advance = Sequence::advance_of(self, view, &facts);
        let (_, offers, opened) = self.open_offers(
            views,
            Opening {
                act: &act,
                advance: &advance,
                nonce: &nonce,
                subject: &subject,
            },
            &call,
            &stage,
            evidence,
        );
        facts.extend(opened);
        let batch = self.declaring(act, advance, facts);
        Ok((
            self.seal(view, batch)?,
            PendingReturnStage {
                id: id.clone(),
                label,
                residual,
                offers,
            },
        ))
    }

    fn ended_return(
        &self,
        view: &EngineView,
        views: &Views,
        report: &ChildReport,
        act: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        let child = &report.child;
        let id = ChildReturnId::new(child.clone(), 0);
        let comparable = |body: &ValueBody| match views.return_shape_of(child) {
            Some(shape) => shape.validate(body.as_str()).ok().map(ValueBody::new),
            None => Some(body.clone()),
        };
        if let Some(submitted) = views.submitted_return(&id) {
            let ChildSubmission::Value { body } = &report.submission else {
                return Err(TransitionError::BranchEnded);
            };
            if comparable(body)
                .is_none_or(|canonical| RawResultDigest::of(canonical.as_str().as_bytes()) != submitted.digest)
            {
                return Err(TransitionError::BranchEnded);
            }
            if let Some(crossed) = views.child_return(&id) {
                return Ok(EngineDecision {
                    append: None,
                    follow_up: FollowUp::Child(ChildFollowUp::Merged {
                        admitted: crossed.body.clone(),
                    }),
                });
            }
            return self.continue_pending(view, report, &id, act);
        }
        if let Some(rejected) = views.rejected_return(&id) {
            let same = match &report.submission {
                ChildSubmission::Value { body } => {
                    RawResultDigest::of(body.as_str().as_bytes()) == rejected.digest
                        || comparable(body).is_some_and(|canonical| {
                            RawResultDigest::of(canonical.as_str().as_bytes()) == rejected.digest
                        })
                }
                ChildSubmission::Void => false,
            };
            return match same {
                true => Ok(EngineDecision {
                    append: None,
                    follow_up: FollowUp::Child(ChildFollowUp::Rejected {
                        reason: rejected.reason.clone(),
                    }),
                }),
                false => Err(TransitionError::BranchEnded),
            };
        }
        let recorded = views.child_return(&id).cloned();
        match (&report.submission, recorded) {
            (ChildSubmission::Void, None) => Ok(EngineDecision {
                append: None,
                follow_up: FollowUp::Child(ChildFollowUp::Ended),
            }),
            (ChildSubmission::Value { body }, Some(crossed)) => {
                match comparable(body).as_ref() == Some(&crossed.body) {
                    true => Ok(EngineDecision {
                        append: None,
                        follow_up: FollowUp::Child(ChildFollowUp::Merged { admitted: crossed.body }),
                    }),
                    false => Err(TransitionError::BranchEnded),
                }
            }
            _ => Err(TransitionError::BranchEnded),
        }
    }

    fn continue_pending(
        &self,
        view: &EngineView,
        report: &ChildReport,
        id: &ChildReturnId,
        act: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        let child = &report.child;
        let working = std::borrow::Cow::Borrowed(view.projection());
        let cast_facts: Vec<Fact> = Vec::new();
        let pending = working
            .view(child)
            .pending_return(id)
            .expect("the caller proved the submission pending")
            .clone();
        let views = working.view(&pending.parent);
        let fold = views.branch_label(child);
        let subject = crate::basis::SubjectKey::Return(id.clone());
        match (&pending.policy, views.candidate(&subject).cloned()) {
            (ReturnPolicy::Sanitized(name), None) => {
                let derived = report.evidence.iter().find_map(|item| match item {
                    Evidence::Sanitizer {
                        sanitizer,
                        source,
                        derived,
                    } if sanitizer == name && source == &pending.digest => Some(derived.clone()),
                    _ => None,
                });
                match derived {
                    None => {
                        let append = match cast_facts.is_empty() {
                            true => None,
                            false => Some(self.decided(view, return_act(child), cast_facts)?),
                        };
                        Ok(EngineDecision {
                            append,
                            follow_up: FollowUp::Child(ChildFollowUp::Resolve(EvidenceRequest::Sanitizer {
                                sanitizer: name.clone(),
                                source: pending.digest,
                                body: pending.body().clone(),
                            })),
                        })
                    }
                    Some(derived) => self.mandatory_derivation(
                        view,
                        &views,
                        id,
                        &pending.fork,
                        name,
                        &fold,
                        pending.receiving.clone(),
                        pending.digest,
                        derived,
                        cast_facts,
                        report.offer_nonce,
                        act,
                    ),
                }
            }
            // A candidate stands and still owes its residual: the stage as it is now.
            (
                _,
                Some(DerivedCandidate::Return {
                    value,
                    residual: Some(residual),
                    ..
                }),
            ) => self.pending_answer(
                view,
                &views,
                id,
                &pending,
                value.label.clone(),
                value.body.clone(),
                residual,
                report.offer_nonce,
                cast_facts,
                act,
            ),
            (_, Some(_)) => unreachable!("a settled return candidate crossed in its own batch"),
            // The submitted fold itself is the raw candidate.
            (ReturnPolicy::Raw, None) => {
                let label = fold.clone();
                let to = pending.receiving.combine(&label);
                // Custody transferred only for a narrowing submission, and a fold only narrows.
                let residual = Narrowing {
                    from: pending.receiving.clone(),
                    to,
                };
                self.pending_answer(
                    view,
                    &views,
                    id,
                    &pending,
                    label,
                    pending.body().clone(),
                    residual,
                    report.offer_nonce,
                    cast_facts,
                    act,
                )
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn pending_answer(
        &self,
        view: &EngineView,
        views: &Views,
        id: &ChildReturnId,
        pending: &crate::projection::SubmittedReturn,
        label: Label,
        body: ValueBody,
        residual: Narrowing,
        nonce: crate::value::OfferNonce,
        facts: Vec<Fact>,
        act: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        let subject = crate::basis::SubjectKey::Return(id.clone());
        if facts.is_empty()
            && let Some((_, offers)) = views.pending_block(&subject)
        {
            return Ok(EngineDecision {
                append: None,
                follow_up: FollowUp::Child(ChildFollowUp::Pending(Box::new(PendingReturnStage {
                    id: id.clone(),
                    label,
                    residual,
                    offers,
                }))),
            });
        }
        let lineage = views.lineage(&subject);
        let menu = self.return_menu(
            views,
            ReturnStageInput {
                child: id.child(),
                label: &label,
                body: &body,
                residual: &residual,
                lineage: &lineage,
            },
            act,
        )?;
        let stage = menu;
        let (batch, staged) = self.pending_stage(
            view,
            views,
            return_act(id.child()),
            nonce,
            id,
            &pending.fork,
            label,
            residual,
            stage,
            facts,
            act,
        )?;
        Ok(EngineDecision {
            append: Some(batch),
            follow_up: FollowUp::Child(ChildFollowUp::Pending(Box::new(staged))),
        })
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
        let observed = match &report.outcome {
            ToolOutcome::Success {
                body: OutcomeBody::Available(raw),
            } => Some(ObservedResult::Available(RawResultDigest::of(raw.as_str().as_bytes()))),
            ToolOutcome::Success {
                body: OutcomeBody::Unavailable,
            } => Some(ObservedResult::Unavailable),
            ToolOutcome::Failure | ToolOutcome::Indeterminate => None,
        };
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

        let admission = match &report.outcome {
            ToolOutcome::Failure => ResultAdmission::Failure,
            ToolOutcome::Indeterminate => ResultAdmission::Indeterminate,
            ToolOutcome::Success {
                body: OutcomeBody::Unavailable,
            } => ResultAdmission::SuccessNoValue,
            ToolOutcome::Success {
                body: OutcomeBody::Available(raw),
            } => {
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
                            Evidence::Sanitizer { .. } | Evidence::Rewrite { .. } => None,
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
                        // The bound sanitizer's application reads its mandate: an undecided
                        // atom is the runtime's ask, never an unapplicable sanitizer.
                        if let Some(registered) = self.registry.sanitizer(&sanitizer) {
                            require_atoms(act, registered.needed_atoms(self.registry.audience().providers()))?;
                        }
                        let (transition, candidate, lineage) = crate::admit::bound_candidate(
                            &self.registry,
                            &views,
                            dispatch,
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
                                via: DerivedVia {
                                    name: sanitizer,
                                    transition,
                                },
                                derived: candidate,
                                lineage,
                                evidence: act.pinned(),
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
            &act.evidence,
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
        Ok(EngineDecision {
            append: Some(self.decided(view, crate::basis::DecidedAct::Outcome(dispatch.clone()), batch)?),
            follow_up: FollowUp::Outcome(OutcomeFollowUp::Closed { admitted }),
        })
    }

    /// The remedy menu a staged return offers, with the group requirement raised before the menu
    /// reads anything. A `Resolve` answer is the caller's to continue: the three callers that can
    /// see one continue differently, so this never decides for them.
    fn return_menu(
        &self,
        views: &Views,
        stage: ReturnStageInput<'_>,
        act: &ActEvidence,
    ) -> Result<Vec<plan::ExecutableRemedyPlan>, TransitionError> {
        require_atoms(act, plan::return_stage_atoms(&self.registry, stage.lineage))?;
        Ok(plan::return_stage(
            &self.registry,
            views,
            stage.child,
            stage.label,
            stage.body,
            stage.residual,
            stage.lineage,
            &self.context(act),
        )?)
    }

    /// The remedy menu a confined result's stage offers, with the group requirement raised before
    /// the menu reads anything.
    fn confined_menu(
        &self,
        contract: &ToolAnnotation,
        receiving: &Label,
        label: &Label,
        residual: &Narrowing,
        lineage: &SanitizerLineage,
        act: &ActEvidence,
    ) -> Result<Vec<plan::ExecutableRemedyPlan>, TransitionError> {
        require_atoms(act, plan::confined_stage_atoms(&self.registry, contract, lineage))?;
        Ok(plan::confined_stage(
            &self.registry,
            contract,
            receiving,
            label,
            residual,
            lineage,
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
        let stage = self.confined_menu(&contract, &receiving, &value.label, &residual, &lineage, evidence)?;
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
        let stage = self.confined_menu(&contract, &receiving, &value.label, &residual, &lineage, evidence)?;
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
            act.evidence.inheriting(&recorded.evidence)?,
            AudienceEvidence::default(),
        )?;
        let follow_up = self.decided_follow_up(views, batch, &proposals, &recorded.released, &under)?;
        act.expansions.absorb_reads(&under.expansions);
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
                            evidence: act.pinned(),
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
            &self.child_return,
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
                    fork: release.prepares_fork.clone(),
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
            evidence: act.pinned(),
        });
        facts.extend(composed.iter().flatten().flat_map(|release| release.facts.clone()));
        // What this decision moves, derived before the offers that have to record where it lands.
        // The declaration prepended below re-derives it over the whole batch; an offer record
        // moves nothing, so the two agree by construction.
        let advance = Sequence::advance_of(self, view, &facts);
        let final_views = working.view(&batch.trajectory);
        let mut refused: Vec<(usize, &ResolvedCall, ToolAnnotation, RawBlock, plan::CallRole)> = Vec::new();
        for (position, call) in composed
            .iter()
            .enumerate()
            .filter(|(_, release)| release.is_none())
            .map(|(position, _)| (position, &proposals[position]))
        {
            let contract = self.validated_contract(call)?.into_owned();
            let raw = match check::evaluate(&contract, &final_views, call, &CallStage::default(), &self.context(act))? {
                CheckOutcome::Block(raw) => raw,
                CheckOutcome::Allow => {
                    unreachable!("an in-batch release only ever adds gaps to a refused sibling's block")
                }
            };
            let role = match batch.spawn == Some(SpawnMark::at(position)) {
                true => plan::CallRole::MarkedSpawn,
                false => plan::CallRole::Ordinary,
            };
            refused.push((position, call, contract, raw, role));
        }
        let block_stage: Vec<SymbolicAtom> = refused
            .iter()
            .flat_map(|(_, _, contract, raw, role)| plan::block_atoms(&self.registry, contract, raw, *role))
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
        let proposals: Vec<ResolvedCall> = batch
            .proposals
            .iter()
            .enumerate()
            .map(|(position, proposed)| {
                self.resolve_call(proposed.tool.clone(), &proposed.arguments)
                    .map(|call| call.with_annotation(proposed.annotation.clone()))
                    .map_err(|error| (position, error))
            })
            .collect::<Result<_, _>>()?;
        if let Some(mark) = batch.spawn
            && let Some(call) = proposals.get(mark.index())
        {
            marked_return_shape(call).map_err(|error| (mark.index(), error))?;
        }
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
            crate::basis::SubjectKey::Return(id) => (
                views.trajectory(),
                crate::value::BlockId::of_return(nonce, id, basis.subject),
            ),
            // A prepared approval is spent by releasing its call, never by an offer of its own.
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
                evidence: under.pinned(),
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
                act.inherit(&pinned)?;
                let under = self.act_evidence(act.evidence.inheriting(&pinned)?, AudienceEvidence::default())?;
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
                    match check::evaluate(&contract, views, &candidate, &stage, &self.context(under))? {
                        CheckOutcome::Block(raw) => {
                            let role = views.call_role(&subject);
                            require_atoms(under, plan::block_atoms(&self.registry, &contract, &raw, role))?;
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
            act.expansions.absorb_reads(&under.expansions);
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
        if let crate::basis::SubjectKey::Return(id) = &recorded.subject {
            let id = id.clone();
            return self.decide_return(view, &views, execution, &recorded, &id, act);
        }
        let call = self.offer_call(&views, &recorded);
        let contract = self.validated_contract(&call)?;
        let stage = views.call_stage(&recorded.subject);
        let role = views.call_role(&recorded.subject);
        let live = match check::evaluate(&contract, &views, &call, &stage, &self.context(act))? {
            CheckOutcome::Block(raw) => {
                require_atoms(act, plan::block_atoms(&self.registry, &contract, &raw, role))?;
                require_atoms(act, plan::plan_atoms(&self.registry, &contract, &recorded.plan))?;
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
        let stage = self.confined_menu(&contract, &receiving, &value.label, &residual, &lineage, act)?;
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
            &act.evidence,
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
            via: crate::candidate::DerivedVia {
                name: sanitizer.clone(),
                transition: registered.transition.applied(),
            },
            derived: DerivedCandidate::Result {
                dispatch: dispatch.clone(),
                source: source_digest,
                from: ConfinedFrom::Offer(execution.offer),
                value: crate::value::LabeledValue::new(body.clone(), label),
                residual,
            },
            lineage,
            evidence: act.pinned(),
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
            &act.evidence,
        )
        .unwrap_or_else(|error| unreachable!("the confined stage admits what this act just derived: {error}"));
        facts.extend(admitted);
        let value = crossed(&facts);
        Ok(EngineDecision {
            append: Some(self.decided(view, crate::basis::DecidedAct::Offer(execution.offer), facts)?),
            follow_up: FollowUp::Offer(OfferFollowUp::Admitted { value }),
        })
    }

    fn decide_return(
        &self,
        view: &EngineView,
        views: &Views,
        execution: &OfferExecution,
        recorded: &crate::projection::RecordedOffer,
        id: &ChildReturnId,
        act: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        let subject = crate::basis::SubjectKey::Return(id.clone());
        let Some(pending) = views.pending_return(id).cloned() else {
            return self.invalidated(view, execution, recorded);
        };
        let fold = views.branch_label(id.child());
        let lineage = views.lineage(&subject);
        let standing = match views.candidate(&subject).cloned() {
            Some(DerivedCandidate::Return {
                value,
                residual: Some(residual),
                ..
            }) => Some((value, residual)),
            Some(_) => return self.invalidated(view, execution, recorded),
            None if pending.policy == ReturnPolicy::Raw => None,
            None => return self.invalidated(view, execution, recorded),
        };
        let (label, body, residual) = match &standing {
            Some((value, residual)) => (value.label.clone(), value.body.clone(), residual.clone()),
            None => (
                fold.clone(),
                pending.body().clone(),
                Narrowing {
                    from: pending.receiving.clone(),
                    to: pending.receiving.combine(&fold),
                },
            ),
        };
        let menu = self.return_menu(
            views,
            ReturnStageInput {
                child: id.child(),
                label: &label,
                body: &body,
                residual: &residual,
                lineage: &lineage,
            },
            act,
        )?;
        let stage = menu;
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
            (OfferOutcome::Approved(evidence), Some(name)) if name.is_attest_schema() && evidence.is_empty() => {
                let applied = Evidence::Sanitizer {
                    sanitizer: name.clone(),
                    source: RawResultDigest::of(body.as_str().as_bytes()),
                    derived: body.clone(),
                };
                self.return_hop(
                    view,
                    views,
                    execution,
                    id,
                    &pending,
                    standing.as_ref().map(|(value, _)| value),
                    &fold,
                    &lineage,
                    name,
                    &applied,
                    facts,
                    act,
                )
            }
            (OfferOutcome::Derived(evidence), Some(sanitizer)) if !sanitizer.is_attest_schema() => self.return_hop(
                view,
                views,
                execution,
                id,
                &pending,
                standing.as_ref().map(|(value, _)| value),
                &fold,
                &lineage,
                sanitizer,
                evidence,
                facts,
                act,
            ),
            (OfferOutcome::Approved(evidence), None) if evidence.is_empty() => self.accept_return(
                view,
                views,
                execution,
                id,
                &pending,
                standing.map(|(value, _)| value),
                &fold,
                &lineage,
                residual,
                facts,
                act,
            ),
            _ => Err(TransitionError::PlanOutcomeMismatch),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn accept_return(
        &self,
        view: &EngineView,
        views: &Views,
        execution: &OfferExecution,
        id: &ChildReturnId,
        pending: &crate::projection::SubmittedReturn,
        candidate: Option<crate::value::LabeledValue>,
        fold: &Label,
        lineage: &SanitizerLineage,
        residual: Narrowing,
        mut facts: Vec<Fact>,
        act: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        let (value, derivation) = match candidate {
            Some(value) => {
                let sanitizer = lineage
                    .names()
                    .last()
                    .expect("a return candidate's lineage names the sanitizer that derived it")
                    .clone();
                let subject = crate::basis::SubjectKey::Return(id.clone());
                let Some(crate::candidate::DerivedVia { transition, .. }) = views.candidate_via(&subject) else {
                    return Err(TransitionError::SanitizerUnapplicable);
                };
                (
                    value,
                    ReturnDerivation::Sanitized {
                        sanitizer,
                        raw_digest: pending.digest,
                        transition: transition.clone(),
                    },
                )
            }
            None => (
                crate::value::LabeledValue::new(pending.body().clone(), fold.clone()),
                ReturnDerivation::Raw,
            ),
        };
        let admitted = value.body.clone();
        facts.extend(branch::crossing_facts(
            views,
            id.child(),
            value,
            derivation,
            Some(residual),
            act.pinned(),
        ));
        Ok(EngineDecision {
            append: Some(self.decided(view, crate::basis::DecidedAct::Offer(execution.offer), facts)?),
            follow_up: FollowUp::Offer(OfferFollowUp::Admitted { value: admitted }),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn return_hop(
        &self,
        view: &EngineView,
        views: &Views,
        execution: &OfferExecution,
        id: &ChildReturnId,
        pending: &crate::projection::SubmittedReturn,
        predecessor: Option<&crate::value::LabeledValue>,
        fold: &Label,
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
        let (from_label, source_digest) = match predecessor {
            Some(value) => (value.label.clone(), RawResultDigest::of(value.body.as_str().as_bytes())),
            None => (fold.clone(), pending.digest),
        };
        if named != sanitizer || source != &source_digest {
            return Err(TransitionError::EvidenceMismatch);
        }
        let registered = self
            .registry
            .sanitizer(sanitizer)
            .ok_or(TransitionError::SanitizerUnapplicable)?;
        if sanitizer.is_attest_schema() && !plan::attest_applicable(views, id.child(), body, &registered.transition) {
            return Err(TransitionError::SanitizerUnapplicable);
        }
        let label = registered
            .derive_output(&from_label, &[], &self.context(act))?
            .ok_or(TransitionError::SanitizerUnapplicable)?;
        if !plan::confined_hop_helps(&pending.receiving, &from_label, &label) {
            return Err(TransitionError::SanitizerUnapplicable);
        }
        let lineage = lineage
            .extend(sanitizer.clone())
            .ok_or(TransitionError::SanitizerUnapplicable)?;
        let residual = crate::admit::confined_residual(&pending.receiving, &label);
        let value = crate::value::LabeledValue::new(body.clone(), label.clone());
        facts.push(Fact::CandidateDerived {
            trajectory: views.trajectory().clone(),
            subject: crate::basis::SubjectKey::Return(id.clone()),
            via: DerivedVia {
                name: sanitizer.clone(),
                transition: registered.transition.applied(),
            },
            derived: DerivedCandidate::Return {
                source: source_digest,
                from: ConfinedFrom::Offer(execution.offer),
                value: value.clone(),
                residual: residual.clone(),
            },
            lineage: lineage.clone(),
            evidence: act.pinned(),
        });
        let Some(residual) = residual else {
            // The successor owes nothing: candidate and merge land atomically.
            facts.extend(branch::crossing_facts(
                views,
                id.child(),
                value,
                ReturnDerivation::Sanitized {
                    sanitizer: sanitizer.clone(),
                    raw_digest: pending.digest,
                    transition: registered.transition.applied(),
                },
                None,
                act.pinned(),
            ));
            return Ok(EngineDecision {
                append: Some(self.decided(view, crate::basis::DecidedAct::Offer(execution.offer), facts)?),
                follow_up: FollowUp::Offer(OfferFollowUp::Admitted { value: body.clone() }),
            });
        };
        // The stage the successor leaves. A hop lands only while its offer's basis is current,
        // so the stage is re-planned from the candidate standing now.
        let menu = self.return_menu(
            views,
            ReturnStageInput {
                child: id.child(),
                label: &label,
                body,
                residual: &residual,
                lineage: &lineage,
            },
            act,
        )?;
        let stage = menu;
        let (batch, staged) = self.pending_stage(
            view,
            views,
            crate::basis::DecidedAct::Offer(execution.offer),
            execution.offer_nonce,
            id,
            &pending.fork,
            label,
            residual,
            stage,
            facts,
            act,
        )?;
        Ok(EngineDecision {
            append: Some(batch),
            follow_up: FollowUp::Offer(OfferFollowUp::ReturnStaged(Box::new(staged))),
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
        let after = check::evaluate(&contract, views, &substituted, &next, &self.context(under))?;
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
            via: DerivedVia {
                name: sanitizer.clone(),
                transition: registered.transition.applied(),
            },
            derived,
            lineage,
            evidence: under.pinned(),
        });
        let staged = Sequence::advance_of(self, view, &facts);
        let act = crate::basis::DecidedAct::Offer(execution.offer);
        let follow_up = match after {
            CheckOutcome::Allow => {
                let (dispatch, opening) =
                    opened_dispatch(&contract, views, &substituted, recorded.subject.clone(), under);
                facts.push(opening);
                OfferFollowUp::Released(Box::new(Released {
                    dispatch,
                    call: substituted,
                    fork: None,
                }))
            }
            CheckOutcome::Block(raw) => {
                let role = views.call_role(&recorded.subject);
                require_atoms(under, plan::block_atoms(&self.registry, &contract, &raw, role))?;
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
            act.inherit(&pinned)?;
            let under = self.act_evidence(act.evidence.inheriting(&pinned)?, AudienceEvidence::default())?;
            let reblocked = self.reblocked(views, recorded, execution, &under)?;
            act.expansions.absorb_reads(&under.expansions);
            return Ok(match reblocked {
                Some(block) => OfferFollowUp::Substituted { block: Box::new(block) },
                None => OfferFollowUp::Invalidated,
            });
        }
        Ok(match views.subject_dispatch(&recorded.subject).cloned() {
            Some(dispatch) if views.is_open(&dispatch) && !views.is_succeeded(&dispatch) => {
                OfferFollowUp::Released(Box::new(Released {
                    dispatch,
                    call: candidate,
                    fork: None,
                }))
            }
            Some(dispatch) => OfferFollowUp::Settled(Box::new(Settled {
                outcome: settled_outcome(views, &dispatch),
                dispatch,
                call: candidate,
            })),
            None => OfferFollowUp::Invalidated,
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
        if let crate::basis::SubjectKey::Return(id) = &recorded.subject {
            return match (end, &execution.outcome, recorded.plan.hop()) {
                (OfferEnd::Accepted, OfferOutcome::Derived(Evidence::Sanitizer { .. }), Some(name))
                    if !name.is_attest_schema() =>
                {
                    Ok(EngineDecision {
                        append: None,
                        follow_up: FollowUp::Offer(self.return_repeat(views, id)),
                    })
                }
                (OfferEnd::Accepted, OfferOutcome::Approved(evidence), Some(name))
                    if name.is_attest_schema() && evidence.is_empty() =>
                {
                    Ok(EngineDecision {
                        append: None,
                        follow_up: FollowUp::Offer(self.return_repeat(views, id)),
                    })
                }
                (OfferEnd::Accepted, OfferOutcome::Approved(evidence), None) if evidence.is_empty() => {
                    Ok(EngineDecision {
                        append: None,
                        follow_up: FollowUp::Offer(self.return_repeat(views, id)),
                    })
                }
                (OfferEnd::Invalidated, _, _) => Ok(EngineDecision {
                    append: None,
                    follow_up: FollowUp::Offer(OfferFollowUp::Invalidated),
                }),
                (OfferEnd::Accepted, OfferOutcome::Derived(_), Some(_)) => Err(TransitionError::PlanOutcomeMismatch),
                (_, OfferOutcome::Derived(_), None)
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

    fn return_repeat(&self, views: &Views, id: &ChildReturnId) -> OfferFollowUp {
        if let Some(crossed) = views.child_return(id) {
            return OfferFollowUp::Admitted {
                value: crossed.body.clone(),
            };
        }
        let subject = crate::basis::SubjectKey::Return(id.clone());
        match (
            views.submitted_return(id),
            views.candidate(&subject),
            views.pending_block(&subject),
        ) {
            (
                // Custody must still stand for the stage to be answerable from the record.
                Some(_),
                Some(DerivedCandidate::Return {
                    value,
                    residual: Some(residual),
                    ..
                }),
                Some((_, offers)),
            ) => OfferFollowUp::ReturnStaged(Box::new(PendingReturnStage {
                id: id.clone(),
                label: value.label.clone(),
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
        let CheckOutcome::Block(raw) = check::evaluate(&contract, views, &call, &stage, &self.context(act))? else {
            return Ok(None);
        };
        let role = views.call_role(&recorded.subject);
        require_atoms(act, plan::block_atoms(&self.registry, &contract, &raw, role))?;
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
            sanitizer: recorded.plan.sanitizer().cloned(),
            basis: views.basis_after(&advance, &subject),
            evidence: act.pinned(),
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
    ) -> Result<ValidatedFactBatch, TransitionRefusal> {
        let empty = EngineView::validated(Projection::empty(0), self.identity, trajectory.clone());
        self.seal(
            &empty,
            vec![Fact::TrajectoryOpened {
                trajectory: trajectory.clone(),
                dialect: self.dialect,
                profile: self.profile().clone(),
                policy_digest: self.identity,
                policy_file_key,
                open_vectors: self.open_vectors(),
            }],
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
        Ok(check::evaluate(&contract, views, call, &CallStage::default(), &context)
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
                role: plan::CallRole::Ordinary,
            },
            &context,
        )
        .expect("engine test plans read no undecided symbolic audience"))
    }

    /// Record a child's returned value at an engine-derived label AND merge it into the direct
    /// parent — one atomic batch, no orphanable intermediate state. A crossing that would narrow
    /// the parent merges nothing: it comes back as [`branch::RawCrossing::Narrows`] carrying the
    /// price, which crosses only through the parent-owned staged return. See [`crate::branch`].
    #[cfg(test)]
    pub(crate) fn submit_child_return(
        &self,
        parent: &Views,
        child: &TrajectoryId,
        body: ValueBody,
    ) -> Result<branch::RawCrossing, BranchError> {
        branch::submit_child_return(parent, child, &body, &AudienceEvidence::default())
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
        evidence: act.pinned(),
    }));
    if let Some(sanitizer) = &approval.sanitizer {
        facts.push(Fact::OutputSanitizerBound {
            trajectory: trajectory.clone(),
            dispatch: dispatch.clone(),
            plan: approval.plan,
            sanitizer: sanitizer.clone(),
            contribution: crate::plan::bound_contribution(registry, contract, sanitizer, &context)
                .expect("the compose gate answers a spent approval's sanitizer atoms")
                .expect("a prepared approval binds an output sanitizer enumeration found applicable"),
            evidence: act.pinned(),
        });
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

fn return_act(child: &TrajectoryId) -> crate::basis::DecidedAct {
    crate::basis::DecidedAct::ChildReturn(ChildReturnId::new(child.clone(), 0))
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

/// The optional shape a marked spawn call authors: its `return_schema` argument,
/// compiled to canonical form. Runtime transports the schema without interpreting it; only this
/// compilation reads it. A schema that does not compile makes the marked call invalid.
pub(crate) fn marked_return_shape(call: &ResolvedCall) -> Result<Option<crate::shape::ReturnShape>, EngineError> {
    match call.arguments().get("return_schema") {
        None => Ok(None),
        Some(authored) => crate::shape::ReturnShape::compile(authored)
            .map(Some)
            .map_err(EngineError::InvalidReturnSchema),
    }
}

fn branch_refusal(error: BranchError) -> TransitionError {
    match error {
        BranchError::NotDirectParent | BranchError::NotForked => TransitionError::NotForked,
        BranchError::AlreadyEnded => TransitionError::BranchEnded,
        other => unreachable!("the child-return boundary refuses before reaching {other}"),
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
        proposed_label: check::committed_label(contract, &current),
        receiving: current.clone(),
        proposed_effects: contract.emits.clone(),
        annotation: call.annotation().cloned(),
        evidence: act.pinned(),
        subject,
    };
    (dispatch, fact)
}

pub(crate) struct SiblingRelease {
    pub(crate) dispatch: DispatchId,
    pub(crate) consumes: Option<crate::value::OfferId>,
    pub(crate) prepares_fork: Option<ForkId>,
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

/// One act's audience reading: the merged pinned evidence its records persist, the
/// membership answers that evidence recomputes to, and the inherited pins — entries earlier
/// records of this chain already pinned, which the operation-scope test excuses. Built only
/// through validation, so a context over it always reads admissible answers.
#[derive(Clone, Debug)]
pub(crate) struct ActEvidence {
    evidence: AudienceEvidence,
    expansions: Expansions,
    inherited: std::cell::RefCell<AudienceEvidence>,
}

impl ActEvidence {
    /// Assemble from parts a caller validated together: the transition validator recomputes
    /// `expansions` from `evidence` before building this. The validator runs its own
    /// per-act operation-scope audit, so no inherited pins are carried here.
    pub(crate) fn validated(evidence: AudienceEvidence, expansions: Expansions) -> ActEvidence {
        ActEvidence {
            evidence,
            expansions,
            inherited: std::cell::RefCell::default(),
        }
    }

    /// The evidence a record of this act pins.
    pub(crate) fn pinned(&self) -> AudienceEvidence {
        self.evidence.clone()
    }

    /// The context's answers and ask log, for the validator's per-act audit.
    pub(crate) fn expansions(&self) -> &Expansions {
        &self.expansions
    }

    /// Count `pins` — entries a record this act continues already pinned — as inherited, so
    /// the operation-scope test excuses them. Interior mutability: overlay contexts built
    /// mid-decision discover pins the act-building event could not name.
    fn inherit(&self, pins: &AudienceEvidence) -> Result<(), crate::audience::EvidenceRefusal> {
        let merged = self.inherited.borrow().inheriting(pins)?;
        *self.inherited.borrow_mut() = merged;
        Ok(())
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
        .filter(|atom| act.expansions.members(atom).is_none())
        .collect();
    if needed.is_empty() {
        return Ok(());
    }
    needed.sort();
    needed.dedup();
    Err(TransitionError::MembershipNeeded { needed })
}

/// The atoms a spent approval's consumption reads: each ruling's mandate over its covered
/// gaps, and the bound output sanitizer's transition.
fn approval_atoms(registry: &Registry, approval: &crate::projection::PreparedApproval) -> Vec<SymbolicAtom> {
    let providers = registry.audience().providers();
    let mut atoms: Vec<SymbolicAtom> = approval
        .rulings
        .iter()
        .filter_map(|given| registry.authority(&given.authority).map(|authority| (authority, given)))
        .flat_map(|(authority, given)| authority.mandate.reads(&given.covers, providers))
        .collect();
    if let Some(sanitizer) = approval.sanitizer.as_ref().and_then(|name| registry.sanitizer(name)) {
        atoms.extend(sanitizer.needed_atoms(providers));
    }
    atoms
}

/// The ordered in-batch composition, position by position: what each proposed sibling
/// does, and the records that say so. `None` at a position is a refusal.
#[allow(clippy::too_many_arguments)]
pub(crate) fn compose_batch<'a>(
    registry: &Registry,
    child_return: &ReturnPolicy,
    working: &mut std::borrow::Cow<'a, Projection>,
    batch: ComposingBatch<'_>,
    proposals: &[ResolvedCall],
    spawn: Option<SpawnMark>,
    approval: &impl Fn(&Views, &ResolvedCall) -> Option<crate::value::OfferId>,
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
        for call in proposals {
            if contract_for_call(registry, call).is_err() {
                // Reported as malformed by the composition below, at its position.
                per_call.push((std::borrow::Cow::Borrowed(act), None));
                continue;
            }
            let spends = if singleton { approval(&views, call) } else { None };
            let under = match spends.and_then(|offer| views.approval(&offer)) {
                Some(prepared) => {
                    act.inherit(&prepared.evidence).map_err(ComposeRefusal::Evidence)?;
                    let merged = act
                        .evidence
                        .inheriting(&prepared.evidence)
                        .map_err(ComposeRefusal::Evidence)?;
                    let expansions = registry
                        .audience()
                        .expansions(&merged)
                        .map_err(ComposeRefusal::Evidence)?;
                    std::borrow::Cow::Owned(ActEvidence::validated(merged, expansions))
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
                        .filter(|atom| under.expansions.members(atom).is_none()),
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
            let consumes = match check::evaluate(
                &contract,
                &views,
                call,
                &CallStage::default(),
                &membership_context(registry, under),
            ) {
                Ok(CheckOutcome::Allow) => None,
                Ok(CheckOutcome::Block(_)) => match spends {
                    Some(offer) => Some(*offer),
                    None => {
                        refused = true;
                        composed.push(None);
                        continue;
                    }
                },
                Err(missing) => return Err(ComposeRefusal::MembershipNeeded(missing)),
            };
            let subject = batch.subject(position);
            let (dispatch, opening) = opened_dispatch(&contract, &views, call, subject, under);
            let mut facts = Vec::new();
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
                    registry, &contract, trajectory, &dispatch, &prepared, under,
                ));
            }
            facts.push(opening);
            let prepares_fork = if spawn == Some(SpawnMark::at(position)) {
                let shape = marked_return_shape(call).map_err(ComposeRefusal::Malformed)?;
                let fork = ForkId::of(&dispatch);
                facts.push(Fact::ForkPrepared {
                    trajectory: trajectory.clone(),
                    fork: fork.clone(),
                    snapshot: views.freeze_basis(),
                    return_policy: child_return.clone(),
                    shape,
                });
                Some(fork)
            } else {
                None
            };
            SiblingRelease {
                dispatch,
                consumes,
                prepares_fork,
                facts,
                evidence: under.pinned(),
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
            act.expansions.absorb_reads(&under.expansions);
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
mod tests {
    use crate::label::DeclaredAudience;
    use std::collections::BTreeSet;

    use super::*;
    use crate::check::Gap;
    use crate::contract::{
        AudienceRequirement, Delta, HistoryRequirement, LabelRequirements, RecipientSpec, Requires, ToolAnnotation,
    };
    use crate::fact::{EffectKind, EffectSet, Fact};
    use crate::label::{Audience, Label, ReaderId, Trust};
    use crate::names::MarkName;
    use crate::projection::Projection;
    use crate::registry::{RegistryConfig, TrustChain};
    use crate::value::{LabeledValue, Provenance, ToolName, TrajectoryId, ValueBody};
    use serde_json::json;

    const SUSPICIOUS: Trust = Trust::new(0);
    const TRUSTED: Trust = Trust::new(1);

    fn nonce() -> crate::value::OfferNonce {
        crate::value::OfferNonce::new([7u8; 32])
    }

    fn traj() -> TrajectoryId {
        TrajectoryId::new("t")
    }

    fn engine(tools: Vec<ToolAnnotation>) -> Engine {
        open_engine(test_config(tools))
    }

    fn engine_at(tools: Vec<ToolAnnotation>, starting: Label) -> Engine {
        open_engine_at(test_config(tools), starting)
    }

    fn declared(tools: Vec<ToolAnnotation>) -> Vec<crate::contract::ToolDeclaration> {
        tools
            .into_iter()
            .map(crate::contract::ToolDeclaration::Declared)
            .collect()
    }

    /// A complete annotation as `annotator` produced it for `bound_to`'s exact canonical call.
    fn pinned_for(
        produced: ToolAnnotation,
        annotator: &str,
        bound_to: &ResolvedCall,
    ) -> crate::contract::PinnedAnnotation {
        crate::contract::PinnedAnnotation::new(
            crate::names::AnnotatorName::new(annotator),
            bound_to.digest(),
            crate::contract::ProducedAnnotation {
                delta: produced.delta,
                emits: produced.emits,
                requires: produced.requires,
            },
        )
    }

    /// An Annotated declaration carrying `tool`'s operational metadata, routed through `annotator`.
    fn annotated(tool: ToolAnnotation, annotator: &str) -> crate::contract::ToolDeclaration {
        crate::contract::ToolDeclaration::Annotated {
            name: tool.name,
            tags: tool.tags,
            description: tool.description,
            parameters: tool.parameters,
            annotator: crate::names::AnnotatorName::new(annotator),
        }
    }

    /// The policy's wildcard: `name = "*"`, no metadata, routed through `by`.
    fn wildcard(by: &str) -> crate::contract::ToolDeclaration {
        crate::contract::ToolDeclaration::Annotated {
            name: ToolName::new(crate::registry::WILDCARD_TOOL_NAME),
            tags: vec![],
            description: None,
            parameters: crate::params::ToolParameters::open(),
            annotator: crate::names::AnnotatorName::new(by),
        }
    }

    /// An Annotator registration with no bounds: every omitted bound resolves to the whole policy
    /// vocabulary at load.
    fn annotator(name: &str) -> crate::registry::AnnotatorDeclaration {
        crate::registry::AnnotatorDeclaration {
            name: crate::names::AnnotatorName::new(name),
            trust: None,
            audiences: None,
            marks: None,
            effects: None,
        }
    }

    fn annotator_with_readers(name: &str, readers: &[&str]) -> crate::registry::AnnotatorDeclaration {
        crate::registry::AnnotatorDeclaration {
            audiences: Some(readers.iter().map(|reader| ReaderId::new(*reader)).collect()),
            ..annotator(name)
        }
    }

    fn test_config(tools: Vec<ToolAnnotation>) -> RegistryConfig {
        RegistryConfig {
            annotators: vec![],
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: declared(tools),
            authorities: vec![],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
        }
    }

    fn opened(e: &Engine) -> Fact {
        opened_root(e, &traj())
    }

    fn opened_root(e: &Engine, trajectory: &TrajectoryId) -> Fact {
        e.open_trajectory(trajectory, crate::profile::PolicyFileKey::of(b"policy"))
            .expect("the engine opens its own root")
            .into_unsealed()
            .remove(0)
    }

    fn read_tool(name: &str, delta: Delta) -> ToolAnnotation {
        ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new(name),
            tags: vec![],
            delta,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires::default(),
        }
    }

    fn suspicious_read() -> ToolAnnotation {
        read_tool(
            "read_suspicious",
            Delta {
                trust: Some(SUSPICIOUS),
                audience: None,
            },
        )
    }

    fn internal_read() -> ToolAnnotation {
        read_tool(
            "read_internal",
            Delta {
                trust: None,
                audience: Some(DeclaredAudience::restricted([ReaderId::new("insider")])),
            },
        )
    }

    fn stray_admission(trajectory: &TrajectoryId, label: Label) -> Fact {
        Fact::ValueAdmitted {
            trajectory: trajectory.clone(),
            value: LabeledValue::new(ValueBody::new("stray"), label),
            provenance: Provenance::ToolResult {
                dispatch: DispatchId::new(trajectory.clone(), call("stray", json!({})).digest(), 0),
            },
        }
    }

    fn suspicious_internal_read() -> ToolAnnotation {
        read_tool(
            "read_suspicious_internal",
            Delta {
                trust: Some(SUSPICIOUS),
                audience: Some(DeclaredAudience::restricted([ReaderId::new("insider")])),
            },
        )
    }

    fn reads(e: &Engine, log: &mut Vec<Fact>, trajectory: &TrajectoryId, tool: &str) -> crate::value::DispatchId {
        let call = call(tool, json!({ "who": "someone" }));
        let id = format!("read-{tool}-{}", log.len());
        let decision = e
            .handle(
                &viewing(e, log),
                batch_on(trajectory, &id, Vec::new(), vec![raw(&call)], None),
            )
            .expect("a registered read decides");
        let dispatch = match answered(&decision) {
            ([release], []) => {
                let dispatch = release.dispatch.clone();
                log.extend(appended_facts(decision));
                dispatch
            }
            ([], [block]) => {
                let accepting = block
                    .block
                    .plans
                    .iter()
                    .filter_map(crate::plan::RemedyPlan::executable)
                    .find(|plan| plan.narrowing().is_some() && plan.sanitizer().is_none() && plan.required.is_empty())
                    .expect("a narrowing read offers its acceptance")
                    .id;
                let offer = block
                    .offers
                    .iter()
                    .find_map(|(offer, plan)| (*plan == accepting).then_some(*offer))
                    .expect("the acceptance plan is offered");
                log.extend(appended_facts(decision));
                let approved = e
                    .handle(
                        &viewing(e, log),
                        EngineEvent::ExecuteOffer(OfferExecution {
                            trajectory: trajectory.clone(),
                            offer,
                            outcome: OfferOutcome::Approved(Vec::new()),
                            offer_nonce: nonce(),
                            audience: crate::audience::AudienceEvidence::default(),
                        }),
                    )
                    .expect("the acceptance approves the read");
                assert!(matches!(
                    approved.follow_up,
                    FollowUp::Offer(OfferFollowUp::Approved { .. })
                ));
                log.extend(appended_facts(approved));
                let released = e
                    .handle(
                        &viewing(e, log),
                        batch_on(
                            trajectory,
                            &format!("{id}-approved"),
                            Vec::new(),
                            vec![raw(&call)],
                            None,
                        ),
                    )
                    .expect("the approved read releases");
                let dispatch = match answered(&released) {
                    ([release], []) => release.dispatch.clone(),
                    other => panic!("the approved read releases, got {other:?}"),
                };
                log.extend(appended_facts(released));
                dispatch
            }
            other => panic!("one read decides one way, got {other:?}"),
        };
        let admitted = e
            .handle(
                &viewing(e, log),
                EngineEvent::Outcome(ToolReport {
                    dispatch: dispatch.clone(),
                    outcome: ToolOutcome::Success {
                        body: OutcomeBody::Available(ValueBody::new("read")),
                    },
                    evidence: Vec::new(),
                    offer_nonce: nonce(),
                    audience: crate::audience::AudienceEvidence::default(),
                }),
            )
            .expect("the raw result admits");
        log.extend(appended_facts(admitted));
        dispatch
    }

    fn forked_child(e: &Engine, log: &[Fact], child: &TrajectoryId) -> Vec<Fact> {
        let view = e
            .view(&traj(), log.to_vec(), log.len() as u64)
            .expect("the parent view builds");
        let spawn = call("spawn", json!({}));
        let decision = e
            .handle(
                &view,
                EngineEvent::Proposals(ProposalBatch {
                    id: crate::transition::ProposalBatchId::new(format!("fork-{}", child.as_str())),
                    trajectory: traj(),
                    provider_results: Vec::new(),
                    proposals: vec![raw(&spawn)],
                    spawn: Some(crate::transition::SpawnMark::at(0)),
                    offer_nonce: nonce(),
                    evidence: Vec::new(),
                    audience: crate::audience::AudienceEvidence::default(),
                }),
            )
            .expect("the marked spawn releases and prepares the fork");
        let FollowUp::Proposals { released, .. } = &decision.follow_up else {
            panic!("a proposal batch answers with proposals")
        };
        let fork = released[0].fork.clone().expect("the marked spawn carries its fork");
        let mut facts = decision.append.expect("the release appends").facts().to_vec();
        let bound_at = log.len() + facts.len();
        let prepared = e
            .view(&traj(), [log.to_vec(), facts.clone()].concat(), bound_at as u64)
            .expect("the prepared view builds");
        let bound = e
            .handle(
                &prepared,
                EngineEvent::BindFork(crate::transition::ForkBinding {
                    fork,
                    child: child.clone(),
                }),
            )
            .expect("the child binds to its fork");
        facts.extend(bound.append.expect("the binding appends").facts().to_vec());
        facts
    }

    fn child_report(log: &[Fact], child: &TrajectoryId, submission: ChildSubmission) -> EngineEvent {
        EngineEvent::ChildReturn(ChildReport {
            child: child.clone(),
            fork: log
                .iter()
                .find_map(|fact| match fact {
                    Fact::ForkOpened { trajectory, fork } if trajectory == child => Some(fork.clone()),
                    _ => None,
                })
                .expect("the log opened a fork for this child"),
            submission,
            evidence: Vec::new(),
            offer_nonce: nonce(),
            audience: crate::audience::AudienceEvidence::default(),
        })
    }

    fn open_engine(cfg: RegistryConfig) -> Engine {
        open_engine_returning(cfg, ReturnPolicy::Raw)
    }

    fn open_engine_at(cfg: RegistryConfig, starting: Label) -> Engine {
        open_engine_returning_at(cfg, ReturnPolicy::Raw, starting)
    }

    fn open_engine_returning(cfg: RegistryConfig, child_return: ReturnPolicy) -> Engine {
        let starting = crate::profile::neutral_starting_label(&cfg.trust_chain);
        open_engine_returning_at(cfg, child_return, starting)
    }

    fn open_engine_returning_at(mut cfg: RegistryConfig, child_return: ReturnPolicy, starting: Label) -> Engine {
        if !cfg.tools.iter().any(|tool| tool.name().as_str() == "spawn") {
            cfg.tools
                .push(crate::contract::ToolDeclaration::Declared(plain_tool("spawn")));
        }
        let profile = crate::profile::ProfileDeclaration {
            starting_label: starting,
            ..crate::profile::covering_declaration(&cfg)
        };
        Engine::open(DeploymentPolicy {
            registry: cfg,
            planner_cap: crate::registry::PlannerCap::default(),
            dialect: PolicyDialectVersion::new(1),
            child_return,
            profile,
        })
        .unwrap()
    }

    fn known(trust: Trust, audience: Audience) -> Label {
        Label::new(trust, audience)
    }

    fn established(trust: Trust, audience: Audience) -> Label {
        Label::new(trust, audience)
    }

    fn partial(trust: Trust, audience: Audience) -> Label {
        Label::new(trust, audience)
    }

    fn call(tool: &str, args: serde_json::Value) -> ResolvedCall {
        ResolvedCall::new(ToolName::new(tool), crate::params::test_arguments(&args))
    }

    fn raw(call: &ResolvedCall) -> crate::transition::ProposedCall {
        crate::transition::ProposedCall {
            tool: call.tool().clone(),
            arguments: call.canonical_arguments().canonical_bytes().to_vec(),
            annotation: call.annotation().cloned(),
        }
    }

    fn check(engine: &Engine, log: &[Fact], call: &ResolvedCall) -> CheckOutcome {
        let p = Projection::build(log, log.len() as u64);
        let t = traj();
        engine.check(&p.view(&t), call).unwrap()
    }

    fn crm_tool() -> ToolAnnotation {
        ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("get_ticket"),
            tags: vec![],
            delta: Delta {
                trust: None,
                audience: Some(DeclaredAudience::restricted([ReaderId::new("insider")])),
            },
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(TRUSTED),
                    audience: vec![],
                },
                ..Requires::default()
            },
        }
    }

    #[test]
    fn permuted_effect_declarations_produce_byte_identical_dispatch_facts() {
        let pay = |emits: [&str; 2]| ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("pay"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new(emits.map(EffectKind::new)).unwrap(),
            requires: Requires::default(),
        };
        let release = |contract: ToolAnnotation| {
            let e = engine(vec![contract]);
            let mut log = vec![opened(&e)];
            let decision = e
                .handle(
                    &viewing(&e, &log),
                    batch_on(&traj(), "b1", Vec::new(), vec![raw(&call("pay", json!({})))], None),
                )
                .expect("the neutral call decides");
            let facts = appended_facts(decision);
            log.extend(facts.clone());
            (facts, log)
        };
        let (ab, log_ab) = release(pay(["spend", "audit"]));
        let (ba, log_ba) = release(pay(["audit", "spend"]));
        assert_eq!(serde_json::to_string(&ab).unwrap(), serde_json::to_string(&ba).unwrap());
        let revision = log_ab.len() as u64;
        assert_eq!(
            Projection::build(&log_ab, revision),
            Projection::build(&log_ba, revision)
        );
    }

    #[test]
    fn clean_call_allows() {
        let e = engine(vec![crm_tool()]);
        let log = vec![opened(&e)];
        match check(&e, &log, &call("get_ticket", json!({}))) {
            CheckOutcome::Block(b) => {
                assert!(b.narrowing.is_some());
                assert!(b.requirement_gaps.is_empty());
            }
            other => panic!("expected narrowing block, got {other:?}"),
        }
    }

    #[test]
    fn the_boundary_releases_an_allowed_proposal_with_its_dispatch() {
        let internal = Audience::restricted([ReaderId::new("insider")]);
        let e = engine_at(vec![crm_tool()], known(TRUSTED, internal));
        let records = vec![opened(&e)];
        let view = e.view(&traj(), records, 1).unwrap();
        let call = call("get_ticket", json!({}));

        let decision = e
            .handle(
                &view,
                EngineEvent::Proposals(ProposalBatch {
                    id: crate::transition::ProposalBatchId::new("b1"),
                    trajectory: traj(),
                    provider_results: Vec::new(),
                    proposals: vec![raw(&call)],
                    spawn: None,
                    offer_nonce: nonce(),
                    evidence: Vec::new(),
                    audience: crate::audience::AudienceEvidence::default(),
                }),
            )
            .unwrap();

        let released = match &decision.follow_up {
            FollowUp::Proposals { released, blocked, .. } if blocked.is_empty() => released.clone(),
            other => panic!("expected a release, got {other:?}"),
        };
        assert_eq!(released.len(), 1);
        assert_eq!(released[0].call, call);
        let appended = decision.append.expect("an allowed call opens a dispatch");
        assert!(matches!(
            &appended.facts()[0],
            Fact::BasisAdvanced { act: crate::basis::DecidedAct::Proposals(batch), advance, .. }
                if batch.as_str() == "b1"
                    && !advance.family
                    && advance.flows == std::collections::BTreeSet::from([traj()])
                    && advance.subjects.is_empty()
        ));
        assert!(matches!(
            &appended.facts()[1],
            Fact::ProposalBatchDecided { batch, .. } if batch.as_str() == "b1"
        ));
        match &appended.facts()[2] {
            Fact::DispatchOpened { dispatch, subject, .. } => {
                assert_eq!(dispatch, &released[0].dispatch);
                assert_eq!(
                    subject,
                    &crate::basis::SubjectKey::Call {
                        trajectory: traj(),
                        batch: crate::transition::ProposalBatchId::new("b1"),
                        position: 0,
                    }
                );
            }
            other => panic!("the decision's first release record is its opening, got {other:?}"),
        }
        assert_eq!(appended.facts().len(), 3);
    }

    #[test]
    fn a_repeated_batch_identity_returns_its_recorded_decision_and_a_reused_one_is_refused() {
        let internal = Audience::restricted([ReaderId::new("insider")]);
        let e = engine_at(vec![crm_tool()], known(TRUSTED, internal));
        let records = vec![opened(&e)];
        let view = e.view(&traj(), records.clone(), 1).unwrap();
        let batch = |proposals: Vec<ResolvedCall>| {
            EngineEvent::Proposals(ProposalBatch {
                id: crate::transition::ProposalBatchId::new("b1"),
                trajectory: traj(),
                provider_results: Vec::new(),
                proposals: proposals.iter().map(raw).collect(),
                spawn: None,
                offer_nonce: nonce(),
                evidence: Vec::new(),
                audience: crate::audience::AudienceEvidence::default(),
            })
        };
        let proposal = call("get_ticket", json!({}));
        let other = call("get_ticket", json!({ "id": "2" }));
        let call = proposal;

        let first = e.handle(&view, batch(vec![call.clone()])).unwrap();
        let appended_facts = first
            .append
            .clone()
            .expect("the first decision records itself")
            .into_unsealed();
        let decided = [records, appended_facts.clone()].concat();
        let after = e.view(&traj(), decided, 2).unwrap();

        let repeat = e.handle(&after, batch(vec![call.clone()])).unwrap();
        assert_eq!(repeat.append, None);
        assert_eq!(repeat.follow_up, first.follow_up);

        assert_eq!(
            e.handle(&after, batch(vec![other.clone()])),
            Err(crate::transition::TransitionError::BatchIdentityConflict)
        );

        let decision = |released: Vec<DispatchId>| Fact::ProposalBatchDecided {
            trajectory: traj(),
            batch: crate::transition::ProposalBatchId::new("b1"),
            proposals: vec![call.clone()],
            spawn: None,
            released,
            evidence: crate::audience::AudienceEvidence::default(),
        };
        let allowed = |records: Vec<Fact>| [vec![opened(&e)], records].concat();
        assert_eq!(
            e.validate_replay(&allowed(vec![decision(vec![]), decision(vec![])])),
            Err(TransitionRefusal::MisdecidedBatch)
        );
        let FollowUp::Proposals { released, .. } = &first.follow_up else {
            panic!("a proposal batch answers with proposals")
        };
        let dispatch = released[0].dispatch.clone();
        assert_eq!(
            e.validate_replay(&allowed(vec![decision(vec![])])),
            Err(TransitionRefusal::MisdecidedBatch)
        );
        assert_eq!(
            e.validate_replay(&allowed(vec![
                decision(vec![dispatch.clone()]),
                opening_of(&first),
                decision(vec![dispatch.clone()])
            ])),
            Err(TransitionRefusal::BatchIdentityConflict)
        );
        assert_eq!(
            e.validate_replay(&allowed(vec![decision(vec![dispatch.clone()])])),
            Err(TransitionRefusal::UnbackedDecision)
        );
        let other_id = |released: Vec<DispatchId>| Fact::ProposalBatchDecided {
            trajectory: traj(),
            batch: crate::transition::ProposalBatchId::new("b2"),
            proposals: vec![call.clone()],
            spawn: None,
            released,
            evidence: crate::audience::AudienceEvidence::default(),
        };
        let opening = appended_facts[1].clone();
        assert_eq!(
            e.validate_replay(&allowed(vec![
                decision(vec![dispatch.clone()]),
                other_id(vec![dispatch.clone()]),
                opening.clone()
            ])),
            Err(TransitionRefusal::UnbackedDecision)
        );
        assert_eq!(
            e.validate_replay(&allowed(vec![
                decision(vec![dispatch.clone(), dispatch.clone()]),
                opening.clone()
            ])),
            Err(TransitionRefusal::MisdecidedBatch)
        );
        assert_eq!(
            e.validate_replay(&allowed(vec![
                Fact::ProposalBatchDecided {
                    trajectory: traj(),
                    batch: crate::transition::ProposalBatchId::new("b3"),
                    proposals: vec![other.clone()],
                    spawn: None,
                    released: vec![dispatch],
                    evidence: crate::audience::AudienceEvidence::default(),
                },
                opening
            ])),
            Err(TransitionRefusal::MisdecidedBatch)
        );
    }

    fn opening_of(decision: &EngineDecision) -> Fact {
        decision
            .append
            .as_ref()
            .expect("the decision records itself")
            .facts()
            .iter()
            .find(|fact| matches!(fact, Fact::DispatchOpened { .. }))
            .expect("a released decision opens its dispatch")
            .clone()
    }

    #[test]
    fn a_decision_cannot_record_a_release_the_check_refuses() {
        let e = engine(vec![crm_tool()]);
        let call = call("get_ticket", json!({}));
        let public = opened(&e);
        let dispatch = DispatchId::new(traj(), call.digest(), 0);
        let forged = vec![
            public,
            Fact::ProposalBatchDecided {
                trajectory: traj(),
                batch: crate::transition::ProposalBatchId::new("b1"),
                proposals: vec![call.clone()],
                spawn: None,
                released: vec![dispatch.clone()],
                evidence: crate::audience::AudienceEvidence::default(),
            },
            Fact::DispatchOpened {
                trajectory: traj(),
                dispatch,
                tool: call.tool().clone(),
                declaration: call.declaration_id(),
                arguments: call.canonical_arguments().clone(),
                proposed_label: Label::new(TRUSTED, Audience::restricted([ReaderId::new("insider")])),
                receiving: Label::new(TRUSTED, Audience::restricted([ReaderId::new("insider")])),
                proposed_effects: crate::fact::EffectSet::default(),
                annotation: None,
                subject: crate::basis::fixture_subject(&traj()),
                evidence: crate::audience::AudienceEvidence::default(),
            },
        ];
        assert_eq!(e.validate_replay(&forged), Err(TransitionRefusal::MisdecidedBatch));

        assert_eq!(
            e.validate_replay(&[forged[0].clone(), forged[2].clone()]),
            Err(TransitionRefusal::UnbackedDecision)
        );
    }

    #[test]
    fn a_repeat_of_a_block_that_has_lifted_reports_a_spent_identity() {
        let e = engine(vec![crm_tool(), internal_read()]);
        let public = vec![opened(&e)];
        let call = call("get_ticket", json!({}));
        let event = EngineEvent::Proposals(ProposalBatch {
            id: crate::transition::ProposalBatchId::new("b1"),
            trajectory: traj(),
            provider_results: Vec::new(),
            proposals: vec![raw(&call)],
            spawn: None,
            offer_nonce: nonce(),
            evidence: Vec::new(),
            audience: crate::audience::AudienceEvidence::default(),
        });

        let view = e.view(&traj(), public.clone(), 1).unwrap();
        let decision = e.handle(&view, event.clone()).unwrap();
        let decided = decision.append.expect("the block records its decision").into_unsealed();

        let mut later = [public, decided].concat();
        reads(&e, &mut later, &traj(), "read_internal");
        let revision = later.len() as u64;
        let after = e.view(&traj(), later, revision).unwrap();

        let FollowUp::Proposals {
            released,
            blocked,
            spent,
            ..
        } = e.handle(&after, event).unwrap().follow_up
        else {
            panic!("a proposal batch answers with proposals")
        };
        assert!(released.is_empty() && blocked.is_empty());
        assert_eq!(spent, vec![call]);
    }

    #[test]
    fn the_boundary_plans_a_blocked_proposal_and_opens_nothing() {
        let e = engine(vec![crm_tool(), plain_tool("send")]);
        let records = vec![opened(&e)];
        let view = e.view(&traj(), records, 1).unwrap();
        let call = call("get_ticket", json!({}));

        let decision = e
            .handle(
                &view,
                EngineEvent::Proposals(ProposalBatch {
                    id: crate::transition::ProposalBatchId::new("b1"),
                    trajectory: traj(),
                    provider_results: Vec::new(),
                    proposals: vec![raw(&call)],
                    spawn: None,
                    offer_nonce: nonce(),
                    evidence: Vec::new(),
                    audience: crate::audience::AudienceEvidence::default(),
                }),
            )
            .unwrap();

        let appended = decision.append.clone().expect("the decision boundary is recorded");
        let offers_opened = appended
            .facts()
            .iter()
            .filter(|fact| matches!(fact, Fact::OfferOpened { .. }))
            .count();
        assert!(matches!(
            &appended.facts()[..2],
            [
                Fact::BasisAdvanced { advance, .. },
                Fact::ProposalBatchDecided { .. }
            ] if advance.is_empty()
        ));
        assert_eq!(offers_opened, appended.facts().len() - 2);
        assert!(offers_opened > 0);
        match &decision.follow_up {
            FollowUp::Proposals { released, blocked, .. } if released.is_empty() => {
                assert_eq!(blocked.len(), 1);
                assert_eq!(blocked[0].call, call);
                assert!(blocked[0].block.raw.narrowing.is_some());
            }
            other => panic!("expected a planned block, got {other:?}"),
        }
    }

    #[test]
    fn repeat_at_same_label_is_not_a_narrowing() {
        let internal = Audience::restricted([ReaderId::new("insider")]);
        let e = engine_at(vec![crm_tool()], known(TRUSTED, internal));
        let log = vec![opened(&e)];
        assert_eq!(check(&e, &log, &call("get_ticket", json!({}))), CheckOutcome::Allow);
    }

    #[test]
    fn an_includes_requirement_reads_the_committed_label() {
        let b_reader = Audience::restricted([ReaderId::new("b")]);
        let share = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("share"),
            tags: vec![],
            delta: Delta {
                trust: None,
                audience: Some(DeclaredAudience::restricted([ReaderId::new("a")])),
            },
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Includes(RecipientSpec::Static(
                        DeclaredAudience::literal(b_reader.clone()),
                    ))],
                },
                ..Requires::default()
            },
        };
        let both = Audience::restricted([ReaderId::new("a"), ReaderId::new("b")]);
        let e = engine_at(vec![share], known(TRUSTED, both.clone()));
        let log = vec![opened(&e)];
        match check(&e, &log, &call("share", json!({}))) {
            CheckOutcome::Block(block) => {
                assert_eq!(
                    block.requirement_gaps,
                    vec![Gap::Includes {
                        recipients: DeclaredAudience::literal(b_reader)
                    }]
                );
                assert_eq!(
                    block.narrowing,
                    Some(crate::check::Narrowing {
                        from: established(TRUSTED, both),
                        to: established(TRUSTED, Audience::restricted([ReaderId::new("a")])),
                    })
                );
            }
            other => panic!("expected the committed-label includes gap, got {other:?}"),
        }
    }

    #[test]
    fn a_trust_floor_reads_the_committed_label() {
        let risky = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("risky"),
            tags: vec![],
            delta: Delta {
                trust: Some(SUSPICIOUS),
                audience: None,
            },
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(TRUSTED),
                    audience: vec![],
                },
                ..Requires::default()
            },
        };
        let e = engine(vec![risky]);
        let log = vec![opened(&e)];
        match check(&e, &log, &call("risky", json!({}))) {
            CheckOutcome::Block(block) => {
                assert_eq!(
                    block.requirement_gaps,
                    vec![Gap::TrustFloor {
                        required: TRUSTED,
                        actual: SUSPICIOUS,
                    }]
                );
                assert!(block.narrowing.is_some());
            }
            other => panic!("expected the committed-label trust gap, got {other:?}"),
        }
    }

    #[test]
    fn a_read_that_narrows_into_the_cap_passes_the_cap() {
        let a_reader = Audience::restricted([ReaderId::new("a")]);
        let scoped = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("scoped"),
            tags: vec![],
            delta: Delta {
                trust: None,
                audience: Some(DeclaredAudience::literal(a_reader.clone())),
            },
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Cap(DeclaredAudience::literal(a_reader))],
                },
                ..Requires::default()
            },
        };
        let both = Audience::restricted([ReaderId::new("a"), ReaderId::new("b")]);
        let e = engine_at(vec![scoped], known(TRUSTED, both));
        let log = vec![opened(&e)];
        match check(&e, &log, &call("scoped", json!({}))) {
            CheckOutcome::Block(block) => {
                assert!(block.requirement_gaps.is_empty(), "narrowing into the cap is not a gap");
                assert!(block.narrowing.is_some());
            }
            other => panic!("expected a narrowing-only soft block, got {other:?}"),
        }
    }

    fn emitting(name: &str, kind: &str) -> ToolAnnotation {
        ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new(name),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new([EffectKind::new(kind)]).unwrap(),
            requires: Requires::default(),
        }
    }

    fn history_guarded(name: &str, requirement: HistoryRequirement) -> ToolAnnotation {
        ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new(name),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                history: vec![requirement],
                ..Requires::default()
            },
        }
    }

    fn open(e: &Engine, log: &mut Vec<Fact>, c: &ResolvedCall) -> crate::value::DispatchId {
        let id = format!("open-{}", log.len());
        let decision = e
            .handle(&viewing(e, log), batch_on(&traj(), &id, Vec::new(), vec![raw(c)], None))
            .expect("a registered call decides");
        let dispatch = match answered(&decision) {
            ([release], []) => release.dispatch.clone(),
            other => panic!("an allowed call releases, got {other:?}"),
        };
        log.extend(appended_facts(decision));
        dispatch
    }

    fn close(
        e: &Engine,
        log: &mut Vec<Fact>,
        dispatch: &crate::value::DispatchId,
        c: &ResolvedCall,
        admission: crate::admit::ResultAdmission,
    ) {
        let p = Projection::build(log, log.len() as u64);
        let batch = admit::admit_result(
            &e.registry,
            &p.view(&traj()),
            dispatch,
            c,
            admission,
            &crate::label::TestContext::default().context(),
            &crate::audience::AudienceEvidence::default(),
        )
        .unwrap();
        log.extend(batch);
    }

    #[test]
    fn a_rewritten_admitted_label_is_refused_at_every_provenance() {
        let internal = Audience::restricted([ReaderId::new("insider")]);
        let e = engine_at(vec![crm_tool()], known(TRUSTED, internal.clone()));
        let call = call("get_ticket", json!({}));
        let mut log = vec![opened(&e)];
        let dispatch = open(&e, &mut log, &call);
        close(
            &e,
            &mut log,
            &dispatch,
            &call,
            crate::admit::ResultAdmission::SuccessRaw {
                body: ValueBody::new("ticket"),
            },
        );
        assert_eq!(e.validate_replay(&log), Ok(()));

        let widened = |label: Label| {
            let mut forged = log.clone();
            let last = forged.len() - 1;
            forged[last] = Fact::ValueAdmitted {
                trajectory: traj(),
                value: LabeledValue::new(ValueBody::new("ticket"), label),
                provenance: Provenance::ToolResult {
                    dispatch: dispatch.clone(),
                },
            };
            e.validate_replay(&forged)
        };
        assert_eq!(
            widened(known(TRUSTED, Audience::public())),
            Err(TransitionRefusal::ForgedLabel)
        );
        let admitted = match log.last() {
            Some(Fact::ValueAdmitted { value, .. }) => value.label.clone(),
            other => panic!("the raw result admits a value, got {other:?}"),
        };
        assert_eq!(widened(admitted), Ok(()));

        let child = TrajectoryId::new("child");
        let e = engine_at(vec![crm_tool()], known(SUSPICIOUS, internal.clone()));
        let mut branched = vec![opened(&e)];
        branched.extend(forked_child(&e, &branched.clone(), &child));
        let crossing = e
            .submit_child_return(
                &Projection::build(&branched, branched.len() as u64).view(&traj()),
                &child,
                ValueBody::new("done"),
            )
            .expect("a non-narrowing return crosses");
        let crossing = merged_crossing(crossing);
        branched.extend(crossing);
        assert_eq!(e.validate_replay(&branched), Ok(()));

        let forge = |index: usize, label: Label| {
            let mut forged = branched.clone();
            forged[index] = match &forged[index] {
                Fact::ChildReturn {
                    id, value, derivation, ..
                } => Fact::ChildReturn {
                    trajectory: child.clone(),
                    id: id.clone(),
                    value: LabeledValue::new(value.body.clone(), label),
                    derivation: derivation.clone(),
                    evidence: crate::audience::AudienceEvidence::default(),
                },
                Fact::ValueAdmitted { value, provenance, .. } => Fact::ValueAdmitted {
                    trajectory: traj(),
                    value: LabeledValue::new(value.body.clone(), label),
                    provenance: provenance.clone(),
                },
                other => panic!("unexpected record at {index}: {other:?}"),
            };
            e.validate_replay(&forged)
        };
        let crossing_at = branched
            .iter()
            .position(|fact| matches!(fact, Fact::ChildReturn { .. }))
            .expect("the return records its crossing");
        assert_eq!(
            forge(crossing_at, known(TRUSTED, Audience::public())),
            Err(TransitionRefusal::ForgedLabel)
        );
        assert_eq!(
            forge(crossing_at + 1, known(TRUSTED, Audience::public())),
            Err(TransitionRefusal::ForgedLabel)
        );
    }

    /// An indeterminate close records no observation and leaves the reservation
    /// standing, because the call may have executed. A report that arrives afterwards is
    /// refused on that ground, not on a contradiction with an observation there is none of.
    #[test]
    fn a_report_after_an_indeterminate_close_is_refused_on_its_own_ground() {
        let internal = Audience::restricted([ReaderId::new("insider")]);
        let e = engine_at(vec![crm_tool()], known(TRUSTED, internal));
        let call = call("get_ticket", json!({}));
        let records = vec![opened(&e)];
        let view = e.view(&traj(), records.clone(), 1).unwrap();
        let decision = e
            .handle(
                &view,
                EngineEvent::Proposals(ProposalBatch {
                    id: crate::transition::ProposalBatchId::new("b1"),
                    trajectory: traj(),
                    provider_results: Vec::new(),
                    proposals: vec![raw(&call)],
                    spawn: None,
                    offer_nonce: nonce(),
                    evidence: Vec::new(),
                    audience: crate::audience::AudienceEvidence::default(),
                }),
            )
            .unwrap();
        let FollowUp::Proposals { released, .. } = decision.follow_up else {
            panic!("a proposal batch answers with proposals")
        };
        let dispatch = released[0].dispatch.clone();
        let log = [records, decision.append.unwrap().facts().to_vec()].concat();
        let released_view = e.view(&traj(), log.clone(), 2).unwrap();

        let report = |outcome: ToolOutcome| ToolReport {
            dispatch: dispatch.clone(),
            outcome,
            evidence: Vec::new(),
            offer_nonce: nonce(),
            audience: crate::audience::AudienceEvidence::default(),
        };
        let closed = e
            .handle(&released_view, EngineEvent::Outcome(report(ToolOutcome::Indeterminate)))
            .expect("an indeterminate outcome closes the dispatch");
        let facts = closed.append.expect("the close appends").facts().to_vec();
        assert!(
            matches!(
                facts.as_slice(),
                [Fact::DispatchClosed {
                    outcome: crate::fact::CloseOutcome::Indeterminate,
                    ..
                }]
            ),
            "the close records the indeterminate outcome and no observation: {facts:?}"
        );

        let after = e.view(&traj(), [log, facts].concat(), 3).unwrap();
        assert_eq!(
            e.handle(
                &after,
                EngineEvent::Outcome(report(ToolOutcome::Success {
                    body: OutcomeBody::Available(ValueBody::new("the ticket")),
                }))
            ),
            Err(crate::transition::TransitionError::ClosedUnobserved)
        );
    }

    #[test]
    fn a_reported_outcome_closes_once_and_repeats_answer_from_the_record() {
        let internal = Audience::restricted([ReaderId::new("insider")]);
        let e = engine_at(vec![crm_tool()], known(TRUSTED, internal));
        let call = call("get_ticket", json!({}));
        let records = vec![opened(&e)];
        let view = e.view(&traj(), records.clone(), 1).unwrap();
        let decision = e
            .handle(
                &view,
                EngineEvent::Proposals(ProposalBatch {
                    id: crate::transition::ProposalBatchId::new("b1"),
                    trajectory: traj(),
                    provider_results: Vec::new(),
                    proposals: vec![raw(&call)],
                    spawn: None,
                    offer_nonce: nonce(),
                    evidence: Vec::new(),
                    audience: crate::audience::AudienceEvidence::default(),
                }),
            )
            .unwrap();
        let FollowUp::Proposals { released, .. } = decision.follow_up else {
            panic!("a proposal batch answers with proposals")
        };
        let dispatch = released[0].dispatch.clone();
        let log = [records, decision.append.unwrap().facts().to_vec()].concat();
        let released_view = e.view(&traj(), log.clone(), 2).unwrap();

        let report = |outcome: ToolOutcome| ToolReport {
            dispatch: dispatch.clone(),
            outcome,
            evidence: Vec::new(),
            offer_nonce: nonce(),
            audience: crate::audience::AudienceEvidence::default(),
        };
        let body = ValueBody::new("the ticket");
        let success = || {
            report(ToolOutcome::Success {
                body: OutcomeBody::Available(body.clone()),
            })
        };

        let closed = e
            .handle(&released_view, EngineEvent::Outcome(success()))
            .expect("the outcome closes the dispatch");
        assert_eq!(
            closed.follow_up,
            FollowUp::Outcome(OutcomeFollowUp::Closed {
                admitted: Some(body.clone())
            })
        );
        let facts = closed.append.expect("the close appends").facts().to_vec();
        assert!(
            matches!(
                facts.as_slice(),
                [Fact::DispatchClosed { .. }, Fact::ValueAdmitted { .. }]
            ),
            "a result that leaves the label where it was moves no basis, so the close declares no advance: {facts:?}"
        );

        let after = e.view(&traj(), [log, facts].concat(), 3).unwrap();
        let repeat = e
            .handle(&after, EngineEvent::Outcome(success()))
            .expect("a repeat of a closed report answers from the record");
        assert_eq!(repeat.append, None);
        assert_eq!(
            repeat.follow_up,
            FollowUp::Outcome(OutcomeFollowUp::Closed { admitted: Some(body) })
        );
        assert_eq!(
            e.handle(
                &after,
                EngineEvent::Outcome(report(ToolOutcome::Success {
                    body: OutcomeBody::Available(ValueBody::new("another ticket")),
                }))
            ),
            Err(crate::transition::TransitionError::ObservationMismatch)
        );
        assert_eq!(
            e.handle(&after, EngineEvent::Outcome(report(ToolOutcome::Failure))),
            Err(crate::transition::TransitionError::ContradictedSuccess)
        );
        assert_eq!(
            e.handle(
                &after,
                EngineEvent::Outcome(report(ToolOutcome::Success {
                    body: OutcomeBody::Unavailable
                }))
            ),
            Err(crate::transition::TransitionError::ObservationMismatch)
        );
        assert_eq!(
            e.handle(
                &after,
                EngineEvent::Outcome(ToolReport {
                    dispatch: DispatchId::new(traj(), call.digest(), 7),
                    outcome: ToolOutcome::Failure,
                    evidence: Vec::new(),
                    offer_nonce: nonce(),
                    audience: crate::audience::AudienceEvidence::default(),
                })
            ),
            Err(crate::transition::TransitionError::UnknownDispatch)
        );
    }

    #[test]
    fn a_bound_sanitizer_checkpoints_before_it_asks_for_the_derivation() {
        let redactor = crate::authority::Sanitizer {
            name: crate::names::SanitizerName::new("redactor"),
            on: crate::authority::SanitizerPoints {
                input: false,
                output: true,
            },
            transition: crate::authority::DeclaredTransition::Trust {
                from_floor: SUSPICIOUS,
                to: TRUSTED,
            },
            scope: crate::authority::Scope::default(),
            hint: None,
        };
        let fetch = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("fetch"),
            tags: vec![],
            delta: Delta {
                trust: Some(SUSPICIOUS),
                audience: None,
            },
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new([EffectKind::new("web.read")]).unwrap(),
            requires: Requires::default(),
        };
        let e = open_engine(RegistryConfig {
            annotators: vec![],
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: declared(vec![fetch]),
            authorities: vec![],
            sanitizers: vec![redactor],
            audience: crate::audience::AudienceConfig::default(),
        });
        let call = call("fetch", json!({}));
        let (log, dispatch) = released_under_output_sanitizer(&e, vec![opened(&e)], &call);
        let view = e.view(&traj(), log.clone(), log.len() as u64).unwrap();

        let raw = ValueBody::new("page bytes");
        let source = crate::value::RawResultDigest::of(raw.as_str().as_bytes());
        let report = |evidence: Vec<crate::transition::Evidence>| ToolReport {
            dispatch: dispatch.clone(),
            outcome: ToolOutcome::Success {
                body: OutcomeBody::Available(raw.clone()),
            },
            evidence,
            offer_nonce: nonce(),
            audience: crate::audience::AudienceEvidence::default(),
        };

        let asked = e.handle(&view, EngineEvent::Outcome(report(Vec::new()))).unwrap();
        assert_eq!(
            asked.follow_up,
            FollowUp::Outcome(OutcomeFollowUp::Resolve(
                crate::transition::EvidenceRequest::Sanitizer {
                    sanitizer: crate::names::SanitizerName::new("redactor"),
                    source,
                    body: raw.clone(),
                }
            ))
        );
        let checkpoint = asked.append.expect("the effects commit before the external step");
        assert!(matches!(
            checkpoint.facts(),
            [
                Fact::BasisAdvanced { .. },
                Fact::DispatchSucceeded {
                    observed: crate::fact::ObservedResult::Available(recorded),
                    ..
                }
            ] if recorded == &source
        ));

        let checkpointed = e
            .view(&traj(), [log.clone(), checkpoint.facts().to_vec()].concat(), 3)
            .unwrap();
        let again = e
            .handle(&checkpointed, EngineEvent::Outcome(report(Vec::new())))
            .unwrap();
        assert_eq!(again.append, None);
        assert_eq!(again.follow_up, asked.follow_up);

        assert_eq!(
            e.handle(
                &checkpointed,
                EngineEvent::Outcome(ToolReport {
                    dispatch: dispatch.clone(),
                    outcome: ToolOutcome::Success {
                        body: OutcomeBody::Available(ValueBody::new("other bytes")),
                    },
                    evidence: Vec::new(),
                    offer_nonce: nonce(),
                    audience: crate::audience::AudienceEvidence::default(),
                })
            ),
            Err(crate::transition::TransitionError::ObservationMismatch)
        );
        assert_eq!(
            e.handle(
                &checkpointed,
                EngineEvent::Outcome(ToolReport {
                    dispatch: dispatch.clone(),
                    outcome: ToolOutcome::Failure,
                    evidence: Vec::new(),
                    offer_nonce: nonce(),
                    audience: crate::audience::AudienceEvidence::default(),
                })
            ),
            Err(crate::transition::TransitionError::ContradictedSuccess)
        );

        let derived = ValueBody::new("page bytes, redacted");
        let crossed = e
            .handle(
                &checkpointed,
                EngineEvent::Outcome(report(vec![crate::transition::Evidence::Sanitizer {
                    sanitizer: crate::names::SanitizerName::new("redactor"),
                    source,
                    derived: derived.clone(),
                }])),
            )
            .expect("the derivation admits");
        assert_eq!(
            crossed.follow_up,
            FollowUp::Outcome(OutcomeFollowUp::Closed {
                admitted: Some(derived)
            })
        );
        let facts = crossed.append.expect("the derivation appends").facts().to_vec();
        assert!(matches!(
            facts.as_slice(),
            [
                Fact::BasisAdvanced { .. },
                Fact::DispatchClosed {
                    outcome: crate::fact::CloseOutcome::Success { effects },
                    ..
                },
                Fact::CandidateDerived { .. },
                Fact::ValueAdmitted { .. }
            ] if effects.is_empty()
        ));

        let whole = [log.clone(), checkpoint.facts().to_vec(), facts.clone()].concat();
        assert_eq!(e.view(&traj(), whole.clone(), 4).map(|_| ()), Ok(()));
        assert_eq!(
            e.view(&traj(), whole[..whole.len() - 1].to_vec(), 4)
                .map(|_| ())
                .unwrap_err(),
            crate::transition::TransitionRefusal::UnadmittedDerivation
        );

        let settled = e.view(&traj(), whole, 4).unwrap();
        let derived = ValueBody::new("page bytes, redacted");
        assert_eq!(
            e.handle(
                &settled,
                EngineEvent::Outcome(report(vec![crate::transition::Evidence::Sanitizer {
                    sanitizer: crate::names::SanitizerName::new("redactor"),
                    source,
                    derived: derived.clone(),
                }])),
            )
            .expect("the same report answers from the record")
            .follow_up,
            FollowUp::Outcome(OutcomeFollowUp::Closed {
                admitted: Some(derived)
            })
        );
        assert_eq!(
            e.handle(
                &settled,
                EngineEvent::Outcome(ToolReport {
                    dispatch: dispatch.clone(),
                    outcome: ToolOutcome::Success {
                        body: OutcomeBody::Available(ValueBody::new("other bytes")),
                    },
                    evidence: Vec::new(),
                    offer_nonce: nonce(),
                    audience: crate::audience::AudienceEvidence::default(),
                })
            ),
            Err(crate::transition::TransitionError::ObservationMismatch)
        );
        assert_eq!(
            e.handle(
                &settled,
                EngineEvent::Outcome(ToolReport {
                    dispatch,
                    outcome: ToolOutcome::Failure,
                    evidence: Vec::new(),
                    offer_nonce: nonce(),
                    audience: crate::audience::AudienceEvidence::default(),
                })
            ),
            Err(crate::transition::TransitionError::ContradictedSuccess)
        );
    }

    fn staged_engine() -> Engine {
        let sanitizer = |name: &str, from_floor: Trust, to: Trust| crate::authority::Sanitizer {
            name: crate::names::SanitizerName::new(name),
            on: crate::authority::SanitizerPoints {
                input: false,
                output: true,
            },
            transition: crate::authority::DeclaredTransition::Trust { from_floor, to },
            scope: crate::authority::Scope::default(),
            hint: None,
        };
        open_engine(RegistryConfig {
            annotators: vec![],
            trust_chain: TrustChain::new(vec!["secret".into(), "suspicious".into(), "trusted".into()]),
            tools: declared(vec![
                ToolAnnotation {
                    description: Some("A test tool.".to_string()),
                    name: ToolName::new("fetch"),
                    tags: vec![],
                    delta: Delta {
                        trust: Some(Trust::new(0)),
                        audience: None,
                    },
                    parameters: crate::params::ToolParameters::open(),
                    emits: EffectSet::new([EffectKind::new("web.read")]).unwrap(),
                    requires: Requires::default(),
                },
                ToolAnnotation {
                    delta: Delta {
                        trust: Some(Trust::new(2)),
                        audience: None,
                    },
                    ..open_tool("ping")
                },
            ]),
            authorities: vec![],
            sanitizers: vec![
                sanitizer("redactor", Trust::new(0), Trust::new(1)),
                sanitizer("scrubber", Trust::new(1), Trust::new(2)),
            ],
            audience: crate::audience::AudienceConfig::default(),
        })
    }

    fn staged_candidate(e: &Engine) -> (Vec<Fact>, DispatchId, ValueBody, EngineDecision) {
        let call = call("fetch", json!({}));
        let mut log = vec![opened(e)];
        let blocked = proposed(e, &log, "b1", nonce(), call.clone()).expect("the batch decides");
        let bound = opened_offers(&appended_facts(blocked))
            .into_iter()
            .find(|(_, plan)| plan.sanitizer() == Some(&crate::names::SanitizerName::new("redactor")))
            .expect("a narrowing block offers the applicable output sanitizer's dispatch path")
            .0;
        log = [
            log.clone(),
            appended_facts(proposed(e, &log, "b1", nonce(), call.clone()).expect("the batch decides")),
        ]
        .concat();
        log = [
            log.clone(),
            appended_facts(execute_offer(e, &log, bound, OfferOutcome::Approved(Vec::new())).expect("the offer runs")),
        ]
        .concat();
        let released = proposed(e, &log, "b2", nonce(), call).expect("the approved call releases");
        let dispatch = match &released.follow_up {
            FollowUp::Proposals { released, .. } => released[0].dispatch.clone(),
            other => panic!("the approved proposal releases, got {other:?}"),
        };
        log = [log, appended_facts(released)].concat();

        let raw = ValueBody::new("page bytes");
        let derived = ValueBody::new("page bytes, redacted");
        let report = |evidence: Vec<crate::transition::Evidence>| ToolReport {
            dispatch: dispatch.clone(),
            outcome: ToolOutcome::Success {
                body: OutcomeBody::Available(raw.clone()),
            },
            evidence,
            offer_nonce: nonce(),
            audience: crate::audience::AudienceEvidence::default(),
        };
        let view = |log: &[Fact]| {
            e.view(&traj(), log.to_vec(), log.len() as u64)
                .expect("the log replays")
        };
        let asked = e
            .handle(&view(&log), EngineEvent::Outcome(report(Vec::new())))
            .expect("the confined result asks for its derivation");
        log = [log, appended_facts(asked)].concat();
        let staged = e
            .handle(
                &view(&log),
                EngineEvent::Outcome(report(vec![crate::transition::Evidence::Sanitizer {
                    sanitizer: crate::names::SanitizerName::new("redactor"),
                    source: crate::value::RawResultDigest::of(raw.as_str().as_bytes()),
                    derived: derived.clone(),
                }])),
            )
            .expect("the derivation stages");
        (log, dispatch, derived, staged)
    }

    fn confined_of(follow_up: &FollowUp) -> &Confined {
        match follow_up {
            FollowUp::Outcome(OutcomeFollowUp::Staged(confined)) | FollowUp::Offer(OfferFollowUp::Staged(confined)) => {
                confined
            }
            other => panic!("a residual-bearing derivation stages, got {other:?}"),
        }
    }

    #[test]
    fn a_narrowing_derivation_stages_its_candidate_instead_of_admitting_it() {
        let e = staged_engine();
        let (log, dispatch, derived, staged) = staged_candidate(&e);
        let confined = confined_of(&staged.follow_up);
        assert_eq!(confined.dispatch, dispatch);
        assert_eq!(confined.candidate.body, derived);
        assert_eq!(
            confined.residual,
            crate::check::Narrowing {
                from: established(Trust::new(2), Audience::public()),
                to: established(Trust::new(1), Audience::public()),
            }
        );
        let facts = appended_facts(staged.clone());
        assert!(
            !facts
                .iter()
                .any(|fact| matches!(fact, Fact::ValueAdmitted { .. } | Fact::DispatchClosed { .. })),
            "a staged candidate admits nothing and closes nothing: {facts:?}"
        );
        let stage: Vec<_> = opened_offers(&facts).into_iter().map(|(_, plan)| plan).collect();
        assert_eq!(
            stage
                .iter()
                .filter_map(plan::ExecutableRemedyPlan::hop)
                .collect::<Vec<_>>(),
            vec![&crate::names::SanitizerName::new("scrubber")],
            "the stage offers the sanitizer that only became applicable once the first hop widened the candidate"
        );
        assert_eq!(
            stage
                .iter()
                .filter_map(plan::ExecutableRemedyPlan::narrowing)
                .collect::<Vec<_>>(),
            vec![&confined.residual],
            "and acceptance of exactly the residual, never a guess made before the derivation existed"
        );

        let log = [log, facts].concat();
        let again = e
            .handle(
                &e.view(&traj(), log.clone(), log.len() as u64).unwrap(),
                EngineEvent::Outcome(ToolReport {
                    dispatch,
                    outcome: ToolOutcome::Success {
                        body: OutcomeBody::Available(ValueBody::new("page bytes")),
                    },
                    evidence: Vec::new(),
                    offer_nonce: nonce(),
                    audience: crate::audience::AudienceEvidence::default(),
                }),
            )
            .expect("the repeat hears the stage");
        assert_eq!(again.append, None);
        assert_eq!(confined_of(&again.follow_up), confined);
    }

    #[test]
    fn a_spend_of_an_offer_the_log_never_accepted_is_refused() {
        let without_acceptance = |log: &[Fact], batch: Vec<Fact>| {
            let kept: Vec<Fact> = batch
                .into_iter()
                .filter(|fact| !matches!(fact, Fact::OfferAccepted { .. }))
                .collect();
            [log.to_vec(), kept].concat()
        };

        let e = staged_engine();
        let (log, _, derived, staged) = staged_candidate(&e);
        let confined = confined_of(&staged.follow_up).clone();
        let log = [log, appended_facts(staged)].concat();
        let hop = confined.offers[0].0;
        let accept = confined.offers[1].0;

        let hopped = appended_facts(
            execute_offer(
                &e,
                &log,
                hop,
                OfferOutcome::Derived(crate::transition::Evidence::Sanitizer {
                    sanitizer: crate::names::SanitizerName::new("scrubber"),
                    source: crate::value::RawResultDigest::of(derived.as_str().as_bytes()),
                    derived: ValueBody::new("page bytes, redacted, scrubbed"),
                }),
            )
            .expect("the hop runs"),
        );
        assert_eq!(
            e.validate_replay(&without_acceptance(&log, hopped)),
            Err(crate::transition::TransitionRefusal::OfferEnded),
            "a confined hop's successor stands on the acceptance that authorised it"
        );

        let accepted = appended_facts(
            execute_offer(&e, &log, accept, OfferOutcome::Approved(Vec::new())).expect("the acceptance runs"),
        );
        assert_eq!(
            e.validate_replay(&without_acceptance(&log, accepted)),
            Err(crate::transition::TransitionRefusal::OfferEnded),
            "a candidate crosses at a cost the agent accepted, never one the log asserts"
        );

        let e = substituting_engine(TRUSTED);
        let proposal = call("post", json!({ "body": "ssn 123" }));
        let log = internal_log(&e);
        let facts = appended_facts(proposed(&e, &log, "b1", nonce(), proposal.clone()).expect("the batch decides"));
        let offers = opened_offers(&facts);
        let log = [log, facts].concat();
        let substituted = appended_facts(
            execute_offer(&e, &log, offers[0].0, substitution(&proposal, REDACTED)).expect("the hop runs"),
        );
        assert_eq!(
            e.validate_replay(&without_acceptance(&log, substituted)),
            Err(crate::transition::TransitionRefusal::OfferEnded),
            "a substituted candidate stands on the acceptance that authorised it"
        );
    }

    #[test]
    fn offer_consults_names_the_external_work_each_offer_needs() {
        use crate::transition::{OfferConsult, OfferOutcome};

        let e = staged_engine();
        let (log, _dispatch, derived, staged) = staged_candidate(&e);
        let confined = confined_of(&staged.follow_up).clone();
        let log = [log, appended_facts(staged)].concat();
        let hop = confined.offers[0].0;
        let accept = confined.offers[1].0;

        assert_eq!(
            e.offer_consults(&viewing(&e, &log), &traj(), &hop),
            Ok(OfferConsult::Sanitizer {
                sanitizer: crate::names::SanitizerName::new("scrubber"),
                source: crate::value::RawResultDigest::of(derived.as_str().as_bytes()),
                body: derived.clone(),
                tool: Some(ToolName::new("fetch")),
            }),
        );
        assert_eq!(
            e.offer_consults(&viewing(&e, &log), &traj(), &accept),
            Ok(OfferConsult::Accept),
        );

        let crossed = [
            log.clone(),
            appended_facts(execute_offer(&e, &log, accept, OfferOutcome::Approved(Vec::new())).expect("it crosses")),
        ]
        .concat();
        assert_eq!(
            e.offer_consults(&viewing(&e, &crossed), &traj(), &accept),
            Ok(OfferConsult::Replay(OfferOutcome::Approved(Vec::new()))),
        );

        let e = substituting_engine(TRUSTED);
        let proposal = call("post", json!({ "body": "ssn 123" }));
        let log = internal_log(&e);
        let facts = appended_facts(proposed(&e, &log, "b1", nonce(), proposal.clone()).expect("the batch decides"));
        let input_hop = opened_offers(&facts)[0].0;
        let log = [log, facts].concat();
        assert_eq!(
            e.offer_consults(&viewing(&e, &log), &traj(), &input_hop),
            Ok(OfferConsult::Rewrite {
                sanitizer: crate::names::SanitizerName::new("redact"),
                call: proposal.clone(),
            }),
        );

        // A spent input hop answers from the record as the rewrite it was, and only for one.
        let hopped = appended_facts(
            execute_offer(&e, &log, input_hop, substitution(&proposal, REDACTED)).expect("the hop runs"),
        );
        let spent = [log, hopped].concat();
        assert_eq!(
            e.offer_consults(&viewing(&e, &spent), &traj(), &input_hop),
            Ok(OfferConsult::Replay(OfferOutcome::Derived(Evidence::Rewrite {
                sanitizer: crate::names::SanitizerName::new("redact"),
                source: crate::value::RawResultDigest::of(&[]),
                derived: ValueBody::new(""),
                annotation: None,
            }))),
        );
        assert_eq!(
            execute_offer(
                &e,
                &spent,
                input_hop,
                OfferOutcome::Derived(Evidence::Sanitizer {
                    sanitizer: crate::names::SanitizerName::new("redact"),
                    source: crate::value::RawResultDigest::of(&[]),
                    derived: ValueBody::new(""),
                }),
            )
            .err(),
            Some(TransitionError::PlanOutcomeMismatch),
            "an output derivation names another kind of offer"
        );
    }

    #[test]
    fn a_confined_stage_is_left_by_a_hop_or_by_accepting_its_residual() {
        let e = staged_engine();
        let (log, dispatch, derived, staged) = staged_candidate(&e);
        let confined = confined_of(&staged.follow_up).clone();
        let log = [log, appended_facts(staged)].concat();
        let plan_of = |offer: &crate::value::OfferId| {
            confined
                .offers
                .iter()
                .find(|(id, _)| id == offer)
                .map(|(_, plan)| *plan)
                .expect("the stage's own offer")
        };
        let scrubbed = ValueBody::new("page bytes, redacted, scrubbed");
        let hop = confined.offers[0].0;
        let accept = confined.offers[1].0;
        assert_ne!(plan_of(&hop), plan_of(&accept));

        assert_eq!(
            execute_offer(
                &e,
                &log,
                hop,
                OfferOutcome::Derived(crate::transition::Evidence::Sanitizer {
                    sanitizer: crate::names::SanitizerName::new("scrubber"),
                    source: crate::value::RawResultDigest::of(b"page bytes"),
                    derived: scrubbed.clone(),
                })
            ),
            Err(TransitionError::EvidenceMismatch),
            "a derivation of the raw bytes is not a derivation of the candidate standing on them"
        );
        assert_eq!(
            execute_offer(&e, &log, hop, OfferOutcome::Approved(Vec::new())),
            Err(TransitionError::PlanOutcomeMismatch)
        );

        let hopped = execute_offer(
            &e,
            &log,
            hop,
            OfferOutcome::Derived(crate::transition::Evidence::Sanitizer {
                sanitizer: crate::names::SanitizerName::new("scrubber"),
                source: crate::value::RawResultDigest::of(derived.as_str().as_bytes()),
                derived: scrubbed.clone(),
            }),
        )
        .expect("the hop runs");
        assert_eq!(
            offer_answer(&hopped),
            &OfferFollowUp::Admitted {
                value: scrubbed.clone()
            }
        );
        let hopped = appended_facts(hopped);
        assert!(
            hopped
                .iter()
                .any(|fact| matches!(fact, Fact::OfferInvalidated { offer, .. } if offer == &accept)),
            "taking the candidate ends every other offer standing on it: {hopped:?}"
        );
        let after = e
            .view(&traj(), [log.clone(), hopped].concat(), (log.len() + 8) as u64)
            .expect("the hop's batch replays");
        assert_eq!(
            after.projection().view(&traj()).current_label(),
            established(Trust::new(2), Audience::public()),
            "a candidate that narrows nothing costs the trajectory nothing"
        );
        assert!(
            after
                .projection()
                .view(&traj())
                .candidate(&crate::basis::SubjectKey::ConfinedResult(dispatch.clone()))
                .is_none(),
            "an admitted candidate leaves the staging model"
        );

        let accepted =
            execute_offer(&e, &log, accept, OfferOutcome::Approved(Vec::new())).expect("the acceptance runs");
        assert_eq!(offer_answer(&accepted), &OfferFollowUp::Admitted { value: derived });
        let accepted = appended_facts(accepted);
        assert!(
            accepted.iter().any(|fact| matches!(
                fact,
                Fact::CandidateAccepted { narrowing, .. } if narrowing == &confined.residual
            )),
            "the acceptance names exactly the residual the candidate owed: {accepted:?}"
        );
        let after = e
            .view(
                &traj(),
                [log.clone(), accepted.clone()].concat(),
                (log.len() + 8) as u64,
            )
            .expect("the acceptance's batch replays");
        assert_eq!(
            after.projection().view(&traj()).current_label(),
            established(Trust::new(1), Audience::public())
        );
        assert!(
            after
                .projection()
                .view(&traj())
                .candidate(&crate::basis::SubjectKey::ConfinedResult(dispatch.clone()))
                .is_none(),
            "accepting the residual ends the stage as surely as hopping past it"
        );
        assert_eq!(
            e.view(
                &traj(),
                [log.clone(), accepted[..accepted.len() - 1].to_vec()].concat(),
                (log.len() + 8) as u64,
            )
            .map(|_| ())
            .unwrap_err(),
            crate::transition::TransitionRefusal::UnadmittedDerivation
        );
        let settled = [log, accepted].concat();
        assert_eq!(
            offer_answer(
                &execute_offer(&e, &settled, accept, OfferOutcome::Approved(Vec::new())).expect("the repeat answers")
            ),
            &OfferFollowUp::Admitted {
                value: ValueBody::new("page bytes, redacted")
            }
        );
        assert_eq!(
            execute_offer(
                &e,
                &settled,
                accept,
                OfferOutcome::Derived(crate::transition::Evidence::Sanitizer {
                    sanitizer: crate::names::SanitizerName::new("scrubber"),
                    source: crate::value::RawResultDigest::of(b"page bytes, redacted"),
                    derived: scrubbed,
                })
            )
            .map(|_| ()),
            Err(TransitionError::PlanOutcomeMismatch)
        );
        assert_eq!(
            execute_offer(
                &e,
                &settled,
                accept,
                OfferOutcome::Approved(vec![stray_evidence(accept)])
            )
            .map(|_| ()),
            Err(TransitionError::PlanOutcomeMismatch),
            "an acceptance carrying evidence is refused after the offer ends, as it is before"
        );
    }

    #[test]
    fn a_confined_stage_goes_stale_with_its_basis_and_is_planned_again() {
        let e = staged_engine();
        let (log, dispatch, _, staged) = staged_candidate(&e);
        let confined = confined_of(&staged.follow_up).clone();
        let log = [log, appended_facts(staged)].concat();
        let log = [
            log.clone(),
            appended_facts(proposed(&e, &log, "b3", nonce(), call("ping", json!({}))).expect("the open call releases")),
        ]
        .concat();
        assert_eq!(
            execute_offer(&e, &log, confined.offers[1].0, OfferOutcome::Approved(Vec::new())),
            Err(TransitionError::StaleOffer)
        );

        let replanned = e
            .handle(
                &e.view(&traj(), log.clone(), log.len() as u64).unwrap(),
                EngineEvent::Outcome(ToolReport {
                    dispatch,
                    outcome: ToolOutcome::Success {
                        body: OutcomeBody::Available(ValueBody::new("page bytes")),
                    },
                    evidence: Vec::new(),
                    offer_nonce: crate::value::OfferNonce::new([13u8; 32]),
                    audience: crate::audience::AudienceEvidence::default(),
                }),
            )
            .expect("the candidate is planned again");
        let fresh = confined_of(&replanned.follow_up);
        assert_eq!(fresh.candidate, confined.candidate, "the candidate itself is durable");
        assert_eq!(fresh.residual, confined.residual);
        assert!(
            fresh
                .offers
                .iter()
                .all(|(offer, _)| !confined.offers.iter().any(|(stale, _)| stale == offer)),
            "a stale offer never revives: the new stage carries identities of its own"
        );
        let fresh = fresh.offers[1].0;
        let log = [log, appended_facts(replanned)].concat();
        assert!(matches!(
            offer_answer(
                &execute_offer(&e, &log, fresh, OfferOutcome::Approved(Vec::new())).expect("the fresh offer runs")
            ),
            OfferFollowUp::Admitted { .. }
        ));
    }

    fn substituting_engine(trust: Trust) -> Engine {
        let partner = Audience::restricted([ReaderId::new("partner")]);
        let post = post_tool;
        open_engine_at(
            RegistryConfig {
                annotators: vec![annotator("acl")],
                trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
                tools: {
                    let mut tools = declared(vec![
                        post("post", vec![crate::names::TagName::new("outbound")]),
                        post("post_untagged", vec![]),
                        restrictable_tool("ping"),
                    ]);
                    tools.push(annotated(
                        post("post_dyn", vec![crate::names::TagName::new("outbound")]),
                        "acl",
                    ));
                    tools
                },
                authorities: vec![crate::authority::Authority {
                    name: AuthorityName::new("officer"),
                    mandate: crate::authority::Mandate {
                        trust_ceiling: Some(TRUSTED),
                        reader_ceiling: Some(DeclaredAudience::literal(partner)),
                        ..crate::authority::Mandate::default()
                    },
                    scope: crate::authority::Scope::default(),
                    hint: None,
                }],
                sanitizers: vec![crate::authority::Sanitizer {
                    name: crate::names::SanitizerName::new("redact"),
                    on: crate::authority::SanitizerPoints {
                        input: true,
                        output: false,
                    },
                    transition: crate::authority::DeclaredTransition::Audience {
                        from_includes: DeclaredAudience::restricted([ReaderId::new("insider")]),
                        to: DeclaredAudience::restricted([ReaderId::new("insider"), ReaderId::new("partner")]),
                    },
                    scope: crate::authority::Scope {
                        tags: vec![crate::names::TagName::new("outbound")],
                    },
                    hint: None,
                }],
                audience: crate::audience::AudienceConfig::default(),
            },
            known(trust, Audience::restricted([ReaderId::new("insider")])),
        )
    }

    fn internal_log(e: &Engine) -> Vec<Fact> {
        vec![opened(e)]
    }

    fn post_tool(name: &str, tags: Vec<crate::names::TagName>) -> ToolAnnotation {
        ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new(name),
            tags,
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::compile(&json!({
                "type": "object",
                "properties": { "body": { "type": "string" } },
                "required": ["body"],
            }))
            .unwrap(),
            emits: EffectSet::new([EffectKind::new("outbound.post")]).unwrap(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(TRUSTED),
                    audience: vec![AudienceRequirement::Includes(RecipientSpec::Static(
                        DeclaredAudience::restricted([ReaderId::new("partner")]),
                    ))],
                },
                ..Requires::default()
            },
        }
    }

    /// The complete annotation `acl` produces for a `post_dyn` call: the declaration's
    /// operational metadata with the recipients the answer requires.
    fn post_dyn_annotation(readers: &[&str]) -> ToolAnnotation {
        let mut produced = post_tool("post_dyn", vec![crate::names::TagName::new("outbound")]);
        produced.requires.label.audience = vec![AudienceRequirement::Includes(RecipientSpec::Static(
            DeclaredAudience::literal(Audience::restricted(
                readers.iter().map(|reader| ReaderId::new(*reader)),
            )),
        ))];
        produced
    }

    fn substitution(call: &ResolvedCall, replacement: &str) -> OfferOutcome {
        OfferOutcome::Derived(crate::transition::Evidence::Rewrite {
            sanitizer: crate::names::SanitizerName::new("redact"),
            source: crate::value::RawResultDigest::of(call.canonical_arguments().canonical_bytes()),
            derived: ValueBody::new(replacement),
            annotation: None,
        })
    }

    const REDACTED: &str = r#"{"body":"[redacted]"}"#;

    #[test]
    fn an_input_hop_is_offered_before_the_ruling_that_covers_the_same_gap() {
        let e = substituting_engine(TRUSTED);
        let log = internal_log(&e);
        let plans = |tool: &str| {
            opened_offers(&appended_facts(
                proposed(&e, &log, "b1", nonce(), call(tool, json!({ "body": "ssn 123" }))).expect("the batch decides"),
            ))
            .into_iter()
            .map(|(_, plan)| plan)
            .collect::<Vec<_>>()
        };
        let tagged = plans("post");
        assert_eq!(
            tagged.iter().map(plan::ExecutableRemedyPlan::hop).collect::<Vec<_>>(),
            vec![Some(&crate::names::SanitizerName::new("redact")), None]
        );
        assert_eq!(
            tagged[1]
                .required
                .iter()
                .map(|required| required.authority.clone())
                .collect::<Vec<_>>(),
            vec![AuthorityName::new("officer")]
        );
        assert!(
            plans("post_untagged").iter().all(|plan| plan.hop().is_none()),
            "a sanitizer scoped to a tag the callee does not carry has no jurisdiction over it"
        );
    }

    #[test]
    fn a_marked_spawn_is_offered_no_input_hop_while_its_identical_sibling_is() {
        let e = substituting_engine(TRUSTED);
        let log = internal_log(&e);
        let post = call("post", json!({ "body": "ssn 123" }));
        let batch = || {
            batch_on(
                &traj(),
                "b1",
                Vec::new(),
                vec![raw(&post), raw(&post)],
                Some(SpawnMark::at(1)),
            )
        };
        let decision = e.handle(&viewing(&e, &log), batch()).expect("the batch decides");
        let hops = |blocked: &[Blocked]| -> Vec<Vec<bool>> {
            blocked
                .iter()
                .map(|block| {
                    block
                        .block
                        .plans
                        .iter()
                        .filter_map(plan::RemedyPlan::executable)
                        .map(|plan| plan.hop().is_some())
                        .collect()
                })
                .collect()
        };
        let (_, blocked) = answered(&decision);
        assert_eq!(hops(blocked), vec![vec![true, false], vec![false]]);
        assert_eq!(blocked[0].offers.len(), 2);
        assert_eq!(blocked[1].offers.len(), 1);
        let facts = appended_facts(decision);
        let log = [log, facts.clone()].concat();
        assert_eq!(e.validate_replay(&log), Ok(()));

        let repeat = e.handle(&viewing(&e, &log), batch()).expect("the repeat answers");
        assert!(repeat.append.is_none());
        let (_, blocked) = answered(&repeat);
        assert_eq!(hops(blocked), vec![vec![true, false], vec![false]]);

        let (unmarked_hop, marked_offer) = {
            let mut opened = facts.iter().filter(|fact| matches!(fact, Fact::OfferOpened { .. }));
            (
                opened.next().expect("the sibling's hop").clone(),
                opened.nth(1).expect("the marked plan").clone(),
            )
        };
        let mut forged = marked_offer;
        if let (Fact::OfferOpened { plan, offer, block, .. }, Fact::OfferOpened { plan: hop, .. }) =
            (&mut forged, &unmarked_hop)
        {
            *plan = hop.clone();
            *offer = crate::value::OfferId::of_plan(block, 9, b"forged");
        }
        let mut with_hop = log.clone();
        with_hop.push(forged);
        assert_eq!(e.validate_replay(&with_hop), Err(TransitionRefusal::UnbackedOffer));
    }

    #[test]
    fn a_substitution_that_clears_the_last_gap_dispatches_in_the_hops_own_batch() {
        let e = substituting_engine(TRUSTED);
        let proposal = call("post", json!({ "body": "ssn 123" }));
        let log = internal_log(&e);
        let facts = appended_facts(proposed(&e, &log, "b1", nonce(), proposal.clone()).expect("the batch decides"));
        let offers = opened_offers(&facts);
        let log = [log, facts].concat();

        let hopped = execute_offer(&e, &log, offers[0].0, substitution(&proposal, REDACTED)).expect("the hop runs");
        let released = match offer_answer(&hopped) {
            OfferFollowUp::Released(released) => (**released).clone(),
            other => panic!("an immediately admissible substitution dispatches, got {other:?}"),
        };
        assert_eq!(released.call.canonical_arguments().canonical_text(), REDACTED);
        let facts = appended_facts(hopped);
        assert!(
            matches!(
                facts.as_slice(),
                [
                    Fact::BasisAdvanced { .. },
                    Fact::OfferAccepted { .. },
                    Fact::OfferInvalidated { offer, .. },
                    Fact::CandidateDerived {
                        derived: DerivedCandidate::Call { call, .. },
                        ..
                    },
                    Fact::DispatchOpened { dispatch, .. },
                ] if offer == &offers[1].0 && call == &released.call && dispatch == &released.dispatch
            ),
            "the hop commits its candidate, ends the sibling standing on the predecessor it \
             replaced, and opens the dispatch: {facts:?}"
        );
        let before = basis_of(&e, &log);
        let log = [log, facts].concat();
        assert_eq!(e.validate_replay(&log), Ok(()));
        assert_eq!(
            basis_of(&e, &log).family,
            before.family.next(),
            "the release the hop earned reserves its effect, and the act declared that advance"
        );

        let mut forged = log.clone();
        let mut second = forged.last().expect("the opening is the batch's last record").clone();
        let Fact::DispatchOpened { dispatch, .. } = &mut second else {
            panic!("the hop's batch ends with its opening")
        };
        *dispatch = DispatchId::new(traj(), *dispatch.digest(), 1);
        forged.push(second);
        assert_eq!(
            e.validate_replay(&forged),
            Err(crate::transition::TransitionRefusal::UnbackedDecision)
        );

        let plain = proposed(&e, &log, "b2", nonce(), call("post", json!({ "body": "[redacted]" })))
            .expect("the batch decides");
        match &plain.follow_up {
            FollowUp::Proposals { released, blocked, .. } => {
                assert!(released.is_empty());
                assert_eq!(
                    blocked[0].block.raw.requirement_gaps,
                    vec![Gap::Includes {
                        recipients: DeclaredAudience::restricted([ReaderId::new("partner")])
                    }]
                );
            }
            other => panic!("a fresh proposal decides as proposals, got {other:?}"),
        }
    }

    #[test]
    fn a_forged_answer_neither_persists_on_a_candidate_nor_releases_one() {
        let e = substituting_engine(TRUSTED);
        let proposal = call("post", json!({ "body": "ssn 123" }));
        let log = internal_log(&e);
        let facts = appended_facts(proposed(&e, &log, "b1", nonce(), proposal.clone()).expect("the batch decides"));
        let offers = opened_offers(&facts);
        let log = [log, facts].concat();
        let facts = appended_facts(
            execute_offer(&e, &log, offers[0].0, substitution(&proposal, REDACTED)).expect("the hop runs"),
        );
        let log = [log, facts].concat();
        assert_eq!(e.validate_replay(&log), Ok(()));

        let answer = || pinned_for(plain_tool("post"), "ghost", &proposal);

        let mut forged = log.clone();
        let candidate = forged.len() - 2;
        let Fact::CandidateDerived {
            derived: DerivedCandidate::Call { call, .. },
            ..
        } = &mut forged[candidate]
        else {
            panic!("the hop's batch records its candidate before the opening")
        };
        *call = call.clone().with_annotation(Some(answer()));
        assert_eq!(
            e.validate_replay(&forged),
            Err(crate::transition::TransitionRefusal::ForgedEvidence),
            "a static declaration is its own annotation: a pinned candidate under it is forged"
        );

        let mut forged = log.clone();
        let Fact::DispatchOpened { annotation, .. } =
            forged.last_mut().expect("the opening is the batch's last record")
        else {
            panic!("the hop's batch ends with its opening")
        };
        *annotation = Some(answer());
        assert_eq!(
            e.validate_replay(&forged),
            Err(crate::transition::TransitionRefusal::UnbackedDecision),
            "a static dispatch record carrying a pin diverges from the decision that released it"
        );
    }

    #[test]
    fn a_repeat_of_a_hop_names_the_dispatch_that_hop_opened() {
        let e = substituting_engine(TRUSTED);
        let proposal = call("post", json!({ "body": "ssn 123" }));
        let log = internal_log(&e);

        let hop_of = |log: &Vec<Fact>, batch: &str| {
            let facts = appended_facts(proposed(&e, log, batch, nonce(), proposal.clone()).expect("the batch decides"));
            let offer = opened_offers(&facts)[0].0;
            ([log.clone(), facts].concat(), offer)
        };
        let released_by = |log: &Vec<Fact>, offer| match offer_answer(
            &execute_offer(&e, log, offer, substitution(&proposal, REDACTED)).expect("the hop runs"),
        ) {
            OfferFollowUp::Released(released) => released.dispatch.clone(),
            other => panic!("an immediately admissible substitution dispatches, got {other:?}"),
        };

        let (log, first) = hop_of(&log, "b1");
        let ran = released_by(&log, first);
        let log = [
            log.clone(),
            appended_facts(execute_offer(&e, &log, first, substitution(&proposal, REDACTED)).expect("the hop runs")),
        ]
        .concat();

        let (log, second) = hop_of(&log, "b2");
        let again = released_by(&log, second);
        assert_ne!(ran, again, "each candidate earns its own dispatch");
        let log = [
            log.clone(),
            appended_facts(execute_offer(&e, &log, second, substitution(&proposal, REDACTED)).expect("the hop runs")),
        ]
        .concat();

        let repeat = |offer| match offer_answer(
            &execute_offer(&e, &log, offer, substitution(&proposal, REDACTED)).expect("the repeat answers"),
        ) {
            OfferFollowUp::Released(released) => released.dispatch.clone(),
            OfferFollowUp::Settled(settled) => settled.dispatch.clone(),
            other => panic!("a spent hop answers from its record, got {other:?}"),
        };
        assert_eq!(repeat(first), ran);
        assert_eq!(repeat(second), again, "the second hop's repeat names its own dispatch");
    }

    #[test]
    fn an_opening_names_the_position_the_decision_released_it_for() {
        let e = engine(vec![plain_tool("ping")]);
        let log = vec![opened(&e)];
        let ping = call("ping", json!({}));
        let decided = e
            .handle(
                &viewing(&e, &log),
                batch("b1", Vec::new(), vec![raw(&ping), raw(&ping)]),
            )
            .expect("the batch decides");
        let (released, blocked) = answered(&decided);
        assert!(blocked.is_empty());
        assert_eq!(released.len(), 2, "identical siblings take their own occurrences");
        let log = [log, appended_facts(decided)].concat();
        assert_eq!(e.validate_replay(&log), Ok(()));

        let subject = |position: u32| crate::basis::SubjectKey::Call {
            trajectory: traj(),
            batch: crate::transition::ProposalBatchId::new("b1"),
            position,
        };
        let openings: Vec<_> = log
            .iter()
            .filter_map(|fact| match fact {
                Fact::DispatchOpened { dispatch, subject, .. } => Some((dispatch.clone(), subject.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(openings[0].1, subject(0));
        assert_eq!(openings[1].1, subject(1));

        let mut forged = log.clone();
        let second = forged
            .iter_mut()
            .filter_map(|fact| match fact {
                Fact::DispatchOpened { subject, .. } => Some(subject),
                _ => None,
            })
            .nth(1)
            .expect("the batch opened two dispatches");
        *second = subject(0);
        assert_eq!(
            e.validate_replay(&forged),
            Err(crate::transition::TransitionRefusal::UnbackedDecision)
        );
    }

    #[test]
    fn a_substitution_that_leaves_a_gap_re_plans_over_the_derived_call() {
        let e = substituting_engine(SUSPICIOUS);
        let proposal = call("post", json!({ "body": "ssn 123" }));
        let log = internal_log(&e);
        let facts = appended_facts(proposed(&e, &log, "b1", nonce(), proposal.clone()).expect("the batch decides"));
        let hop = opened_offers(&facts)[0].0;
        let log = [log, facts].concat();

        let hopped = execute_offer(&e, &log, hop, substitution(&proposal, REDACTED)).expect("the hop runs");
        let block = match offer_answer(&hopped) {
            OfferFollowUp::Substituted { block } => (**block).clone(),
            other => panic!("a substitution that still blocks re-plans, got {other:?}"),
        };
        assert_eq!(block.call.canonical_arguments().canonical_text(), REDACTED);
        assert_eq!(
            block.block.raw.requirement_gaps,
            vec![Gap::TrustFloor {
                required: TRUSTED,
                actual: SUSPICIOUS,
            }],
            "the substitution cleared the recipients its `to` declared, and nothing else"
        );
        let facts = appended_facts(hopped);
        let stage = opened_offers(&facts);
        assert_eq!(
            stage.len(),
            1,
            "the spent sanitizer is not offered again on its own successor"
        );
        assert!(stage[0].1.hop().is_none());
        let log = [log, facts].concat();

        let (offer, plan) = stage[0].clone();
        let evidence = evidence_for(
            offer,
            &plan,
            "post",
            partial(SUSPICIOUS, Audience::restricted([ReaderId::new("insider")])),
        );
        let approved = execute_offer(&e, &log, offer, OfferOutcome::Approved(evidence)).expect("the officer answers");
        assert_eq!(
            offer_answer(&approved),
            &OfferFollowUp::Approved {
                call: Box::new(block.call.clone())
            }
        );
        let log = [log, appended_facts(approved)].concat();

        let released = proposed(&e, &log, "b2", nonce(), block.call).expect("the approved call releases");
        match &released.follow_up {
            FollowUp::Proposals { released, .. } => assert_eq!(released.len(), 1),
            other => panic!("the approved proposal releases, got {other:?}"),
        }
        let log = [log, appended_facts(released)].concat();
        assert_eq!(e.validate_replay(&log), Ok(()));
        match &proposed(&e, &log, "b3", nonce(), proposal)
            .expect("the batch decides")
            .follow_up
        {
            FollowUp::Proposals { released, blocked, .. } => {
                assert!(released.is_empty());
                assert_eq!(blocked.len(), 1);
            }
            other => panic!("a fresh proposal decides as proposals, got {other:?}"),
        }
    }

    #[test]
    fn a_replacement_the_engine_cannot_bind_leaves_the_hop_unspent() {
        let e = substituting_engine(TRUSTED);
        let proposal = call("post", json!({ "body": "ssn 123" }));
        let log = internal_log(&e);
        let facts = appended_facts(proposed(&e, &log, "b1", nonce(), proposal.clone()).expect("the batch decides"));
        let hop = opened_offers(&facts)[0].0;
        let log = [log, facts].concat();
        let run = |outcome: OfferOutcome| execute_offer(&e, &log, hop, outcome).map(|_| ());

        for unbindable in [
            r#"{"body":"a"} {"body":"b"}"#,
            r#"["body"]"#,
            r#"{"body":7}"#,
            r#"{"other":"a"}"#,
            "",
        ] {
            assert!(
                matches!(
                    run(substitution(&proposal, unbindable)),
                    Err(TransitionError::Call(EngineError::InvalidCall(_)))
                ),
                "{unbindable:?} was bound as a replacement call"
            );
        }
        assert_eq!(
            run(OfferOutcome::Derived(crate::transition::Evidence::Sanitizer {
                sanitizer: crate::names::SanitizerName::new("officer"),
                source: crate::value::RawResultDigest::of(proposal.canonical_arguments().canonical_bytes()),
                derived: ValueBody::new(REDACTED),
            })),
            Err(TransitionError::EvidenceMismatch)
        );
        assert_eq!(
            run(substitution(&call("post", json!({ "body": "other" })), REDACTED)),
            Err(TransitionError::EvidenceMismatch),
            "a derivation of other bytes is not a derivation of this candidate's arguments"
        );
        assert_eq!(
            run(OfferOutcome::Approved(Vec::new())),
            Err(TransitionError::PlanOutcomeMismatch)
        );
        assert!(execute_offer(&e, &log, hop, substitution(&proposal, REDACTED)).is_ok());
    }

    #[test]
    fn a_proposal_missing_its_annotation_refuses_the_event_and_appends_nothing() {
        let e = substituting_engine(TRUSTED);
        let log = internal_log(&e);
        let unpinned = call("post_dyn", json!({ "body": "ssn 123" }));
        assert_eq!(
            e.handle(
                &viewing(&e, &log),
                batch_on(&traj(), "b1", Vec::new(), vec![raw(&unpinned), raw(&unpinned)], None)
            ),
            Err(TransitionError::AnnotationNeeded {
                annotators: vec![crate::names::AnnotatorName::new("acl")]
            })
        );
        let unpinned_post = call("post", json!({ "body": "ssn 123" }));
        let foreign = unpinned_post.clone().with_annotation(Some(pinned_for(
            post_dyn_annotation(&["partner"]),
            "acl",
            &unpinned_post,
        )));
        assert!(
            matches!(
                e.handle(
                    &viewing(&e, &log),
                    batch_on(&traj(), "b2", Vec::new(), vec![raw(&foreign)], None)
                ),
                Err(TransitionError::ForeignAnnotation { .. })
            ),
            "a static declaration is its own annotation; an annotator's pin on it is foreign"
        );
        let sibling = call("post_dyn", json!({ "body": "other" }));
        let restated = call("post_dyn", json!({ "body": "ssn 123" })).with_annotation(Some(pinned_for(
            post_dyn_annotation(&["partner"]),
            "acl",
            &sibling,
        )));
        assert!(
            matches!(
                e.handle(
                    &viewing(&e, &log),
                    batch_on(&traj(), "b3", Vec::new(), vec![raw(&restated)], None)
                ),
                Err(TransitionError::ForeignAnnotation { .. })
            ),
            "annotation evidence binds the exact call it judged: a sibling call cannot reuse it"
        );
    }

    #[test]
    fn a_hop_goes_stale_with_its_basis_and_a_spent_one_answers_from_the_record() {
        let e = substituting_engine(TRUSTED);
        let proposal = call("post", json!({ "body": "ssn 123" }));
        let log = internal_log(&e);
        let facts = appended_facts(proposed(&e, &log, "b1", nonce(), proposal.clone()).expect("the batch decides"));
        let hop = opened_offers(&facts)[0].0;
        let log = [log, facts].concat();

        let moved = [
            log.clone(),
            appended_facts(proposed(&e, &log, "b2", nonce(), call("ping", json!({}))).expect("the open call releases")),
        ]
        .concat();
        assert_eq!(
            execute_offer(&e, &moved, hop, substitution(&proposal, REDACTED)).map(|_| ()),
            Err(TransitionError::StaleOffer)
        );

        let taken = execute_offer(&e, &log, hop, substitution(&proposal, REDACTED)).expect("the hop runs");
        let answer = offer_answer(&taken).clone();
        let dispatch = match &answer {
            OfferFollowUp::Released(released) => released.dispatch.clone(),
            other => panic!("an immediately admissible substitution dispatches, got {other:?}"),
        };
        let log = [log, appended_facts(taken)].concat();
        let repeat = execute_offer(&e, &log, hop, substitution(&proposal, REDACTED)).expect("the repeat answers");
        assert_eq!(repeat.append, None);
        assert_eq!(offer_answer(&repeat), &answer);
        assert_eq!(
            execute_offer(&e, &log, hop, OfferOutcome::Approved(Vec::new())).map(|_| ()),
            Err(TransitionError::PlanOutcomeMismatch)
        );

        let body = ValueBody::new("posted");
        let reported = e
            .handle(
                &e.view(&traj(), log.clone(), log.len() as u64).unwrap(),
                EngineEvent::Outcome(ToolReport {
                    dispatch,
                    outcome: ToolOutcome::Success {
                        body: OutcomeBody::Available(body.clone()),
                    },
                    evidence: Vec::new(),
                    offer_nonce: nonce(),
                    audience: crate::audience::AudienceEvidence::default(),
                }),
            )
            .expect("the raw result crosses");
        let log = [log, appended_facts(reported)].concat();
        let settled = execute_offer(&e, &log, hop, substitution(&proposal, REDACTED)).expect("the repeat answers");
        assert_eq!(settled.append, None);
        assert!(
            matches!(
                offer_answer(&settled),
                OfferFollowUp::Settled(settled)
                    if settled.outcome == crate::transition::SettledOutcome::Closed { admitted: Some(body.clone()) }
            ),
            "got {:?}",
            offer_answer(&settled)
        );
    }

    #[test]
    fn a_child_return_ends_the_branch_once_and_a_repeat_answers_from_the_record() {
        let internal = Audience::restricted([ReaderId::new("insider")]);
        let e = engine_at(vec![], known(SUSPICIOUS, internal.clone()));
        let child = TrajectoryId::new("child");
        let mut log = vec![opened(&e)];
        log.extend(forked_child(&e, &log.clone(), &child));
        let view = e.view(&traj(), log.clone(), log.len() as u64).unwrap();

        let body = ValueBody::new("the child's answer");
        let report = |submission: ChildSubmission| child_report(&log, &child, submission);
        let merged = e
            .handle(&view, report(ChildSubmission::Value { body: body.clone() }))
            .expect("a non-narrowing crossing merges");
        assert_eq!(
            merged.follow_up,
            FollowUp::Child(ChildFollowUp::Merged { admitted: body.clone() })
        );
        let facts = merged.append.expect("the crossing appends").facts().to_vec();
        let after = e.view(&traj(), [log.clone(), facts].concat(), 9).unwrap();

        let repeat = e
            .handle(&after, report(ChildSubmission::Value { body: body.clone() }))
            .expect("a repeat of the ending submission answers from the record");
        assert_eq!(repeat.append, None);
        assert_eq!(
            repeat.follow_up,
            FollowUp::Child(ChildFollowUp::Merged { admitted: body })
        );
        assert_eq!(
            e.handle(
                &after,
                report(ChildSubmission::Value {
                    body: ValueBody::new("a second answer")
                })
            ),
            Err(crate::transition::TransitionError::BranchEnded)
        );
        assert_eq!(
            e.handle(&after, report(ChildSubmission::Void)),
            Err(crate::transition::TransitionError::BranchEnded)
        );

        assert_eq!(
            e.handle(
                &after,
                EngineEvent::ChildReturn(ChildReport {
                    child: TrajectoryId::new("stranger"),
                    fork: fork_in(&log, &child),
                    submission: ChildSubmission::Void,
                    evidence: Vec::new(),
                    offer_nonce: nonce(),
                    audience: crate::audience::AudienceEvidence::default(),
                })
            ),
            Err(crate::transition::TransitionError::UnopenedTrajectory)
        );
    }

    #[test]
    fn a_marked_spawn_prepares_its_fork_and_the_child_binds_to_it() {
        let e = engine(vec![plain_tool("spawn")]);
        let call = call("spawn", json!({}));
        let records = vec![opened(&e)];
        let view = e.view(&traj(), records.clone(), 1).unwrap();
        let batch = |spawn: Option<crate::transition::SpawnMark>| {
            EngineEvent::Proposals(ProposalBatch {
                id: crate::transition::ProposalBatchId::new("b1"),
                trajectory: traj(),
                provider_results: Vec::new(),
                proposals: vec![raw(&call)],
                spawn,
                offer_nonce: nonce(),
                evidence: Vec::new(),
                audience: crate::audience::AudienceEvidence::default(),
            })
        };

        let decision = e
            .handle(&view, batch(Some(crate::transition::SpawnMark::at(0))))
            .expect("a marked spawn releases and prepares");
        let FollowUp::Proposals { released, .. } = &decision.follow_up else {
            panic!("a proposal batch answers with proposals")
        };
        let fork = released[0].fork.clone().expect("the release carries its fork");
        assert_eq!(fork, crate::value::ForkId::of(&released[0].dispatch));
        let facts = decision.append.clone().expect("the release appends").facts().to_vec();
        assert!(matches!(
            facts.as_slice(),
            [
                Fact::ProposalBatchDecided { .. },
                Fact::DispatchOpened { .. },
                Fact::ForkPrepared { .. }
            ]
        ));
        let log = [records, facts].concat();
        let prepared = e.view(&traj(), log.clone(), 2).unwrap();
        assert_eq!(
            e.handle(&prepared, batch(None)),
            Err(crate::transition::TransitionError::BatchIdentityConflict)
        );
        let child = TrajectoryId::new("child");
        let bind = |fork: crate::value::ForkId, child: TrajectoryId| {
            EngineEvent::BindFork(crate::transition::ForkBinding { fork, child })
        };
        let bound = e
            .handle(&prepared, bind(fork.clone(), child.clone()))
            .expect("the child binds");
        assert_eq!(bound.follow_up, FollowUp::Fork { child: child.clone() });
        let opened = bound.append.expect("the binding appends").facts().to_vec();
        assert!(matches!(opened.as_slice(), [Fact::ForkOpened { .. }]));

        let after = e.view(&traj(), [log.clone(), opened].concat(), 3).unwrap();
        let child_views = after.views(&child).expect("the bound child is opened");
        assert_eq!(child_views.current_label(), partial(TRUSTED, Audience::public()));
        assert_eq!(child_views.parent_of(&child), Some(&traj()));

        let repeat = e.handle(&after, bind(fork.clone(), child.clone())).unwrap();
        assert_eq!(repeat.append, None);
        assert_eq!(
            e.handle(&after, bind(fork.clone(), TrajectoryId::new("other"))),
            Err(crate::transition::TransitionError::UnbindableFork)
        );
        let unprepared = crate::value::ForkId::of(&DispatchId::new(traj(), call.digest(), 9));
        assert_eq!(
            e.handle(&after, bind(unprepared, TrajectoryId::new("other"))),
            Err(crate::transition::TransitionError::UnbindableFork)
        );
        assert_eq!(
            e.handle(&prepared, bind(fork.clone(), traj())),
            Err(crate::transition::TransitionError::ChildAlreadyUsed)
        );

        let ran = [
            log.clone(),
            vec![Fact::DispatchClosed {
                trajectory: traj(),
                dispatch: fork.dispatch().clone(),
                outcome: crate::fact::CloseOutcome::Success {
                    effects: crate::fact::EffectSet::default(),
                },
            }],
        ]
        .concat();
        let after_run = e.view(&traj(), ran, 3).unwrap();
        let repeat = e
            .handle(&after_run, batch(Some(crate::transition::SpawnMark::at(0))))
            .expect("the repeat answers from the record");
        assert_eq!(repeat.append, None);
        match repeat.follow_up {
            FollowUp::Proposals { released, .. } => {
                assert!(released.is_empty(), "an invoked call is not re-released");
                assert_eq!(
                    e.fork_status(&after_run, &fork),
                    ForkStatus::Prepared,
                    "its fork still awaits a child"
                );
            }
            other => panic!("expected a proposal answer, got {other:?}"),
        }

        let failed = [
            log,
            vec![Fact::DispatchClosed {
                trajectory: traj(),
                dispatch: fork.dispatch().clone(),
                outcome: crate::fact::CloseOutcome::Failure,
            }],
        ]
        .concat();
        let after_failure = e.view(&traj(), failed, 3).unwrap();
        assert_eq!(
            e.handle(&after_failure, bind(fork, child)),
            Err(crate::transition::TransitionError::UnbindableFork)
        );
    }

    #[test]
    fn fork_of_answers_the_same_advanced_or_rebuilt_and_a_refused_bind_leaves_it() {
        let e = engine(vec![plain_tool("spawn")]);
        let call = call("spawn", json!({}));
        let records = vec![opened(&e)];
        let mut held = e.view(&traj(), records.clone(), 1).unwrap();
        let child = TrajectoryId::new("child");

        let prepared = e
            .handle(
                &held,
                EngineEvent::Proposals(ProposalBatch {
                    id: crate::transition::ProposalBatchId::new("b1"),
                    trajectory: traj(),
                    provider_results: Vec::new(),
                    proposals: vec![raw(&call)],
                    spawn: Some(crate::transition::SpawnMark::at(0)),
                    offer_nonce: nonce(),
                    evidence: Vec::new(),
                    audience: crate::audience::AudienceEvidence::default(),
                }),
            )
            .expect("a marked spawn releases and prepares");
        let FollowUp::Proposals { released, .. } = &prepared.follow_up else {
            panic!("a proposal batch answers with proposals")
        };
        let fork = released[0].fork.clone().expect("the release carries its fork");
        assert_eq!(e.fork_of(&held, &child), None, "no binding yet, no fork for the child");
        let prepared_batch = prepared.append.clone().expect("the release appends");
        held.advance(&prepared_batch).unwrap();

        let bound = e
            .handle(
                &held,
                EngineEvent::BindFork(crate::transition::ForkBinding {
                    fork: fork.clone(),
                    child: child.clone(),
                }),
            )
            .expect("the child binds");
        let bound_batch = bound.append.clone().expect("the binding appends");
        held.advance(&bound_batch).unwrap();
        assert_eq!(e.fork_of(&held, &child), Some(fork.clone()));

        let whole = [records, prepared_batch.facts().to_vec(), bound_batch.facts().to_vec()].concat();
        let cold = e.view(&traj(), whole.clone(), whole.len() as u64).unwrap();
        assert_eq!(e.fork_of(&cold, &child), Some(fork.clone()));

        assert_eq!(
            e.handle(
                &held,
                EngineEvent::BindFork(crate::transition::ForkBinding {
                    fork: fork.clone(),
                    child: TrajectoryId::new("other"),
                }),
            ),
            Err(crate::transition::TransitionError::UnbindableFork),
        );
        assert_eq!(e.fork_of(&held, &child), Some(fork));
        assert_eq!(e.fork_of(&held, &TrajectoryId::new("other")), None);
    }

    #[test]
    fn a_fork_preparation_replays_only_as_its_marked_release() {
        let e = engine(vec![plain_tool("spawn")]);
        let call = call("spawn", json!({}));
        let records = vec![opened(&e)];
        let view = e.view(&traj(), records.clone(), 1).unwrap();
        let marked = e
            .handle(
                &view,
                EngineEvent::Proposals(ProposalBatch {
                    id: crate::transition::ProposalBatchId::new("b1"),
                    trajectory: traj(),
                    provider_results: Vec::new(),
                    proposals: vec![raw(&call)],
                    spawn: Some(crate::transition::SpawnMark::at(0)),
                    offer_nonce: nonce(),
                    evidence: Vec::new(),
                    audience: crate::audience::AudienceEvidence::default(),
                }),
            )
            .unwrap();
        let batch = marked.append.expect("the release appends").facts().to_vec();
        assert_eq!(e.validate_replay(&[records.clone(), batch.clone()].concat()), Ok(()));

        let mut unmarked = batch.clone();
        if let Fact::ProposalBatchDecided { spawn, .. } = &mut unmarked[0] {
            *spawn = None;
        }
        assert_eq!(
            e.validate_replay(&[records.clone(), unmarked].concat()),
            Err(TransitionRefusal::UnbackedDecision)
        );
        let displaced = [
            records.clone(),
            vec![
                batch[0].clone(),
                batch[1].clone(),
                stray_admission(&traj(), known(SUSPICIOUS, Audience::public())),
                batch[2].clone(),
            ],
        ]
        .concat();
        assert_eq!(e.validate_replay(&displaced), Err(TransitionRefusal::UnbackedDecision));
    }

    #[test]
    fn a_spawn_mark_takes_declared_context_control() {
        let config = RegistryConfig {
            annotators: vec![],
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: declared(vec![plain_tool("spawn")]),
            authorities: vec![],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
        };
        let mut declaration = crate::profile::covering_declaration(&config);
        declaration.context_control = false;
        let e = Engine::open(DeploymentPolicy {
            registry: config,
            planner_cap: crate::registry::PlannerCap::default(),
            dialect: PolicyDialectVersion::new(1),
            child_return: ReturnPolicy::Raw,
            profile: declaration,
        })
        .expect("an uncontrolled deployment opens");
        let view = e.view(&traj(), vec![opened(&e)], 1).unwrap();
        assert_eq!(
            e.handle(
                &view,
                EngineEvent::Proposals(ProposalBatch {
                    id: crate::transition::ProposalBatchId::new("b1"),
                    trajectory: traj(),
                    provider_results: Vec::new(),
                    proposals: vec![raw(&call("spawn", json!({})))],
                    spawn: Some(crate::transition::SpawnMark::at(0)),
                    offer_nonce: nonce(),
                    evidence: Vec::new(),
                    audience: crate::audience::AudienceEvidence::default(),
                })
            ),
            Err(crate::transition::TransitionError::SpawnUncontrolled)
        );
    }

    fn spawn_family(e: &Engine, schema: Option<&serde_json::Value>, child: &TrajectoryId) -> Vec<Fact> {
        let args = match schema {
            Some(schema) => json!({ "return_schema": schema }),
            None => json!({}),
        };
        let call = call("spawn", args);
        let records = vec![opened(e)];
        let view = e.view(&traj(), records.clone(), 1).unwrap();
        let decision = e
            .handle(
                &view,
                EngineEvent::Proposals(ProposalBatch {
                    id: crate::transition::ProposalBatchId::new("b1"),
                    trajectory: traj(),
                    provider_results: Vec::new(),
                    proposals: vec![raw(&call)],
                    spawn: Some(crate::transition::SpawnMark::at(0)),
                    offer_nonce: nonce(),
                    evidence: Vec::new(),
                    audience: crate::audience::AudienceEvidence::default(),
                }),
            )
            .expect("the marked spawn releases and prepares");
        let FollowUp::Proposals { released, .. } = &decision.follow_up else {
            panic!("a proposal batch answers with proposals")
        };
        let fork = released[0].fork.clone().expect("the release carries its fork");
        let log = [records, decision.append.expect("the release appends").facts().to_vec()].concat();
        let prepared = e.view(&traj(), log.clone(), log.len() as u64).unwrap();
        let bound = e
            .handle(
                &prepared,
                EngineEvent::BindFork(crate::transition::ForkBinding {
                    fork,
                    child: child.clone(),
                }),
            )
            .expect("the child binds");
        [log, bound.append.expect("the binding appends").facts().to_vec()].concat()
    }

    #[test]
    fn a_shaped_fork_persists_its_shape_and_gates_the_crossing() {
        let e = engine(vec![plain_tool("spawn")]);
        let schema = json!({
            "type": "object",
            "properties": {
                "verdict": { "type": "string", "enum": ["allow", "deny"] },
                "confidence": { "type": "integer", "minimum": 0, "maximum": 100 },
            },
            "required": ["verdict", "confidence"],
        });
        let child = TrajectoryId::new("child");
        let log = spawn_family(&e, Some(&schema), &child);
        let persisted = log
            .iter()
            .find_map(|fact| match fact {
                Fact::ForkPrepared { shape, .. } => Some(shape.clone()),
                _ => None,
            })
            .expect("the preparation records");
        assert_eq!(
            persisted,
            Some(crate::shape::ReturnShape::compile(&schema).unwrap()),
            "the preparation persists the compiled shape itself"
        );

        let view = e.view(&traj(), log.clone(), log.len() as u64).unwrap();
        let report = |text: &str| {
            child_report(
                &log,
                &child,
                ChildSubmission::Value {
                    body: ValueBody::new(text),
                },
            )
        };
        assert!(matches!(
            e.handle(&view, report("free text")),
            Err(crate::transition::TransitionError::ReturnShapeMismatch(_))
        ));
        assert!(matches!(
            e.handle(&view, report(r#"{"verdict":"allow","confidence":101}"#)),
            Err(crate::transition::TransitionError::ReturnShapeMismatch(_))
        ));

        let canonical = ValueBody::new(r#"{"confidence":97,"verdict":"allow"}"#);
        let merged = e
            .handle(&view, report("{ \"verdict\": \"allow\",  \"confidence\": 97 }"))
            .expect("a conforming submission crosses");
        assert_eq!(
            merged.follow_up,
            FollowUp::Child(ChildFollowUp::Merged {
                admitted: canonical.clone()
            })
        );
        let ended = [
            log.clone(),
            merged.append.expect("the crossing appends").facts().to_vec(),
        ]
        .concat();
        let after = e.view(&traj(), ended.clone(), ended.len() as u64).unwrap();

        let repeat = e
            .handle(&after, report(r#"{"verdict": "allow", "confidence": 97}"#))
            .expect("a repeat of the ending submission answers from the record");
        assert_eq!(repeat.append, None);
        assert_eq!(
            repeat.follow_up,
            FollowUp::Child(ChildFollowUp::Merged { admitted: canonical })
        );
        assert_eq!(
            e.handle(&after, report(r#"{"verdict":"deny","confidence":3}"#)),
            Err(crate::transition::TransitionError::BranchEnded)
        );
        assert_eq!(
            e.handle(&after, report("free text")),
            Err(crate::transition::TransitionError::BranchEnded)
        );
    }

    #[test]
    fn a_void_return_bypasses_the_shape_gate() {
        let e = engine(vec![plain_tool("spawn")]);
        let schema = json!({
            "type": "object",
            "properties": { "flag": { "type": "boolean" } },
            "required": ["flag"],
        });
        let child = TrajectoryId::new("child");
        let log = spawn_family(&e, Some(&schema), &child);
        let view = e.view(&traj(), log.clone(), log.len() as u64).unwrap();
        let ended = e
            .handle(&view, child_report(&log, &child, ChildSubmission::Void))
            .expect("a void return ends the shaped branch");
        assert_eq!(ended.follow_up, FollowUp::Child(ChildFollowUp::Ended));
        assert!(matches!(
            ended.append.expect("the void appends").facts(),
            [Fact::Boundary {
                kind: crate::fact::BoundaryKind::VoidReturn,
                ..
            }]
        ));
    }

    #[test]
    fn an_uncompilable_return_schema_refuses_the_marked_batch() {
        let e = engine(vec![plain_tool("spawn")]);
        let free = json!({
            "type": "object",
            "properties": { "note": { "type": "string" } },
            "required": ["note"],
        });
        let call = call("spawn", json!({ "return_schema": free }));
        let view = e.view(&traj(), vec![opened(&e)], 1).unwrap();
        let batch = |id: &str, spawn: Option<crate::transition::SpawnMark>| {
            EngineEvent::Proposals(ProposalBatch {
                id: crate::transition::ProposalBatchId::new(id),
                trajectory: traj(),
                provider_results: Vec::new(),
                proposals: vec![raw(&call)],
                spawn,
                offer_nonce: nonce(),
                evidence: Vec::new(),
                audience: crate::audience::AudienceEvidence::default(),
            })
        };
        let refused = e
            .handle(&view, batch("b1", Some(crate::transition::SpawnMark::at(0))))
            .expect("a malformed batch still answers");
        assert!(matches!(
            refused.follow_up,
            FollowUp::Malformed {
                position: 0,
                error: EngineError::InvalidReturnSchema(_)
            }
        ));
        let sealed = refused.append.map(|batch| batch.facts().to_vec()).unwrap_or_default();
        assert!(
            sealed
                .iter()
                .all(|fact| !matches!(fact, Fact::DispatchOpened { .. } | Fact::ForkPrepared { .. })),
            "nothing releases and no fork is prepared"
        );
        let released = e.handle(&view, batch("b2", None)).expect("an unmarked call releases");
        assert!(matches!(released.follow_up, FollowUp::Proposals { .. }));
    }

    #[test]
    fn replay_holds_a_fork_to_the_shape_its_spawn_authored() {
        let e = engine(vec![plain_tool("spawn")]);
        let schema = json!({
            "type": "object",
            "properties": { "flag": { "type": "boolean" } },
            "required": ["flag"],
        });
        let child = TrajectoryId::new("child");
        let log = spawn_family(&e, Some(&schema), &child);
        assert_eq!(e.validate_replay(&log), Ok(()));

        let prepared_at = log
            .iter()
            .position(|fact| matches!(fact, Fact::ForkPrepared { .. }))
            .expect("the preparation records");
        let reshape = |base: &[Fact], shape: Option<crate::shape::ReturnShape>| {
            let mut forged = base.to_vec();
            let Fact::ForkPrepared { shape: stored, .. } = &mut forged[prepared_at] else {
                unreachable!("the position was just found")
            };
            *stored = shape;
            e.validate_replay(&forged)
        };
        assert_eq!(reshape(&log, None), Err(TransitionRefusal::ForkShapeMismatch));
        let other = crate::shape::ReturnShape::compile(&json!({
            "type": "object",
            "properties": { "count": { "type": "integer", "minimum": 0, "maximum": 10 } },
            "required": ["count"],
        }))
        .unwrap();
        assert_eq!(
            reshape(&log, Some(other.clone())),
            Err(TransitionRefusal::ForkShapeMismatch)
        );
        let plain = spawn_family(&e, None, &child);
        assert_eq!(e.validate_replay(&plain), Ok(()));
        assert_eq!(reshape(&plain, Some(other)), Err(TransitionRefusal::ForkShapeMismatch));

        let view = e.view(&traj(), log.clone(), log.len() as u64).unwrap();
        let merged = e
            .handle(
                &view,
                child_report(
                    &log,
                    &child,
                    ChildSubmission::Value {
                        body: ValueBody::new(r#"{"flag":true}"#),
                    },
                ),
            )
            .expect("a conforming submission crosses");
        let ended = [log, merged.append.expect("the crossing appends").facts().to_vec()].concat();
        assert_eq!(e.validate_replay(&ended), Ok(()));
        let crossing_at = ended
            .iter()
            .position(|fact| matches!(fact, Fact::ChildReturn { .. }))
            .expect("the return records its crossing");
        let rebody = |text: &str| {
            let mut forged = ended.clone();
            let Fact::ChildReturn { value, .. } = &mut forged[crossing_at] else {
                unreachable!("the position was just found")
            };
            *value = LabeledValue::new(ValueBody::new(text), value.label.clone());
            e.validate_replay(&forged)
        };
        assert_eq!(rebody("free text"), Err(TransitionRefusal::ReturnShapeViolation));
        assert_eq!(
            rebody(r#"{ "flag": true }"#),
            Err(TransitionRefusal::ReturnShapeViolation)
        );
    }

    fn neutral_tool() -> ToolAnnotation {
        ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("read_note"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires::default(),
        }
    }

    fn emitting_tool() -> ToolAnnotation {
        ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("send_note"),
            emits: EffectSet::new([EffectKind::new("email.sent")]).unwrap(),
            ..neutral_tool()
        }
    }

    fn quiet_subject() -> crate::basis::SubjectKey {
        crate::basis::SubjectKey::Call {
            trajectory: traj(),
            batch: crate::transition::ProposalBatchId::new("never-decided"),
            position: 0,
        }
    }

    fn basis_of(e: &Engine, log: &[Fact]) -> crate::basis::PolicyBasis {
        e.view(&traj(), log.to_vec(), log.len() as u64)
            .expect("the log replays")
            .projection()
            .view(&traj())
            .basis_for(&quiet_subject())
    }

    fn decide(e: &Engine, log: &[Fact], id: &str, call: &ResolvedCall) -> EngineDecision {
        let view = e
            .view(&traj(), log.to_vec(), log.len() as u64)
            .expect("the log replays");
        e.handle(
            &view,
            EngineEvent::Proposals(ProposalBatch {
                id: crate::transition::ProposalBatchId::new(id),
                trajectory: traj(),
                provider_results: Vec::new(),
                proposals: vec![raw(call)],
                spawn: None,
                offer_nonce: nonce(),
                evidence: Vec::new(),
                audience: crate::audience::AudienceEvidence::default(),
            }),
        )
        .expect("the batch decides")
    }

    fn appended_facts(decision: EngineDecision) -> Vec<Fact> {
        decision.append.expect("the decision appends").facts().to_vec()
    }

    #[test]
    fn a_neutral_release_advances_no_basis_component() {
        let e = engine(vec![neutral_tool()]);
        let log = vec![opened(&e)];
        let before = basis_of(&e, &log);
        let facts = appended_facts(decide(&e, &log, "b1", &call("read_note", json!({}))));
        assert!(!facts.iter().any(|fact| matches!(fact, Fact::BasisAdvanced { .. })));
        assert_eq!(basis_of(&e, &[log, facts].concat()), before);
    }

    #[test]
    fn a_release_advances_the_components_its_contract_can_move() {
        let effects = engine(vec![emitting_tool()]);
        let log = vec![opened(&effects)];
        let before = basis_of(&effects, &log);
        let facts = appended_facts(decide(&effects, &log, "b1", &call("send_note", json!({}))));
        let after = basis_of(&effects, &[log.clone(), facts].concat());
        assert_eq!(after.family, before.family.next());
        assert_eq!(after.flow, before.flow, "a `delta = {{}}` result restricts nothing");

        let restricting = engine_at(
            vec![crm_tool()],
            known(TRUSTED, Audience::restricted([ReaderId::new("insider")])),
        );
        let internal = vec![opened(&restricting)];
        let before = basis_of(&restricting, &internal);
        let facts = appended_facts(decide(&restricting, &internal, "b1", &call("get_ticket", json!({}))));
        let after = basis_of(&restricting, &[internal, facts].concat());
        assert_eq!(after.flow, before.flow.next());
        assert_eq!(after.family, before.family, "it reserves no effect");
    }

    #[test]
    fn a_blocked_proposal_leaves_every_basis_component_where_it_was() {
        let e = engine(vec![crm_tool()]);
        let log = vec![opened(&e)];
        let before = basis_of(&e, &log);
        let facts = appended_facts(decide(&e, &log, "b1", &call("get_ticket", json!({}))));
        assert!(
            facts
                .iter()
                .any(|fact| matches!(fact, Fact::BasisAdvanced { advance, .. } if advance.is_empty()))
        );
        assert_eq!(basis_of(&e, &[log, facts].concat()), before);
    }

    #[test]
    fn a_declaration_that_disagrees_with_its_records_is_refused() {
        let e = engine(vec![emitting_tool()]);
        let opening = vec![opened(&e)];
        let batch = appended_facts(decide(&e, &opening, "b1", &call("send_note", json!({}))));
        let log = [opening.clone(), batch.clone()].concat();
        assert_eq!(e.validate_replay(&log), Ok(()));

        let rewritten = |mutate: &dyn Fn(&mut Fact)| {
            let mut facts = batch.clone();
            mutate(&mut facts[0]);
            e.validate_replay(&[opening.clone(), facts].concat())
        };
        assert_eq!(
            rewritten(&|fact| {
                if let Fact::BasisAdvanced { advance, .. } = fact {
                    advance.flows.insert(traj());
                }
            }),
            Err(TransitionRefusal::UnbackedAdvance)
        );
        assert_eq!(
            rewritten(&|fact| {
                if let Fact::BasisAdvanced { advance, .. } = fact {
                    advance.family = false;
                }
            }),
            Err(TransitionRefusal::UndeclaredAdvance)
        );
        assert_eq!(
            rewritten(&|fact| {
                if let Fact::BasisAdvanced { act, .. } = fact {
                    *act = crate::basis::DecidedAct::Proposals(crate::transition::ProposalBatchId::new("other"));
                }
            }),
            Err(TransitionRefusal::UnbackedAdvance)
        );
    }

    fn blocked_batch(e: &Engine, log: &[Fact], id: &str, nonce: crate::value::OfferNonce) -> EngineDecision {
        proposed(e, log, id, nonce, call("get_ticket", json!({}))).expect("the batch decides")
    }

    fn proposed(
        e: &Engine,
        log: &[Fact],
        id: &str,
        nonce: crate::value::OfferNonce,
        proposal: ResolvedCall,
    ) -> Result<EngineDecision, TransitionError> {
        let view = e
            .view(&traj(), log.to_vec(), log.len() as u64)
            .expect("the log replays");
        e.handle(
            &view,
            EngineEvent::Proposals(ProposalBatch {
                id: crate::transition::ProposalBatchId::new(id),
                trajectory: traj(),
                provider_results: Vec::new(),
                proposals: vec![raw(&proposal)],
                spawn: None,
                offer_nonce: nonce,
                evidence: Vec::new(),
                audience: crate::audience::AudienceEvidence::default(),
            }),
        )
    }

    fn released_under_output_sanitizer(e: &Engine, log: Vec<Fact>, call: &ResolvedCall) -> (Vec<Fact>, DispatchId) {
        let blocked = appended_facts(proposed(e, &log, "san-block", nonce(), call.clone()).expect("the call blocks"));
        let (offer, _) = opened_offers(&blocked)
            .into_iter()
            .find(|(_, plan)| plan.sanitizer().is_some())
            .expect("a confined result point offers the sanitize settlement");
        let log = [log, blocked].concat();

        let approved = appended_facts(
            execute_offer(e, &log, offer, OfferOutcome::Approved(Vec::new())).expect("the offer executes"),
        );
        let log = [log, approved].concat();

        let release =
            appended_facts(proposed(e, &log, "san-release", nonce(), call.clone()).expect("the approval releases"));
        let dispatch = release
            .iter()
            .find_map(|fact| match fact {
                Fact::DispatchOpened { dispatch, .. } => Some(dispatch.clone()),
                _ => None,
            })
            .expect("the release opens the dispatch");
        ([log, release].concat(), dispatch)
    }

    fn two_officer_engine() -> Engine {
        use crate::authority::{Authority, Mandate};
        let officer = |name: &str| Authority {
            name: AuthorityName::new(name),
            mandate: Mandate {
                trust_ceiling: Some(TRUSTED),
                ..Mandate::default()
            },
            scope: crate::authority::Scope::default(),
            hint: None,
        };
        let cfg = RegistryConfig {
            annotators: vec![],
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: declared(vec![ToolAnnotation {
                description: Some("A test tool.".to_string()),
                name: ToolName::new("wire"),
                tags: vec![],
                delta: Delta::NONE,
                parameters: crate::params::ToolParameters::open(),
                emits: EffectSet::default(),
                requires: Requires {
                    label: LabelRequirements {
                        trust_floor: Some(TRUSTED),
                        audience: vec![],
                    },
                    ..Requires::default()
                },
            }]),
            authorities: vec![officer("officer-a"), officer("officer-b")],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
        };
        open_engine_at(cfg, known(SUSPICIOUS, Audience::public()))
    }

    fn opened_offers(facts: &[Fact]) -> Vec<(crate::value::OfferId, plan::ExecutableRemedyPlan)> {
        facts
            .iter()
            .filter_map(|fact| match fact {
                Fact::OfferOpened { offer, plan, .. } => Some((*offer, plan.clone())),
                _ => None,
            })
            .collect()
    }

    fn evidence_for(
        offer: crate::value::OfferId,
        plan: &plan::ExecutableRemedyPlan,
        tool: &str,
        fold: Label,
    ) -> Vec<crate::execute::AuthorityEvidence> {
        plan.required
            .iter()
            .map(|required| crate::execute::AuthorityEvidence {
                offer,
                authority: required.authority.clone(),
                covers: required.covers.clone(),
                reviewed: crate::execute::AuthorityReview {
                    tool: ToolName::new(tool),
                    trajectory_label: fold.clone(),
                },
            })
            .collect()
    }

    fn execute_offer(
        e: &Engine,
        log: &[Fact],
        offer: crate::value::OfferId,
        outcome: OfferOutcome,
    ) -> Result<EngineDecision, TransitionError> {
        execute_offer_with(e, log, offer, outcome, crate::audience::AudienceEvidence::default())
    }

    fn execute_offer_with(
        e: &Engine,
        log: &[Fact],
        offer: crate::value::OfferId,
        outcome: OfferOutcome,
        audience: crate::audience::AudienceEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        let view = e
            .view(&traj(), log.to_vec(), log.len() as u64)
            .expect("the log replays");
        e.handle(
            &view,
            EngineEvent::ExecuteOffer(OfferExecution {
                trajectory: traj(),
                offer,
                outcome,
                offer_nonce: crate::value::OfferNonce::new([11u8; 32]),
                audience,
            }),
        )
    }

    fn offer_answer(decision: &EngineDecision) -> &OfferFollowUp {
        match &decision.follow_up {
            FollowUp::Offer(answer) => answer,
            other => panic!("an offer execution answers with an offer follow-up, not {other:?}"),
        }
    }

    /// A tool that releases freely at the trajectory's own trust yet declares a delta, so its
    /// result can restrict the trajectory and its release moves the flow basis.
    fn restrictable_tool(name: &str) -> ToolAnnotation {
        ToolAnnotation {
            delta: Delta {
                trust: Some(TRUSTED),
                audience: None,
            },
            ..open_tool(name)
        }
    }

    fn open_tool(name: &str) -> ToolAnnotation {
        ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new(name),
            tags: vec![],
            delta: crate::contract::Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires::default(),
        }
    }

    #[test]
    fn one_call_carries_one_current_approval() {
        let e = engine(vec![crm_tool()]);
        let log = vec![opened(&e)];
        let first = appended_facts(blocked_batch(&e, &log, "b1", nonce()));
        let one = opened_offers(&first)[0].0;
        let log = [log, first].concat();
        let second = appended_facts(blocked_batch(&e, &log, "b2", crate::value::OfferNonce::new([3u8; 32])));
        let other = opened_offers(&second)[0].0;
        let log = [log, second].concat();
        assert_ne!(one, other, "each block surfaces its own offer for the same call");

        let approved = appended_facts(
            execute_offer(&e, &log, one, OfferOutcome::Approved(Vec::new())).expect("the first offer executes"),
        );
        let log = [log, approved].concat();
        assert_eq!(
            execute_offer(&e, &log, other, OfferOutcome::Approved(Vec::new())),
            Err(TransitionError::ApprovalPending)
        );
    }

    #[test]
    fn a_denial_inside_an_offer_execution_is_held_to_that_offer() {
        let e = two_officer_engine();
        let opening = vec![opened(&e)];
        let decision = proposed(&e, &opening, "b1", nonce(), call("wire", json!({}))).expect("the batch decides");
        let opened = appended_facts(decision);
        let offers = opened_offers(&opened);
        let authority = offers[0].1.required[0].authority.clone();
        let opening = [opening, opened].concat();
        let denial = appended_facts(
            execute_offer(&e, &opening, offers[0].0, OfferOutcome::Denied { authority }).expect("the denial records"),
        );
        assert_eq!(e.validate_replay(&[opening.clone(), denial.clone()].concat()), Ok(()));

        let position = denial
            .iter()
            .position(|fact| matches!(fact, Fact::Denial { .. }))
            .expect("the execution recorded one");
        let mut elsewhere = denial;
        if let Fact::Denial { digest, .. } = &mut elsewhere[position] {
            *digest = call("wire", json!({ "to": "someone" })).digest();
        }
        assert_eq!(
            e.validate_replay(&[opening, elsewhere].concat()),
            Err(TransitionRefusal::UnbackedDenial)
        );
    }

    #[test]
    fn a_release_that_declares_no_advance_cannot_spend_its_approval() {
        let e = engine(vec![crm_tool()]);
        let log = vec![opened(&e)];
        let opened = appended_facts(blocked_batch(&e, &log, "b1", nonce()));
        let offer = opened_offers(&opened)[0].0;
        let log = [log, opened].concat();
        let approved = appended_facts(
            execute_offer(&e, &log, offer, OfferOutcome::Approved(Vec::new())).expect("the offer executes"),
        );
        let opening = [log, approved].concat();
        let release =
            appended_facts(proposed(&e, &opening, "b2", nonce(), call("get_ticket", json!({}))).expect("it releases"));

        let undeclared: Vec<Fact> = release
            .into_iter()
            .filter(|fact| !matches!(fact, Fact::BasisAdvanced { .. }))
            .collect();
        assert_eq!(
            e.validate_replay(&[opening, undeclared].concat()),
            Err(TransitionRefusal::MisdecidedBatch)
        );
    }

    #[test]
    fn an_output_sanitizer_plan_binds_its_sanitizer_when_the_approval_releases() {
        let declassify = crate::authority::Sanitizer {
            name: crate::names::SanitizerName::new("declassify"),
            on: crate::authority::SanitizerPoints {
                input: false,
                output: true,
            },
            transition: crate::authority::DeclaredTransition::Audience {
                from_includes: DeclaredAudience::restricted([ReaderId::new("insider")]),
                to: DeclaredAudience::literal(Audience::public()),
            },
            scope: crate::authority::Scope::default(),
            hint: None,
        };
        let e = open_engine(RegistryConfig {
            annotators: vec![],
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: declared(vec![crm_tool()]),
            authorities: vec![],
            sanitizers: vec![declassify],
            audience: crate::audience::AudienceConfig::default(),
        });
        let log = vec![opened(&e)];
        let opened = appended_facts(blocked_batch(&e, &log, "b1", nonce()));
        let (offer, plan) = opened_offers(&opened)
            .into_iter()
            .find(|(_, plan)| plan.sanitizer().is_some())
            .expect("a confined result point offers the sanitize settlement");
        let log = [log, opened].concat();

        let approved = appended_facts(
            execute_offer(&e, &log, offer, OfferOutcome::Approved(Vec::new())).expect("the offer executes"),
        );
        match approved.iter().find(|fact| matches!(fact, Fact::CallApproved { .. })) {
            Some(Fact::CallApproved { sanitizer, .. }) => assert_eq!(sanitizer.as_ref(), plan.sanitizer()),
            other => panic!("the approval carries its binding, not {other:?}"),
        }
        let log = [log, approved].concat();

        let release = appended_facts(
            proposed(&e, &log, "b2", nonce(), call("get_ticket", json!({}))).expect("the approval releases"),
        );
        let bound = release
            .iter()
            .position(|fact| matches!(fact, Fact::OutputSanitizerBound { .. }))
            .expect("the release binds the sanitizer");
        let opening = release
            .iter()
            .position(|fact| matches!(fact, Fact::DispatchOpened { .. }))
            .expect("and then opens");
        assert!(
            bound < opening,
            "the binding lands before the dispatch it withholds for"
        );
        assert_eq!(e.validate_replay(&[log, release].concat()), Ok(()));
    }

    #[test]
    fn a_sibling_offer_is_held_to_the_same_menu_as_the_offer_that_derived_it() {
        let e = two_officer_engine();
        let opening = vec![opened(&e)];
        let batch =
            appended_facts(proposed(&e, &opening, "b1", nonce(), call("wire", json!({}))).expect("the batch decides"));
        assert_eq!(e.validate_replay(&[opening.clone(), batch.clone()].concat()), Ok(()));

        let sibling = batch
            .iter()
            .enumerate()
            .filter(|(_, fact)| matches!(fact, Fact::OfferOpened { .. }))
            .map(|(at, _)| at)
            .nth(1)
            .expect("the block opened two");
        let rewritten = |mutate: &dyn Fn(&mut Fact)| {
            let mut facts = batch.clone();
            mutate(&mut facts[sibling]);
            e.validate_replay(&[opening.clone(), facts].concat())
        };
        assert_eq!(
            rewritten(&|fact| {
                if let Fact::OfferOpened { plan, .. } = fact {
                    plan.required.clear();
                }
            }),
            Err(TransitionRefusal::UnbackedOffer)
        );
        assert_eq!(
            rewritten(&|fact| {
                if let Fact::OfferOpened { call: rendered, .. } = fact {
                    *rendered = call("wire", json!({ "to": "elsewhere" })).digest();
                }
            }),
            Err(TransitionRefusal::UnbackedOffer)
        );
        assert_eq!(
            rewritten(&|fact| {
                if let Fact::OfferOpened { basis, .. } = fact {
                    basis.family = basis.family.next();
                }
            }),
            Err(TransitionRefusal::ForgedBasis)
        );
    }

    #[test]
    fn a_surfacing_offers_its_whole_menu_exactly_once_under_one_block() {
        let e = two_officer_engine();
        let opening = vec![opened(&e)];
        let batch =
            appended_facts(proposed(&e, &opening, "b1", nonce(), call("wire", json!({}))).expect("the batch decides"));
        let offers: Vec<usize> = batch
            .iter()
            .enumerate()
            .filter(|(_, fact)| matches!(fact, Fact::OfferOpened { .. }))
            .map(|(at, _)| at)
            .collect();
        assert_eq!(offers.len(), 2, "two officers, two plans, two offers");
        assert_eq!(e.validate_replay(&[opening.clone(), batch.clone()].concat()), Ok(()));

        for at in &offers {
            let mut facts = batch.clone();
            facts.remove(*at);
            assert_eq!(
                e.validate_replay(&[opening.clone(), facts].concat()),
                Err(TransitionRefusal::IncompleteMenu)
            );
        }
        let mut doubled = batch.clone();
        let mut again = batch[offers[1]].clone();
        if let Fact::OfferOpened { offer, .. } = &mut again {
            *offer = crate::value::OfferId::of_plan(
                &crate::value::BlockId::of_proposal(
                    &nonce(),
                    &traj(),
                    &crate::transition::ProposalBatchId::new("b1"),
                    0,
                    &call("wire", json!({})).digest(),
                ),
                7,
                b"again",
            );
        }
        doubled.push(again);
        assert_eq!(
            e.validate_replay(&[opening.clone(), doubled].concat()),
            Err(TransitionRefusal::PlanReoffered)
        );
        let mut split = batch.clone();
        if let Fact::OfferOpened { block, .. } = &mut split[offers[1]] {
            *block = crate::value::BlockId::of_proposal(
                &crate::value::OfferNonce::new([9u8; 32]),
                &traj(),
                &crate::transition::ProposalBatchId::new("b1"),
                0,
                &call("wire", json!({})).digest(),
            );
        }
        assert_eq!(
            e.validate_replay(&[opening.clone(), split].concat()),
            Err(TransitionRefusal::SplitBlock)
        );
        let log = [opening.clone(), batch.clone()].concat();
        let first_block = match &batch[offers[0]] {
            Fact::OfferOpened { block, .. } => *block,
            _ => unreachable!(),
        };
        let mut second = appended_facts(
            proposed(
                &e,
                &log,
                "b2",
                crate::value::OfferNonce::new([5u8; 32]),
                call("wire", json!({ "to": "bob" })),
            )
            .expect("the batch decides"),
        );
        assert_eq!(e.validate_replay(&[log.clone(), second.clone()].concat()), Ok(()));
        for fact in &mut second {
            if let Fact::OfferOpened { block, .. } = fact {
                *block = first_block;
            }
        }
        assert_eq!(
            e.validate_replay(&[log, second].concat()),
            Err(TransitionRefusal::BlockReused)
        );
    }

    #[test]
    fn sibling_blocks_of_one_batch_are_each_held_to_their_own_menu() {
        let e = two_officer_engine();
        let opening = vec![opened(&e)];
        let view = e.view(&traj(), opening.clone(), 1).expect("the log replays");
        let batch = appended_facts(
            e.handle(
                &view,
                EngineEvent::Proposals(ProposalBatch {
                    id: crate::transition::ProposalBatchId::new("b1"),
                    trajectory: traj(),
                    provider_results: Vec::new(),
                    proposals: vec![
                        raw(&call("wire", json!({ "to": "a" }))),
                        raw(&call("wire", json!({ "to": "b" }))),
                    ],
                    spawn: None,
                    offer_nonce: nonce(),
                    evidence: Vec::new(),
                    audience: crate::audience::AudienceEvidence::default(),
                }),
            )
            .expect("the batch decides"),
        );
        let offers: Vec<usize> = batch
            .iter()
            .enumerate()
            .filter(|(_, fact)| matches!(fact, Fact::OfferOpened { .. }))
            .map(|(at, _)| at)
            .collect();
        assert_eq!(offers.len(), 4, "two blocks of two plans");
        let head = &batch[..offers[0]];
        let [a1, a2, b1, b2] = [
            batch[offers[0]].clone(),
            batch[offers[1]].clone(),
            batch[offers[2]].clone(),
            batch[offers[3]].clone(),
        ];
        let replay = |offers: Vec<Fact>| e.validate_replay(&[opening.clone(), head.to_vec(), offers].concat());
        assert_eq!(replay(vec![a1.clone(), a2.clone(), b1.clone(), b2.clone()]), Ok(()));
        assert_eq!(replay(vec![b1.clone(), b2.clone(), a1.clone(), a2.clone()]), Ok(()));
        assert_eq!(
            replay(vec![a1.clone(), b1.clone(), b2.clone(), a2.clone()]),
            Err(TransitionRefusal::IncompleteMenu)
        );
        let mut a3 = a2.clone();
        if let Fact::OfferOpened { offer, block, .. } = &mut a3 {
            *offer = crate::value::OfferId::of_plan(block, 3, b"third");
        }
        assert_eq!(replay(vec![a1, a2, b1, b2, a3]), Err(TransitionRefusal::BlockReused));
    }

    #[test]
    fn a_fully_denied_block_re_plans_to_an_empty_menu_that_replays() {
        let e = two_officer_engine();
        let mut log = vec![opened(&e)];
        let opened =
            appended_facts(proposed(&e, &log, "b1", nonce(), call("wire", json!({}))).expect("the batch decides"));
        log.extend(opened.clone());
        let mut offers = opened_offers(&opened);
        for round in 0..2 {
            let (offer, plan) = offers.remove(0);
            let authority = plan.required[0].authority.clone();
            let done =
                execute_offer(&e, &log, offer, OfferOutcome::Denied { authority }).expect("the denial is recorded");
            let fresh = match offer_answer(&done) {
                OfferFollowUp::Denied { block } => block.offers.clone(),
                other => panic!("a denial answers with the re-planned block, not {other:?}"),
            };
            let facts = appended_facts(done);
            log.extend(facts.clone());
            assert_eq!(fresh.len(), 1 - round);
            offers = opened_offers(&facts);
        }
        assert!(offers.is_empty(), "nothing left to offer");
        assert_eq!(e.validate_replay(&log), Ok(()));
    }

    #[test]
    fn evidence_gathered_for_one_offer_cannot_approve_another() {
        let e = two_officer_engine();
        let log = vec![opened(&e)];
        let first = appended_facts(
            proposed(&e, &log, "b1", nonce(), call("wire", json!({ "to": "alice" }))).expect("the batch decides"),
        );
        let (one, plan) = opened_offers(&first)[0].clone();
        let log = [log, first].concat();
        let second = appended_facts(
            proposed(
                &e,
                &log,
                "b2",
                crate::value::OfferNonce::new([5u8; 32]),
                call("wire", json!({ "to": "mallory" })),
            )
            .expect("the second batch decides"),
        );
        let (other, other_plan) = opened_offers(&second)[0].clone();
        let log = [log, second].concat();

        assert_eq!(plan.required, other_plan.required);
        let fold = partial(SUSPICIOUS, Audience::public());
        assert_eq!(
            evidence_for(one, &plan, "wire", fold.clone())
                .into_iter()
                .map(|given| (given.authority, given.covers, given.reviewed))
                .collect::<Vec<_>>(),
            evidence_for(other, &other_plan, "wire", fold.clone())
                .into_iter()
                .map(|given| (given.authority, given.covers, given.reviewed))
                .collect::<Vec<_>>(),
            "only the offer distinguishes them"
        );

        let gathered_for_one = evidence_for(one, &plan, "wire", fold.clone());
        assert!(
            matches!(
                execute_offer(&e, &log, other, OfferOutcome::Approved(gathered_for_one)),
                Err(TransitionError::Plan(PlanError::EvidenceOfferMismatch))
            ),
            "one offer's approval does not admit another's call"
        );
        let approved = appended_facts(
            execute_offer(
                &e,
                &log,
                other,
                OfferOutcome::Approved(evidence_for(other, &other_plan, "wire", fold)),
            )
            .expect("its own evidence approves it"),
        );
        let position = approved
            .iter()
            .position(|fact| matches!(fact, Fact::CallApproved { .. }))
            .expect("the acceptance prepared one");
        let mut swapped = approved;
        if let Fact::CallApproved { rulings, .. } = &mut swapped[position] {
            rulings[0].offer = one;
        }
        assert_eq!(
            e.validate_replay(&[log, swapped].concat()),
            Err(TransitionRefusal::UnbackedApproval)
        );
    }

    #[test]
    fn a_repeat_returns_the_recorded_outcome_of_a_dispatch_that_already_ran() {
        let e = engine(vec![open_tool("note")]);
        let log = vec![opened(&e)];
        let note = call("note", json!({}));
        let release = proposed(&e, &log, "b1", nonce(), note.clone()).expect("the note releases");
        let dispatch = match &release.follow_up {
            FollowUp::Proposals { released, .. } => released[0].dispatch.clone(),
            other => panic!("a proposal batch answers with proposals, not {other:?}"),
        };
        let log = [log, appended_facts(release)].concat();
        let view = e.view(&traj(), log.clone(), log.len() as u64).expect("the log replays");
        let closed = e
            .handle(
                &view,
                EngineEvent::Outcome(ToolReport {
                    dispatch: dispatch.clone(),
                    outcome: ToolOutcome::Success {
                        body: OutcomeBody::Available(ValueBody::new("result")),
                    },
                    evidence: Vec::new(),
                    offer_nonce: nonce(),
                    audience: crate::audience::AudienceEvidence::default(),
                }),
            )
            .expect("the outcome closes it");
        let log = [log, appended_facts(closed)].concat();

        let repeat = proposed(&e, &log, "b1", nonce(), note.clone()).expect("the repeat answers");
        assert!(repeat.append.is_none(), "a repeat appends nothing");
        match &repeat.follow_up {
            FollowUp::Proposals { released, settled, .. } => {
                assert!(
                    released.is_empty(),
                    "a dispatch that already ran is not handed back again"
                );
                assert_eq!(
                    settled,
                    &vec![Settled {
                        dispatch,
                        call: note,
                        outcome: SettledOutcome::Closed {
                            admitted: Some(ValueBody::new("result")),
                        },
                    }]
                );
            }
            other => panic!("a proposal batch answers with proposals, not {other:?}"),
        }
    }

    #[test]
    fn a_matching_proposal_consumes_its_approval_and_opens_the_dispatch() {
        let e = engine(vec![crm_tool()]);
        let log = vec![opened(&e)];
        let opened = appended_facts(blocked_batch(&e, &log, "b1", nonce()));
        let (offer, plan) = opened_offers(&opened)[0].clone();
        let log = [log, opened].concat();
        let approved = appended_facts(
            execute_offer(&e, &log, offer, OfferOutcome::Approved(Vec::new())).expect("the offer executes"),
        );
        let log = [log, approved].concat();

        let release = proposed(&e, &log, "b2", nonce(), call("get_ticket", json!({}))).expect("the approval releases");
        let dispatch = match &release.follow_up {
            FollowUp::Proposals { released, .. } => released[0].dispatch.clone(),
            other => panic!("a proposal batch answers with proposals, not {other:?}"),
        };
        let facts = appended_facts(release);
        match &facts[..] {
            [
                Fact::BasisAdvanced { .. },
                Fact::ProposalBatchDecided { released, .. },
                Fact::CallApprovalConsumed {
                    offer: spent,
                    dispatch: opened_for,
                    ..
                },
                Fact::Acceptance {
                    plan: accepted_under,
                    narrowing,
                    ..
                },
                Fact::DispatchOpened { dispatch: opening, .. },
            ] => {
                assert_eq!(released, &vec![dispatch.clone()]);
                assert_eq!((spent, opened_for, opening), (&offer, &dispatch, &dispatch));
                assert_eq!(accepted_under, &plan.id);
                assert_eq!(Some(narrowing), plan.narrowing());
            }
            other => panic!("unexpected release batch {other:?}"),
        }
        assert_eq!(e.validate_replay(&[log, facts].concat()), Ok(()));
    }

    #[test]
    fn an_approval_releases_once() {
        let e = engine(vec![crm_tool()]);
        let log = vec![opened(&e)];
        let opened = appended_facts(blocked_batch(&e, &log, "b1", nonce()));
        let offer = opened_offers(&opened)[0].0;
        let log = [log, opened].concat();
        let approved = appended_facts(
            execute_offer(&e, &log, offer, OfferOutcome::Approved(Vec::new())).expect("the offer executes"),
        );
        let log = [log, approved].concat();
        let ticket = call("get_ticket", json!({}));
        let first = appended_facts(proposed(&e, &log, "b2", nonce(), ticket.clone()).expect("the approval releases"));
        let log = [log, first].concat();

        let second = proposed(&e, &log, "b3", nonce(), ticket).expect("the second proposal decides");
        match &second.follow_up {
            FollowUp::Proposals { released, blocked, .. } => {
                assert!(released.is_empty(), "a spent approval releases nothing");
                assert_eq!(blocked.len(), 1);
            }
            other => panic!("a proposal batch answers with proposals, not {other:?}"),
        }
    }

    #[test]
    fn only_a_basis_moving_release_stales_an_approval() {
        let prepared = |e: &Engine, log: Vec<Fact>| {
            let opened = appended_facts(blocked_batch(e, &log, "b1", nonce()));
            let offer = opened_offers(&opened)[0].0;
            let log = [log, opened].concat();
            let approved = appended_facts(
                execute_offer(e, &log, offer, OfferOutcome::Approved(Vec::new())).expect("the offer executes"),
            );
            [log, approved].concat()
        };
        let releases = |e: &Engine, log: &[Fact], id: &str| {
            let decision = proposed(e, log, id, nonce(), call("get_ticket", json!({}))).expect("the batch decides");
            match &decision.follow_up {
                FollowUp::Proposals { released, .. } => !released.is_empty(),
                other => panic!("a proposal batch answers with proposals, not {other:?}"),
            }
        };

        let e = engine(vec![crm_tool(), neutral_tool()]);
        let log = prepared(&e, vec![opened(&e)]);
        let neutral = appended_facts(
            proposed(&e, &log, "b2", nonce(), call("read_note", json!({}))).expect("the neutral call releases"),
        );
        assert!(
            releases(&e, &[log, neutral].concat(), "b3"),
            "a neutral release stales nothing"
        );

        let e = engine(vec![crm_tool(), strict_tool("send")]);
        let log = prepared(&e, vec![opened(&e)]);
        let elsewhere = appended_facts(
            proposed(&e, &log, "b2", nonce(), call("get_ticket", json!({ "id": "other" })))
                .expect("the other proposal decides"),
        );
        assert!(releases(&e, &[log, elsewhere].concat(), "b3"), "a block stales nothing");

        let e = engine(vec![crm_tool(), restrictable_tool("note")]);
        let log = prepared(&e, vec![opened(&e)]);
        let restricting =
            appended_facts(proposed(&e, &log, "b2", nonce(), call("note", json!({}))).expect("the note releases"));
        assert!(
            !releases(&e, &[log, restricting].concat(), "b3"),
            "a release that can restrict the trajectory stales the approval it did not belong to"
        );
    }

    /// A neutral delta under an Annotator is not neutral: the produced annotation owns the
    /// output, so the release moves the basis an approval was taken on.
    #[test]
    fn an_annotated_release_stales_an_approval() {
        let classified = ToolAnnotation {
            name: ToolName::new("read_note"),
            ..neutral_tool()
        };
        let pin = pinned_for(
            {
                let mut produced = classified.clone();
                produced.delta = Delta {
                    trust: None,
                    audience: Some(DeclaredAudience::literal(Audience::public())),
                };
                produced
            },
            "classify",
            &call("read_note", json!({})),
        );

        let mut cfg = test_config(vec![crm_tool()]);
        cfg.tools.push(annotated(classified, "classify"));
        cfg.annotators.push(annotator("classify"));
        let e = open_engine(cfg);
        let log = vec![opened(&e)];
        let blocked = appended_facts(blocked_batch(&e, &log, "b1", nonce()));
        let offer = opened_offers(&blocked)[0].0;
        let log = [log, blocked].concat();
        let approved = appended_facts(
            execute_offer(&e, &log, offer, OfferOutcome::Approved(Vec::new())).expect("the offer executes"),
        );
        let log = [log, approved].concat();
        let note = proposed(
            &e,
            &log,
            "b2",
            nonce(),
            call("read_note", json!({})).with_annotation(Some(pin)),
        )
        .expect("the classified note decides");
        assert!(
            matches!(&note.follow_up, FollowUp::Proposals { released, .. } if !released.is_empty()),
            "a public answer narrows nothing, so the note releases: {:?}",
            note.follow_up
        );
        let log = [log, appended_facts(note)].concat();

        let decision = proposed(&e, &log, "b3", nonce(), call("get_ticket", json!({}))).expect("the batch decides");
        let released = match &decision.follow_up {
            FollowUp::Proposals { released, .. } => released.clone(),
            other => panic!("a proposal batch answers with proposals, not {other:?}"),
        };
        assert!(
            released.is_empty(),
            "the approval was taken on a basis the pinned release moved"
        );
    }

    #[test]
    fn a_later_basis_change_does_not_revoke_an_open_dispatch() {
        let e = engine(vec![crm_tool(), open_tool("note")]);
        let log = vec![opened(&e)];
        let opened = appended_facts(blocked_batch(&e, &log, "b1", nonce()));
        let offer = opened_offers(&opened)[0].0;
        let log = [log, opened].concat();
        let approved = appended_facts(
            execute_offer(&e, &log, offer, OfferOutcome::Approved(Vec::new())).expect("the offer executes"),
        );
        let log = [log, approved].concat();
        let release = proposed(&e, &log, "b2", nonce(), call("get_ticket", json!({}))).expect("the approval releases");
        let dispatch = match &release.follow_up {
            FollowUp::Proposals { released, .. } => released[0].dispatch.clone(),
            other => panic!("a proposal batch answers with proposals, not {other:?}"),
        };
        let log = [log, appended_facts(release)].concat();
        let moved =
            appended_facts(proposed(&e, &log, "b3", nonce(), call("note", json!({}))).expect("the note releases"));
        let log = [log, moved].concat();

        let view = e.view(&traj(), log.clone(), log.len() as u64).expect("the log replays");
        let closed = e
            .handle(
                &view,
                EngineEvent::Outcome(ToolReport {
                    dispatch,
                    outcome: ToolOutcome::Failure,
                    evidence: Vec::new(),
                    offer_nonce: nonce(),
                    audience: crate::audience::AudienceEvidence::default(),
                }),
            )
            .expect("the dispatch is still the engine's to close");
        assert_eq!(e.validate_replay(&[log, appended_facts(closed)].concat()), Ok(()));
    }

    #[test]
    fn a_forged_release_cannot_depart_from_the_approval_it_spends() {
        let e = engine(vec![crm_tool()]);
        let log = vec![opened(&e)];
        let opened = appended_facts(blocked_batch(&e, &log, "b1", nonce()));
        let offer = opened_offers(&opened)[0].0;
        let log = [log, opened].concat();
        let approved = appended_facts(
            execute_offer(&e, &log, offer, OfferOutcome::Approved(Vec::new())).expect("the offer executes"),
        );
        let opening = [log, approved].concat();
        let release =
            appended_facts(proposed(&e, &opening, "b2", nonce(), call("get_ticket", json!({}))).expect("it releases"));
        assert_eq!(e.validate_replay(&[opening.clone(), release.clone()].concat()), Ok(()));

        let position = release
            .iter()
            .position(|fact| matches!(fact, Fact::Acceptance { .. }))
            .expect("the plan carried a narrowing");
        let mut relabelled = release.clone();
        if let Fact::Acceptance { plan, .. } = &mut relabelled[position] {
            *plan = plan::PlanId::new(plan.value() + 1);
        }
        assert_eq!(
            e.validate_replay(&[opening.clone(), relabelled].concat()),
            Err(TransitionRefusal::UnbackedApproval)
        );
        let mut dropped = release;
        dropped.remove(position);
        assert_eq!(
            e.validate_replay(&[opening, dropped].concat()),
            Err(TransitionRefusal::UnbackedApproval)
        );
    }

    #[test]
    fn an_executed_offer_prepares_its_call_and_releases_nothing() {
        let e = engine(vec![crm_tool()]);
        let log = vec![opened(&e)];
        let decision = blocked_batch(&e, &log, "b1", nonce());
        let opened = appended_facts(decision);
        let (offer, plan) = opened_offers(&opened)[0].clone();
        let log = [log, opened].concat();

        let done = execute_offer(&e, &log, offer, OfferOutcome::Approved(Vec::new())).expect("the offer executes");
        let proposal = match offer_answer(&done) {
            OfferFollowUp::Approved { call } => (**call).clone(),
            other => panic!("a full approval prepares a call, not {other:?}"),
        };
        assert_eq!(proposal, call("get_ticket", json!({})));
        let facts = appended_facts(done);
        assert!(
            !facts.iter().any(|fact| matches!(
                fact,
                Fact::DispatchOpened { .. } | Fact::Ruling { .. } | Fact::Acceptance { .. }
            )),
            "an approval reserves nothing and lets no executor run"
        );
        match &facts[..] {
            [
                Fact::BasisAdvanced { .. },
                Fact::OfferAccepted { offer: accepted, .. },
                Fact::CallApproved {
                    offer: approved,
                    call: approved_call,
                    plan: approved_plan,
                    acceptance,
                    rulings,
                    ..
                },
            ] => {
                assert_eq!((accepted, approved), (&offer, &offer));
                assert_eq!(approved_call, &proposal);
                assert_eq!(approved_plan, &plan.id);
                assert_eq!(acceptance.as_ref(), plan.narrowing());
                assert!(rulings.is_empty());
            }
            other => panic!("unexpected approval batch {other:?}"),
        }
        assert_eq!(e.validate_replay(&[log, facts].concat()), Ok(()));
    }

    #[test]
    fn accepting_one_offer_ends_every_sibling_on_its_candidate() {
        let e = two_officer_engine();
        let log = vec![opened(&e)];
        let wire = call("wire", json!({}));
        let decision = proposed(&e, &log, "b1", nonce(), wire.clone()).expect("the batch decides");
        let opened = appended_facts(decision);
        let offers = opened_offers(&opened);
        assert_eq!(offers.len(), 2, "each officer's grouped assignment is its own offer");
        let (chosen, plan) = offers[0].clone();
        let sibling = offers[1].0;
        let log = [log, opened].concat();

        let evidence = evidence_for(chosen, &plan, "wire", partial(SUSPICIOUS, Audience::public()));
        let done = execute_offer(&e, &log, chosen, OfferOutcome::Approved(evidence)).expect("the offer executes");
        let facts = appended_facts(done);
        assert!(
            facts
                .iter()
                .any(|fact| matches!(fact, Fact::OfferInvalidated { offer, .. } if offer == &sibling)),
            "the sibling on the taken candidate ends in the same batch"
        );
        let log = [log, facts].concat();
        assert_eq!(e.validate_replay(&log), Ok(()));

        assert!(matches!(
            execute_offer(&e, &log, sibling, OfferOutcome::Approved(Vec::new())),
            Ok(decision) if offer_answer(&decision) == &OfferFollowUp::Invalidated
        ));
        let repeat = proposed(&e, &log, "b1", nonce(), wire).expect("the repeat answers");
        assert!(offers_of(&repeat).is_empty());
    }

    #[test]
    fn a_repeated_selection_answers_from_the_record() {
        let e = engine(vec![crm_tool()]);
        let log = vec![opened(&e)];
        let decision = blocked_batch(&e, &log, "b1", nonce());
        let opened = appended_facts(decision);
        let offer = opened_offers(&opened)[0].0;
        let log = [log, opened].concat();
        let first = execute_offer(&e, &log, offer, OfferOutcome::Approved(Vec::new())).expect("the offer executes");
        let prepared = offer_answer(&first).clone();
        let log = [log, appended_facts(first)].concat();

        let repeat = execute_offer(&e, &log, offer, OfferOutcome::Approved(Vec::new())).expect("the repeat answers");
        assert!(repeat.append.is_none(), "a repeat appends nothing");
        assert_eq!(offer_answer(&repeat), &prepared);
        assert_eq!(
            execute_offer(
                &e,
                &log,
                offer,
                OfferOutcome::Denied {
                    authority: AuthorityName::new("officer-a"),
                },
            ),
            Err(TransitionError::TerminalOffer)
        );
    }

    #[test]
    fn an_offer_whose_basis_moved_is_refused() {
        let e = engine(vec![crm_tool(), restrictable_tool("note")]);
        let log = vec![opened(&e)];
        let decision = blocked_batch(&e, &log, "b1", nonce());
        let opened = appended_facts(decision);
        let offer = opened_offers(&opened)[0].0;
        let log = [log, opened].concat();

        let release = proposed(&e, &log, "b2", nonce(), call("note", json!({}))).expect("the note releases");
        let log = [log, appended_facts(release)].concat();
        assert_eq!(
            execute_offer(&e, &log, offer, OfferOutcome::Approved(Vec::new())),
            Err(TransitionError::StaleOffer)
        );
    }

    #[test]
    fn a_denial_ends_the_plans_that_named_the_authority_and_re_offers_the_rest() {
        let e = two_officer_engine();
        let log = vec![opened(&e)];
        let wire = call("wire", json!({}));
        let decision = proposed(&e, &log, "b1", nonce(), wire.clone()).expect("the batch decides");
        let opened = appended_facts(decision);
        let offers = opened_offers(&opened);
        let denied_authority = offers[0].1.required[0].authority.clone();
        let (target, survivor) = (offers[0].0, offers[1].0);
        let log = [log, opened].concat();

        let done = execute_offer(
            &e,
            &log,
            target,
            OfferOutcome::Denied {
                authority: denied_authority.clone(),
            },
        )
        .expect("the denial is recorded");
        let fresh = match offer_answer(&done) {
            OfferFollowUp::Denied { block } => block.offers.clone(),
            other => panic!("a denial answers with the re-planned block, not {other:?}"),
        };
        let facts = appended_facts(done);
        assert!(facts.iter().any(
            |fact| matches!(fact, Fact::Denial { digest, authority, .. } if digest == &wire.digest() && authority == &denied_authority)
        ));
        let ended: Vec<_> = facts
            .iter()
            .filter_map(|fact| match fact {
                Fact::OfferDenied { offer, .. } => Some(*offer),
                _ => None,
            })
            .collect();
        assert_eq!(ended, vec![target], "only the plans naming the denying authority end");

        assert_eq!(fresh.len(), 1);
        assert_ne!(fresh[0].0, survivor);
        let log = [log, facts].concat();
        assert_eq!(e.validate_replay(&log), Ok(()));
        assert_eq!(
            execute_offer(&e, &log, survivor, OfferOutcome::Approved(Vec::new())),
            Err(TransitionError::StaleOffer),
            "the pre-denial offer cannot revive"
        );
        let (fresh_offer, fresh_plan) = opened_offers(&log)
            .into_iter()
            .find(|(offer, _)| offer == &fresh[0].0)
            .expect("the re-plan opened it");
        let evidence = evidence_for(
            fresh_offer,
            &fresh_plan,
            "wire",
            partial(SUSPICIOUS, Audience::public()),
        );
        let executed =
            execute_offer(&e, &log, fresh_offer, OfferOutcome::Approved(evidence)).expect("the fresh offer executes");
        assert!(matches!(offer_answer(&executed), OfferFollowUp::Approved { .. }));
    }

    #[test]
    fn a_denial_by_an_unassigned_authority_is_refused() {
        let e = two_officer_engine();
        let log = vec![opened(&e)];
        let decision = proposed(&e, &log, "b1", nonce(), call("wire", json!({}))).expect("the batch decides");
        let opened = appended_facts(decision);
        let offers = opened_offers(&opened);
        let other = offers[1].1.required[0].authority.clone();
        let log = [log, opened].concat();
        assert_eq!(
            execute_offer(&e, &log, offers[0].0, OfferOutcome::Denied { authority: other }),
            Err(TransitionError::UnassignedAuthority)
        );
    }

    #[test]
    fn an_incomplete_or_misreviewed_evidence_set_is_refused() {
        let e = two_officer_engine();
        let log = vec![opened(&e)];
        let decision = proposed(&e, &log, "b1", nonce(), call("wire", json!({}))).expect("the batch decides");
        let opened = appended_facts(decision);
        let (offer, plan) = opened_offers(&opened)[0].clone();
        let log = [log, opened].concat();
        let complete = evidence_for(offer, &plan, "wire", partial(SUSPICIOUS, Audience::public()));

        assert!(matches!(
            execute_offer(&e, &log, offer, OfferOutcome::Approved(Vec::new())),
            Err(TransitionError::Plan(PlanError::RulingAssignmentMismatch))
        ));
        let mut rerouted = complete.clone();
        rerouted[0].authority = AuthorityName::new("officer-b");
        assert!(
            matches!(
                execute_offer(&e, &log, offer, OfferOutcome::Approved(rerouted)),
                Err(TransitionError::Plan(PlanError::RulingAssignmentMismatch))
            ),
            "an overlapping mandate cannot reroute the grouping the agent was shown"
        );
        let mut moved_fold = complete;
        moved_fold[0].reviewed.trajectory_label = partial(TRUSTED, Audience::public());
        assert!(matches!(
            execute_offer(&e, &log, offer, OfferOutcome::Approved(moved_fold)),
            Err(TransitionError::Plan(PlanError::ReviewMismatch))
        ));
    }

    #[test]
    fn an_unknown_or_foreign_offer_is_refused() {
        let e = engine(vec![crm_tool()]);
        let log = vec![opened(&e)];
        let decision = blocked_batch(&e, &log, "b1", nonce());
        let opened = appended_facts(decision);
        let offer = opened_offers(&opened)[0].0;
        let log = [log, opened].concat();

        let unknown = crate::value::OfferId::of_plan(
            &crate::value::BlockId::of_proposal(
                &nonce(),
                &traj(),
                &crate::transition::ProposalBatchId::new("nowhere"),
                0,
                &call("get_ticket", json!({})).digest(),
            ),
            0,
            b"{}",
        );
        assert_eq!(
            execute_offer(&e, &log, unknown, OfferOutcome::Approved(Vec::new())),
            Err(TransitionError::UnknownOffer)
        );
        let elsewhere = TrajectoryId::new("elsewhere");
        let log = [log.clone(), forked_child(&e, &log, &elsewhere)].concat();
        let view = e.view(&traj(), log.clone(), log.len() as u64).expect("the log replays");
        assert_eq!(
            e.handle(
                &view,
                EngineEvent::ExecuteOffer(OfferExecution {
                    trajectory: elsewhere,
                    offer,
                    outcome: OfferOutcome::Approved(Vec::new()),
                    offer_nonce: nonce(),
                    audience: crate::audience::AudienceEvidence::default(),
                }),
            ),
            Err(TransitionError::OfferElsewhere)
        );
    }

    #[test]
    fn a_forged_approval_record_is_refused() {
        let e = engine(vec![crm_tool()]);
        let opening = vec![opened(&e)];
        let decision = blocked_batch(&e, &opening, "b1", nonce());
        let opened = appended_facts(decision);
        let offer = opened_offers(&opened)[0].0;
        let opening = [opening, opened].concat();
        let approval = appended_facts(
            execute_offer(&e, &opening, offer, OfferOutcome::Approved(Vec::new())).expect("the offer executes"),
        );
        assert_eq!(e.validate_replay(&[opening.clone(), approval.clone()].concat()), Ok(()));

        let position = approval
            .iter()
            .position(|fact| matches!(fact, Fact::CallApproved { .. }))
            .expect("the acceptance prepared one");
        let rewritten = |mutate: &dyn Fn(&mut Fact)| {
            let mut facts = approval.clone();
            mutate(&mut facts[position]);
            e.validate_replay(&[opening.clone(), facts].concat())
        };
        assert_eq!(
            rewritten(&|fact| {
                if let Fact::CallApproved { call: approved, .. } = fact {
                    *approved = call("get_ticket", json!({ "id": "other" }));
                }
            }),
            Err(TransitionRefusal::UnbackedApproval)
        );
        assert_eq!(
            rewritten(&|fact| {
                if let Fact::CallApproved { acceptance, .. } = fact {
                    *acceptance = None;
                }
            }),
            Err(TransitionRefusal::UnbackedApproval)
        );
        assert_eq!(
            rewritten(&|fact| {
                if let Fact::CallApproved { basis, .. } = fact {
                    basis.subject = basis.subject.next();
                }
            }),
            Err(TransitionRefusal::ForgedBasis)
        );
        let orphan: Vec<Fact> = approval
            .iter()
            .filter(|fact| !matches!(fact, Fact::OfferAccepted { .. }))
            .cloned()
            .collect();
        assert_eq!(
            e.validate_replay(&[opening, orphan].concat()),
            Err(TransitionRefusal::OfferEnded)
        );
    }

    #[test]
    fn an_acceptance_whose_batch_prepares_no_approval_is_refused() {
        let e = two_officer_engine();
        let opening = vec![opened(&e)];
        let wire = call("wire", json!({}));
        let opened = appended_facts(proposed(&e, &opening, "b1", nonce(), wire.clone()).expect("the batch decides"));
        let offers = opened_offers(&opened);
        let (chosen, plan) = offers[0].clone();
        let opening = [opening, opened].concat();
        let elsewhere = appended_facts(proposed(&e, &opening, "b2", nonce(), wire).expect("the batch decides"));
        let unrelated = opened_offers(&elsewhere)[0].0;
        let opening = [opening, elsewhere].concat();

        let evidence = evidence_for(chosen, &plan, "wire", partial(SUSPICIOUS, Audience::public()));
        let approval = appended_facts(
            execute_offer(&e, &opening, chosen, OfferOutcome::Approved(evidence)).expect("the offer executes"),
        );
        assert_eq!(e.validate_replay(&[opening.clone(), approval.clone()].concat()), Ok(()));

        let position = approval
            .iter()
            .position(|fact| matches!(fact, Fact::CallApproved { .. }))
            .expect("the acceptance prepared one");
        let mut truncated = approval.clone();
        truncated.remove(position);
        assert!(
            truncated
                .iter()
                .any(|fact| matches!(fact, Fact::OfferInvalidated { .. }))
        );
        let stopped = [opening.clone(), truncated].concat();
        assert_eq!(
            e.validate_replay(&stopped),
            Err(TransitionRefusal::UndischargedAcceptance)
        );
        assert_eq!(
            e.view(&traj(), stopped.clone(), stopped.len() as u64).err(),
            Some(TransitionRefusal::UndischargedAcceptance)
        );

        let mut deferred = approval.clone();
        deferred.insert(position, stray_admission(&traj(), known(TRUSTED, Audience::public())));
        assert_eq!(
            e.validate_replay(&[opening.clone(), deferred].concat()),
            Err(TransitionRefusal::UndischargedAcceptance)
        );

        let mut foreign = approval;
        foreign.insert(
            position,
            Fact::OfferInvalidated {
                trajectory: traj(),
                offer: unrelated,
            },
        );
        assert_eq!(
            e.validate_replay(&[opening, foreign].concat()),
            Err(TransitionRefusal::UndischargedAcceptance)
        );
    }

    #[test]
    fn the_approval_window_admits_no_ending_from_before_the_offer_it_approves() {
        let e = two_officer_engine();
        let log = vec![opened(&e)];
        let wire = call("wire", json!({}));
        let opened = appended_facts(proposed(&e, &log, "b1", nonce(), wire).expect("the batch decides"));
        let offers = opened_offers(&opened);
        let authority = offers[0].1.required[0].authority.clone();
        let (target, survivor) = (offers[0].0, offers[1].0);
        let log = [log, opened].concat();

        let denial = appended_facts(
            execute_offer(&e, &log, target, OfferOutcome::Denied { authority }).expect("the denial is recorded"),
        );
        let log = [log, denial].concat();
        let (fresh, fresh_plan) = opened_offers(&log)
            .into_iter()
            .find(|(offer, _)| offer != &target && offer != &survivor)
            .expect("the denial re-offered the surviving plan");
        assert_eq!(
            execute_offer(&e, &log, survivor, OfferOutcome::Approved(Vec::new())),
            Err(TransitionError::StaleOffer),
            "the predecessor is stale, and no record ended it"
        );

        let evidence = evidence_for(fresh, &fresh_plan, "wire", partial(SUSPICIOUS, Audience::public()));
        let approval = appended_facts(
            execute_offer(&e, &log, fresh, OfferOutcome::Approved(evidence)).expect("the fresh offer executes"),
        );
        assert_eq!(e.validate_replay(&[log.clone(), approval.clone()].concat()), Ok(()));

        let position = approval
            .iter()
            .position(|fact| matches!(fact, Fact::CallApproved { .. }))
            .expect("the acceptance prepared one");
        let mut forged = approval;
        forged.insert(
            position,
            Fact::OfferInvalidated {
                trajectory: traj(),
                offer: survivor,
            },
        );
        assert_eq!(
            e.validate_replay(&[log, forged].concat()),
            Err(TransitionRefusal::UndischargedAcceptance)
        );
    }

    #[test]
    fn a_repeat_of_an_unapproved_acceptance_answers_a_refusal() {
        let e = engine(vec![crm_tool()]);
        let opening = vec![opened(&e)];
        let opened = appended_facts(blocked_batch(&e, &opening, "b1", nonce()));
        let offer = opened_offers(&opened)[0].0;
        let approval = appended_facts(
            execute_offer(
                &e,
                &[opening.clone(), opened.clone()].concat(),
                offer,
                OfferOutcome::Approved(Vec::new()),
            )
            .expect("the offer executes"),
        );
        let unpaired: Vec<Fact> = [opening, opened, approval]
            .concat()
            .into_iter()
            .filter(|fact| !matches!(fact, Fact::CallApproved { .. }))
            .collect();

        let mut projection = Projection::empty(unpaired.len() as u64);
        for fact in &unpaired {
            projection.fold(fact);
        }
        let view = crate::transition::EngineView::validated(projection, e.identity, traj());
        assert_eq!(
            e.handle(
                &view,
                EngineEvent::ExecuteOffer(OfferExecution {
                    trajectory: traj(),
                    offer,
                    outcome: OfferOutcome::Approved(Vec::new()),
                    offer_nonce: nonce(),
                    audience: crate::audience::AudienceEvidence::default(),
                }),
            ),
            Err(TransitionError::Invalid(TransitionRefusal::UndischargedAcceptance))
        );
    }

    fn offers_of(decision: &EngineDecision) -> Vec<(crate::value::OfferId, plan::PlanId)> {
        match &decision.follow_up {
            FollowUp::Proposals { blocked, .. } => blocked[0].offers.clone(),
            other => panic!("a proposal batch answers with proposals, not {other:?}"),
        }
    }

    #[test]
    fn a_block_binds_one_durable_offer_to_each_of_its_plans() {
        let e = engine(vec![crm_tool()]);
        let log = vec![opened(&e)];
        let decision = blocked_batch(&e, &log, "b1", nonce());
        let offers = offers_of(&decision);
        assert!(!offers.is_empty());

        let facts = appended_facts(decision);
        let opened: Vec<_> = facts
            .iter()
            .filter_map(|fact| match fact {
                Fact::OfferOpened { offer, basis, plan, .. } => Some((*offer, *basis, plan.id)),
                _ => None,
            })
            .collect();
        assert_eq!(
            opened.iter().map(|(id, _, plan)| (*id, *plan)).collect::<Vec<_>>(),
            offers,
            "the follow-up names exactly the offers the batch opened"
        );
        assert_eq!(
            opened.iter().filter(|(_, basis, _)| basis != &opened[0].1).count(),
            0,
            "siblings opened together share one basis"
        );
        assert_eq!(
            opened.iter().map(|(id, ..)| *id).collect::<BTreeSet<_>>().len(),
            opened.len()
        );
        let elsewhere = offers_of(&blocked_batch(&e, &log, "b1", crate::value::OfferNonce::new([9u8; 32])));
        assert!(
            elsewhere
                .iter()
                .all(|(id, _)| !offers.iter().any(|(mine, _)| mine == id))
        );
    }

    #[test]
    fn a_repeated_block_answers_with_the_offers_it_already_opened() {
        let e = engine(vec![crm_tool()]);
        let log = vec![opened(&e)];
        let first = blocked_batch(&e, &log, "b1", nonce());
        let opened = offers_of(&first);
        let log = [log, appended_facts(first)].concat();

        let repeat = blocked_batch(&e, &log, "b1", nonce());
        assert!(repeat.append.is_none(), "a repeat appends nothing");
        assert_eq!(offers_of(&repeat), opened);
    }

    #[test]
    fn a_forged_offer_record_is_refused() {
        let e = engine(vec![crm_tool()]);
        let opening = vec![opened(&e)];
        let batch = appended_facts(blocked_batch(&e, &opening, "b1", nonce()));
        assert_eq!(e.validate_replay(&[opening.clone(), batch.clone()].concat()), Ok(()));

        let position = batch
            .iter()
            .position(|fact| matches!(fact, Fact::OfferOpened { .. }))
            .expect("the block opened an offer");
        let rewritten = |mutate: &dyn Fn(&mut Fact)| {
            let mut facts = batch.clone();
            mutate(&mut facts[position]);
            e.validate_replay(&[opening.clone(), facts].concat())
        };
        assert_eq!(
            rewritten(&|fact| {
                if let Fact::OfferOpened { plan, .. } = fact {
                    plan.steps.clear();
                }
            }),
            Err(TransitionRefusal::UnbackedOffer)
        );
        assert_eq!(
            rewritten(&|fact| {
                if let Fact::OfferOpened { basis, .. } = fact {
                    basis.flow = basis.flow.next();
                }
            }),
            Err(TransitionRefusal::ForgedBasis)
        );
        let doubled = [opening.clone(), batch.clone(), vec![batch[position].clone()]].concat();
        assert_eq!(e.validate_replay(&doubled), Err(TransitionRefusal::OfferReopened));
    }

    #[test]
    fn a_release_replays_only_as_the_records_its_decision_obliged() {
        let internal = Audience::restricted([ReaderId::new("insider")]);
        let e = engine_at(vec![crm_tool()], known(TRUSTED, internal));
        let call = call("get_ticket", json!({}));
        let records = vec![opened(&e)];
        let view = e.view(&traj(), records.clone(), 1).unwrap();
        let decision = e
            .handle(
                &view,
                EngineEvent::Proposals(ProposalBatch {
                    id: crate::transition::ProposalBatchId::new("b1"),
                    trajectory: traj(),
                    provider_results: Vec::new(),
                    proposals: vec![raw(&call)],
                    spawn: None,
                    offer_nonce: nonce(),
                    evidence: Vec::new(),
                    audience: crate::audience::AudienceEvidence::default(),
                }),
            )
            .unwrap();
        let batch = decision.append.expect("the release appends").facts().to_vec();
        let log = [records.clone(), batch.clone()].concat();
        assert_eq!(e.validate_replay(&log), Ok(()));

        let interleaved = [
            records.clone(),
            vec![
                batch[0].clone(),
                batch[1].clone(),
                stray_admission(&traj(), known(SUSPICIOUS, Audience::public())),
                batch[2].clone(),
            ],
        ]
        .concat();
        assert_eq!(
            e.validate_replay(&interleaved),
            Err(TransitionRefusal::UnbackedDecision)
        );

        let tampered = |mutate: &dyn Fn(&mut Fact)| {
            let mut facts = batch.clone();
            mutate(&mut facts[2]);
            e.validate_replay(&[records.clone(), facts].concat())
        };
        assert_eq!(
            tampered(&|fact| {
                if let Fact::DispatchOpened { annotation, .. } = fact {
                    // A pin on a statically declared dispatch record cannot restate the
                    // decision that released it: the decided call carries none.
                    let ghost = ResolvedCall::new(
                        crate::value::ToolName::new("forged"),
                        crate::params::test_arguments(&json!({})),
                    );
                    *annotation = Some(pinned_for(plain_tool("forged"), "ghost", &ghost));
                }
            }),
            Err(TransitionRefusal::UnbackedDecision)
        );
        assert_eq!(
            tampered(&|fact| {
                if let Fact::DispatchOpened { proposed_effects, .. } = fact {
                    *proposed_effects = EffectSet::new([EffectKind::new("email.sent")]).unwrap();
                }
            }),
            Err(TransitionRefusal::EffectsMismatch)
        );
        assert_eq!(
            tampered(&|fact| {
                if let Fact::DispatchOpened { proposed_label, .. } = fact {
                    *proposed_label = Label::top();
                }
            }),
            Err(TransitionRefusal::ForgedLabel)
        );
    }

    #[test]
    fn a_crossing_replays_only_with_its_admission_and_its_merge() {
        let child = TrajectoryId::new("child");
        let internal = Audience::restricted([ReaderId::new("insider")]);
        let e = engine_at(vec![], known(SUSPICIOUS, internal.clone()));
        let mut log = vec![opened(&e)];
        log.extend(forked_child(&e, &log.clone(), &child));
        let projection = Projection::build(&log, log.len() as u64);
        let trajectory = traj();
        let crossing = e
            .submit_child_return(&projection.view(&trajectory), &child, ValueBody::new("the answer"))
            .expect("a non-narrowing crossing merges");
        let crossing = merged_crossing(crossing);
        let whole = [log.clone(), crossing.clone()].concat();
        assert_eq!(e.validate_replay(&whole), Ok(()));

        assert_eq!(
            e.validate_replay(&[log.clone(), vec![crossing[0].clone()]].concat()),
            Err(TransitionRefusal::UnmergedCrossing)
        );
        assert_eq!(
            e.validate_replay(&[log.clone(), vec![crossing[0].clone(), crossing[2].clone()]].concat()),
            Err(TransitionRefusal::RepeatAdmission)
        );
        let mut forged = whole.clone();
        if let Fact::ValueAdmitted { value, .. } = &mut forged[log.len() + 1] {
            *value = LabeledValue::new(ValueBody::new("something else"), value.label.clone());
        }
        assert_eq!(e.validate_replay(&forged), Err(TransitionRefusal::ForgedLabel));
    }

    #[test]
    fn a_result_replays_only_for_a_successful_close() {
        let internal = Audience::restricted([ReaderId::new("insider")]);
        let e = engine_at(vec![crm_tool()], known(TRUSTED, internal));
        let call = call("get_ticket", json!({}));
        let mut log = vec![opened(&e)];
        let dispatch = open(&e, &mut log, &call);
        let admitted = Fact::ValueAdmitted {
            trajectory: traj(),
            value: LabeledValue::new(
                ValueBody::new("ticket"),
                e.registry()
                    .tool(call.tool())
                    .unwrap()
                    .declared()
                    .expect("a static declaration")
                    .output_label(),
            ),
            provenance: Provenance::ToolResult {
                dispatch: dispatch.clone(),
            },
        };
        let closed = |outcome: crate::fact::CloseOutcome| Fact::DispatchClosed {
            trajectory: traj(),
            dispatch: dispatch.clone(),
            outcome,
        };
        assert_eq!(
            e.validate_replay(
                &[
                    log.clone(),
                    vec![
                        closed(crate::fact::CloseOutcome::Success {
                            effects: EffectSet::default()
                        }),
                        admitted.clone()
                    ]
                ]
                .concat()
            ),
            Ok(())
        );
        for refused in [
            crate::fact::CloseOutcome::Failure,
            crate::fact::CloseOutcome::Indeterminate,
        ] {
            assert_eq!(
                e.validate_replay(&[log.clone(), vec![closed(refused), admitted.clone()]].concat()),
                Err(TransitionRefusal::DispatchNotOpen)
            );
        }
    }

    #[test]
    fn an_ended_branch_releases_nothing_more() {
        let child = TrajectoryId::new("child");
        let internal = Audience::restricted([ReaderId::new("insider")]);
        let e = engine_at(vec![crm_tool()], known(TRUSTED, internal));
        let mut log = vec![opened(&e)];
        log.extend(forked_child(&e, &log.clone(), &child));
        let ended = branch::submit_void_return(&Projection::build(&log, log.len() as u64).view(&traj()), &child)
            .expect("the child ends with no value");
        log.extend(ended);
        let view = e.view(&traj(), log.clone(), log.len() as u64).unwrap();

        let call = call("get_ticket", json!({}));
        assert_eq!(
            e.handle(
                &view,
                EngineEvent::Proposals(ProposalBatch {
                    id: crate::transition::ProposalBatchId::new("b1"),
                    trajectory: child.clone(),
                    provider_results: Vec::new(),
                    proposals: vec![raw(&call)],
                    spawn: None,
                    offer_nonce: nonce(),
                    evidence: Vec::new(),
                    audience: crate::audience::AudienceEvidence::default(),
                })
            ),
            Err(crate::transition::TransitionError::BranchEnded)
        );
        let forged = [
            log,
            vec![Fact::DispatchOpened {
                trajectory: child.clone(),
                dispatch: DispatchId::new(child.clone(), call.digest(), 0),
                tool: call.tool().clone(),
                declaration: call.declaration_id(),
                arguments: call.canonical_arguments().clone(),
                proposed_label: Label::new(TRUSTED, Audience::restricted([ReaderId::new("insider")])),
                receiving: Label::new(TRUSTED, Audience::restricted([ReaderId::new("insider")])),
                proposed_effects: EffectSet::default(),
                annotation: None,
                subject: crate::basis::fixture_subject(&child),
                evidence: crate::audience::AudienceEvidence::default(),
            }],
        ]
        .concat();
        assert_eq!(e.validate_replay(&forged), Err(TransitionRefusal::BranchEnded));
    }

    #[test]
    fn an_admission_after_a_checkpoint_carries_the_bytes_it_observed() {
        let internal = Audience::restricted([ReaderId::new("insider")]);
        let e = engine_at(vec![crm_tool()], known(TRUSTED, internal));
        let call = call("get_ticket", json!({}));
        let mut log = vec![opened(&e)];
        let dispatch = open(&e, &mut log, &call);
        let body = ValueBody::new("the ticket");
        let checkpoint = admit::observe_success(
            &e.registry,
            &Projection::build(&log, log.len() as u64).view(&traj()),
            &dispatch,
            &call,
            crate::fact::ObservedResult::Available(crate::value::RawResultDigest::of(body.as_str().as_bytes())),
        )
        .expect("an open dispatch checkpoints");
        log.extend(checkpoint);
        let views = Projection::build(&log, log.len() as u64);

        assert_eq!(
            admit::admit_result(
                &e.registry,
                &views.view(&traj()),
                &dispatch,
                &call,
                crate::admit::ResultAdmission::SuccessRaw {
                    body: ValueBody::new("other bytes"),
                },
                &crate::label::TestContext::default().context(),
                &crate::audience::AudienceEvidence::default(),
            )
            .expect_err("other bytes are another observation"),
            crate::admit::AdmitError::ObservationMismatch
        );
        assert!(
            admit::admit_result(
                &e.registry,
                &views.view(&traj()),
                &dispatch,
                &call,
                crate::admit::ResultAdmission::SuccessRaw { body },
                &crate::label::TestContext::default().context(),
                &crate::audience::AudienceEvidence::default(),
            )
            .is_ok(),
            "the observed bytes admit"
        );
    }

    #[test]
    fn a_sanitizer_settlement_replays_only_where_the_deployment_confines_the_result() {
        let declassify = crate::authority::Sanitizer {
            name: crate::names::SanitizerName::new("declassify"),
            on: crate::authority::SanitizerPoints {
                input: false,
                output: true,
            },
            transition: crate::authority::DeclaredTransition::Audience {
                from_includes: DeclaredAudience::restricted([ReaderId::new("insider")]),
                to: DeclaredAudience::literal(Audience::public()),
            },
            scope: crate::authority::Scope::default(),
            hint: None,
        };
        let config = RegistryConfig {
            annotators: vec![],
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: declared(vec![crm_tool()]),
            authorities: vec![],
            sanitizers: vec![declassify],
            audience: crate::audience::AudienceConfig::default(),
        };
        let call = call("get_ticket", json!({}));

        let confining = open_engine(config.clone());
        let (log, _) = released_under_output_sanitizer(&confining, vec![opened(&confining)], &call);
        assert_eq!(confining.validate_replay(&log), Ok(()));

        let mut declaration = crate::profile::covering_declaration(&config);
        declaration.confined_results.clear();
        let unconfined = Engine::open(DeploymentPolicy {
            registry: config,
            planner_cap: crate::registry::PlannerCap::default(),
            dialect: PolicyDialectVersion::new(1),
            child_return: ReturnPolicy::Raw,
            profile: declaration,
        })
        .expect("an unconfined deployment opens");
        let forged = [vec![opened(&unconfined)], log[1..].to_vec()].concat();
        assert_eq!(
            unconfined.validate_replay(&forged),
            Err(TransitionRefusal::UnbackedOffer)
        );
    }

    fn reservation_tools() -> Vec<ToolAnnotation> {
        vec![
            emitting("send", "email.sent"),
            history_guarded("guard", HistoryRequirement::NoPrior(EffectKind::new("email.sent"))),
            history_guarded("wants", HistoryRequirement::Prior(EffectKind::new("email.sent"))),
        ]
    }

    #[test]
    fn an_open_dispatch_reserves_its_emits_for_no_prior_only() {
        let e = engine(reservation_tools());
        let mut log = vec![opened(&e)];
        assert_eq!(check(&e, &log, &call("guard", json!({}))), CheckOutcome::Allow);
        let send = call("send", json!({}));
        let dispatch = open(&e, &mut log, &send);
        match check(&e, &log, &call("guard", json!({}))) {
            CheckOutcome::Block(b) => {
                assert_eq!(b.requirement_gaps, vec![Gap::NoPrior(EffectKind::new("email.sent"))])
            }
            other => panic!("expected a reservation-failed no_prior, got {other:?}"),
        }
        match check(&e, &log, &call("wants", json!({}))) {
            CheckOutcome::Block(b) => {
                assert_eq!(b.requirement_gaps, vec![Gap::Prior(EffectKind::new("email.sent"))])
            }
            other => panic!("expected prior unfulfilled by a reservation, got {other:?}"),
        }
        close(
            &e,
            &mut log,
            &dispatch,
            &send,
            crate::admit::ResultAdmission::SuccessRaw {
                body: ValueBody::new("sent"),
            },
        );
        match check(&e, &log, &call("guard", json!({}))) {
            CheckOutcome::Block(b) => {
                assert_eq!(b.requirement_gaps, vec![Gap::NoPrior(EffectKind::new("email.sent"))])
            }
            other => panic!("expected a committed-effect no_prior failure, got {other:?}"),
        }
        assert_eq!(check(&e, &log, &call("wants", json!({}))), CheckOutcome::Allow);
    }

    #[test]
    fn a_failed_dispatch_evaporates_its_reservation() {
        let e = engine(reservation_tools());
        let mut log = vec![opened(&e)];
        let send = call("send", json!({}));
        let dispatch = open(&e, &mut log, &send);
        close(&e, &mut log, &dispatch, &send, crate::admit::ResultAdmission::Failure);
        assert_eq!(check(&e, &log, &call("guard", json!({}))), CheckOutcome::Allow);
        match check(&e, &log, &call("wants", json!({}))) {
            CheckOutcome::Block(b) => {
                assert_eq!(b.requirement_gaps, vec![Gap::Prior(EffectKind::new("email.sent"))])
            }
            other => panic!("expected prior still unmet, got {other:?}"),
        }
    }

    #[test]
    fn an_indeterminate_close_keeps_the_reservation() {
        let e = engine(reservation_tools());
        let mut log = vec![opened(&e)];
        let send = call("send", json!({}));
        let dispatch = open(&e, &mut log, &send);
        close(
            &e,
            &mut log,
            &dispatch,
            &send,
            crate::admit::ResultAdmission::Indeterminate,
        );
        let p = Projection::build(&log, log.len() as u64);
        assert!(!p.view(&traj()).is_open(&dispatch), "the dispatch is closed");
        match check(&e, &log, &call("guard", json!({}))) {
            CheckOutcome::Block(b) => {
                assert_eq!(b.requirement_gaps, vec![Gap::NoPrior(EffectKind::new("email.sent"))])
            }
            other => panic!("expected the reservation to outlive the close, got {other:?}"),
        }
        match check(&e, &log, &call("wants", json!({}))) {
            CheckOutcome::Block(b) => {
                assert_eq!(b.requirement_gaps, vec![Gap::Prior(EffectKind::new("email.sent"))])
            }
            other => panic!("expected prior unmet, got {other:?}"),
        }
    }

    #[test]
    fn two_reservations_of_one_kind_settle_independently() {
        let e = engine(reservation_tools());
        let mut log = vec![opened(&e)];
        let send = call("send", json!({}));
        let first = open(&e, &mut log, &send);
        let second = open(&e, &mut log, &send);
        assert_ne!(first, second, "a repeat call is a new dispatch occurrence");
        close(&e, &mut log, &first, &send, crate::admit::ResultAdmission::Failure);
        match check(&e, &log, &call("guard", json!({}))) {
            CheckOutcome::Block(b) => {
                assert_eq!(b.requirement_gaps, vec![Gap::NoPrior(EffectKind::new("email.sent"))])
            }
            other => panic!("expected the second reservation to hold, got {other:?}"),
        }
        close(&e, &mut log, &second, &send, crate::admit::ResultAdmission::Failure);
        assert_eq!(check(&e, &log, &call("guard", json!({}))), CheckOutcome::Allow);
    }

    #[test]
    fn a_calls_own_emits_never_fail_its_own_check() {
        let selfguard = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("selfguard"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new([EffectKind::new("email.sent")]).unwrap(),
            requires: Requires {
                history: vec![HistoryRequirement::NoPrior(EffectKind::new("email.sent"))],
                ..Requires::default()
            },
        };
        let e = engine(vec![selfguard]);
        let mut log = vec![opened(&e)];
        let c = call("selfguard", json!({}));
        assert_eq!(check(&e, &log, &c), CheckOutcome::Allow);
        let _dispatch = open(&e, &mut log, &c);
        match check(&e, &log, &c) {
            CheckOutcome::Block(b) => {
                assert_eq!(b.requirement_gaps, vec![Gap::NoPrior(EffectKind::new("email.sent"))])
            }
            other => panic!("expected the open dispatch to reserve, got {other:?}"),
        }
    }

    #[test]
    fn a_success_checkpoint_settles_while_the_dispatch_stays_open() {
        let scan = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("scan"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new([EffectKind::new("read")]).unwrap(),
            requires: Requires::default(),
        };
        let tools = vec![
            scan,
            history_guarded("guard_read", HistoryRequirement::NoPrior(EffectKind::new("read"))),
            history_guarded("wants_read", HistoryRequirement::Prior(EffectKind::new("read"))),
        ];
        let e = engine(tools);
        let mut log = vec![opened(&e)];
        let scan_call = call("scan", json!({}));
        let dispatch = open(&e, &mut log, &scan_call);
        assert!(matches!(
            check(&e, &log, &call("guard_read", json!({}))),
            CheckOutcome::Block(_)
        ));
        assert!(matches!(
            check(&e, &log, &call("wants_read", json!({}))),
            CheckOutcome::Block(_)
        ));
        let p = Projection::build(&log, log.len() as u64);
        let batch = admit::observe_success(
            &e.registry,
            &p.view(&traj()),
            &dispatch,
            &scan_call,
            ObservedResult::Unavailable,
        )
        .unwrap();
        log.extend(batch);
        let p = Projection::build(&log, log.len() as u64);
        assert!(p.view(&traj()).is_open(&dispatch));
        assert_eq!(check(&e, &log, &call("wants_read", json!({}))), CheckOutcome::Allow);
        assert!(matches!(
            check(&e, &log, &call("guard_read", json!({}))),
            CheckOutcome::Block(_)
        ));
    }

    #[test]
    fn attention_is_always_a_gap() {
        let tool = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("wire"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                attention: vec![MarkName::new("signoff")],
                ..Requires::default()
            },
        };
        let e = engine(vec![tool]);
        let log = vec![opened(&e)];
        match check(&e, &log, &call("wire", json!({}))) {
            CheckOutcome::Block(b) => {
                assert!(b.requirement_gaps.contains(&Gap::Attention(MarkName::new("signoff"))))
            }
            other => panic!("expected attention gap, got {other:?}"),
        }
    }

    #[test]
    fn replay_holds_a_fork_to_its_parents_frozen_basis() {
        let e = engine(vec![plain_tool("spawn"), suspicious_read()]);
        let child = TrajectoryId::new("child");
        let mut base = vec![opened(&e)];
        reads(&e, &mut base, &traj(), "read_suspicious");
        let basis_after = |log: &[Fact]| Projection::build(log, log.len() as u64).view(&traj()).freeze_basis();

        let seeded = forked_child(&e, &base, &child);
        let log = [base.clone(), seeded.clone()].concat();
        assert_eq!(e.validate_replay(&log), Ok(()));

        let mut dropped = log.clone();
        let snapshot = dropped
            .iter_mut()
            .rev()
            .find_map(|fact| match fact {
                Fact::ForkPrepared { snapshot, .. } => Some(snapshot),
                _ => None,
            })
            .expect("the release prepared a fork");
        *snapshot = basis_after(&[opened(&e)]);
        assert_eq!(e.validate_replay(&dropped), Err(TransitionRefusal::ForkBasisMismatch));

        let fork = seeded
            .iter()
            .find_map(|fact| match fact {
                Fact::ForkPrepared { fork, .. } => Some(fork.clone()),
                _ => None,
            })
            .expect("the release prepared a fork");
        let prepared: Vec<Fact> = seeded
            .iter()
            .take_while(|fact| !matches!(fact, Fact::ForkOpened { .. }))
            .cloned()
            .collect();
        let self_bind = [
            base.clone(),
            prepared,
            vec![Fact::ForkOpened {
                trajectory: traj(),
                fork,
            }],
        ]
        .concat();
        assert_eq!(
            e.validate_replay(&self_bind),
            Err(TransitionRefusal::ChildActiveBeforeFork)
        );
    }

    #[test]
    fn a_value_admitted_under_an_unopened_dispatch_is_refused_on_replay() {
        let e = engine(vec![suspicious_read()]);
        let read_call = crate::value::ResolvedCall::new(
            ToolName::new("read_suspicious"),
            crate::params::test_arguments(&serde_json::json!({})),
        );
        assert_eq!(
            e.validate_replay(&[
                opened(&e),
                Fact::ValueAdmitted {
                    trajectory: traj(),
                    value: crate::value::LabeledValue::new(
                        crate::value::ValueBody::new("page"),
                        Label::new(SUSPICIOUS, Audience::public()),
                    ),
                    provenance: crate::value::Provenance::ToolResult {
                        dispatch: DispatchId::new(traj(), read_call.digest(), 7),
                    },
                }
            ]),
            Err(TransitionRefusal::UnknownDispatch)
        );
    }

    /// Two-tier selection on the proposal path: a name the policy writes decides under its
    /// exact declaration and never falls to the wildcard; only a name it does not write owes
    /// the wildcard annotator's annotation.
    #[test]
    fn an_exact_declaration_beats_the_wildcard_on_a_proposal() {
        let mut cfg = test_config(vec![plain_tool("read")]);
        cfg.tools.push(wildcard("any"));
        cfg.annotators.push(annotator("any"));
        let e = open_engine(cfg);
        let log = vec![opened(&e)];
        let exact = proposed(&e, &log, "b1", nonce(), call("read", json!({}))).expect("the static declaration decides");
        assert!(
            matches!(&exact.follow_up, FollowUp::Proposals { released, .. } if !released.is_empty()),
            "the exact declaration releases without an annotation consult: {:?}",
            exact.follow_up
        );
        assert_eq!(
            proposed(&e, &log, "b2", nonce(), call("ghost", json!({}))).err(),
            Some(TransitionError::AnnotationNeeded {
                annotators: vec![crate::names::AnnotatorName::new("any")]
            })
        );
    }

    #[test]
    fn includes_missing_placeholder_is_an_invalid_call_and_still_fails_closed_underneath() {
        let send = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("send_email"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::test_string_argument_schema("to"),
            emits: EffectSet::default(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Includes(RecipientSpec::Placeholder("to".into()))],
                },
                ..Requires::default()
            },
        };
        let e = engine(vec![send]);
        let malformed = call("send_email", json!({}));
        let log = vec![opened(&e)];
        let p = Projection::build(&log, log.len() as u64);
        assert!(matches!(
            e.check(&p.view(&traj()), &malformed),
            Err(EngineError::InvalidCall(_))
        ));

        let contract = e
            .registry
            .tool(&ToolName::new("send_email"))
            .unwrap()
            .declared()
            .expect("a static declaration");
        let evaluate = |log: &[Fact]| {
            let p = Projection::build(log, log.len() as u64);
            let parts = crate::label::TestContext::default();
            crate::check::evaluate(
                contract,
                &p.view(&traj()),
                &malformed,
                &CallStage::default(),
                &parts.context(),
            )
        };
        match evaluate(&log) {
            Ok(CheckOutcome::Block(b)) => assert!(matches!(b.requirement_gaps.as_slice(), [Gap::Includes { .. }])),
            other => panic!("expected includes gap on a malformed call, got {other:?}"),
        }
    }

    #[test]
    fn required_rulings_route_each_gap_to_its_authority() {
        use crate::authority::{Authority, Mandate};
        use crate::names::AuthorityName;

        let wire = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("wire"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(TRUSTED),
                    audience: vec![],
                },
                ..Requires::default()
            },
        };
        let officer = Authority {
            name: AuthorityName::new("officer"),
            mandate: Mandate {
                trust_ceiling: Some(TRUSTED),
                ..Mandate::default()
            },
            scope: crate::authority::Scope::default(),
            hint: None,
        };
        let cfg = RegistryConfig {
            annotators: vec![],
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: declared(vec![wire]),
            authorities: vec![officer],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
        };
        let e = open_engine_at(cfg, known(SUSPICIOUS, Audience::public()));
        let log = vec![opened(&e)];
        let p = Projection::build(&log, log.len() as u64);
        let t = traj();
        let wire_call = call("wire", json!({}));
        let raw = match e.check(&p.view(&t), &wire_call).unwrap() {
            CheckOutcome::Block(raw) => raw,
            other => panic!("expected a block, got {other:?}"),
        };
        let planned = e.plan(&p.view(&t), &wire_call, &raw).unwrap();
        assert_eq!(planned.plans.len(), 1);
        let required = &planned.plans[0].executable().expect("an authority plan").required;
        assert_eq!(required.len(), 1);
        assert_eq!(required[0].authority, AuthorityName::new("officer"));
        assert_eq!(
            required[0].covers,
            vec![Gap::TrustFloor {
                required: TRUSTED,
                actual: SUSPICIOUS,
            }]
        );
    }

    fn strict_tool(name: &str) -> ToolAnnotation {
        ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new(name),
            tags: vec![],
            parameters: crate::params::ToolParameters::compile(&json!({
                "type": "object",
                "properties": { "to": { "type": "string" } },
                "required": ["to"],
            }))
            .unwrap(),
            delta: Delta::NONE,
            emits: EffectSet::default(),
            requires: Requires::default(),
        }
    }

    #[test]
    fn schema_invalid_arguments_are_an_invalid_call_at_every_fresh_entry_point() {
        let e = engine(vec![strict_tool("send")]);
        let log = vec![opened(&e)];
        let p = Projection::build(&log, 1);
        let t = traj();
        let views = p.view(&t);
        let bogus = call("send", json!({ "bogus": 1 }));
        assert!(matches!(e.check(&views, &bogus), Err(EngineError::InvalidCall(_))));
        let raw = crate::check::RawBlock {
            requirement_gaps: vec![],
            narrowing: None,
        };
        assert!(matches!(e.plan(&views, &bogus, &raw), Err(EngineError::InvalidCall(_))));
        assert_eq!(
            e.check(&views, &call("send", json!({ "to": "hr" }))).unwrap(),
            CheckOutcome::Allow
        );
    }

    #[test]
    fn resolve_call_owns_tool_lookup_scanning_and_schema_binding() {
        let e = engine(vec![strict_tool("send")]);

        let resolved = e
            .resolve_call(ToolName::new("send"), br#"{ "to": "hr" }"#)
            .expect("the registered schema accepts the call");
        assert_eq!(resolved.canonical_arguments().canonical_text(), r#"{"to":"hr"}"#);

        assert!(matches!(
            e.resolve_call(ToolName::new("send"), br#"{"to":"hr","to":"finance"}"#),
            Err(EngineError::InvalidCall(ArgumentError::DuplicateKey(key))) if key == "to"
        ));
        assert!(matches!(
            e.resolve_call(ToolName::new("send"), br#"{"bogus":true}"#),
            Err(EngineError::InvalidCall(ArgumentError::Schema(_)))
        ));
        // A name no declaration and no wildcard covers has no contract: the refusal is typed.
        assert!(matches!(
            e.resolve_call(ToolName::new("ghost"), br#"{}"#),
            Err(EngineError::UnknownTool(name)) if name == "ghost"
        ));
        // With a wildcard, the same name resolves onto it at ordinal zero.
        let mut cfg = test_config(vec![strict_tool("send")]);
        cfg.tools.push(wildcard("any"));
        cfg.annotators.push(annotator("any"));
        let covered = open_engine(cfg);
        let ghost = covered
            .resolve_call(ToolName::new("ghost"), br#"{}"#)
            .expect("the wildcard covers the name");
        assert_eq!(ghost.declaration_id(), crate::value::ToolDeclarationId::default());
    }

    #[test]
    fn ordered_selectors_choose_once_before_schema_validation() {
        let mut first = plain_tool("read(path:secret*)");
        first.parameters = crate::params::ToolParameters::compile(&json!({
            "type": "object",
            "properties": { "path": { "type": "string" }, "token": { "type": "string" } },
            "required": ["path", "token"]
        }))
        .unwrap();
        let mut overlap = plain_tool("read(path:*)");
        overlap.parameters = crate::params::test_string_argument_schema("path");
        let fallback = plain_tool("read");
        let e = engine(vec![first, overlap, fallback]);

        let selected = e
            .resolve_call(ToolName::new("read"), br#"{"path":"secret.txt","token":"ok"}"#)
            .unwrap();
        assert_eq!(
            selected.declaration_id(),
            crate::value::ToolDeclarationId::new(0).unwrap()
        );
        assert!(
            matches!(
                e.resolve_call(ToolName::new("read"), br#"{"path":"secret.txt"}"#),
                Err(EngineError::InvalidCall(ArgumentError::Schema(_)))
            ),
            "the first match's schema failure must not fall through"
        );
        let overlap = e.resolve_call(ToolName::new("read"), br#"{"path":"public"}"#).unwrap();
        assert_eq!(
            overlap.declaration_id(),
            crate::value::ToolDeclarationId::new(1).unwrap()
        );
        let fallback = e.resolve_call(ToolName::new("read"), br#"{}"#).unwrap();
        assert_eq!(
            fallback.declaration_id(),
            crate::value::ToolDeclarationId::new(2).unwrap()
        );
        let no_fallback = engine(vec![plain_tool("read(path:secret*)")]);
        assert!(matches!(
            no_fallback.resolve_call(ToolName::new("read"), br#"{}"#),
            Err(EngineError::InvalidCall(ArgumentError::NoMatchingContract))
        ));
        assert!(matches!(
            e.resolve_call(ToolName::new("read"), br#"{"path":"x","path":"y"}"#),
            Err(EngineError::InvalidCall(ArgumentError::DuplicateKey(_)))
        ));
    }

    #[test]
    fn selector_order_and_matchers_move_policy_identity_but_normalized_wildcards_do_not() {
        let identity = |names: &[&str]| {
            engine(names.iter().map(|name| plain_tool(name)).collect())
                .identity()
                .bytes()
                .to_owned()
        };
        let base = identity(&["read(path:secret*)", "read"]);
        assert_ne!(identity(&["read", "read(path:secret*)"]), base);
        assert_ne!(identity(&["read(path:private*)", "read"]), base);
        assert_eq!(identity(&["read(path:secret**)", "read"]), base);
        assert_eq!(identity(&["read", "send"]), identity(&["send", "read"]));

        // A conjunction is commutative, so clause order is spelling, not policy: the two
        // spellings are one matcher and one identity. Naming another argument is not.
        let conjunction = identity(&["read(path:secret*,mode:rw)", "read"]);
        assert_eq!(identity(&["read(mode:rw,path:secret*)", "read"]), conjunction);
        assert_ne!(conjunction, base, "a second clause is a different predicate");
        assert_ne!(identity(&["read(path:secret*,mode:ro)", "read"]), conjunction);
        assert_ne!(identity(&["read(path:secret*,scope:rw)", "read"]), conjunction);
    }

    #[test]
    fn a_substitution_that_selects_another_ordered_contract_is_a_new_call_under_it() {
        let e = engine(vec![plain_tool("read(path:safe*)"), plain_tool("read(path:*)")]);
        let selected = e
            .resolve_call(ToolName::new("read"), br#"{"path":"private.txt"}"#)
            .unwrap();
        assert_eq!(
            selected.declaration_id(),
            crate::value::ToolDeclarationId::new(1).unwrap()
        );
        let rewrite = |body: &str| substituted_call(&e.registry, &selected, &ValueBody::new(body), None);

        // Arguments that select another declaration render a new call under it: the selected
        // ordinal, nothing carried.
        let fresh = rewrite(r#"{"path":"safe.txt"}"#).expect("a new call under declaration 0");
        assert_eq!(fresh.declaration_id(), crate::value::ToolDeclarationId::new(0).unwrap());
        assert!(fresh.annotation().is_none());
        assert!(
            !e.registry
                .selection_matches(&selected.substituting(fresh.canonical_arguments().clone()))
        );

        // Arguments that stay in the declaration render the substitution.
        let kept = rewrite(r#"{"path":"other-private.txt"}"#).expect("the substitution");
        assert_eq!(kept, selected.substituting(kept.canonical_arguments().clone()));

        // Arguments no declaration selects, or that fail the selected schema, mint nothing.
        assert!(matches!(
            rewrite(r#"{"path":7}"#),
            Err(TransitionError::Call(EngineError::InvalidCall(
                ArgumentError::NoMatchingContract
            )))
        ));
    }

    /// One harness tool under two ordered contracts, both in the input sanitizers' scope.
    /// `read(path:public/*)` at 0 uses an annotator that reads the complete call and owns the
    /// recipients it requires; `read(path:private/*)` at 1 records a classified read and
    /// requires `partner` and every desk in `desks` statically. `redact` widens the audience
    /// from internal to internal+partner, `widen` from partner to partner+auditor.
    fn ordered_read_engine(desks: &[&str]) -> Engine {
        ordered_read_engine_tagged(desks, "outbound")
    }

    /// [`ordered_read_engine`] with the private contract carrying `private_tag` instead of
    /// `outbound`.
    fn ordered_read_engine_tagged(desks: &[&str], private_tag: &str) -> Engine {
        let read = ordered_read;
        let includes = |reader: &str| {
            AudienceRequirement::Includes(RecipientSpec::Static(DeclaredAudience::literal(Audience::restricted(
                [ReaderId::new(reader)],
            ))))
        };
        let public = annotated(read("read(path:public/*)"), "classify");
        let private = ToolAnnotation {
            emits: classified_read(),
            tags: vec![crate::names::TagName::new(private_tag)],
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(TRUSTED),
                    audience: std::iter::once("partner")
                        .chain(desks.iter().copied())
                        .map(includes)
                        .collect(),
                },
                ..Requires::default()
            },
            ..read("read(path:private/*)")
        };
        open_engine_at(
            RegistryConfig {
                annotators: vec![annotator_with_readers(
                    "classify",
                    &["insider", "partner", "auditor", "legal", "press"],
                )],
                trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
                tools: {
                    let mut tools = vec![public];
                    tools.extend(declared(vec![private, plain_tool("note")]));
                    tools
                },
                authorities: vec![],
                sanitizers: vec![
                    input_sanitizer("redact", &["insider"], &["insider", "partner"]),
                    input_sanitizer("widen", &["partner"], &["insider", "partner", "auditor"]),
                ],
                audience: crate::audience::AudienceConfig::default(),
            },
            known(TRUSTED, Audience::restricted([ReaderId::new("insider")])),
        )
    }

    fn classified_read() -> EffectSet {
        EffectSet::new([EffectKind::new("classified.read")]).unwrap()
    }

    fn input_sanitizer(name: &str, from: &[&str], to: &[&str]) -> crate::authority::Sanitizer {
        let audience = |readers: &[&str]| {
            DeclaredAudience::literal(Audience::restricted(
                readers.iter().map(|reader| ReaderId::new(*reader)),
            ))
        };
        crate::authority::Sanitizer {
            name: crate::names::SanitizerName::new(name),
            on: crate::authority::SanitizerPoints {
                input: true,
                output: false,
            },
            transition: crate::authority::DeclaredTransition::Audience {
                from_includes: audience(from),
                to: audience(to),
            },
            scope: crate::authority::Scope {
                tags: vec![crate::names::TagName::new("outbound")],
            },
            hint: None,
        }
    }

    fn ordered_read(name: &str) -> ToolAnnotation {
        ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new(name),
            tags: vec![crate::names::TagName::new("outbound")],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::compile(&json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"],
                "additionalProperties": false,
            }))
            .unwrap(),
            emits: EffectSet::default(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(TRUSTED),
                    audience: vec![],
                },
                ..Requires::default()
            },
        }
    }

    /// The complete annotation `classify` produces for a `read` call under the public
    /// declaration: the declaration's operational metadata with the readers the call requires.
    /// It is evidence for exactly one canonical call: a rewrite is annotated afresh or not at all.
    fn read_pin(bound_to: &ResolvedCall, readers: &[&str]) -> crate::contract::PinnedAnnotation {
        let mut produced = ordered_read("read");
        produced.requires.label.audience = readers
            .iter()
            .map(|reader| {
                AudienceRequirement::Includes(RecipientSpec::Static(DeclaredAudience::literal(Audience::restricted(
                    [ReaderId::new(*reader)],
                ))))
            })
            .collect();
        pinned_for(produced, "classify", bound_to)
    }

    fn read_of(e: &Engine, path: &str) -> ResolvedCall {
        e.resolve_call(ToolName::new("read"), format!(r#"{{"path":"{path}"}}"#).as_bytes())
            .expect("the call resolves")
    }

    fn rewrite(
        call: &ResolvedCall,
        sanitizer: &str,
        replacement: &str,
        annotation: Option<crate::contract::PinnedAnnotation>,
    ) -> OfferOutcome {
        OfferOutcome::Derived(crate::transition::Evidence::Rewrite {
            sanitizer: crate::names::SanitizerName::new(sanitizer),
            source: crate::value::RawResultDigest::of(call.canonical_arguments().canonical_bytes()),
            derived: ValueBody::new(replacement),
            annotation,
        })
    }

    fn hop_named(facts: &[Fact], name: &str) -> crate::value::OfferId {
        opened_offers(facts)
            .into_iter()
            .find(|(_, plan)| plan.hop() == Some(&crate::names::SanitizerName::new(name)))
            .map(|(offer, _)| offer)
            .unwrap_or_else(|| panic!("the {name} hop is offered"))
    }

    fn released_by(decision: &EngineDecision) -> Released {
        match offer_answer(decision) {
            OfferFollowUp::Released(released) => (**released).clone(),
            other => panic!("the rewrite clears the last gap and dispatches, got {other:?}"),
        }
    }

    fn opened_contract_of(
        facts: &[Fact],
    ) -> (
        crate::value::ToolDeclarationId,
        EffectSet,
        Option<crate::contract::PinnedAnnotation>,
    ) {
        facts
            .iter()
            .find_map(|fact| match fact {
                Fact::DispatchOpened {
                    declaration,
                    proposed_effects,
                    annotation,
                    ..
                } => Some((*declaration, proposed_effects.clone(), annotation.clone())),
                _ => None,
            })
            .expect("the rewrite dispatches")
    }

    fn includes_gap(reader: &str) -> Gap {
        Gap::Includes {
            recipients: DeclaredAudience::restricted([ReaderId::new(reader)]),
        }
    }

    #[test]
    fn a_rewrite_that_selects_another_contract_is_judged_under_it_with_the_answers_consulted_for_it() {
        let e = ordered_read_engine(&["auditor", "legal"]);
        let proposal = read_of(&e, "private/q3.md");
        assert_eq!(
            proposal.declaration_id(),
            crate::value::ToolDeclarationId::new(1).unwrap()
        );
        let log = vec![opened(&e)];
        let facts = appended_facts(proposed(&e, &log, "b1", nonce(), proposal.clone()).expect("the batch decides"));
        let hop = hop_named(&facts, "redact");
        let log = [log, facts].concat();
        let public = r#"{"path":"public/q3.md"}"#;

        // The rewritten arguments select the public declaration, whose annotator was never
        // asked: the rewrite is a new call under it, and its annotation is owed first.
        assert_eq!(
            execute_offer(&e, &log, hop, rewrite(&proposal, "redact", public, None)).err(),
            Some(TransitionError::AnnotationNeeded {
                annotators: vec![crate::names::AnnotatorName::new("classify")]
            })
        );
        let answer = read_pin(&read_of(&e, "public/q3.md"), &["partner"]);
        let hopped = execute_offer(
            &e,
            &log,
            hop,
            rewrite(&proposal, "redact", public, Some(answer.clone())),
        )
        .expect("the hop runs");
        let released = released_by(&hopped);
        assert_eq!(
            released.call.declaration_id(),
            crate::value::ToolDeclarationId::new(0).unwrap()
        );
        assert_eq!(released.call.annotation(), Some(&answer));
        let facts = appended_facts(hopped);
        assert_eq!(
            opened_contract_of(&facts),
            (
                crate::value::ToolDeclarationId::new(0).unwrap(),
                EffectSet::default(),
                released.call.annotation().cloned()
            ),
            "the opening records the public declaration: its effects, not the classified read's, and its annotation"
        );
        let log = [log, facts].concat();
        assert_eq!(e.validate_replay(&log), Ok(()));

        // Replay holds the record to the same rule: the persisted declaration is the one the
        // arguments select, and the tool is the one the sanitizer rewrote — another tool's open
        // declaration is no place to land.
        let derived_at = log
            .iter()
            .position(|fact| matches!(fact, Fact::CandidateDerived { .. }))
            .expect("the hop derived a candidate");
        for (forged_call, refusal) in [
            (
                ResolvedCall::new_keyed(
                    ToolName::new("note"),
                    crate::value::ToolDeclarationId::new(0).unwrap(),
                    released.call.canonical_arguments().clone(),
                ),
                TransitionRefusal::ForgedLabel,
            ),
            (
                ResolvedCall::new_keyed(
                    ToolName::new("read"),
                    crate::value::ToolDeclarationId::new(1).unwrap(),
                    released.call.canonical_arguments().clone(),
                ),
                TransitionRefusal::SanitizerUnapplicable,
            ),
        ] {
            let mut forged = log.clone();
            let Fact::CandidateDerived {
                derived: DerivedCandidate::Call { call, .. },
                ..
            } = &mut forged[derived_at]
            else {
                unreachable!("found above")
            };
            *call = forged_call;
            assert_eq!(e.validate_replay(&forged), Err(refusal));
        }
    }

    #[test]
    fn a_rewrite_into_the_classified_declaration_records_its_effect_and_carries_no_annotation() {
        let e = ordered_read_engine(&[]);
        let unpinned = read_of(&e, "public/q3.md");
        let proposal = unpinned
            .clone()
            .with_annotation(Some(read_pin(&unpinned, &["partner"])));
        assert_eq!(
            proposal.declaration_id(),
            crate::value::ToolDeclarationId::new(0).unwrap()
        );
        let log = vec![opened(&e)];
        let facts = appended_facts(proposed(&e, &log, "b1", nonce(), proposal.clone()).expect("the batch decides"));
        let hop = hop_named(&facts, "redact");
        let log = [log, facts].concat();

        let hopped = execute_offer(
            &e,
            &log,
            hop,
            rewrite(&proposal, "redact", r#"{"path":"private/q3.md"}"#, None),
        )
        .expect("the hop runs");
        let released = released_by(&hopped);
        assert_eq!(
            released.call.declaration_id(),
            crate::value::ToolDeclarationId::new(1).unwrap()
        );
        assert!(
            released.call.annotation().is_none(),
            "the classified declaration is statically declared; the public annotation does not ride along"
        );
        let facts = appended_facts(hopped);
        assert_eq!(
            opened_contract_of(&facts),
            (
                crate::value::ToolDeclarationId::new(1).unwrap(),
                classified_read(),
                None
            ),
            "the opening records the classified read the selected declaration emits"
        );
        assert_eq!(e.validate_replay(&[log.clone(), facts].concat()), Ok(()));

        // A rewrite that stays in the annotated declaration is annotated afresh or not at all:
        // the proposal's annotation never rides through, and none means the rewrite still owes one.
        let public = r#"{"path":"public/q4.md"}"#;
        assert_eq!(
            execute_offer(&e, &log, hop, rewrite(&proposal, "redact", public, None)).err(),
            Some(TransitionError::AnnotationNeeded {
                annotators: vec![crate::names::AnnotatorName::new("classify")]
            })
        );
        let fresh = read_pin(&read_of(&e, "public/q4.md"), &["partner"]);
        let kept = released_by(
            &execute_offer(&e, &log, hop, rewrite(&proposal, "redact", public, Some(fresh.clone())))
                .expect("the hop runs"),
        );
        assert_eq!(
            kept.call.declaration_id(),
            crate::value::ToolDeclarationId::new(0).unwrap()
        );
        assert_eq!(kept.call.annotation(), Some(&fresh));
    }

    #[test]
    fn a_rewrite_into_a_contract_the_sanitizer_does_not_reach_is_refused() {
        let e = ordered_read_engine_tagged(&[], "classified");
        let unpinned = read_of(&e, "public/q3.md");
        let proposal = unpinned
            .clone()
            .with_annotation(Some(read_pin(&unpinned, &["partner"])));
        let log = vec![opened(&e)];
        let facts = appended_facts(proposed(&e, &log, "b1", nonce(), proposal.clone()).expect("the batch decides"));
        let hop = hop_named(&facts, "redact");
        let log = [log, facts].concat();

        // `redact` reaches `outbound` contracts. The public contract is one; the private contract
        // the rewritten arguments select is tagged to keep sanitizers off it, so the rewrite is
        // refused even though the private contract's own requirements would be met.
        assert_eq!(
            execute_offer(
                &e,
                &log,
                hop,
                rewrite(&proposal, "redact", r#"{"path":"private/q3.md"}"#, None),
            )
            .err(),
            Some(TransitionError::SanitizerUnapplicable)
        );
        assert_eq!(e.validate_replay(&log), Ok(()));
    }

    #[test]
    fn a_rewrite_within_the_declaration_is_annotated_afresh() {
        let e = ordered_read_engine(&["auditor", "legal"]);
        let proposal = read_of(&e, "private/q3.md");
        let log = vec![opened(&e)];
        let facts = appended_facts(proposed(&e, &log, "b1", nonce(), proposal.clone()).expect("the batch decides"));
        let redact = hop_named(&facts, "redact");
        let log = [log, facts].concat();

        // The first hop selects the public declaration; its annotation requires the auditor,
        // which the redaction does not reach, so the rewritten call blocks on that one gap — the
        // public declaration's, not the classified read's partner, auditor and legal desks.
        let answer = read_pin(&read_of(&e, "public/q3.md"), &["auditor"]);
        let hopped = execute_offer(
            &e,
            &log,
            redact,
            rewrite(&proposal, "redact", r#"{"path":"public/q3.md"}"#, Some(answer.clone())),
        )
        .expect("the hop runs");
        let block = match offer_answer(&hopped) {
            OfferFollowUp::Substituted { block } => (**block).clone(),
            other => panic!("the substitution re-plans over the derived call, got {other:?}"),
        };
        assert_eq!(
            block.call.declaration_id(),
            crate::value::ToolDeclarationId::new(0).unwrap()
        );
        assert_eq!(block.call.annotation(), Some(&answer));
        assert_eq!(block.block.raw.requirement_gaps, vec![includes_gap("auditor")]);
        let facts = appended_facts(hopped);
        let widen = hop_named(&facts, "widen");
        let log = [log, facts].concat();

        // Deciding the batch again reads the candidate under the contract it selected.
        match proposed(&e, &log, "b1", nonce(), proposal.clone())
            .expect("the batch answers from the record")
            .follow_up
        {
            FollowUp::Proposals { blocked, .. } => {
                assert_eq!(blocked[0].call, block.call);
                assert_eq!(blocked[0].block.raw.requirement_gaps, vec![includes_gap("auditor")]);
            }
            other => panic!("a repeated batch answers as proposals, got {other:?}"),
        }

        // The second hop stays in the public declaration: annotation evidence binds the exact
        // canonical call, so the rewrite carries the fresh annotation obtained for it.
        let widened = read_pin(&read_of(&e, "public/q3-v2.md"), &["auditor"]);
        let hopped = execute_offer(
            &e,
            &log,
            widen,
            rewrite(
                &block.call,
                "widen",
                r#"{"path":"public/q3-v2.md"}"#,
                Some(widened.clone()),
            ),
        )
        .expect("the hop runs");
        let released = released_by(&hopped);
        assert_eq!(
            released.call.declaration_id(),
            crate::value::ToolDeclarationId::new(0).unwrap()
        );
        assert_eq!(released.call.annotation(), Some(&widened));
        let log = [log, appended_facts(hopped)].concat();
        assert_eq!(e.validate_replay(&log), Ok(()));
    }

    #[test]
    fn a_rewrite_that_selects_a_contract_with_other_gaps_is_no_remedy_for_this_block() {
        let e = ordered_read_engine(&["auditor"]);
        let proposal = read_of(&e, "private/q3.md");
        let log = vec![opened(&e)];
        let facts = appended_facts(proposed(&e, &log, "b1", nonce(), proposal.clone()).expect("the batch decides"));
        let hop = hop_named(&facts, "redact");
        let log = [log, facts].concat();
        let public = r#"{"path":"public/q3.md"}"#;

        // The block wants partner and auditor. A rewritten call that wants the press desk, or the
        // auditor and legal desks together, is blocked on something this block never was: the hop
        // improves nothing, lands no record, and the offer stands.
        for readers in [&["press"][..], &["auditor", "legal"][..]] {
            let answer = read_pin(&read_of(&e, "public/q3.md"), readers);
            assert_eq!(
                execute_offer(&e, &log, hop, rewrite(&proposal, "redact", public, Some(answer))).err(),
                Some(TransitionError::SanitizerUnapplicable)
            );
        }
        // A replacement the selected contract's schema refuses mints no call.
        assert!(matches!(
            execute_offer(
                &e,
                &log,
                hop,
                rewrite(&proposal, "redact", r#"{"path":"public/q3.md","extra":1}"#, None,),
            ),
            Err(TransitionError::Call(EngineError::InvalidCall(_)))
        ));
        assert_eq!(e.validate_replay(&log), Ok(()));
    }

    #[test]
    fn a_rewrite_into_a_contract_reading_a_group_asks_for_its_answer_afresh() {
        let send = |name: &str, audience: AudienceRequirement| ToolAnnotation {
            parameters: crate::params::test_string_argument_schema("to"),
            tags: vec![crate::names::TagName::new("outbound")],
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![audience],
                },
                ..Requires::default()
            },
            ..plain_tool(name)
        };
        let e = open_engine_at(
            RegistryConfig {
                annotators: vec![],
                trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
                tools: declared(vec![
                    send(
                        "send(to:@*)",
                        AudienceRequirement::Includes(RecipientSpec::Placeholder("to".into())),
                    ),
                    send(
                        "send",
                        AudienceRequirement::Includes(RecipientSpec::Static(DeclaredAudience::literal(
                            Audience::restricted([ReaderId::new("partner")]),
                        ))),
                    ),
                ]),
                authorities: vec![],
                sanitizers: vec![input_sanitizer(
                    "redact",
                    &["insider"],
                    &["insider", "partner", "partner@corp.com"],
                )],
                audience: slack_groups(&["team"]),
            },
            known(TRUSTED, Audience::restricted([ReaderId::new("insider")])),
        );
        let proposal = e
            .resolve_call(ToolName::new("send"), br#"{"to":"partner-desk"}"#)
            .expect("the call resolves");
        assert_eq!(
            proposal.declaration_id(),
            crate::value::ToolDeclarationId::new(1).unwrap()
        );
        let log = vec![opened(&e)];
        let facts = appended_facts(proposed(&e, &log, "b1", nonce(), proposal.clone()).expect("the batch decides"));
        let hop = hop_named(&facts, "redact");
        let log = [log, facts].concat();
        let group = r#"{"to":"@team"}"#;

        // The rewritten argument names a group under the first contract: this act owes its answer,
        // whatever any earlier act pinned.
        assert_eq!(
            execute_offer(&e, &log, hop, rewrite(&proposal, "redact", group, None)).err(),
            Some(TransitionError::MembershipNeeded {
                needed: vec![group_atom("team")]
            })
        );
        let answer = source_evidence(vec![user_group(
            "team",
            vec![slack_member("slack:UP", Some("partner@corp.com"))],
        )]);
        let hopped = execute_offer_with(&e, &log, hop, rewrite(&proposal, "redact", group, None), answer.clone())
            .expect("the hop runs");
        let released = released_by(&hopped);
        assert_eq!(
            released.call.declaration_id(),
            crate::value::ToolDeclarationId::new(0).unwrap()
        );
        let facts = appended_facts(hopped);
        assert!(facts.iter().any(|fact| matches!(
            fact,
            Fact::DispatchOpened { evidence, .. } if evidence == &answer
        )));
        assert_eq!(e.validate_replay(&[log, facts].concat()), Ok(()));
    }

    #[test]
    fn replay_refuses_a_later_contract_for_arguments_matching_an_earlier_one() {
        let e = engine(vec![plain_tool("read(path:*)"), plain_tool("read")]);
        let call = e
            .resolve_call(ToolName::new("read"), br#"{"path":"secret.txt"}"#)
            .unwrap();
        let opening = vec![opened(&e)];
        let mut facts = appended_facts(proposed(&e, &opening, "b1", nonce(), call).expect("the call releases"));
        assert_eq!(e.validate_replay(&[opening.clone(), facts.clone()].concat()), Ok(()));

        let later = crate::value::ToolDeclarationId::new(1).unwrap();
        for fact in &mut facts {
            match fact {
                Fact::ProposalBatchDecided { proposals, .. } => {
                    let original = &proposals[0];
                    proposals[0] =
                        ResolvedCall::new_keyed(original.tool().clone(), later, original.canonical_arguments().clone());
                }
                Fact::DispatchOpened { declaration, .. } => *declaration = later,
                _ => {}
            }
        }
        assert_eq!(
            e.validate_replay(&[opening, facts].concat()),
            Err(TransitionRefusal::MisdecidedBatch)
        );
    }

    #[test]
    fn replay_refuses_a_corrupt_dispatched_call() {
        let e = engine(vec![strict_tool("send")]);
        let mut log = vec![opened(&e)];
        let good = call("send", json!({ "to": "hr" }));
        open(&e, &mut log, &good);
        assert_eq!(e.validate_replay(&log), Ok(()));

        let dispatched = |tool: &str, payload: serde_json::Value, minted_from: &ResolvedCall| {
            vec![
                opened(&e),
                Fact::DispatchOpened {
                    trajectory: traj(),
                    dispatch: DispatchId::new(traj(), minted_from.digest(), 0),
                    tool: ToolName::new(tool),
                    declaration: Default::default(),
                    arguments: crate::params::test_arguments(&payload),
                    proposed_label: established(TRUSTED, Audience::public()),
                    receiving: established(TRUSTED, Audience::public()),
                    proposed_effects: EffectSet::default(),
                    annotation: None,
                    subject: crate::basis::fixture_subject(&traj()),
                    evidence: crate::audience::AudienceEvidence::default(),
                },
            ]
        };
        // A record naming a tool nothing covers is refused by name, before any backing check.
        let ghost_call = call("ghost", json!({}));
        assert_eq!(
            e.validate_replay(&dispatched("ghost", json!({}), &ghost_call)),
            Err(TransitionRefusal::UnknownTool("ghost".to_string()))
        );
        // A dispatch no decision backs is refused as such.
        let undecided = call("send", json!({ "to": "hr" }));
        assert_eq!(
            e.validate_replay(&dispatched("send", json!({ "to": "hr" }), &undecided)),
            Err(TransitionRefusal::UnbackedDecision)
        );
        let smuggled = call("send", json!({ "bogus": 1 }));
        assert!(matches!(
            e.validate_replay(&dispatched("send", json!({ "bogus": 1 }), &smuggled)),
            Err(TransitionRefusal::InvalidPayload(_))
        ));
        assert!(matches!(
            e.validate_replay(&dispatched("send", json!({ "to": "hr" }), &smuggled)),
            Err(TransitionRefusal::DigestMismatch)
        ));
        let mut forged_contract = dispatched("send", json!({ "to": "hr" }), &good);
        let Fact::DispatchOpened { declaration, .. } = &mut forged_contract[1] else {
            unreachable!("the fixture opens one dispatch")
        };
        *declaration = crate::value::ToolDeclarationId::new(99).unwrap();
        assert!(matches!(
            e.validate_replay(&forged_contract),
            Err(TransitionRefusal::UnknownTool(name)) if name == "send"
        ));
    }

    #[test]
    fn the_ruling_binds_the_payload_by_digest_and_never_copies_it() {
        use crate::authority::{Authority, Mandate};
        use crate::names::AuthorityName;
        let mut wire = strict_tool("wire");
        wire.requires.label.trust_floor = Some(TRUSTED);
        let officer = Authority {
            name: AuthorityName::new("officer"),
            mandate: Mandate {
                trust_ceiling: Some(TRUSTED),
                ..Mandate::default()
            },
            scope: crate::authority::Scope::default(),
            hint: None,
        };
        let cfg = RegistryConfig {
            annotators: vec![],
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: declared(vec![wire]),
            authorities: vec![officer],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
        };
        let e = open_engine_at(cfg, known(SUSPICIOUS, Audience::public()));

        let wire_call = call("wire", json!({ "to": "distinctive-recipient-hr" }));
        let log = vec![opened(&e)];
        let blocked = appended_facts(proposed(&e, &log, "b1", nonce(), wire_call.clone()).expect("the call blocks"));
        let (offer, plan) = opened_offers(&blocked)
            .into_iter()
            .find(|(_, plan)| !plan.required.is_empty())
            .expect("the block offers an authority plan");
        let log = [log, blocked].concat();

        let evidence = evidence_for(offer, &plan, "wire", partial(SUSPICIOUS, Audience::public()));
        let approved = appended_facts(
            execute_offer(&e, &log, offer, OfferOutcome::Approved(evidence)).expect("the offer executes"),
        );
        let log = [log, approved].concat();

        let batch =
            appended_facts(proposed(&e, &log, "b2", nonce(), wire_call.clone()).expect("the approval releases"));
        let serialized = serde_json::to_string(&batch).unwrap();
        let carriers: Vec<&Fact> = batch
            .iter()
            .filter(|fact| {
                serde_json::to_string(fact)
                    .unwrap()
                    .contains("distinctive-recipient-hr")
            })
            .collect();
        assert!(
            matches!(
                carriers.as_slice(),
                [Fact::ProposalBatchDecided { .. }, Fact::DispatchOpened { .. }]
            ),
            "the payload lands on the proposal record and the dispatch, not {carriers:?}"
        );
        assert!(matches!(batch.last().unwrap(), Fact::DispatchOpened { .. }));
        let restored: Vec<Fact> = serde_json::from_str(&serialized).unwrap();
        assert_eq!(restored, batch);
    }

    #[test]
    fn an_opening_records_the_label_the_call_was_proposed_at() {
        let internal = Audience::restricted([ReaderId::new("insider")]);
        let e = engine_at(vec![crm_tool()], known(TRUSTED, internal.clone()));
        let mut log = vec![opened(&e)];
        open(&e, &mut log, &call("get_ticket", json!({})));
        match log.last().expect("the release opens a dispatch") {
            Fact::DispatchOpened { proposed_label, .. } => {
                assert_eq!(*proposed_label, established(TRUSTED, internal));
            }
            other => panic!("expected DispatchOpened, got {other:?}"),
        }
    }

    fn plain_tool(name: &str) -> ToolAnnotation {
        ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new(name),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires::default(),
        }
    }

    fn engine_with_provider_run(tools: Vec<ToolAnnotation>, provider_run: &[&str]) -> Engine {
        provider_run_engine(
            RegistryConfig {
                annotators: vec![],
                trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
                tools: declared(tools),
                authorities: vec![],
                sanitizers: vec![],
                audience: crate::audience::AudienceConfig::default(),
            },
            provider_run,
        )
    }

    fn provider_run_engine(cfg: RegistryConfig, provider_run: &[&str]) -> Engine {
        let mut declaration = crate::profile::covering_declaration(&cfg);
        for name in provider_run {
            declaration
                .executor_exceptions
                .insert(ToolName::new(*name), crate::profile::ExecutorClass::ProviderRun);
            declaration.confined_results.remove(&ToolName::new(*name));
        }
        Engine::open(DeploymentPolicy {
            registry: cfg,
            planner_cap: crate::registry::PlannerCap::default(),
            dialect: PolicyDialectVersion::new(1),
            child_return: ReturnPolicy::Raw,
            profile: declaration,
        })
        .unwrap()
    }

    #[test]
    fn a_proposal_naming_a_provider_run_tool_is_malformed_at_every_fresh_entry_point() {
        let e = engine_with_provider_run(vec![plain_tool("search")], &["search"]);
        let log = vec![opened(&e)];
        let p = Projection::build(&log, 1);
        let t = traj();
        let views = p.view(&t);
        let proposed = call("search", json!({}));
        assert!(matches!(
            e.check(&views, &proposed),
            Err(EngineError::ProviderRunTool(name)) if name == "search"
        ));
        let raw = crate::check::RawBlock {
            requirement_gaps: vec![],
            narrowing: None,
        };
        assert!(matches!(
            e.plan(&views, &proposed, &raw),
            Err(EngineError::ProviderRunTool(_))
        ));
        assert!(matches!(
            e.resolve_call(ToolName::new("search"), b"{}"),
            Err(EngineError::ProviderRunTool(_))
        ));
        // A provider-run name has no checkable contract at all, so a persisted batch naming it at
        // ordinal zero is not an undeclared tool's batch: replay refuses the name itself.
        let forged = vec![
            opened(&e),
            Fact::ProposalBatchDecided {
                trajectory: traj(),
                batch: crate::transition::ProposalBatchId::new("b1"),
                proposals: vec![proposed],
                spawn: None,
                released: vec![],
                evidence: crate::audience::AudienceEvidence::default(),
            },
        ];
        assert!(matches!(
            e.validate_replay(&forged),
            Err(TransitionRefusal::UnknownTool(name)) if name == "search"
        ));
    }

    /// A released tool nothing covers is refused by name on replay. Under a wildcard the name
    /// is covered, but the wildcard prescribes its annotator's mandate: a record carrying a
    /// static pin for it is forged, and so is a record naming any other ordinal.
    #[test]
    fn replay_refuses_a_released_tool_nothing_covers_and_a_forged_wildcard_record() {
        let e = engine(vec![crm_tool()]);
        let ghost = call("ghost", json!({}));
        let forged = |proposal: ResolvedCall| {
            let dispatch = DispatchId::new(traj(), proposal.digest(), 0);
            vec![
                opened(&e),
                Fact::ProposalBatchDecided {
                    trajectory: traj(),
                    batch: crate::transition::ProposalBatchId::new("b1"),
                    proposals: vec![proposal.clone()],
                    spawn: None,
                    released: vec![dispatch.clone()],
                    evidence: crate::audience::AudienceEvidence::default(),
                },
                Fact::DispatchOpened {
                    trajectory: traj(),
                    dispatch,
                    tool: proposal.tool().clone(),
                    declaration: proposal.declaration_id(),
                    arguments: proposal.canonical_arguments().clone(),
                    proposed_label: established(TRUSTED, Audience::public()),
                    receiving: established(TRUSTED, Audience::public()),
                    proposed_effects: EffectSet::default(),
                    annotation: None,
                    subject: crate::basis::fixture_subject(&traj()),
                    evidence: crate::audience::AudienceEvidence::default(),
                },
            ]
        };
        // No wildcard: the name has no contract at all, and replay refuses it as such.
        assert_eq!(
            e.validate_replay(&forged(ghost.clone())),
            Err(TransitionRefusal::UnknownTool("ghost".to_string()))
        );

        // A wildcard covers the name, but the record's static pin is not its annotator's.
        let mut cfg = test_config(vec![crm_tool()]);
        cfg.tools.push(wildcard("any"));
        cfg.annotators.push(annotator("any"));
        let covered = open_engine(cfg);
        let forged_static = {
            let mut log = forged(ghost.clone());
            log[0] = opened(&covered);
            log
        };
        assert_eq!(
            covered.validate_replay(&forged_static),
            Err(TransitionRefusal::ForgedEvidence)
        );
        let second = ResolvedCall::new_keyed(
            ghost.tool().clone(),
            crate::value::ToolDeclarationId::new(1).unwrap(),
            ghost.canonical_arguments().clone(),
        );
        // A covered name has exactly one ordinal: a fresh selection contradicts the record.
        let forged_ordinal = {
            let mut log = forged(second);
            log[0] = opened(&covered);
            log
        };
        assert_eq!(
            covered.validate_replay(&forged_ordinal),
            Err(TransitionRefusal::MisdecidedBatch)
        );
    }

    #[test]
    fn provider_run_tools_leave_every_plan_family() {
        let mut target = plain_tool("wire");
        target.requires = Requires {
            history: vec![HistoryRequirement::Prior(EffectKind::new("k"))],
            ..Requires::default()
        };
        let mut emitter = plain_tool("emit");
        emitter.emits = EffectSet::new([EffectKind::new("k")]).unwrap();
        let log = vec![crate::profile::opening_at(traj(), known(TRUSTED, Audience::public()))];
        let offered_tools = |e: &Engine| -> Vec<String> {
            let p = Projection::build(&log, 1);
            let t = traj();
            let wire = call("wire", json!({}));
            let raw = match e.check(&p.view(&t), &wire).unwrap() {
                CheckOutcome::Block(raw) => raw,
                other => panic!("expected a block, got {other:?}"),
            };
            e.plan(&p.view(&t), &wire, &raw)
                .unwrap()
                .plans
                .iter()
                .filter_map(|plan| match plan {
                    plan::RemedyPlan::Redispatch(redispatch) => Some(redispatch.tool().as_str().to_string()),
                    plan::RemedyPlan::Executable(_) => None,
                })
                .collect()
        };
        let enforced = engine(vec![target.clone(), emitter.clone()]);
        assert_eq!(offered_tools(&enforced), ["emit"]);
        let split = engine_with_provider_run(vec![target, emitter], &["emit"]);
        assert_eq!(offered_tools(&split), Vec::<String>::new());
    }

    #[test]
    fn the_opening_batch_carries_the_identity_and_derived_vectors() {
        let e = engine_with_provider_run(vec![plain_tool("send"), plain_tool("search")], &["search"]);
        let t = traj();
        let key = crate::profile::PolicyFileKey::of(b"the policy file");
        let batch = e
            .open_trajectory(&t, key.clone())
            .expect("a fresh root's opening seals");
        assert_eq!(batch.basis(), 0, "the opening stands on the empty log");
        match batch.facts() {
            [
                Fact::TrajectoryOpened {
                    trajectory,
                    dialect,
                    profile,
                    policy_digest,
                    policy_file_key,
                    open_vectors,
                },
            ] => {
                assert_eq!(policy_file_key, &key, "the opening names the file it opened under");
                assert_eq!(trajectory, &t);
                assert_eq!(*dialect, PolicyDialectVersion::new(1));
                assert_eq!(profile, e.profile());
                assert_eq!(*policy_digest, e.identity());
                assert_eq!(open_vectors, &e.open_vectors());
                assert_eq!(open_vectors.len(), 1);
            }
            other => panic!("expected exactly the opening record, got {other:?}"),
        }
        let wire = serde_json::to_string(batch.facts()).unwrap();
        assert_eq!(serde_json::from_str::<Vec<Fact>>(&wire).unwrap(), batch.facts());
    }

    #[test]
    fn cold_replay_verifies_the_opening_strictly() {
        use crate::transition::OpeningTransitionRefusal;
        let e = engine_with_provider_run(vec![plain_tool("send"), plain_tool("search")], &["search"]);
        let t = traj();
        let opening = opened_root(&e, &t);
        let mut valid = vec![opening.clone()];
        reads(&e, &mut valid, &t, "send");
        let admitted = valid[1].clone();
        let replay =
            |family: &TrajectoryId, facts: Vec<Fact>| e.view(family, facts.clone(), facts.len() as u64).map(|_| ());

        assert_eq!(replay(&t, valid.clone()), Ok(()));
        assert_eq!(replay(&t, vec![admitted.clone()]), Err(TransitionRefusal::Unopened));
        assert_eq!(
            replay(&t, vec![admitted.clone(), opening.clone()]),
            Err(TransitionRefusal::Unopened)
        );
        assert_eq!(
            replay(&t, vec![opening.clone(), opening.clone()]),
            Err(TransitionRefusal::Opening(OpeningTransitionRefusal::Duplicate))
        );
        assert_eq!(
            replay(&t, [valid.clone(), vec![opening.clone()]].concat()),
            Err(TransitionRefusal::Opening(OpeningTransitionRefusal::Duplicate))
        );
        assert_eq!(
            replay(&TrajectoryId::new("other"), vec![opening.clone()]),
            Err(TransitionRefusal::Opening(OpeningTransitionRefusal::WrongTrajectory {
                found: "t".to_string()
            }))
        );

        let mutated = |mutate: &dyn Fn(&mut Fact)| {
            let mut fact = opening.clone();
            mutate(&mut fact);
            replay(&t, vec![fact])
        };
        assert_eq!(
            mutated(&|fact| {
                if let Fact::TrajectoryOpened { dialect, .. } = fact {
                    *dialect = PolicyDialectVersion::new(9);
                }
            }),
            Err(TransitionRefusal::Opening(
                OpeningTransitionRefusal::UnsupportedDialect { found: 9 }
            ))
        );
        assert_eq!(
            mutated(&|fact| {
                if let Fact::TrajectoryOpened { policy_digest, .. } = fact {
                    let other = engine(vec![plain_tool("send")]);
                    *policy_digest = other.identity();
                }
            }),
            Err(TransitionRefusal::Opening(OpeningTransitionRefusal::DigestMismatch))
        );
        assert_eq!(
            mutated(&|fact| {
                if let Fact::TrajectoryOpened { profile, .. } = fact {
                    let other = engine(vec![plain_tool("send")]);
                    *profile = other.profile().clone();
                }
            }),
            Err(TransitionRefusal::Opening(OpeningTransitionRefusal::ProfileMismatch))
        );
        assert_eq!(
            mutated(&|fact| {
                if let Fact::TrajectoryOpened { open_vectors, .. } = fact {
                    open_vectors.clear();
                }
            }),
            Err(TransitionRefusal::Opening(OpeningTransitionRefusal::VectorMismatch))
        );
    }

    #[test]
    fn a_fork_carries_the_deployments_child_return_binding() {
        let cfg = RegistryConfig {
            annotators: vec![],
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: declared(vec![plain_tool("spawn")]),
            authorities: vec![],
            sanitizers: vec![crate::authority::Sanitizer {
                name: crate::names::SanitizerName::new("redactor"),
                on: crate::authority::SanitizerPoints {
                    input: false,
                    output: true,
                },
                transition: crate::authority::DeclaredTransition::Trust {
                    from_floor: SUSPICIOUS,
                    to: TRUSTED,
                },
                scope: crate::authority::Scope::default(),
                hint: None,
            }],
            audience: crate::audience::AudienceConfig::default(),
        };
        let bound = ReturnPolicy::Sanitized(crate::names::SanitizerName::new("redactor"));
        let e = Engine::open(DeploymentPolicy {
            registry: cfg.clone(),
            planner_cap: crate::registry::PlannerCap::default(),
            dialect: PolicyDialectVersion::new(1),
            child_return: bound.clone(),
            profile: crate::profile::covering_declaration(&cfg),
        })
        .unwrap();
        let view = e.view(&traj(), vec![opened(&e)], 1).unwrap();
        let decision = e
            .handle(
                &view,
                EngineEvent::Proposals(ProposalBatch {
                    id: crate::transition::ProposalBatchId::new("b1"),
                    trajectory: traj(),
                    provider_results: Vec::new(),
                    proposals: vec![raw(&call("spawn", json!({})))],
                    spawn: Some(crate::transition::SpawnMark::at(0)),
                    offer_nonce: nonce(),
                    evidence: Vec::new(),
                    audience: crate::audience::AudienceEvidence::default(),
                }),
            )
            .expect("the marked spawn releases and prepares the fork");
        let prepared = decision
            .append
            .expect("the release appends")
            .facts()
            .iter()
            .find_map(|fact| match fact {
                Fact::ForkPrepared { return_policy, .. } => Some(return_policy.clone()),
                _ => None,
            })
            .expect("the release prepares a fork");
        assert_eq!(prepared, bound);
    }

    #[test]
    fn a_root_folds_from_its_openings_starting_label() {
        let internal = Audience::restricted([ReaderId::new("insider")]);
        let e = engine_at(vec![plain_tool("send")], known(SUSPICIOUS, internal.clone()));
        let t = traj();
        let starting = partial(SUSPICIOUS, internal.clone());

        let replayed = e.view(&t, vec![opened(&e)], 1).expect("the opened root replays");
        assert_eq!(
            replayed.views(&t).expect("the root is opened").current_label(),
            starting
        );
        assert_eq!(Projection::build(&[opened(&e)], 1).view(&t).current_label(), starting);
        let mut advanced = EngineView::validated(Projection::empty(0), e.identity(), t.clone());
        advanced
            .advance(
                &e.open_trajectory(&t, crate::profile::PolicyFileKey::of(b"policy"))
                    .expect("the opening seals"),
            )
            .expect("the sealed opening advances the empty view");
        assert_eq!(
            advanced.views(&t).expect("the root is opened").current_label(),
            starting
        );

        let send = call("send", json!({}));
        let released = e
            .handle(&replayed, batch("b1", Vec::new(), vec![raw(&send)]))
            .expect("a neutral send releases");
        let dispatch = match &released.follow_up {
            FollowUp::Proposals { released, .. } => released[0].dispatch.clone(),
            other => panic!("the send releases, got {other:?}"),
        };
        let mut log = [vec![opened(&e)], appended_facts(released)].concat();
        let closed = e
            .handle(
                &viewing(&e, &log),
                EngineEvent::Outcome(ToolReport {
                    dispatch,
                    outcome: ToolOutcome::Success {
                        body: OutcomeBody::Available(ValueBody::new("sent")),
                    },
                    evidence: Vec::new(),
                    offer_nonce: nonce(),
                    audience: crate::audience::AudienceEvidence::default(),
                }),
            )
            .expect("the result admits");
        log.extend(appended_facts(closed));
        let after = viewing(&e, &log);
        let views = after.views(&t).expect("the root is opened");
        assert_eq!(
            views.current_label(),
            starting,
            "a neutral result leaves the fold in place"
        );
    }

    #[test]
    fn an_unopened_trajectory_has_no_views_and_takes_no_event() {
        let e = engine(vec![plain_tool("send")]);
        let unopened = TrajectoryId::new("child");
        let view = viewing(&e, &[opened(&e)]);
        assert!(view.views(&traj()).is_some());
        assert!(view.views(&unopened).is_none());
        assert_eq!(
            e.handle(
                &view,
                batch_on(&unopened, "b1", Vec::new(), vec![raw(&call("send", json!({})))], None),
            ),
            Err(TransitionError::UnopenedTrajectory)
        );
        assert_eq!(
            e.validate_replay(&[
                opened(&e),
                stray_admission(&unopened, known(SUSPICIOUS, Audience::public()))
            ]),
            Err(TransitionRefusal::ForeignTrajectory)
        );
    }

    fn raw_call(tool: &str, arguments: &[u8]) -> crate::transition::ProposedCall {
        crate::transition::ProposedCall {
            tool: ToolName::new(tool),
            arguments: arguments.to_vec(),
            annotation: None,
        }
    }

    fn exposed(tool: &str, body: &str) -> crate::transition::ProviderResult {
        crate::transition::ProviderResult {
            tool: ToolName::new(tool),
            body: ValueBody::new(body),
        }
    }

    fn viewing(e: &Engine, log: &[Fact]) -> EngineView {
        e.view(&traj(), log.to_vec(), log.len() as u64)
            .expect("the log replays")
    }

    fn batch_on(
        trajectory: &TrajectoryId,
        id: &str,
        provider_results: Vec<crate::transition::ProviderResult>,
        proposals: Vec<crate::transition::ProposedCall>,
        spawn: Option<SpawnMark>,
    ) -> EngineEvent {
        EngineEvent::Proposals(ProposalBatch {
            id: crate::transition::ProposalBatchId::new(id),
            trajectory: trajectory.clone(),
            provider_results,
            proposals,
            spawn,
            offer_nonce: nonce(),
            evidence: Vec::new(),
            audience: crate::audience::AudienceEvidence::default(),
        })
    }

    fn batch(
        id: &str,
        provider_results: Vec<crate::transition::ProviderResult>,
        proposals: Vec<crate::transition::ProposedCall>,
    ) -> EngineEvent {
        batch_on(&traj(), id, provider_results, proposals, None)
    }

    fn answered(decision: &EngineDecision) -> (&[Released], &[Blocked]) {
        match &decision.follow_up {
            FollowUp::Proposals { released, blocked, .. } => (released, blocked),
            other => panic!("a proposal batch answers with proposals, not {other:?}"),
        }
    }

    fn tool_names(released: &[Released]) -> Vec<&str> {
        released.iter().map(|release| release.call.tool().as_str()).collect()
    }

    fn blocked_names(blocked: &[Blocked]) -> Vec<&str> {
        blocked.iter().map(|block| block.call.tool().as_str()).collect()
    }

    fn batch_engine() -> Engine {
        let mut seen = plain_tool("seen");
        seen.emits = EffectSet::new([EffectKind::new("k")]).unwrap();
        let mut wire = plain_tool("wire");
        wire.requires = Requires {
            history: vec![HistoryRequirement::Prior(EffectKind::new("k"))],
            ..Requires::default()
        };
        let mut guard = plain_tool("guard");
        guard.requires = Requires {
            history: vec![HistoryRequirement::NoPrior(EffectKind::new("k"))],
            ..Requires::default()
        };
        let mut emit = plain_tool("emit");
        emit.emits = EffectSet::new([EffectKind::new("k")]).unwrap();
        engine_with_provider_run(
            vec![seen, wire, guard, emit, plain_tool("quiet"), plain_tool("spawn")],
            &["seen"],
        )
    }

    fn opening_log(e: &Engine) -> Vec<Fact> {
        vec![opened(e)]
    }

    #[test]
    fn an_exposed_provider_run_result_is_history_for_every_sibling() {
        let e = batch_engine();
        let log = opening_log(&e);
        let decision = e
            .handle(
                &viewing(&e, &log),
                batch(
                    "b1",
                    vec![exposed("seen", "the provider ran it")],
                    vec![raw(&call("wire", json!({}))), raw(&call("guard", json!({})))],
                ),
            )
            .expect("the batch decides");
        let (released, blocked) = answered(&decision);
        assert_eq!(tool_names(released), ["wire"]);
        assert_eq!(blocked_names(blocked), ["guard"]);
        assert!(
            blocked[0]
                .block
                .raw
                .requirement_gaps
                .contains(&Gap::NoPrior(EffectKind::new("k")))
        );

        let facts = appended_facts(decision);
        match &facts[..2] {
            [
                Fact::BasisAdvanced { .. },
                Fact::ValueAdmitted { value, provenance, .. },
            ] => {
                assert_eq!(value.body, ValueBody::new("the provider ran it"));
                assert_eq!(value.label, Delta::NONE.output_label());
                assert_eq!(
                    provenance,
                    &Provenance::ProviderRun {
                        tool: ToolName::new("seen"),
                        batch: crate::transition::ProposalBatchId::new("b1"),
                        position: 0,
                        effects: EffectSet::new([EffectKind::new("k")]).unwrap(),
                        evidence: crate::audience::AudienceEvidence::default(),
                    }
                );
            }
            other => panic!("the admissions open the batch, not {other:?}"),
        }
        assert!(
            !facts
                .iter()
                .any(|fact| matches!(fact, Fact::DispatchSucceeded { .. } | Fact::DispatchClosed { .. }))
        );
        assert_eq!(e.validate_replay(&[log, facts].concat()), Ok(()));
    }

    #[test]
    fn a_provider_run_result_the_response_hides_establishes_no_effect() {
        let e = batch_engine();
        let log = opening_log(&e);
        let decision = e
            .handle(
                &viewing(&e, &log),
                batch(
                    "b1",
                    Vec::new(),
                    vec![raw(&call("wire", json!({}))), raw(&call("guard", json!({})))],
                ),
            )
            .expect("the batch decides");
        let (released, blocked) = answered(&decision);
        assert_eq!(tool_names(released), ["guard"]);
        assert_eq!(blocked_names(blocked), ["wire"]);
        assert!(
            blocked[0]
                .block
                .raw
                .requirement_gaps
                .contains(&Gap::Prior(EffectKind::new("k")))
        );
    }

    #[test]
    fn a_malformed_sibling_mediates_none_of_them_and_the_admissions_stand() {
        let e = batch_engine();
        let log = opening_log(&e);
        // A sibling nothing covers is one of the malformed shapes: no contract, no decision,
        // and the provider-run admissions stand.
        for (position, malformed) in [
            (1, raw_call("seen", b"{}")),
            (1, raw_call("quiet", b"not json")),
            (1, raw_call("nowhere", b"{}")),
        ] {
            let decision = e
                .handle(
                    &viewing(&e, &log),
                    batch(
                        "b1",
                        vec![exposed("seen", "the provider ran it")],
                        vec![raw(&call("quiet", json!({}))), malformed],
                    ),
                )
                .expect("the batch answers");
            match &decision.follow_up {
                FollowUp::Malformed { position: at, .. } => assert_eq!(*at, position),
                other => panic!("a malformed batch answers with its refusal, not {other:?}"),
            }
            let facts = appended_facts(decision);
            assert!(matches!(&facts[0], Fact::BasisAdvanced { .. }));
            assert!(matches!(
                &facts[1],
                Fact::ValueAdmitted {
                    provenance: Provenance::ProviderRun { .. },
                    ..
                }
            ));
            assert_eq!(facts.len(), 2, "nothing beyond the admissions: {facts:?}");
            assert_eq!(e.validate_replay(&[log.clone(), facts].concat()), Ok(()));
        }
    }

    #[test]
    fn a_retry_of_a_malformed_batch_admits_its_results_once_and_then_decides() {
        let e = batch_engine();
        let log = opening_log(&e);
        let malformed = || {
            batch(
                "b1",
                vec![exposed("seen", "the provider ran it")],
                vec![raw_call("quiet", b"not json")],
            )
        };
        let first = e.handle(&viewing(&e, &log), malformed()).expect("the batch answers");
        let log = [log, appended_facts(first)].concat();

        let repeat = e.handle(&viewing(&e, &log), malformed()).expect("the repeat answers");
        assert!(matches!(repeat.follow_up, FollowUp::Malformed { position: 0, .. }));
        assert_eq!(repeat.append, None, "a repeat admits nothing a second time");

        let corrected = e
            .handle(
                &viewing(&e, &log),
                batch(
                    "b1",
                    vec![exposed("seen", "the provider ran it")],
                    vec![raw(&call("wire", json!({})))],
                ),
            )
            .expect("the corrected batch decides");
        let (released, _) = answered(&corrected);
        assert_eq!(tool_names(released), ["wire"], "the admission is history for the retry");
        let facts = appended_facts(corrected);
        assert!(
            !facts.iter().any(|fact| matches!(fact, Fact::ValueAdmitted { .. })),
            "the results were admitted by the first attempt: {facts:?}"
        );
        assert_eq!(e.validate_replay(&[log, facts].concat()), Ok(()));
    }

    #[test]
    fn a_batch_identity_is_bound_to_its_exposed_results_and_their_trajectory() {
        let e = batch_engine();
        let log = opening_log(&e);
        let first = e
            .handle(
                &viewing(&e, &log),
                batch(
                    "b1",
                    vec![exposed("seen", "the provider ran it")],
                    vec![raw_call("quiet", b"not json")],
                ),
            )
            .expect("the batch answers");
        let log = [log, appended_facts(first)].concat();

        let other_body = e.handle(
            &viewing(&e, &log),
            batch(
                "b1",
                vec![exposed("seen", "something else")],
                vec![raw(&call("quiet", json!({})))],
            ),
        );
        assert_eq!(other_body.unwrap_err(), TransitionError::BatchIdentityConflict);

        let dropped = e.handle(
            &viewing(&e, &log),
            batch("b1", Vec::new(), vec![raw(&call("quiet", json!({})))]),
        );
        assert_eq!(dropped.unwrap_err(), TransitionError::BatchIdentityConflict);

        let other = TrajectoryId::new("other");
        let with_other = [log.clone(), forked_child(&e, &log, &other)].concat();
        let elsewhere = e.handle(
            &viewing(&e, &with_other),
            batch_on(
                &other,
                "b1",
                vec![exposed("seen", "the provider ran it")],
                vec![raw(&call("quiet", json!({})))],
                None,
            ),
        );
        assert_eq!(elsewhere.unwrap_err(), TransitionError::BatchIdentityConflict);

        let decided = appended_facts(
            e.handle(
                &viewing(&e, &log),
                batch("b2", Vec::new(), vec![raw(&call("quiet", json!({})))]),
            )
            .expect("the batch decides"),
        );
        let log = [log, decided].concat();
        let smuggled = e.handle(
            &viewing(&e, &log),
            batch(
                "b2",
                vec![exposed("seen", "the provider ran it")],
                vec![raw(&call("quiet", json!({})))],
            ),
        );
        assert_eq!(smuggled.unwrap_err(), TransitionError::BatchIdentityConflict);

        let swapped = e.handle(
            &viewing(&e, &log),
            batch(
                "b1",
                vec![exposed("nowhere", "the provider ran it")],
                vec![raw(&call("quiet", json!({})))],
            ),
        );
        assert_eq!(swapped.unwrap_err(), TransitionError::BatchIdentityConflict);
    }

    #[test]
    fn an_exposed_result_of_a_tool_this_deployment_releases_is_refused() {
        let e = batch_engine();
        let log = opening_log(&e);
        for (tool, expected) in [
            ("quiet", EngineError::NotProviderRun("quiet".to_string())),
            ("nowhere", EngineError::UnknownTool("nowhere".to_string())),
        ] {
            let refusal = e.handle(
                &viewing(&e, &log),
                batch(
                    "b1",
                    vec![exposed(tool, "a body")],
                    vec![raw(&call("quiet", json!({})))],
                ),
            );
            assert_eq!(refusal.unwrap_err(), TransitionError::Call(expected));
        }
    }

    #[test]
    fn a_provider_admission_advances_flow_only_when_it_moves_the_label_and_family_only_for_its_effects() {
        let e = engine_with_provider_run(
            vec![
                plain_tool("quiet"),
                {
                    let mut emitting = plain_tool("loud");
                    emitting.emits = EffectSet::new([EffectKind::new("k")]).unwrap();
                    emitting
                },
                {
                    let mut narrowing = plain_tool("insider");
                    narrowing.delta = Delta {
                        trust: None,
                        audience: Some(DeclaredAudience::restricted([ReaderId::new("insider")])),
                    };
                    narrowing
                },
            ],
            &["quiet", "loud", "insider"],
        );
        let log = opening_log(&e);
        let declared = |tool: &str| {
            let decision = e
                .handle(
                    &viewing(&e, &log),
                    batch("b1", vec![exposed(tool, "a body")], Vec::new()),
                )
                .expect("an admission-only batch decides");
            match &appended_facts(decision)[0] {
                Fact::BasisAdvanced { advance, .. } => advance.clone(),
                other => panic!("the declaration opens the batch, not {other:?}"),
            }
        };
        let quiet = declared("quiet");
        assert!(
            quiet.flows.is_empty(),
            "an admission at the identity label moves no flow: nothing an open offer reads changed"
        );
        assert!(
            !quiet.family,
            "an observation with no declared effects moves no family state"
        );
        let loud = declared("loud");
        assert!(loud.family);
        assert!(loud.flows.is_empty(), "effects move the family, not the flow");
        assert!(
            declared("insider").flows.contains(&traj()),
            "an admission that narrows the label moves the flow"
        );
    }

    mod admission_law {
        use super::*;
        use proptest::prelude::*;

        /// Provider-run tools whose results meet the fold at the identity or narrow one known
        /// dimension. A pending dimension is not a provider-run construct; that case is the
        /// projection's own unit test.
        const TOOLS: [&str; 3] = ["identity", "suspicious", "insider"];

        fn labeled_tool(name: &str) -> ToolAnnotation {
            let mut tool = plain_tool(name);
            tool.delta = match name {
                "identity" => Delta::NONE,
                "suspicious" => Delta {
                    trust: Some(Trust::new(0)),
                    audience: None,
                },
                "insider" => Delta {
                    trust: None,
                    audience: Some(DeclaredAudience::restricted([ReaderId::new("insider")])),
                },
                other => panic!("no labeled tool named {other}"),
            };
            tool
        }

        proptest! {
            /// Effect-free admissions move neither family nor subject, so across them an open
            /// offer stays current exactly while the trajectory's partial label — bound and
            /// pending sources alike — stays where the offer found it.
            #[test]
            fn an_offer_outlives_exactly_the_admissions_that_leave_the_label_unchanged(
                admitted in prop::collection::vec(0usize..TOOLS.len(), 0..4),
            ) {
                let mut tools = vec![crm_tool()];
                tools.extend(TOOLS.iter().map(|name| labeled_tool(name)));
                let e = engine_with_provider_run(tools, &TOOLS);
                let log = opening_log(&e);
                let opened = appended_facts(blocked_batch(&e, &log, "b1", nonce()));
                let offer = opened_offers(&opened)[0].0;
                let mut log = [log, opened].concat();
                let label_of = |log: &[Fact]| {
                    e.view(&traj(), log.to_vec(), log.len() as u64)
                        .expect("the log replays")
                        .views(&traj())
                        .expect("the root is opened")
                        .current_label()
                };
                let at_open = label_of(&log);
                for (i, tool) in admitted.iter().enumerate() {
                    let decision = e
                        .handle(
                            &viewing(&e, &log),
                            batch(&format!("b{}", i + 2), vec![exposed(TOOLS[*tool], "a body")], Vec::new()),
                        )
                        .expect("an admission-only batch decides");
                    log.extend(appended_facts(decision));
                }
                let now = label_of(&log);
                let stale = matches!(
                    execute_offer(&e, &log, offer, OfferOutcome::Approved(Vec::new())),
                    Err(TransitionError::StaleOffer)
                );
                prop_assert_eq!(stale, now != at_open);
            }
        }
    }

    #[test]
    fn an_admission_only_batch_decides_and_an_empty_one_is_no_event() {
        let e = batch_engine();
        let log = opening_log(&e);
        let decision = e
            .handle(
                &viewing(&e, &log),
                batch("b1", vec![exposed("seen", "the provider ran it")], Vec::new()),
            )
            .expect("an admission-only batch decides");
        let (released, blocked) = answered(&decision);
        assert!(released.is_empty() && blocked.is_empty());
        let facts = appended_facts(decision);
        assert!(matches!(
            &facts[2],
            Fact::ProposalBatchDecided { proposals, released, .. } if proposals.is_empty() && released.is_empty()
        ));
        assert_eq!(e.validate_replay(&[log.clone(), facts].concat()), Ok(()));

        let empty = e.handle(&viewing(&e, &log), batch("b2", Vec::new(), Vec::new()));
        assert_eq!(empty.unwrap_err(), TransitionError::EmptyBatch);

        let marked = e.handle(
            &viewing(&e, &log),
            batch_on(&traj(), "b3", Vec::new(), Vec::new(), Some(SpawnMark::at(0))),
        );
        assert_eq!(marked.unwrap_err(), TransitionError::SpawnMarkOutOfRange);
    }

    #[test]
    fn a_siblings_release_is_state_for_the_siblings_after_it() {
        let e = batch_engine();
        let log = opening_log(&e);
        let decision = e
            .handle(
                &viewing(&e, &log),
                batch(
                    "b1",
                    Vec::new(),
                    vec![
                        raw(&call("quiet", json!({}))),
                        raw(&call("quiet", json!({}))),
                        raw(&call("emit", json!({}))),
                        raw(&call("guard", json!({}))),
                    ],
                ),
            )
            .expect("the batch decides");
        let (released, blocked) = answered(&decision);
        assert_eq!(tool_names(released), ["quiet", "quiet", "emit"]);
        assert_eq!(
            (released[0].dispatch.occurrence(), released[1].dispatch.occurrence()),
            (0, 1),
            "two identical siblings are two occurrences of one call"
        );
        assert_eq!(blocked_names(blocked), ["guard"]);
        assert!(
            blocked[0]
                .block
                .raw
                .requirement_gaps
                .contains(&Gap::NoPrior(EffectKind::new("k")))
        );
        let facts = appended_facts(decision);
        assert_eq!(e.validate_replay(&[log, facts].concat()), Ok(()));
    }

    #[test]
    fn a_refused_sibling_is_planned_against_the_batchs_final_state() {
        let mut strict = plain_tool("strict");
        strict.requires = Requires {
            label: LabelRequirements {
                trust_floor: Some(TRUSTED),
                audience: vec![],
            },
            history: vec![HistoryRequirement::NoPrior(EffectKind::new("k"))],
            ..Requires::default()
        };
        let mut emit = plain_tool("emit");
        emit.emits = EffectSet::new([EffectKind::new("k")]).unwrap();
        let e = engine_at(vec![strict, emit], known(SUSPICIOUS, Audience::public()));

        let log = vec![opened(&e)];
        let decision = e
            .handle(
                &viewing(&e, &log),
                batch(
                    "b1",
                    Vec::new(),
                    vec![raw(&call("strict", json!({}))), raw(&call("emit", json!({})))],
                ),
            )
            .expect("the batch decides");
        let (released, blocked) = answered(&decision);
        assert_eq!(tool_names(released), ["emit"]);
        assert_eq!(blocked_names(blocked), ["strict"]);
        let gaps = &blocked[0].block.raw.requirement_gaps;
        assert!(
            gaps.contains(&Gap::TrustFloor {
                required: TRUSTED,
                actual: SUSPICIOUS
            }),
            "the gap it was refused for: {gaps:?}"
        );
        assert!(
            gaps.contains(&Gap::NoPrior(EffectKind::new("k"))),
            "the gap the sibling's own release opened: {gaps:?}"
        );
        let facts = appended_facts(decision);
        assert_eq!(e.validate_replay(&[log, facts].concat()), Ok(()));
    }

    #[test]
    fn a_marked_spawn_prepares_its_fork_from_its_own_position() {
        let mut spawn = plain_tool("spawn");
        spawn.requires = Requires {
            history: vec![HistoryRequirement::Prior(EffectKind::new("k"))],
            ..Requires::default()
        };
        let e = engine(vec![plain_tool("quiet"), spawn]);
        let log = opening_log(&e);
        let decision = e
            .handle(
                &viewing(&e, &log),
                batch_on(
                    &traj(),
                    "b1",
                    Vec::new(),
                    vec![raw(&call("quiet", json!({}))), raw(&call("spawn", json!({})))],
                    Some(SpawnMark::at(1)),
                ),
            )
            .expect("the batch decides");
        let (released, blocked) = answered(&decision);
        assert_eq!(tool_names(released), ["quiet"]);
        assert_eq!(blocked_names(blocked), ["spawn"]);
        assert!(released[0].fork.is_none(), "the unmarked sibling prepares nothing");
        let facts = appended_facts(decision);
        assert!(
            !facts.iter().any(|fact| matches!(fact, Fact::ForkPrepared { .. })),
            "a refused spawn prepares no fork: {facts:?}"
        );

        let released_spawn = e
            .handle(
                &viewing(&e, &log),
                batch_on(
                    &traj(),
                    "b2",
                    Vec::new(),
                    vec![raw(&call("quiet", json!({}))), raw(&call("quiet", json!({})))],
                    Some(SpawnMark::at(1)),
                ),
            )
            .expect("the batch decides");
        let (released, _) = answered(&released_spawn);
        assert!(released[0].fork.is_none());
        let fork = released[1].fork.clone().expect("the marked sibling prepared its fork");
        assert_eq!(fork, ForkId::of(&released[1].dispatch));
        let facts = appended_facts(released_spawn);
        assert_eq!(e.validate_replay(&[log, facts].concat()), Ok(()));
    }

    #[test]
    fn an_approval_is_spent_only_by_a_singleton_batch() {
        let e = engine(vec![crm_tool(), neutral_tool()]);
        let log = opening_log(&e);
        let opened = appended_facts(blocked_batch(&e, &log, "b1", nonce()));
        let offer = opened_offers(&opened)[0].0;
        let log = [log, opened].concat();
        let approved = appended_facts(
            execute_offer(&e, &log, offer, OfferOutcome::Approved(Vec::new())).expect("the offer executes"),
        );
        let log = [log, approved].concat();

        let decision = e
            .handle(
                &viewing(&e, &log),
                batch(
                    "b2",
                    Vec::new(),
                    vec![raw(&call("get_ticket", json!({}))), raw(&call("read_note", json!({})))],
                ),
            )
            .expect("the batch decides");
        let (released, blocked) = answered(&decision);
        assert_eq!(tool_names(released), ["read_note"]);
        assert_eq!(blocked_names(blocked), ["get_ticket"]);
        let facts = appended_facts(decision);
        assert!(
            !facts
                .iter()
                .any(|fact| matches!(fact, Fact::CallApprovalConsumed { .. })),
            "a sibling batch spends no approval: {facts:?}"
        );
        let alone = e
            .handle(
                &viewing(&e, &[log.clone(), facts].concat()),
                batch("b3", Vec::new(), vec![raw(&call("get_ticket", json!({})))]),
            )
            .expect("the singleton decides");
        let (released, _) = answered(&alone);
        assert_eq!(tool_names(released), ["get_ticket"]);
    }

    #[test]
    fn an_identity_admission_leaves_the_approval_its_own_batch_spends_current() {
        let e = engine_with_provider_run(vec![crm_tool(), plain_tool("seen")], &["seen"]);
        let log = opening_log(&e);
        let opened = appended_facts(blocked_batch(&e, &log, "b1", nonce()));
        let offer = opened_offers(&opened)[0].0;
        let log = [log, opened].concat();
        let approved = appended_facts(
            execute_offer(&e, &log, offer, OfferOutcome::Approved(Vec::new())).expect("the offer executes"),
        );
        let log = [log, approved].concat();

        let decision = e
            .handle(
                &viewing(&e, &log),
                batch(
                    "b2",
                    vec![exposed("seen", "the provider ran it")],
                    vec![raw(&call("get_ticket", json!({})))],
                ),
            )
            .expect("the batch decides");
        let (released, blocked) = answered(&decision);
        assert_eq!(
            tool_names(released),
            ["get_ticket"],
            "an admission at the identity label leaves the approval current, and the batch spends it"
        );
        assert!(blocked.is_empty());
        assert_eq!(
            e.validate_replay(&[log.clone(), appended_facts(decision)].concat()),
            Ok(())
        );

        let release = appended_facts(
            e.handle(
                &viewing(&e, &log),
                batch("b3", Vec::new(), vec![raw(&call("get_ticket", json!({})))]),
            )
            .expect("the singleton releases"),
        );
        assert_eq!(e.validate_replay(&[log.clone(), release.clone()].concat()), Ok(()));
        let mut spliced = release;
        spliced.insert(
            1,
            Fact::ValueAdmitted {
                trajectory: traj(),
                value: LabeledValue::new(ValueBody::new("the provider ran it"), Delta::NONE.output_label()),
                provenance: Provenance::ProviderRun {
                    tool: ToolName::new("seen"),
                    batch: crate::transition::ProposalBatchId::new("b3"),
                    position: 0,
                    effects: EffectSet::default(),
                    evidence: crate::audience::AudienceEvidence::default(),
                },
            },
        );
        // Under its own declared act, at the next position, with the contract's label and
        // effects, an identity-label provider admission is a record the engine could have
        // produced from a batch that carried the result: nothing distinguishes the splice from
        // that log, and it moves no basis, so replay has nothing to refuse.
        assert_eq!(e.validate_replay(&[log, spliced].concat()), Ok(()));
    }

    #[test]
    fn a_repeat_of_a_multi_sibling_batch_answers_each_position_from_the_record() {
        let e = batch_engine();
        let log = opening_log(&e);
        let proposals = || {
            vec![
                raw(&call("quiet", json!({}))),
                raw(&call("quiet", json!({}))),
                raw(&call("wire", json!({}))),
            ]
        };
        let first = e
            .handle(&viewing(&e, &log), batch("b1", Vec::new(), proposals()))
            .expect("the batch decides");
        let (released, _) = answered(&first);
        let dispatches: Vec<DispatchId> = released.iter().map(|release| release.dispatch.clone()).collect();
        let log = [log, appended_facts(first)].concat();

        let repeat = e
            .handle(&viewing(&e, &log), batch("b1", Vec::new(), proposals()))
            .expect("the repeat answers");
        assert_eq!(repeat.append, None);
        let (released, blocked) = answered(&repeat);
        assert_eq!(
            released.iter().map(|r| r.dispatch.clone()).collect::<Vec<_>>(),
            dispatches
        );
        assert_eq!(blocked_names(blocked), ["wire"]);
    }

    #[test]
    fn a_repeat_matches_each_sibling_to_the_dispatch_its_own_annotation_opened() {
        let mut notify = plain_tool("notify");
        notify.parameters = crate::params::test_string_argument_schema("room");
        let internal = Audience::restricted([ReaderId::new("insider")]);
        let mut cfg = test_config(vec![]);
        cfg.tools.push(annotated(notify.clone(), "acl"));
        cfg.annotators
            .push(annotator_with_readers("acl", &["insider", "outsider"]));
        let e = open_engine_at(cfg, known(TRUSTED, internal.clone()));
        let log = vec![opened(&e)];
        let arguments = json!({ "room": "lobby" });
        let pinned = |audience: &Audience| {
            let mut produced = notify.clone();
            produced.requires.label.audience = vec![AudienceRequirement::Includes(RecipientSpec::Static(
                DeclaredAudience::literal(audience.clone()),
            ))];
            let unpinned = call("notify", arguments.clone());
            raw(&unpinned
                .clone()
                .with_annotation(Some(pinned_for(produced, "acl", &unpinned))))
        };
        let outsider = Audience::restricted([ReaderId::new("outsider")]);
        let proposals = || vec![pinned(&outsider), pinned(&internal)];

        let first = e
            .handle(&viewing(&e, &log), batch("b1", Vec::new(), proposals()))
            .expect("the batch decides");
        let (released, blocked) = answered(&first);
        assert_eq!(released.len(), 1);
        assert_eq!(blocked.len(), 1);
        let ran = released[0].dispatch.clone();
        let required_includes = |call: &ResolvedCall| match call
            .annotation()
            .expect("an annotated proposal carries its pin")
            .produced()
            .requires
            .label
            .audience
            .as_slice()
        {
            [AudienceRequirement::Includes(RecipientSpec::Static(recipients))] => Audience::of_declared(recipients),
            other => panic!("one produced includes requirement, got {other:?}"),
        };
        assert_eq!(required_includes(&released[0].call), internal.clone());
        let log = [log, appended_facts(first)].concat();

        let repeat = e
            .handle(&viewing(&e, &log), batch("b1", Vec::new(), proposals()))
            .expect("the repeat answers");
        assert_eq!(repeat.append, None);
        let (released, blocked) = answered(&repeat);
        assert_eq!(released.len(), 1);
        assert_eq!(released[0].dispatch, ran);
        assert_eq!(
            required_includes(&released[0].call),
            internal.clone(),
            "the repeat names the sibling that actually ran"
        );
        assert_eq!(blocked.len(), 1);
        assert_eq!(required_includes(&blocked[0].call), outsider.clone());
    }

    fn audience_engine(authorities: Vec<crate::authority::Authority>, starting: Label) -> Engine {
        let mut send = plain_tool("send");
        send.parameters = crate::params::test_string_argument_schema("to");
        send.requires = Requires {
            label: LabelRequirements {
                trust_floor: None,
                audience: vec![AudienceRequirement::Includes(RecipientSpec::Placeholder("to".into()))],
            },
            ..Requires::default()
        };
        open_engine_at(
            RegistryConfig {
                annotators: vec![],
                trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
                tools: declared(vec![send]),
                authorities,
                sanitizers: vec![],
                audience: slack_groups(&["team", "wide", "nobody"]),
            },
            starting,
        )
    }

    fn slack_groups(handles: &[&str]) -> crate::audience::AudienceConfig {
        crate::audience::AudienceConfig {
            sources: vec![crate::audience::SourceRegistration {
                provider: "slack".to_string(),
                templates: vec![crate::audience::SelectorTemplate::new("user-group/<handle>")],
            }],
            groups: handles
                .iter()
                .map(|handle| crate::audience::NamedAudience {
                    name: crate::names::GroupName::new(*handle),
                    within: None,
                    from: vec![crate::audience::SelectorSpec {
                        provider: "slack".to_string(),
                        selector: format!("user-group/{handle}"),
                    }],
                })
                .collect(),
            ..crate::audience::AudienceConfig::default()
        }
    }

    fn send_to(to: &str) -> crate::transition::ProposedCall {
        raw(&call("send", json!({ "to": to })))
    }

    fn slack_member(id: &str, email: Option<&str>) -> crate::audience::MemberClaims {
        crate::audience::MemberClaims {
            id: id.to_string(),
            verified_email: email.map(str::to_string),
        }
    }

    fn user_group(handle: &str, members: Vec<crate::audience::MemberClaims>) -> crate::audience::SourceClaims {
        crate::audience::SourceClaims {
            provider: "slack".to_string(),
            selector: format!("user-group/{handle}"),
            members,
        }
    }

    fn source_evidence(sources: Vec<crate::audience::SourceClaims>) -> crate::audience::AudienceEvidence {
        crate::audience::AudienceEvidence {
            sources,
            ..crate::audience::AudienceEvidence::default()
        }
    }

    fn evidenced_batch(
        id: &str,
        proposals: Vec<crate::transition::ProposedCall>,
        audience: crate::audience::AudienceEvidence,
    ) -> EngineEvent {
        EngineEvent::Proposals(ProposalBatch {
            id: crate::transition::ProposalBatchId::new(id),
            trajectory: traj(),
            provider_results: Vec::new(),
            proposals,
            spawn: None,
            offer_nonce: nonce(),
            evidence: Vec::new(),
            audience,
        })
    }

    fn group_atom(handle: &str) -> crate::label::SymbolicAtom {
        crate::label::SymbolicAtom::Group(crate::label::GroupRef::Named(crate::names::GroupName::new(handle)))
    }

    fn group_audience(handle: &str) -> DeclaredAudience {
        DeclaredAudience::Union(
            crate::label::Clause::new(
                [],
                [crate::label::GroupRef::Named(crate::names::GroupName::new(handle))],
                [],
            )
            .expect("a group clause names no reader"),
        )
    }

    fn corp_reader(local: &str) -> ReaderId {
        ReaderId::new(format!("{local}@corp.com"))
    }

    #[test]
    fn a_public_placeholder_argument_is_the_public_audience() {
        let e = audience_engine(vec![], known(TRUSTED, Audience::restricted([ReaderId::new("auditor")])));
        let restricted = vec![opened(&e)];
        let decision = e
            .handle(
                &viewing(&e, &restricted),
                batch("b1", Vec::new(), vec![send_to("public")]),
            )
            .expect("the batch decides");
        let (released, blocked) = answered(&decision);
        assert!(released.is_empty());
        assert_eq!(
            blocked[0].block.raw.requirement_gaps,
            vec![crate::check::Gap::Includes {
                recipients: DeclaredAudience::Public
            }]
        );
        assert!(
            blocked[0].offers.is_empty(),
            "no authority holds a reader ceiling, so nothing covers a Public recipient"
        );

        let e = audience_engine(vec![], known(TRUSTED, Audience::public()));
        let public = vec![opened(&e)];
        let decision = e
            .handle(&viewing(&e, &public), batch("b2", Vec::new(), vec![send_to("public")]))
            .expect("the batch decides");
        let (released, blocked) = answered(&decision);
        assert_eq!(tool_names(released), ["send"]);
        assert!(blocked.is_empty());
    }

    #[test]
    fn a_public_reader_ceiling_covers_a_public_placeholder_recipient() {
        let officer = crate::authority::Authority {
            name: AuthorityName::new("officer"),
            mandate: crate::authority::Mandate {
                reader_ceiling: Some(DeclaredAudience::literal(Audience::public())),
                ..crate::authority::Mandate::default()
            },
            scope: crate::authority::Scope::default(),
            hint: None,
        };
        let e = audience_engine(
            vec![officer],
            known(TRUSTED, Audience::restricted([ReaderId::new("auditor")])),
        );
        let restricted = vec![opened(&e)];
        let decision = e
            .handle(
                &viewing(&e, &restricted),
                batch("b1", Vec::new(), vec![send_to("public")]),
            )
            .expect("the batch decides");
        let (_, blocked) = answered(&decision);
        assert_eq!(blocked[0].offers.len(), 1, "one ruling plan names the officer");
    }

    #[test]
    fn a_group_placeholder_reads_the_acts_pinned_answer() {
        let e = audience_engine(
            vec![],
            known(
                TRUSTED,
                Audience::restricted([corp_reader("alice"), corp_reader("bob")]),
            ),
        );
        let log = vec![opened(&e)];
        let evidence = source_evidence(vec![
            user_group("team", vec![slack_member("slack:UA", Some("alice@corp.com"))]),
            user_group(
                "wide",
                vec![
                    slack_member("slack:UA", Some("alice@corp.com")),
                    slack_member("slack:UC", Some("carol@other.com")),
                ],
            ),
            user_group("nobody", vec![]),
        ]);
        let decision = e
            .handle(
                &viewing(&e, &log),
                evidenced_batch(
                    "b1",
                    vec![send_to("@team"), send_to("@wide"), send_to("@nobody")],
                    evidence.clone(),
                ),
            )
            .expect("the batch decides");
        let (released, blocked) = answered(&decision);
        assert_eq!(
            tool_names(released),
            ["send", "send"],
            "a covered group and an empty one release"
        );
        assert_eq!(blocked.len(), 1);
        assert_eq!(
            blocked[0].block.raw.requirement_gaps,
            vec![crate::check::Gap::Includes {
                recipients: group_audience("wide")
            }],
            "the gap names the audience symbolically, never a resolved reader list"
        );
        let facts = appended_facts(decision);
        for fact in &facts {
            if let Fact::DispatchOpened { evidence: pinned, .. } = fact {
                assert_eq!(pinned, &evidence, "each opening pins the act's answers");
            }
        }
        assert!(facts.iter().any(|fact| matches!(fact, Fact::DispatchOpened { .. })));
        assert_eq!(e.validate_replay(&[log, facts].concat()), Ok(()));
    }

    #[test]
    fn an_unanswered_group_refuses_the_batch_with_no_fact() {
        let e = audience_engine(vec![], known(TRUSTED, Audience::restricted([corp_reader("alice")])));
        let log = vec![opened(&e)];
        let needed = e.handle(&viewing(&e, &log), batch("b1", Vec::new(), vec![send_to("@team")]));
        assert_eq!(
            needed,
            Err(TransitionError::MembershipNeeded {
                needed: vec![group_atom("team")]
            })
        );

        // An engine whose policy registers no audience source still asks — the atom is the
        // engine's question — and routing the ask is where the runtime fails operationally.
        let unregistered = engine_at(
            vec![{
                let mut send = plain_tool("send");
                send.parameters = crate::params::test_string_argument_schema("to");
                send.requires = Requires {
                    label: LabelRequirements {
                        trust_floor: None,
                        audience: vec![AudienceRequirement::Includes(RecipientSpec::Placeholder("to".into()))],
                    },
                    ..Requires::default()
                };
                send
            }],
            known(TRUSTED, Audience::restricted([corp_reader("alice")])),
        );
        assert!(matches!(
            unregistered.handle(
                &viewing(&unregistered, &[opened(&unregistered)]),
                batch("b1", Vec::new(), vec![send_to("@team")])
            ),
            Err(TransitionError::MembershipNeeded { .. })
        ));
        assert!(matches!(
            unregistered
                .registry
                .audience()
                .needed_primitives(&[group_atom("team")]),
            Err(crate::audience::Unroutable::UnknownGroup(_))
        ));

        // Evidence outside the registered sources is refused, not ignored.
        let foreign = source_evidence(vec![crate::audience::SourceClaims {
            provider: "github".to_string(),
            selector: "org/x/members".to_string(),
            members: vec![],
        }]);
        assert!(matches!(
            e.handle(
                &viewing(&e, &log),
                evidenced_batch("b2", vec![send_to("@team")], foreign)
            ),
            Err(TransitionError::ForeignEvidence(
                crate::audience::EvidenceRefusal::UnroutableSelector { .. }
            ))
        ));

        // Routable answers beyond the asked atoms are refused, not carried along: evidence
        // is scoped to the operation's own asks and inherited pins, never pre-loaded.
        let surplus = source_evidence(vec![
            user_group("team", vec![slack_member("slack:UA", Some("alice@corp.com"))]),
            user_group("nobody", vec![]),
        ]);
        assert_eq!(
            e.handle(
                &viewing(&e, &log),
                evidenced_batch("b3", vec![send_to("@team")], surplus),
            ),
            Err(TransitionError::Invalid(TransitionRefusal::UnrequestedEvidence {
                entry: "source slack:user-group/nobody".to_string()
            }))
        );
        let exact = source_evidence(vec![user_group(
            "team",
            vec![slack_member("slack:UA", Some("alice@corp.com"))],
        )]);
        let decision = e
            .handle(&viewing(&e, &log), evidenced_batch("b3", vec![send_to("@team")], exact))
            .expect("the asked answers decide");
        assert_eq!(tool_names(answered(&decision).0), ["send"]);

        let decision = e
            .handle(&viewing(&e, &log), batch("b4", Vec::new(), vec![send_to("@")]))
            .expect("a malformed spelling still decides");
        assert!(answered(&decision).0.is_empty());
    }

    #[test]
    fn replay_refuses_pinned_evidence_no_operation_requested() {
        let e = audience_engine(vec![], known(TRUSTED, Audience::restricted([corp_reader("alice")])));
        let log = vec![opened(&e)];
        let exact = source_evidence(vec![user_group(
            "team",
            vec![slack_member("slack:UA", Some("alice@corp.com"))],
        )]);
        let decision = e
            .handle(&viewing(&e, &log), evidenced_batch("b1", vec![send_to("@team")], exact))
            .expect("the asked answers decide");
        let mut facts = appended_facts(decision);
        assert_eq!(e.validate_replay(&[log.clone(), facts.clone()].concat()), Ok(()));
        // Tamper: a routable-but-unrequested answer smuggled consistently into every pinned
        // evidence field of the act, so only the operation-scope audit can catch it.
        for fact in &mut facts {
            match fact {
                Fact::ProposalBatchDecided { evidence, .. } | Fact::DispatchOpened { evidence, .. } => {
                    evidence.sources.push(user_group("nobody", vec![]));
                }
                _ => {}
            }
        }
        assert_eq!(
            e.validate_replay(&[log, facts].concat()),
            Err(TransitionRefusal::UnrequestedEvidence {
                entry: "source slack:user-group/nobody".to_string()
            })
        );
    }

    #[test]
    fn audience_evidence_is_batch_payload() {
        let e = audience_engine(vec![], known(TRUSTED, Audience::restricted([corp_reader("alice")])));
        let log = vec![opened(&e)];
        let team = |member: crate::audience::MemberClaims| source_evidence(vec![user_group("team", vec![member])]);
        let first = e
            .handle(
                &viewing(&e, &log),
                evidenced_batch(
                    "b1",
                    vec![send_to("@team")],
                    team(slack_member("slack:UA", Some("alice@corp.com"))),
                ),
            )
            .expect("the batch decides");
        let ran = answered(&first).0[0].dispatch.clone();
        let log = [log, appended_facts(first)].concat();
        let repeat = e
            .handle(
                &viewing(&e, &log),
                evidenced_batch(
                    "b1",
                    vec![send_to("@team")],
                    team(slack_member("slack:UA", Some("alice@corp.com"))),
                ),
            )
            .expect("the repeat answers");
        assert_eq!(repeat.append, None);
        assert_eq!(answered(&repeat).0[0].dispatch, ran);
        assert_eq!(
            e.handle(
                &viewing(&e, &log),
                evidenced_batch(
                    "b1",
                    vec![send_to("@team")],
                    team(slack_member("slack:UB", Some("bob@corp.com"))),
                ),
            ),
            Err(TransitionError::BatchIdentityConflict)
        );
    }

    #[test]
    fn replay_refuses_tampered_audience_evidence() {
        let e = audience_engine(vec![], known(TRUSTED, Audience::restricted([corp_reader("alice")])));
        let records = vec![opened(&e)];
        let decision = e
            .handle(
                &viewing(&e, &records),
                evidenced_batch(
                    "b1",
                    vec![send_to("@team")],
                    source_evidence(vec![user_group(
                        "team",
                        vec![slack_member("slack:UA", Some("alice@corp.com"))],
                    )]),
                ),
            )
            .expect("the batch decides");
        let facts = appended_facts(decision);
        assert_eq!(e.validate_replay(&[records.clone(), facts.clone()].concat()), Ok(()));
        let tampered = |mutate: &dyn Fn(&mut Fact)| {
            let mut facts = facts.clone();
            for fact in &mut facts {
                mutate(fact);
            }
            e.validate_replay(&[records.clone(), facts].concat())
        };
        // An opening whose pins differ from what its act consumed is forged.
        assert_eq!(
            tampered(&|fact| {
                if let Fact::DispatchOpened { evidence, .. } = fact {
                    *evidence = crate::audience::AudienceEvidence::default();
                }
            }),
            Err(TransitionRefusal::ForgedEvidence)
        );
        // A decision recorded under answers that cannot back it is refused.
        assert_eq!(
            tampered(&|fact| {
                if let Fact::ProposalBatchDecided { evidence, .. } = fact {
                    *evidence = source_evidence(vec![user_group(
                        "team",
                        vec![slack_member("slack:UB", Some("bob@corp.com"))],
                    )]);
                }
            }),
            Err(TransitionRefusal::MisdecidedBatch),
            "the recorded release does not follow from the substituted answer"
        );
        assert_eq!(
            tampered(&|fact| {
                if let Fact::ProposalBatchDecided { evidence, .. } = fact {
                    *evidence = crate::audience::AudienceEvidence::default();
                }
            }),
            Err(TransitionRefusal::ForgedEvidence),
            "a decision over an unanswered symbolic audience claims answers it does not pin"
        );
    }

    #[test]
    fn a_call_approval_extends_the_offered_calls_pins() {
        let officer = crate::authority::Authority {
            name: AuthorityName::new("officer"),
            mandate: crate::authority::Mandate {
                reader_ceiling: Some(DeclaredAudience::literal(Audience::public())),
                ..crate::authority::Mandate::default()
            },
            scope: crate::authority::Scope::default(),
            hint: None,
        };
        let alice = Audience::restricted([corp_reader("alice")]);
        let e = audience_engine(vec![officer], known(TRUSTED, alice.clone()));
        let opening = vec![opened(&e)];
        let evidence = source_evidence(vec![user_group(
            "wide",
            vec![
                slack_member("slack:UA", Some("alice@corp.com")),
                slack_member("slack:UC", Some("carol@other.com")),
            ],
        )]);
        let blocked = appended_facts(
            e.handle(
                &viewing(&e, &opening),
                evidenced_batch("b1", vec![send_to("@wide")], evidence.clone()),
            )
            .expect("the batch decides"),
        );
        let (offer, plan) = opened_offers(&blocked)[0].clone();
        let log = [opening, blocked].concat();
        let approved = appended_facts(
            execute_offer(
                &e,
                &log,
                offer,
                OfferOutcome::Approved(evidence_for(offer, &plan, "send", partial(TRUSTED, alice))),
            )
            .expect("the officer's ruling arms the call"),
        );
        assert!(approved.iter().any(|fact| matches!(
            fact,
            Fact::CallApproved { evidence: pinned, .. } if pinned.contains(&evidence)
        )));
        assert_eq!(e.validate_replay(&[log.clone(), approved.clone()].concat()), Ok(()));
        let mut tampered = approved;
        for fact in &mut tampered {
            if let Fact::CallApproved { evidence, .. } = fact {
                *evidence = crate::audience::AudienceEvidence::default();
            }
        }
        assert_eq!(
            e.validate_replay(&[log, tampered].concat()),
            Err(TransitionRefusal::ForgedEvidence),
            "an approval that drops the offer's pins is forged"
        );
    }

    fn wide_as_reported() -> Vec<crate::audience::MemberClaims> {
        vec![
            slack_member("slack:UA", Some("alice@corp.com")),
            slack_member("slack:UC", Some("carol@other.com")),
        ]
    }

    /// The officer's approval, armed over `wide` reported as alice and an outsider, for a
    /// proposal whose own evidence the test supplies: the spend reads that evidence under the
    /// approval's pins.
    fn spending_under_pins(fresh: crate::audience::AudienceEvidence) -> Result<EngineDecision, TransitionError> {
        let officer = crate::authority::Authority {
            name: AuthorityName::new("officer"),
            mandate: crate::authority::Mandate {
                reader_ceiling: Some(DeclaredAudience::literal(Audience::public())),
                ..crate::authority::Mandate::default()
            },
            scope: crate::authority::Scope::default(),
            hint: None,
        };
        let alice = Audience::restricted([corp_reader("alice")]);
        let e = audience_engine(vec![officer], known(TRUSTED, alice.clone()));
        let opening = vec![opened(&e)];
        let pinned = source_evidence(vec![user_group("wide", wide_as_reported())]);
        let blocked = appended_facts(
            e.handle(
                &viewing(&e, &opening),
                evidenced_batch("b1", vec![send_to("@wide")], pinned),
            )
            .expect("the batch decides"),
        );
        let (offer, plan) = opened_offers(&blocked)[0].clone();
        let log = [opening, blocked].concat();
        let approved = appended_facts(
            execute_offer(
                &e,
                &log,
                offer,
                OfferOutcome::Approved(evidence_for(offer, &plan, "send", partial(TRUSTED, alice))),
            )
            .expect("the officer's ruling arms the call"),
        );
        let log = [log, approved].concat();
        e.handle(&viewing(&e, &log), evidenced_batch("b2", vec![send_to("@wide")], fresh))
    }

    /// A fresh answer for a key the approval pinned is read under the pin: the same answer
    /// again is fine, a different one is a contradiction the spend refuses — never an entry
    /// silently dropped in favour of the pin.
    #[test]
    fn a_spend_refuses_a_fresh_answer_that_contradicts_a_pinned_key() {
        let same = source_evidence(vec![user_group("wide", wide_as_reported())]);
        assert!(
            spending_under_pins(same).is_ok(),
            "restating the pin is not a contradiction"
        );
        let contradicting = source_evidence(vec![user_group(
            "wide",
            vec![slack_member("slack:UB", Some("bob@corp.com"))],
        )]);
        assert!(matches!(
            spending_under_pins(contradicting),
            Err(TransitionError::ForeignEvidence(
                crate::audience::EvidenceRefusal::ContradictedPin { .. }
            ))
        ));
    }

    /// Two admissible evidence sets can still disagree once merged — the pinned selector and
    /// a fresh one report the same member under different verified addresses. The spend
    /// refuses the merged reading as it refuses any other conflicting claim.
    #[test]
    fn a_spend_refuses_a_merged_reading_whose_claims_conflict() {
        let conflicting = source_evidence(vec![user_group(
            "narrow",
            vec![slack_member("slack:UA", Some("mallory@other.com"))],
        )]);
        assert_eq!(
            spending_under_pins(conflicting).err(),
            Some(TransitionError::ForeignEvidence(
                crate::audience::EvidenceRefusal::ConflictingClaims {
                    id: "slack:UA".to_string()
                }
            ))
        );
    }

    #[test]
    fn replay_refuses_a_provider_admission_no_act_declared() {
        let e = batch_engine();
        let log = opening_log(&e);
        let decided = appended_facts(
            e.handle(
                &viewing(&e, &log),
                batch("b1", vec![exposed("seen", "the provider ran it")], Vec::new()),
            )
            .expect("an admission-only batch decides"),
        );
        assert_eq!(e.validate_replay(&[log.clone(), decided.clone()].concat()), Ok(()));

        let undeclared: Vec<Fact> = decided
            .iter()
            .filter(|fact| !matches!(fact, Fact::BasisAdvanced { .. }))
            .cloned()
            .collect();
        assert_eq!(
            e.validate_replay(&[log.clone(), undeclared].concat()),
            Err(TransitionRefusal::UndeclaredAdmission)
        );

        let child = TrajectoryId::new("child");
        let seeded = forked_child(&e, &log, &child);
        let after_seed = [log.clone(), seeded.clone()].concat();
        let ended = appended_facts(
            e.handle(
                &viewing(&e, &after_seed),
                child_report(&after_seed, &child, ChildSubmission::Void),
            )
            .expect("the child ends its errand"),
        );
        let forked = [log, seeded, ended].concat();
        let on_ended: Vec<Fact> = decided
            .into_iter()
            .map(|fact| match fact {
                Fact::BasisAdvanced { act, advance, .. } => Fact::BasisAdvanced {
                    trajectory: child.clone(),
                    act,
                    advance: crate::basis::BasisAdvance {
                        flows: [child.clone()].into(),
                        ..advance
                    },
                },
                Fact::ValueAdmitted { value, provenance, .. } => Fact::ValueAdmitted {
                    trajectory: child.clone(),
                    value,
                    provenance,
                },
                other => other,
            })
            .filter(|fact| !matches!(fact, Fact::ProposalBatchDecided { .. }))
            .collect();
        assert_eq!(
            e.validate_replay(&[forked, on_ended].concat()),
            Err(TransitionRefusal::BranchEnded)
        );
    }

    #[test]
    fn replay_refuses_one_batchs_admissions_split_across_two_decisions() {
        let e = batch_engine();
        let log = opening_log(&e);
        let decided = appended_facts(
            e.handle(
                &viewing(&e, &log),
                batch(
                    "b1",
                    vec![exposed("seen", "the first"), exposed("seen", "the second")],
                    Vec::new(),
                ),
            )
            .expect("an admission-only batch decides"),
        );
        assert_eq!(e.validate_replay(&[log.clone(), decided.clone()].concat()), Ok(()));

        let split = vec![
            decided[0].clone(),
            decided[1].clone(),
            decided[0].clone(),
            decided[2].clone(),
        ];
        assert_eq!(
            e.validate_replay(&[log, split].concat()),
            Err(TransitionRefusal::SplitAdmission)
        );
    }

    #[test]
    fn replay_refuses_a_decision_whose_own_declaration_is_missing() {
        let e = batch_engine();
        let opening = opening_log(&e);
        let decide = |log: &[Fact], id: &str| {
            appended_facts(
                e.handle(
                    &viewing(&e, log),
                    batch(id, Vec::new(), vec![raw(&call("emit", json!({})))]),
                )
                .expect("the batch decides"),
            )
        };
        let log = [opening.clone(), decide(&opening, "b1")].concat();
        let second = decide(&log, "b2");
        assert_eq!(e.validate_replay(&[log.clone(), second.clone()].concat()), Ok(()));

        let undeclared: Vec<Fact> = second
            .into_iter()
            .filter(|fact| !matches!(fact, Fact::BasisAdvanced { .. }))
            .collect();
        assert_eq!(
            e.validate_replay(&[log, undeclared].concat()),
            Err(TransitionRefusal::UndeclaredAdvance)
        );
    }

    #[test]
    fn a_decision_releases_exactly_what_the_ordered_composition_allows() {
        let e = batch_engine();
        let log = opening_log(&e);
        let decided = appended_facts(
            e.handle(
                &viewing(&e, &log),
                batch(
                    "b1",
                    Vec::new(),
                    vec![
                        raw(&call("quiet", json!({}))),
                        raw(&call("emit", json!({}))),
                        raw(&call("guard", json!({}))),
                    ],
                ),
            )
            .expect("the batch decides"),
        );
        assert_eq!(e.validate_replay(&[log.clone(), decided.clone()].concat()), Ok(()));

        let rewritten = |mutate: &dyn Fn(&mut Vec<DispatchId>)| {
            let mut facts = decided.clone();
            for fact in &mut facts {
                if let Fact::ProposalBatchDecided { released, .. } = fact {
                    mutate(released);
                }
            }
            e.validate_replay(&[log.clone(), facts].concat())
        };
        assert_eq!(
            rewritten(&|released| {
                released.pop();
            }),
            Err(TransitionRefusal::MisdecidedBatch)
        );
        assert_eq!(
            rewritten(&|released| released.swap(0, 1)),
            Err(TransitionRefusal::MisdecidedBatch)
        );
        assert_eq!(
            rewritten(&|released| {
                let first = released[0].clone();
                released.push(first);
            }),
            Err(TransitionRefusal::MisdecidedBatch)
        );
    }

    #[test]
    fn replay_refuses_a_rewritten_provider_admission() {
        let e = batch_engine();
        let log = opening_log(&e);
        let decided = appended_facts(
            e.handle(
                &viewing(&e, &log),
                batch(
                    "b1",
                    vec![exposed("seen", "the provider ran it")],
                    vec![raw(&call("quiet", json!({})))],
                ),
            )
            .expect("the batch decides"),
        );
        assert_eq!(e.validate_replay(&[log.clone(), decided.clone()].concat()), Ok(()));

        let rewritten = |mutate: &dyn Fn(&mut Fact)| {
            let mut facts = decided.clone();
            mutate(&mut facts[1]);
            e.validate_replay(&[log.clone(), facts].concat())
        };
        assert_eq!(
            rewritten(&|fact| {
                if let Fact::ValueAdmitted { value, .. } = fact {
                    value.label = Label::new(TRUSTED, Audience::restricted([ReaderId::new("forged")]));
                }
            }),
            Err(TransitionRefusal::ForgedLabel)
        );
        assert_eq!(
            rewritten(&|fact| {
                if let Fact::ValueAdmitted {
                    provenance: Provenance::ProviderRun { effects, .. },
                    ..
                } = fact
                {
                    *effects = EffectSet::default();
                }
            }),
            Err(TransitionRefusal::EffectsMismatch)
        );
        assert_eq!(
            rewritten(&|fact| {
                if let Fact::ValueAdmitted {
                    provenance: Provenance::ProviderRun { position, .. },
                    ..
                } = fact
                {
                    *position = 1;
                }
            }),
            Err(TransitionRefusal::WrongAdmissionPosition)
        );
        assert_eq!(
            rewritten(&|fact| {
                if let Fact::ValueAdmitted {
                    provenance: Provenance::ProviderRun { tool, .. },
                    ..
                } = fact
                {
                    *tool = ToolName::new("quiet");
                }
            }),
            Err(TransitionRefusal::NotProviderRun("quiet".to_string()))
        );
        let admission_only = appended_facts(
            e.handle(
                &viewing(&e, &log),
                batch("b2", vec![exposed("seen", "the provider ran it")], Vec::new()),
            )
            .expect("an admission-only batch decides"),
        );
        let mut reordered = admission_only.clone();
        reordered.swap(1, 2);
        assert_eq!(e.validate_replay(&[log.clone(), admission_only].concat()), Ok(()));
        assert_eq!(
            e.validate_replay(&[log, reordered].concat()),
            Err(TransitionRefusal::AdmissionAfterDecision)
        );
    }

    #[test]
    fn a_batch_admits_every_result_it_exposed() {
        let e = batch_engine();
        let log = opening_log(&e);
        let decision = e
            .handle(
                &viewing(&e, &log),
                batch(
                    "b1",
                    vec![exposed("seen", "the first"), exposed("seen", "the second")],
                    vec![raw(&call("wire", json!({})))],
                ),
            )
            .expect("the batch decides");
        let (released, _) = answered(&decision);
        assert_eq!(tool_names(released), ["wire"]);
        let facts = appended_facts(decision);
        let slots: Vec<u32> = facts
            .iter()
            .filter_map(|fact| match fact {
                Fact::ValueAdmitted {
                    provenance: Provenance::ProviderRun { position, .. },
                    ..
                } => Some(*position),
                _ => None,
            })
            .collect();
        assert_eq!(slots, [0, 1]);
        assert_eq!(e.validate_replay(&[log.clone(), facts.clone()].concat()), Ok(()));

        let child = TrajectoryId::new("child");
        let seeded = forked_child(&e, &log, &child);
        let forked = [log, seeded].concat();
        let decided = appended_facts(
            e.handle(
                &viewing(&e, &forked),
                batch(
                    "b2",
                    vec![exposed("seen", "the first"), exposed("seen", "the second")],
                    Vec::new(),
                ),
            )
            .expect("the batch decides"),
        );
        assert_eq!(e.validate_replay(&[forked.clone(), decided.clone()].concat()), Ok(()));
        let mut elsewhere = decided;
        if let Fact::ValueAdmitted { trajectory, .. } = &mut elsewhere[2] {
            *trajectory = child;
        }
        assert_eq!(
            e.validate_replay(&[forked, elsewhere].concat()),
            Err(TransitionRefusal::ForeignAdmission)
        );
    }

    fn returning_registry(sanitizers: Vec<crate::authority::Sanitizer>) -> RegistryConfig {
        RegistryConfig {
            annotators: vec![],
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: declared(vec![
                plain_tool("spawn"),
                open_tool("fetch"),
                suspicious_read(),
                internal_read(),
                suspicious_internal_read(),
            ]),
            authorities: vec![],
            sanitizers,
            audience: crate::audience::AudienceConfig::default(),
        }
    }

    fn lifting_sanitizer(name: &str) -> crate::authority::Sanitizer {
        crate::authority::Sanitizer {
            name: SanitizerName::new(name),
            on: crate::authority::SanitizerPoints {
                input: false,
                output: true,
            },
            transition: crate::authority::DeclaredTransition::Trust {
                from_floor: SUSPICIOUS,
                to: TRUSTED,
            },
            scope: crate::authority::Scope::default(),
            hint: None,
        }
    }

    fn pending_stage_of(decision: &EngineDecision) -> &PendingReturnStage {
        match &decision.follow_up {
            FollowUp::Child(ChildFollowUp::Pending(stage)) => stage,
            other => panic!("expected a pending return stage, got {other:?}"),
        }
    }

    fn with_fetched_page(e: &Engine, log: Vec<Fact>, child: &TrajectoryId) -> Vec<Fact> {
        let fetch = call("fetch", json!({}));
        let released = e
            .handle(
                &viewing(e, &log),
                batch_on(child, "bf", Vec::new(), vec![raw(&fetch)], None),
            )
            .expect("the child's fetch releases");
        let released = appended_facts(released);
        let dispatch = released
            .iter()
            .find_map(|fact| match fact {
                Fact::DispatchOpened {
                    trajectory, dispatch, ..
                } if trajectory == child => Some(dispatch.clone()),
                _ => None,
            })
            .expect("the release opens the dispatch");
        let opened = [log, released].concat();
        let admitted = e
            .handle(
                &viewing(e, &opened),
                EngineEvent::Outcome(ToolReport {
                    dispatch,
                    outcome: ToolOutcome::Success {
                        body: OutcomeBody::Available(ValueBody::new("page")),
                    },
                    evidence: Vec::new(),
                    offer_nonce: nonce(),
                    audience: crate::audience::AudienceEvidence::default(),
                }),
            )
            .expect("the open result admits");
        [opened, appended_facts(admitted)].concat()
    }

    fn merged_crossing(crossing: crate::branch::RawCrossing) -> Vec<Fact> {
        match crossing {
            crate::branch::RawCrossing::Merged(facts) => facts,
            crate::branch::RawCrossing::Narrows(narrowing) => {
                panic!("expected a merged crossing, got {narrowing:?}")
            }
        }
    }

    fn fork_in(log: &[Fact], child: &TrajectoryId) -> ForkId {
        log.iter()
            .find_map(|fact| match fact {
                Fact::ForkOpened { trajectory, fork } if trajectory == child => Some(fork.clone()),
                _ => None,
            })
            .expect("the fork opened")
    }

    /// Authority evidence an `Approved` outcome may not carry: a plan that assigns no
    /// authority is accepted only with an empty vector, so any entry is a mismatch.
    fn stray_evidence(offer: crate::value::OfferId) -> crate::execute::AuthorityEvidence {
        crate::execute::AuthorityEvidence {
            offer,
            authority: crate::names::AuthorityName::new("officer"),
            covers: Vec::new(),
            reviewed: crate::execute::AuthorityReview {
                tool: ToolName::new("anything"),
                trajectory_label: established(Trust::new(0), Audience::public()),
            },
        }
    }

    fn return_offer(log: &[Fact], hop: bool) -> crate::value::OfferId {
        log.iter()
            .rev()
            .find_map(|fact| match fact {
                Fact::OfferOpened {
                    offer,
                    plan,
                    subject: crate::basis::SubjectKey::Return(_),
                    ..
                } if plan.hop().is_some() == hop => Some(*offer),
                _ => None,
            })
            .expect("the stage offers the plan")
    }

    fn evidenced_report(log: &[Fact], child: &TrajectoryId, body: &ValueBody, evidence: Vec<Evidence>) -> EngineEvent {
        EngineEvent::ChildReturn(ChildReport {
            child: child.clone(),
            fork: fork_in(log, child),
            submission: ChildSubmission::Value { body: body.clone() },
            evidence,
            offer_nonce: nonce(),
            audience: crate::audience::AudienceEvidence::default(),
        })
    }

    #[test]
    fn a_narrowing_fork_return_transfers_custody_and_opens_the_parents_stage() {
        let e = engine(vec![plain_tool("spawn"), suspicious_read(), suspicious_internal_read()]);
        let child = TrajectoryId::new("child");
        let internal = Audience::restricted([ReaderId::new("insider")]);
        let mut log = spawn_family(&e, None, &child);
        reads(&e, &mut log, &child, "read_suspicious_internal");
        let body = ValueBody::new("what I found");

        let decision = e
            .handle(
                &viewing(&e, &log),
                child_report(&log, &child, ChildSubmission::Value { body: body.clone() }),
            )
            .expect("a narrowing fork submission transfers custody");
        let stage = pending_stage_of(&decision).clone();
        assert_eq!(
            stage.residual,
            Narrowing {
                from: established(TRUSTED, Audience::public()),
                to: established(SUSPICIOUS, internal.clone()),
            }
        );
        assert_eq!(stage.offers.len(), 1);

        let appended = appended_facts(decision);
        let ended = [log.clone(), appended.clone()].concat();
        assert_eq!(e.validate_replay(&ended), Ok(()));
        let submitted = ended
            .iter()
            .filter_map(|fact| match fact {
                Fact::ReturnSubmitted {
                    trajectory,
                    fork,
                    parent,
                    digest,
                    body: stored,
                    policy,
                    ..
                } => Some((trajectory, fork, parent, digest, stored, policy)),
                _ => None,
            })
            .collect::<Vec<_>>();
        let [(trajectory, fork, parent, digest, stored, policy)] = submitted.as_slice() else {
            panic!("the raw payload is persisted exactly once");
        };
        assert_eq!((*trajectory, *parent), (&child, &traj()));
        assert_eq!(*fork, &fork_in(&log, &child));
        assert_eq!(*digest, &RawResultDigest::of(body.as_str().as_bytes()));
        assert_eq!(*stored, &body);
        assert_eq!(*policy, &ReturnPolicy::Raw);
        assert!(appended.iter().any(|fact| matches!(fact, Fact::OfferOpened { .. })));
        assert!(
            appended
                .iter()
                .all(|fact| !matches!(fact, Fact::OfferOpened { trajectory, .. } if trajectory != &traj()))
        );

        let after = Projection::build(&ended, ended.len() as u64);
        let parent = traj();
        let views = after.view(&parent);
        assert!(views.has_ended(&child));
        assert!(views.child_return(&ChildReturnId::new(child.clone(), 0)).is_none());
        assert_eq!(views.current_label(), established(TRUSTED, Audience::public()));

        let again = e
            .handle(
                &viewing(&e, &ended),
                child_report(&ended, &child, ChildSubmission::Value { body: body.clone() }),
            )
            .expect("the repeat answers from the record");
        assert_eq!(again.append, None);
        assert_eq!(pending_stage_of(&again).offers, stage.offers);

        assert_eq!(
            e.handle(
                &viewing(&e, &ended),
                child_report(
                    &ended,
                    &child,
                    ChildSubmission::Value {
                        body: ValueBody::new("another answer"),
                    },
                ),
            ),
            Err(TransitionError::BranchEnded)
        );
        assert_eq!(
            e.handle(
                &viewing(&e, &ended),
                child_report(&ended, &child, ChildSubmission::Void)
            ),
            Err(TransitionError::BranchEnded)
        );
    }

    #[test]
    fn the_parents_acceptance_crosses_the_submitted_return() {
        let e = engine(vec![plain_tool("spawn"), suspicious_read(), suspicious_internal_read()]);
        let child = TrajectoryId::new("child");
        let internal = Audience::restricted([ReaderId::new("insider")]);
        let mut log = spawn_family(&e, None, &child);
        reads(&e, &mut log, &child, "read_suspicious_internal");
        let body = ValueBody::new("what I found");
        let decision = e
            .handle(
                &viewing(&e, &log),
                child_report(&log, &child, ChildSubmission::Value { body: body.clone() }),
            )
            .expect("the submission transfers custody");
        let stage = pending_stage_of(&decision).clone();
        let ended = [log, appended_facts(decision)].concat();

        let accept = return_offer(&ended, false);
        let crossed = execute_offer(&e, &ended, accept, OfferOutcome::Approved(vec![]))
            .expect("the parent accepts the exact residual");
        assert!(matches!(
            offer_answer(&crossed),
            OfferFollowUp::Admitted { value } if value == &body
        ));
        let crossing = appended_facts(crossed);
        assert!(crossing.iter().any(|fact| matches!(
            fact,
            Fact::ChildReturn {
                derivation: ReturnDerivation::Raw,
                value,
                ..
            } if value.body == body && value.label == known(SUSPICIOUS, internal.clone())
        )));
        assert!(crossing.iter().any(|fact| matches!(
            fact,
            Fact::ChildReturnAcceptance { narrowing, .. } if narrowing == &stage.residual
        )));
        let merged = [ended, crossing].concat();
        assert_eq!(e.validate_replay(&merged), Ok(()));
        let views = Projection::build(&merged, merged.len() as u64);
        assert_eq!(views.view(&traj()).current_label(), established(SUSPICIOUS, internal));

        let repeat = execute_offer(&e, &merged, accept, OfferOutcome::Approved(vec![])).expect("the repeat answers");
        assert_eq!(repeat.append, None);
        assert!(matches!(
            offer_answer(&repeat),
            OfferFollowUp::Admitted { value } if value == &body
        ));
        assert_eq!(
            execute_offer(
                &e,
                &merged,
                accept,
                OfferOutcome::Approved(vec![stray_evidence(accept)])
            )
            .map(|_| ()),
            Err(TransitionError::PlanOutcomeMismatch),
            "an acceptance carrying evidence is refused after the offer ends, as it is before"
        );
        let again = e
            .handle(
                &viewing(&e, &merged),
                child_report(&merged, &child, ChildSubmission::Value { body: body.clone() }),
            )
            .expect("the repeat answers");
        assert_eq!(again.append, None);
        assert_eq!(
            again.follow_up,
            FollowUp::Child(ChildFollowUp::Merged { admitted: body })
        );
    }

    fn return_hops(log: &[Fact]) -> Vec<SanitizerName> {
        log.iter()
            .filter_map(|fact| match fact {
                Fact::OfferOpened {
                    plan,
                    subject: crate::basis::SubjectKey::Return(_),
                    ..
                } => plan.hop().cloned(),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_return_stage_offers_only_the_output_sanitizers_that_help() {
        let internal = Audience::restricted([ReaderId::new("insider")]);
        let input_only = crate::authority::Sanitizer {
            name: SanitizerName::new("input-declassify"),
            on: crate::authority::SanitizerPoints {
                input: true,
                output: false,
            },
            transition: crate::authority::DeclaredTransition::Audience {
                from_includes: DeclaredAudience::literal(internal.clone()),
                to: DeclaredAudience::literal(Audience::public()),
            },
            scope: crate::authority::Scope::default(),
            hint: None,
        };
        let sideways = crate::authority::Sanitizer {
            name: SanitizerName::new("to-finance"),
            on: crate::authority::SanitizerPoints {
                input: false,
                output: true,
            },
            transition: crate::authority::DeclaredTransition::Audience {
                from_includes: DeclaredAudience::literal(internal.clone()),
                to: DeclaredAudience::restricted([ReaderId::new("finance")]),
            },
            scope: crate::authority::Scope::default(),
            hint: None,
        };
        let e = open_engine(returning_registry(vec![
            lifting_sanitizer("redactor"),
            input_only,
            sideways,
        ]));
        let child = TrajectoryId::new("child");
        let mut log = spawn_family(&e, None, &child);
        reads(&e, &mut log, &child, "read_suspicious_internal");
        let decision = e
            .handle(
                &viewing(&e, &log),
                child_report(
                    &log,
                    &child,
                    ChildSubmission::Value {
                        body: ValueBody::new("what I found"),
                    },
                ),
            )
            .expect("the submission transfers custody");
        assert_eq!(
            pending_stage_of(&decision).offers.len(),
            2,
            "acceptance and the one hop that helps"
        );
        assert_eq!(
            return_hops(&appended_facts(decision)),
            vec![SanitizerName::new("redactor")]
        );
    }

    #[test]
    fn an_unconfined_child_return_stages_acceptance_alone() {
        let cfg = returning_registry(vec![lifting_sanitizer("redactor")]);
        let mut declaration = crate::profile::covering_declaration(&cfg);
        declaration.confined_child_return = false;
        let e = Engine::open(DeploymentPolicy {
            registry: cfg,
            planner_cap: crate::registry::PlannerCap::default(),
            dialect: PolicyDialectVersion::new(1),
            child_return: ReturnPolicy::Raw,
            profile: declaration,
        })
        .expect("an unconfined child return opens");
        let child = TrajectoryId::new("child");
        let mut log = spawn_family(&e, None, &child);
        reads(&e, &mut log, &child, "read_suspicious_internal");
        let decision = e
            .handle(
                &viewing(&e, &log),
                child_report(
                    &log,
                    &child,
                    ChildSubmission::Value {
                        body: ValueBody::new("what I found"),
                    },
                ),
            )
            .expect("the submission transfers custody");
        assert_eq!(pending_stage_of(&decision).offers.len(), 1);
        assert!(return_hops(&appended_facts(decision)).is_empty());
    }

    #[test]
    fn a_merge_that_restricts_the_parent_replays_only_with_its_acceptance() {
        let e = engine(vec![plain_tool("spawn"), suspicious_read(), suspicious_internal_read()]);
        let child = TrajectoryId::new("child");
        let mut log = spawn_family(&e, None, &child);
        reads(&e, &mut log, &child, "read_suspicious_internal");
        let decision = e
            .handle(
                &viewing(&e, &log),
                child_report(
                    &log,
                    &child,
                    ChildSubmission::Value {
                        body: ValueBody::new("what I found"),
                    },
                ),
            )
            .expect("the submission transfers custody");
        let ended = [log, appended_facts(decision)].concat();
        let crossing = appended_facts(
            execute_offer(&e, &ended, return_offer(&ended, false), OfferOutcome::Approved(vec![]))
                .expect("the parent accepts the exact residual"),
        );
        assert_eq!(e.validate_replay(&[ended.clone(), crossing.clone()].concat()), Ok(()));

        let stripped: Vec<Fact> = crossing
            .into_iter()
            .filter(|fact| !matches!(fact, Fact::ChildReturnAcceptance { .. }))
            .collect();
        assert_eq!(
            e.validate_replay(&[ended, stripped].concat()),
            Err(TransitionRefusal::ReturnNarrowsParent)
        );
    }

    #[test]
    fn an_inapplicable_sanitizer_charges_the_return_no_resolution() {
        let mut scoped = lifting_sanitizer("scoped-lifter");
        scoped.scope = crate::authority::Scope {
            tags: vec![crate::names::TagName::new("web")],
        };
        let mut cfg = returning_registry(vec![scoped]);
        // A tool the sanitizer's scope reaches, so the sanitizer is one a result could
        // meet. A child return originates from no tool, which is what leaves it unreached.
        cfg.tools
            .push(crate::contract::ToolDeclaration::Declared(ToolAnnotation {
                tags: vec![crate::names::TagName::new("web")],
                ..open_tool("browse")
            }));
        let e = open_engine(cfg);
        let child = TrajectoryId::new("child");
        let mut log = spawn_family(&e, None, &child);
        reads(&e, &mut log, &child, "read_suspicious_internal");
        let log = with_fetched_page(&e, log, &child);
        let body = ValueBody::new("what I found");
        let decision = e
            .handle(
                &viewing(&e, &log),
                child_report(&log, &child, ChildSubmission::Value { body: body.clone() }),
            )
            .expect("the submission transfers custody");
        let stage = pending_stage_of(&decision);
        assert_eq!(stage.offers.len(), 1, "acceptance alone — no hop, no resolution");
        let appended = appended_facts(decision);
        assert!(appended.iter().any(|fact| matches!(fact, Fact::ReturnSubmitted { .. })));
        assert_eq!(e.validate_replay(&[log, appended].concat()), Ok(()));
    }

    #[test]
    fn a_definitively_inapplicable_sanitizer_charges_the_return_no_resolution() {
        let unreachable_from = crate::authority::Sanitizer {
            name: SanitizerName::new("external-scrub"),
            on: crate::authority::SanitizerPoints {
                input: false,
                output: true,
            },
            transition: crate::authority::DeclaredTransition::Audience {
                from_includes: DeclaredAudience::restricted([ReaderId::new("external")]),
                to: DeclaredAudience::literal(Audience::public()),
            },
            scope: crate::authority::Scope::default(),
            hint: None,
        };
        let e = open_engine(returning_registry(vec![unreachable_from]));
        let child = TrajectoryId::new("child");
        let mut log = spawn_family(&e, None, &child);
        reads(&e, &mut log, &child, "read_suspicious_internal");
        let log = with_fetched_page(&e, log, &child);
        let body = ValueBody::new("what I found");
        let decision = e
            .handle(
                &viewing(&e, &log),
                child_report(&log, &child, ChildSubmission::Value { body: body.clone() }),
            )
            .expect("the submission transfers custody");
        let stage = pending_stage_of(&decision);
        assert_eq!(
            stage.offers.len(),
            1,
            "acceptance alone — the failed `from` is not resolvable away"
        );
        let appended = appended_facts(decision);
        assert!(appended.iter().any(|fact| matches!(fact, Fact::ReturnSubmitted { .. })));
        assert_eq!(e.validate_replay(&[log, appended].concat()), Ok(()));
    }

    #[test]
    fn an_acceptance_crosses_after_the_parents_own_fold_moved() {
        let e = engine(vec![plain_tool("spawn"), suspicious_read(), suspicious_internal_read()]);
        let child = TrajectoryId::new("child");
        let internal = Audience::restricted([ReaderId::new("insider")]);
        let mut log = spawn_family(&e, None, &child);
        reads(&e, &mut log, &child, "read_suspicious_internal");
        let body = ValueBody::new("what I found");
        let decision = e
            .handle(
                &viewing(&e, &log),
                child_report(&log, &child, ChildSubmission::Value { body: body.clone() }),
            )
            .expect("the submission transfers custody");
        let stage = pending_stage_of(&decision).clone();
        let mut ended = [log, appended_facts(decision)].concat();
        reads(&e, &mut ended, &traj(), "read_suspicious");
        let stale = return_offer(&ended, false);
        assert_eq!(
            execute_offer(&e, &ended, stale, OfferOutcome::Approved(vec![])),
            Err(TransitionError::StaleOffer)
        );
        let redriven = e
            .handle(
                &viewing(&e, &ended),
                EngineEvent::ChildReturn(ChildReport {
                    child: child.clone(),
                    fork: fork_in(&ended, &child),
                    submission: ChildSubmission::Value { body: body.clone() },
                    evidence: Vec::new(),
                    offer_nonce: crate::value::OfferNonce::new([9u8; 32]),
                    audience: crate::audience::AudienceEvidence::default(),
                }),
            )
            .expect("the re-drive plans the stage again under fresh entropy");
        let restage = pending_stage_of(&redriven).clone();
        assert_eq!(restage.residual, stage.residual);
        assert_ne!(restage.offers[0].0, stale);
        let ended = [ended, appended_facts(redriven)].concat();

        let crossed = execute_offer(&e, &ended, restage.offers[0].0, OfferOutcome::Approved(vec![]))
            .expect("the acceptance crosses the pinned residual over the moved fold");
        let crossing = appended_facts(crossed);
        assert!(crossing.iter().any(|fact| matches!(
            fact,
            Fact::ChildReturnAcceptance { narrowing, .. } if narrowing == &stage.residual
        )));
        let merged = [ended, crossing].concat();
        assert_eq!(e.validate_replay(&merged), Ok(()));
        let views = Projection::build(&merged, merged.len() as u64);
        assert_eq!(views.view(&traj()).current_label(), established(SUSPICIOUS, internal));
    }

    #[test]
    fn a_staged_sanitizer_hop_replaces_the_candidate_and_replans() {
        let e = open_engine(returning_registry(vec![lifting_sanitizer("redactor")]));
        let child = TrajectoryId::new("child");
        let internal = Audience::restricted([ReaderId::new("insider")]);
        let mut log = spawn_family(&e, None, &child);
        reads(&e, &mut log, &child, "read_suspicious_internal");
        let body = ValueBody::new("what I found");
        let decision = e
            .handle(
                &viewing(&e, &log),
                child_report(&log, &child, ChildSubmission::Value { body: body.clone() }),
            )
            .expect("the submission transfers custody");
        assert_eq!(pending_stage_of(&decision).offers.len(), 2);
        let ended = [log, appended_facts(decision)].concat();
        let hop = return_offer(&ended, true);
        let raw_digest = RawResultDigest::of(body.as_str().as_bytes());

        assert_eq!(
            execute_offer(
                &e,
                &ended,
                hop,
                OfferOutcome::Derived(Evidence::Sanitizer {
                    sanitizer: SanitizerName::new("redactor"),
                    source: RawResultDigest::of(b"other bytes"),
                    derived: ValueBody::new("clean"),
                }),
            ),
            Err(TransitionError::EvidenceMismatch)
        );

        let clean = ValueBody::new("clean");
        let hopped = execute_offer(
            &e,
            &ended,
            hop,
            OfferOutcome::Derived(Evidence::Sanitizer {
                sanitizer: SanitizerName::new("redactor"),
                source: raw_digest,
                derived: clean.clone(),
            }),
        )
        .expect("the hop lands the successor candidate");
        let restaged = match offer_answer(&hopped) {
            OfferFollowUp::ReturnStaged(stage) => (**stage).clone(),
            other => panic!("a still-narrowing successor re-stages, got {other:?}"),
        };
        assert_eq!(restaged.label, known(TRUSTED, internal.clone()));
        assert_eq!(
            restaged.residual,
            Narrowing {
                from: established(TRUSTED, Audience::public()),
                to: established(TRUSTED, internal.clone()),
            }
        );
        assert_eq!(restaged.offers.len(), 1);
        let staged_log = [ended, appended_facts(hopped)].concat();
        assert_eq!(e.validate_replay(&staged_log), Ok(()));

        let mut reforged = staged_log.clone();
        for fact in &mut reforged {
            if let Fact::CandidateDerived {
                derived: DerivedCandidate::Return { from, .. },
                ..
            } = fact
            {
                *from = ConfinedFrom::Bound;
            }
        }
        assert_eq!(
            e.validate_replay(&reforged),
            Err(TransitionRefusal::UndischargedAcceptance)
        );

        let accepted = execute_offer(&e, &staged_log, restaged.offers[0].0, OfferOutcome::Approved(vec![]))
            .expect("the acceptance crosses the derivation");
        assert!(matches!(
            offer_answer(&accepted),
            OfferFollowUp::Admitted { value } if value == &clean
        ));
        let crossing = appended_facts(accepted);
        assert!(crossing.iter().any(|fact| matches!(
            fact,
            Fact::ChildReturn {
                derivation: ReturnDerivation::Sanitized { sanitizer, raw_digest: recorded, .. },
                value,
                ..
            } if sanitizer.as_str() == "redactor" && recorded == &raw_digest && value.body == clean
        )));
        let merged = [staged_log, crossing].concat();
        assert_eq!(e.validate_replay(&merged), Ok(()));
        assert_eq!(
            Projection::build(&merged, merged.len() as u64)
                .view(&traj())
                .current_label(),
            established(TRUSTED, internal)
        );
    }

    #[test]
    fn a_hop_that_settles_the_residual_crosses_in_its_own_batch() {
        let e = open_engine(returning_registry(vec![lifting_sanitizer("redactor")]));
        let child = TrajectoryId::new("child");
        let mut log = spawn_family(&e, None, &child);
        reads(&e, &mut log, &child, "read_suspicious");
        let body = ValueBody::new("what I found");
        let decision = e
            .handle(
                &viewing(&e, &log),
                child_report(&log, &child, ChildSubmission::Value { body: body.clone() }),
            )
            .expect("the submission transfers custody");
        let ended = [log, appended_facts(decision)].concat();

        let clean = ValueBody::new("clean");
        let hopped = execute_offer(
            &e,
            &ended,
            return_offer(&ended, true),
            OfferOutcome::Derived(Evidence::Sanitizer {
                sanitizer: SanitizerName::new("redactor"),
                source: RawResultDigest::of(body.as_str().as_bytes()),
                derived: clean.clone(),
            }),
        )
        .expect("the settling hop crosses atomically");
        assert!(matches!(
            offer_answer(&hopped),
            OfferFollowUp::Admitted { value } if value == &clean
        ));
        let crossing = appended_facts(hopped);
        assert!(
            crossing
                .iter()
                .all(|fact| !matches!(fact, Fact::ChildReturnAcceptance { .. }))
        );
        assert!(crossing.iter().any(|fact| matches!(
            fact,
            Fact::Boundary {
                kind: crate::fact::BoundaryKind::Merge { .. },
                ..
            }
        )));
        assert_eq!(e.validate_replay(&[ended, crossing].concat()), Ok(()));
    }

    #[test]
    fn a_mandatory_binding_holds_custody_and_its_first_derivation_crosses() {
        let quarantine = ReturnPolicy::Sanitized(SanitizerName::new("quarantine"));
        let e = open_engine_returning(returning_registry(vec![lifting_sanitizer("quarantine")]), quarantine);
        let child = TrajectoryId::new("child");
        let mut log = spawn_family(&e, None, &child);
        reads(&e, &mut log, &child, "read_suspicious");
        let body = ValueBody::new("what I found");
        let raw_digest = RawResultDigest::of(body.as_str().as_bytes());

        let decision = e
            .handle(
                &viewing(&e, &log),
                child_report(&log, &child, ChildSubmission::Value { body: body.clone() }),
            )
            .expect("an applicable submission transfers custody");
        let request = FollowUp::Child(ChildFollowUp::Resolve(EvidenceRequest::Sanitizer {
            sanitizer: SanitizerName::new("quarantine"),
            source: raw_digest,
            body: body.clone(),
        }));
        assert_eq!(decision.follow_up, request);
        let ended = [log, appended_facts(decision)].concat();
        assert_eq!(e.validate_replay(&ended), Ok(()));
        assert!(ended.iter().any(|fact| matches!(
            fact,
            Fact::ReturnSubmitted { policy, .. } if policy == &ReturnPolicy::Sanitized(SanitizerName::new("quarantine"))
        )));
        assert!(
            Projection::build(&ended, ended.len() as u64)
                .view(&traj())
                .has_ended(&child)
        );

        let retry = e
            .handle(
                &viewing(&e, &ended),
                child_report(&ended, &child, ChildSubmission::Value { body: body.clone() }),
            )
            .expect("the handoff stays available");
        assert_eq!(retry.append, None);
        assert_eq!(retry.follow_up, request);

        let clean = ValueBody::new("clean");
        let derived = e
            .handle(
                &viewing(&e, &ended),
                evidenced_report(
                    &ended,
                    &child,
                    &body,
                    vec![Evidence::Sanitizer {
                        sanitizer: SanitizerName::new("quarantine"),
                        source: raw_digest,
                        derived: clean.clone(),
                    }],
                ),
            )
            .expect("the first valid derivation crosses");
        assert_eq!(
            derived.follow_up,
            FollowUp::Child(ChildFollowUp::Merged {
                admitted: clean.clone()
            })
        );
        let crossing = appended_facts(derived);
        assert!(crossing.iter().any(|fact| matches!(
            fact,
            Fact::CandidateDerived {
                derived: DerivedCandidate::Return {
                    from: ConfinedFrom::Bound,
                    ..
                },
                ..
            }
        )));
        assert!(crossing.iter().any(|fact| matches!(
            fact,
            Fact::ChildReturn { value, .. } if value.label == known(TRUSTED, Audience::public())
        )));
        let merged = [ended, crossing].concat();
        assert_eq!(e.validate_replay(&merged), Ok(()));

        let mut forged = merged.clone();
        for fact in &mut forged {
            if let Fact::ChildReturn { value, derivation, .. } = fact {
                *derivation = ReturnDerivation::Raw;
                *value = LabeledValue::new(body.clone(), known(SUSPICIOUS, Audience::public()));
            }
        }
        assert_eq!(e.validate_replay(&forged), Err(TransitionRefusal::ReturnPolicyMismatch));
    }

    #[test]
    fn a_narrowing_mandatory_derivation_enters_the_staged_pipeline() {
        let e = open_engine_returning(
            returning_registry(vec![lifting_sanitizer("quarantine")]),
            ReturnPolicy::Sanitized(SanitizerName::new("quarantine")),
        );
        let child = TrajectoryId::new("child");
        let internal = Audience::restricted([ReaderId::new("insider")]);
        let mut log = spawn_family(&e, None, &child);
        reads(&e, &mut log, &child, "read_suspicious_internal");
        let body = ValueBody::new("what I found");
        let decision = e
            .handle(
                &viewing(&e, &log),
                child_report(&log, &child, ChildSubmission::Value { body: body.clone() }),
            )
            .expect("the submission transfers custody");
        let ended = [log, appended_facts(decision)].concat();

        let clean = ValueBody::new("clean");
        let staged = e
            .handle(
                &viewing(&e, &ended),
                evidenced_report(
                    &ended,
                    &child,
                    &body,
                    vec![Evidence::Sanitizer {
                        sanitizer: SanitizerName::new("quarantine"),
                        source: RawResultDigest::of(body.as_str().as_bytes()),
                        derived: clean.clone(),
                    }],
                ),
            )
            .expect("the narrowing derivation stages");
        let stage = pending_stage_of(&staged).clone();
        assert_eq!(stage.label, known(TRUSTED, internal.clone()));
        assert_eq!(stage.offers.len(), 1);
        let pending = [ended, appended_facts(staged)].concat();
        assert_eq!(e.validate_replay(&pending), Ok(()));

        let accepted = execute_offer(&e, &pending, stage.offers[0].0, OfferOutcome::Approved(vec![]))
            .expect("the acceptance crosses the confined candidate");
        assert!(matches!(
            offer_answer(&accepted),
            OfferFollowUp::Admitted { value } if value == &clean
        ));
        let merged = [pending, appended_facts(accepted)].concat();
        assert_eq!(e.validate_replay(&merged), Ok(()));
        assert_eq!(
            Projection::build(&merged, merged.len() as u64)
                .view(&traj())
                .current_label(),
            established(TRUSTED, internal)
        );
    }

    #[test]
    fn an_unmet_mandate_rejects_the_submission_without_external_io() {
        let picky = crate::authority::Sanitizer {
            name: SanitizerName::new("picky"),
            on: crate::authority::SanitizerPoints {
                input: false,
                output: true,
            },
            transition: crate::authority::DeclaredTransition::Trust {
                from_floor: TRUSTED,
                to: TRUSTED,
            },
            scope: crate::authority::Scope::default(),
            hint: None,
        };
        let e = open_engine_returning(
            returning_registry(vec![picky]),
            ReturnPolicy::Sanitized(SanitizerName::new("picky")),
        );
        let child = TrajectoryId::new("child");
        let mut log = spawn_family(&e, None, &child);
        reads(&e, &mut log, &child, "read_suspicious");
        let body = ValueBody::new("what I found");

        let decision = e
            .handle(
                &viewing(&e, &log),
                child_report(&log, &child, ChildSubmission::Value { body: body.clone() }),
            )
            .expect("the rejection is a decision, not an error");
        assert_eq!(
            decision.follow_up,
            FollowUp::Child(ChildFollowUp::Rejected {
                reason: ReturnRejection::MandateUnmet,
            })
        );
        let appended = appended_facts(decision);
        let [rejected @ Fact::ReturnRejected { digest, reason, .. }] = appended.as_slice() else {
            panic!("only the typed terminal records, got {appended:?}");
        };
        assert_eq!(digest, &RawResultDigest::of(body.as_str().as_bytes()));
        assert_eq!(reason, &ReturnRejection::MandateUnmet);
        let json = serde_json::to_value(rejected).unwrap();
        let fields = json
            .get("ReturnRejected")
            .and_then(serde_json::Value::as_object)
            .expect("the record serializes under its own tag");
        assert_eq!(
            fields.keys().map(String::as_str).collect::<BTreeSet<_>>(),
            BTreeSet::from(["trajectory", "id", "fork", "digest", "reason"])
        );

        let ended = [log, appended].concat();
        assert_eq!(e.validate_replay(&ended), Ok(()));
        let views = Projection::build(&ended, ended.len() as u64);
        assert!(views.view(&traj()).has_ended(&child));
        assert_eq!(
            views.view(&traj()).current_label(),
            established(TRUSTED, Audience::public())
        );

        let repeat = e
            .handle(
                &viewing(&e, &ended),
                child_report(&ended, &child, ChildSubmission::Value { body: body.clone() }),
            )
            .expect("the repeat answers from the record");
        assert_eq!(repeat.append, None);
        assert_eq!(
            repeat.follow_up,
            FollowUp::Child(ChildFollowUp::Rejected {
                reason: ReturnRejection::MandateUnmet,
            })
        );
        assert_eq!(
            e.handle(
                &viewing(&e, &ended),
                child_report(
                    &ended,
                    &child,
                    ChildSubmission::Value {
                        body: ValueBody::new("another answer"),
                    },
                ),
            ),
            Err(TransitionError::BranchEnded)
        );

        let mut flipped = ended;
        for fact in &mut flipped {
            if let Fact::ReturnRejected { reason, .. } = fact {
                *reason = ReturnRejection::PreconditionUnmet;
            }
        }
        assert_eq!(
            e.validate_replay(&flipped),
            Err(TransitionRefusal::ReturnRecordMismatch)
        );
    }

    #[test]
    fn return_custody_records_replay_only_as_produced() {
        let e = engine(vec![plain_tool("spawn"), suspicious_read(), suspicious_internal_read()]);
        let child = TrajectoryId::new("child");
        let mut log = spawn_family(&e, None, &child);
        reads(&e, &mut log, &child, "read_suspicious_internal");
        let body = ValueBody::new("what I found");
        let decision = e
            .handle(
                &viewing(&e, &log),
                child_report(&log, &child, ChildSubmission::Value { body: body.clone() }),
            )
            .expect("the submission transfers custody");
        let ended = [log, appended_facts(decision)].concat();
        assert_eq!(e.validate_replay(&ended), Ok(()));

        let mutated = |mutate: &dyn Fn(&mut Fact)| {
            let mut forged = ended.clone();
            for fact in &mut forged {
                if matches!(fact, Fact::ReturnSubmitted { .. }) {
                    mutate(fact);
                }
            }
            e.validate_replay(&forged)
        };
        assert_eq!(
            mutated(&|fact| {
                let Fact::ReturnSubmitted { label, .. } = fact else {
                    unreachable!()
                };
                *label = partial(TRUSTED, Audience::public());
            }),
            Err(TransitionRefusal::ForgedLabel)
        );
        assert_eq!(
            mutated(&|fact| {
                let Fact::ReturnSubmitted { digest, .. } = fact else {
                    unreachable!()
                };
                *digest = RawResultDigest::of(b"forged");
            }),
            Err(TransitionRefusal::ReturnRecordMismatch)
        );
        assert_eq!(
            mutated(&|fact| {
                let Fact::ReturnSubmitted { body, .. } = fact else {
                    unreachable!()
                };
                *body = ValueBody::new("forged");
            }),
            Err(TransitionRefusal::ReturnRecordMismatch)
        );
        assert_eq!(
            mutated(&|fact| {
                let Fact::ReturnSubmitted { policy, .. } = fact else {
                    unreachable!()
                };
                *policy = ReturnPolicy::Sanitized(SanitizerName::new("ghost"));
            }),
            Err(TransitionRefusal::ForkReturnPolicyMismatch)
        );
        assert_eq!(
            mutated(&|fact| {
                let Fact::ReturnSubmitted { parent, .. } = fact else {
                    unreachable!()
                };
                *parent = TrajectoryId::new("stranger");
            }),
            Err(TransitionRefusal::ForeignTrajectory)
        );
        let submitted = ended
            .iter()
            .find(|fact| matches!(fact, Fact::ReturnSubmitted { .. }))
            .cloned()
            .expect("the custody record stands");
        assert_eq!(
            e.validate_replay(&[ended.clone(), vec![submitted]].concat()),
            Err(TransitionRefusal::BranchEnded)
        );
    }

    #[test]
    fn the_first_mandatory_derivation_is_never_an_offer() {
        let e = open_engine_returning(
            returning_registry(vec![lifting_sanitizer("quarantine")]),
            ReturnPolicy::Sanitized(SanitizerName::new("quarantine")),
        );
        let child = TrajectoryId::new("child");
        let mut log = spawn_family(&e, None, &child);
        reads(&e, &mut log, &child, "read_suspicious_internal");
        let body = ValueBody::new("what I found");
        let decision = e
            .handle(
                &viewing(&e, &log),
                child_report(&log, &child, ChildSubmission::Value { body: body.clone() }),
            )
            .expect("the submission transfers custody");
        let ended = [log, appended_facts(decision)].concat();
        let derived = e
            .handle(
                &viewing(&e, &ended),
                evidenced_report(
                    &ended,
                    &child,
                    &body,
                    vec![Evidence::Sanitizer {
                        sanitizer: SanitizerName::new("quarantine"),
                        source: RawResultDigest::of(body.as_str().as_bytes()),
                        derived: ValueBody::new("clean"),
                    }],
                ),
            )
            .expect("the mandatory derivation stages");
        let staged = [ended, appended_facts(derived)].concat();
        assert_eq!(e.validate_replay(&staged), Ok(()));

        let id = ChildReturnId::new(child.clone(), 0);
        let unopened = crate::value::OfferId::of_plan(
            &crate::value::BlockId::of_return(&nonce(), &id, crate::basis::SubjectGeneration::ZERO),
            0,
            b"forged",
        );
        let mut reforged = staged;
        for fact in &mut reforged {
            if let Fact::CandidateDerived {
                derived: DerivedCandidate::Return { from, .. },
                ..
            } = fact
            {
                *from = ConfinedFrom::Offer(unopened);
            }
        }
        assert_eq!(e.validate_replay(&reforged), Err(TransitionRefusal::UnknownOffer));
    }

    fn verdict_schema() -> serde_json::Value {
        json!({
            "type": "object",
            "properties": { "verdict": { "type": "string", "enum": ["allow", "deny"] } },
            "required": ["verdict"],
        })
    }

    #[test]
    fn a_conforming_shaped_crossing_retains_the_child_label() {
        let e = engine(vec![plain_tool("spawn"), suspicious_read(), suspicious_internal_read()]);
        let child = TrajectoryId::new("child");
        let mut log = spawn_family(&e, Some(&verdict_schema()), &child);
        reads(&e, &mut log, &child, "read_suspicious");
        let body = ValueBody::new(r#"{"verdict":"allow"}"#);
        let decision = e
            .handle(
                &viewing(&e, &log),
                child_report(&log, &child, ChildSubmission::Value { body: body.clone() }),
            )
            .expect("the conforming submission transfers custody");
        let stage = pending_stage_of(&decision).clone();
        assert_eq!(stage.offers.len(), 1);
        let ended = [log, appended_facts(decision)].concat();
        let accepted = execute_offer(&e, &ended, stage.offers[0].0, OfferOutcome::Approved(vec![]))
            .expect("the acceptance crosses the raw submission");
        let crossing = appended_facts(accepted);
        assert!(crossing.iter().any(|fact| matches!(
            fact,
            Fact::ChildReturn {
                derivation: ReturnDerivation::Raw,
                value,
                ..
            } if value.label == known(SUSPICIOUS, Audience::public()) && value.body == body
        )));
        let merged = [ended, crossing].concat();
        assert_eq!(e.validate_replay(&merged), Ok(()));
        assert_eq!(
            Projection::build(&merged, merged.len() as u64)
                .view(&traj())
                .current_label(),
            established(SUSPICIOUS, Audience::public())
        );
    }

    #[test]
    fn an_unshaped_fork_offers_no_attest_hop() {
        let e = open_engine(returning_registry(vec![lifting_sanitizer("attest-schema")]));
        let child = TrajectoryId::new("child");
        let mut log = spawn_family(&e, None, &child);
        reads(&e, &mut log, &child, "read_suspicious");
        let decision = e
            .handle(
                &viewing(&e, &log),
                child_report(
                    &log,
                    &child,
                    ChildSubmission::Value {
                        body: ValueBody::new("free text"),
                    },
                ),
            )
            .expect("the submission transfers custody");
        assert_eq!(pending_stage_of(&decision).offers.len(), 1);
    }

    #[test]
    fn a_shaped_fork_without_attest_follows_the_ordinary_lifecycle() {
        let e = open_engine(returning_registry(vec![lifting_sanitizer("redactor")]));
        let child = TrajectoryId::new("child");
        let mut log = spawn_family(&e, Some(&verdict_schema()), &child);
        reads(&e, &mut log, &child, "read_suspicious_internal");
        let decision = e
            .handle(
                &viewing(&e, &log),
                child_report(
                    &log,
                    &child,
                    ChildSubmission::Value {
                        body: ValueBody::new(r#"{"verdict":"allow"}"#),
                    },
                ),
            )
            .expect("the conforming submission transfers custody");
        assert_eq!(pending_stage_of(&decision).offers.len(), 2);
    }

    #[test]
    fn an_attest_hop_derives_the_bytes_unchanged_in_engine() {
        let e = open_engine(returning_registry(vec![lifting_sanitizer("attest-schema")]));
        let child = TrajectoryId::new("child");
        let internal = Audience::restricted([ReaderId::new("insider")]);
        let mut log = spawn_family(&e, Some(&verdict_schema()), &child);
        reads(&e, &mut log, &child, "read_suspicious_internal");
        let body = ValueBody::new(r#"{"verdict":"allow"}"#);
        let raw_digest = RawResultDigest::of(body.as_str().as_bytes());
        let decision = e
            .handle(
                &viewing(&e, &log),
                child_report(&log, &child, ChildSubmission::Value { body: body.clone() }),
            )
            .expect("the conforming submission transfers custody");
        assert_eq!(pending_stage_of(&decision).offers.len(), 2);
        let ended = [log, appended_facts(decision)].concat();
        let hop = return_offer(&ended, true);

        assert_eq!(
            e.offer_consults(&viewing(&e, &ended), &traj(), &hop),
            Ok(crate::transition::OfferConsult::Accept),
        );

        assert_eq!(
            execute_offer(
                &e,
                &ended,
                hop,
                OfferOutcome::Derived(Evidence::Sanitizer {
                    sanitizer: SanitizerName::new("attest-schema"),
                    source: raw_digest,
                    derived: body.clone(),
                }),
            ),
            Err(TransitionError::PlanOutcomeMismatch)
        );

        let hopped = execute_offer(&e, &ended, hop, OfferOutcome::Approved(vec![]))
            .expect("the engine applies the reserved builtin itself");
        let restaged = match offer_answer(&hopped) {
            OfferFollowUp::ReturnStaged(stage) => (**stage).clone(),
            other => panic!("the audience residual re-stages, got {other:?}"),
        };
        assert_eq!(restaged.label, known(TRUSTED, internal.clone()));
        assert_eq!(restaged.offers.len(), 1);
        let staged_log = [ended, appended_facts(hopped)].concat();
        assert_eq!(e.validate_replay(&staged_log), Ok(()));

        assert_eq!(
            e.offer_consults(&viewing(&e, &staged_log), &traj(), &hop),
            Ok(crate::transition::OfferConsult::Replay(OfferOutcome::Approved(
                Vec::new()
            ))),
        );

        let repeat = execute_offer(&e, &staged_log, hop, OfferOutcome::Approved(vec![]))
            .expect("the spent attest selection answers from the record");
        assert_eq!(repeat.append, None);
        assert!(matches!(offer_answer(&repeat), OfferFollowUp::ReturnStaged(_)));
        assert_eq!(
            execute_offer(
                &e,
                &staged_log,
                hop,
                OfferOutcome::Derived(Evidence::Sanitizer {
                    sanitizer: SanitizerName::new("attest-schema"),
                    source: raw_digest,
                    derived: body.clone(),
                }),
            ),
            Err(TransitionError::PlanOutcomeMismatch)
        );

        let accepted = execute_offer(&e, &staged_log, restaged.offers[0].0, OfferOutcome::Approved(vec![]))
            .expect("the acceptance crosses the attested value");
        assert!(matches!(
            offer_answer(&accepted),
            OfferFollowUp::Admitted { value } if value == &body
        ));
        let crossing = appended_facts(accepted);
        assert!(crossing.iter().any(|fact| matches!(
            fact,
            Fact::ChildReturn {
                derivation: ReturnDerivation::Sanitized { sanitizer, raw_digest: recorded, .. },
                value,
                ..
            } if sanitizer.as_str() == "attest-schema" && recorded == &raw_digest && value.body == body
        )));
        let merged = [staged_log, crossing].concat();
        assert_eq!(e.validate_replay(&merged), Ok(()));
        assert_eq!(
            Projection::build(&merged, merged.len() as u64)
                .view(&traj())
                .current_label(),
            established(TRUSTED, internal)
        );

        let mut forged = merged;
        for fact in &mut forged {
            if let Fact::CandidateDerived {
                derived: DerivedCandidate::Return { value, .. },
                ..
            } = fact
            {
                value.body = ValueBody::new(r#"{"verdict":"deny"}"#);
            }
        }
        assert_eq!(e.validate_replay(&forged), Err(TransitionRefusal::ForgedLabel));
    }

    #[test]
    fn a_tool_output_block_never_offers_the_reserved_attest_schema() {
        let e = open_engine(RegistryConfig {
            annotators: vec![],
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: declared(vec![ToolAnnotation {
                description: Some("A test tool.".to_string()),
                name: ToolName::new("fetch"),
                tags: vec![],
                delta: Delta {
                    trust: Some(SUSPICIOUS),
                    audience: None,
                },
                parameters: crate::params::ToolParameters::open(),
                emits: EffectSet::new([EffectKind::new("web.read")]).unwrap(),
                requires: Requires::default(),
            }]),
            authorities: vec![],
            sanitizers: vec![lifting_sanitizer("redactor"), lifting_sanitizer("attest-schema")],
            audience: crate::audience::AudienceConfig::default(),
        });
        let log = vec![opened(&e)];
        let blocked = proposed(&e, &log, "b1", nonce(), call("fetch", json!({}))).expect("the batch decides");
        let offered: Vec<_> = opened_offers(&appended_facts(blocked))
            .into_iter()
            .filter_map(|(_, plan)| plan.sanitizer().cloned())
            .collect();
        assert!(offered.contains(&SanitizerName::new("redactor")));
        assert!(!offered.iter().any(|name| name.is_attest_schema()));
    }

    #[test]
    fn a_fork_base_below_the_mandate_ceiling_excludes_the_attest_hop() {
        let attest = crate::authority::Sanitizer {
            name: SanitizerName::new("attest-schema"),
            on: crate::authority::SanitizerPoints {
                input: false,
                output: true,
            },
            transition: crate::authority::DeclaredTransition::Trust {
                from_floor: SUSPICIOUS,
                to: Trust::new(2),
            },
            scope: crate::authority::Scope::default(),
            hint: None,
        };
        let e = open_engine_at(
            RegistryConfig {
                annotators: vec![],
                trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into(), "gold".into()]),
                tools: declared(vec![plain_tool("spawn"), suspicious_read()]),
                authorities: vec![],
                sanitizers: vec![attest],
                audience: crate::audience::AudienceConfig::default(),
            },
            known(TRUSTED, Audience::public()),
        );
        let child = TrajectoryId::new("child");
        let mut log = spawn_family(&e, Some(&verdict_schema()), &child);
        reads(&e, &mut log, &child, "read_suspicious");
        let decision = e
            .handle(
                &viewing(&e, &log),
                child_report(
                    &log,
                    &child,
                    ChildSubmission::Value {
                        body: ValueBody::new(r#"{"verdict":"allow"}"#),
                    },
                ),
            )
            .expect("the submission transfers custody");
        assert_eq!(pending_stage_of(&decision).offers.len(), 1);
    }

    #[test]
    fn a_child_attest_binding_crosses_the_conforming_return_in_engine() {
        let e = open_engine_returning(
            returning_registry(vec![lifting_sanitizer("attest-schema")]),
            ReturnPolicy::Sanitized(SanitizerName::new("attest-schema")),
        );
        let child = TrajectoryId::new("child");
        let mut log = spawn_family(&e, Some(&verdict_schema()), &child);
        reads(&e, &mut log, &child, "read_suspicious");
        let body = ValueBody::new(r#"{"verdict":"allow"}"#);
        let decision = e
            .handle(
                &viewing(&e, &log),
                child_report(&log, &child, ChildSubmission::Value { body: body.clone() }),
            )
            .expect("the applicable submission crosses");
        assert_eq!(
            decision.follow_up,
            FollowUp::Child(ChildFollowUp::Merged { admitted: body.clone() })
        );
        let merged = [log, appended_facts(decision)].concat();
        assert_eq!(e.validate_replay(&merged), Ok(()));
        let views = Projection::build(&merged, merged.len() as u64);
        assert!(views.view(&traj()).has_ended(&child));
        assert_eq!(
            views.view(&traj()).current_label(),
            established(TRUSTED, Audience::public())
        );
        let repeat = e
            .handle(
                &viewing(&e, &merged),
                child_report(&merged, &child, ChildSubmission::Value { body: body.clone() }),
            )
            .expect("the repeat answers from the record");
        assert_eq!(repeat.append, None);
        assert_eq!(
            repeat.follow_up,
            FollowUp::Child(ChildFollowUp::Merged { admitted: body })
        );
    }

    #[test]
    fn a_shape_mismatch_under_a_child_attest_binding_rejects_terminally() {
        let e = open_engine_returning(
            returning_registry(vec![lifting_sanitizer("attest-schema")]),
            ReturnPolicy::Sanitized(SanitizerName::new("attest-schema")),
        );
        let child = TrajectoryId::new("child");
        let mut log = spawn_family(&e, Some(&verdict_schema()), &child);
        reads(&e, &mut log, &child, "read_suspicious");
        let body = ValueBody::new(r#"{"verdict":"maybe"}"#);
        let decision = e
            .handle(
                &viewing(&e, &log),
                child_report(&log, &child, ChildSubmission::Value { body: body.clone() }),
            )
            .expect("the rejection is a decision, not an error");
        assert_eq!(
            decision.follow_up,
            FollowUp::Child(ChildFollowUp::Rejected {
                reason: ReturnRejection::PreconditionUnmet,
            })
        );
        let appended = appended_facts(decision);
        let [Fact::ReturnRejected { digest, .. }] = appended.as_slice() else {
            panic!("only the typed terminal records, got {appended:?}");
        };
        assert_eq!(digest, &RawResultDigest::of(body.as_str().as_bytes()));
        let ended = [log, appended].concat();
        assert_eq!(e.validate_replay(&ended), Ok(()));
        assert!(
            Projection::build(&ended, ended.len() as u64)
                .view(&traj())
                .has_ended(&child)
        );

        let repeat = e
            .handle(
                &viewing(&e, &ended),
                child_report(&ended, &child, ChildSubmission::Value { body: body.clone() }),
            )
            .expect("the repeat answers from the record");
        assert_eq!(repeat.append, None);
        assert_eq!(
            repeat.follow_up,
            FollowUp::Child(ChildFollowUp::Rejected {
                reason: ReturnRejection::PreconditionUnmet,
            })
        );
        assert_eq!(
            e.handle(
                &viewing(&e, &ended),
                child_report(
                    &ended,
                    &child,
                    ChildSubmission::Value {
                        body: ValueBody::new(r#"{"verdict":"allow"}"#),
                    },
                ),
            ),
            Err(TransitionError::BranchEnded)
        );

        let mut flipped = ended;
        for fact in &mut flipped {
            if let Fact::ReturnRejected { reason, .. } = fact {
                *reason = ReturnRejection::MandateUnmet;
            }
        }
        assert_eq!(
            e.validate_replay(&flipped),
            Err(TransitionRefusal::ReturnRecordMismatch)
        );
    }

    #[test]
    fn an_unshaped_fork_under_a_child_attest_binding_rejects_the_return() {
        let e = open_engine_returning(
            returning_registry(vec![lifting_sanitizer("attest-schema")]),
            ReturnPolicy::Sanitized(SanitizerName::new("attest-schema")),
        );
        let child = TrajectoryId::new("child");
        let mut log = spawn_family(&e, None, &child);
        reads(&e, &mut log, &child, "read_suspicious");
        let decision = e
            .handle(
                &viewing(&e, &log),
                child_report(
                    &log,
                    &child,
                    ChildSubmission::Value {
                        body: ValueBody::new("free text"),
                    },
                ),
            )
            .expect("the rejection is a decision, not an error");
        assert_eq!(
            decision.follow_up,
            FollowUp::Child(ChildFollowUp::Rejected {
                reason: ReturnRejection::PreconditionUnmet,
            })
        );
        let ended = [log, appended_facts(decision)].concat();
        assert_eq!(e.validate_replay(&ended), Ok(()));
    }

    mod recovery_routes {
        use super::*;
        use crate::route::{Certainty, Contingency, RecoveryRoute, RouteDepth, RouteError, RouteOutcome, RouteStep};
        use std::collections::BTreeSet;

        fn subject(batch: &str) -> crate::basis::SubjectKey {
            crate::basis::SubjectKey::Call {
                trajectory: traj(),
                batch: crate::transition::ProposalBatchId::new(batch),
                position: 0,
            }
        }

        #[test]
        fn depth_one_through_the_engine_is_the_offers_the_block_opened() {
            let e = two_officer_engine();
            let log = vec![opened(&e)];
            let wire = call("wire", json!({}));
            let blocked = appended_facts(proposed(&e, &log, "b1", nonce(), wire.clone()).expect("the call blocks"));
            let offered: Vec<RecoveryRoute> = opened_offers(&blocked)
                .into_iter()
                .map(|(_, plan)| RecoveryRoute {
                    steps: plan
                        .required
                        .iter()
                        .map(|required| RouteStep::Authorize {
                            authority: required.authority.clone(),
                            covers: required.covers.clone(),
                            call: wire.digest(),
                        })
                        .collect(),
                    outcome: RouteOutcome::Complete,
                })
                .collect();
            assert_eq!(offered.len(), 2);
            for route in &offered {
                let [RouteStep::Authorize { authority, .. }] = &route.steps[..] else {
                    panic!("one ruling per offer: {route:?}");
                };
                assert_eq!(
                    route.certainty(),
                    Certainty::Contingent(BTreeSet::from([Contingency::AuthorityDecision {
                        authority: authority.clone(),
                    }]))
                );
            }
            let log = [log, blocked].concat();
            let view = e.view(&traj(), log.clone(), log.len() as u64).expect("the log replays");

            for depth in [RouteDepth::ONE, RouteDepth::new(4).unwrap()] {
                let found = e
                    .recovery_routes(
                        &view,
                        &subject("b1"),
                        &crate::audience::AudienceEvidence::default(),
                        depth,
                    )
                    .expect("a blocked call has a route search");
                assert_eq!(found, offered, "no tool clears a trust floor, so depth adds nothing");
            }
            assert_eq!(
                e.view(&traj(), log.clone(), log.len() as u64).map(|_| ()),
                Ok(()),
                "the search appended nothing"
            );
        }

        #[test]
        fn only_a_blocked_standing_call_has_routes() {
            let e = two_officer_engine();
            let log = vec![opened(&e)];
            let view = e.view(&traj(), log.clone(), log.len() as u64).expect("the log replays");
            let at = |subject: &crate::basis::SubjectKey| {
                e.recovery_routes(
                    &view,
                    subject,
                    &crate::audience::AudienceEvidence::default(),
                    RouteDepth::ONE,
                )
            };

            assert_eq!(at(&subject("never")), Err(RouteError::UnknownSubject));
            assert_eq!(
                engine(vec![neutral_tool()]).recovery_routes(
                    &view,
                    &subject("never"),
                    &crate::audience::AudienceEvidence::default(),
                    RouteDepth::ONE
                ),
                Err(RouteError::ForeignView),
                "a view built under another policy is refused before anything is read"
            );
            assert_eq!(
                at(&crate::basis::SubjectKey::Return(ChildReturnId::new(
                    TrajectoryId::new("child"),
                    0
                ))),
                Err(RouteError::NotACallSubject)
            );

            let e = engine_at(
                vec![crm_tool()],
                known(TRUSTED, Audience::restricted([ReaderId::new("insider")])),
            );
            let log = vec![opened(&e)];
            let released = appended_facts(
                proposed(&e, &log, "b1", nonce(), call("get_ticket", json!({}))).expect("the batch decides"),
            );
            assert!(released.iter().any(|fact| matches!(fact, Fact::DispatchOpened { .. })));
            let log = [log, released].concat();
            let view = e.view(&traj(), log.clone(), log.len() as u64).expect("the log replays");
            assert_eq!(
                e.recovery_routes(
                    &view,
                    &subject("b1"),
                    &crate::audience::AudienceEvidence::default(),
                    RouteDepth::ONE
                ),
                Err(RouteError::NotBlocked),
                "a released call stands decided but passes its check, so there is nothing to plan"
            );
        }

        #[test]
        fn a_route_reads_the_ordered_contract_the_standing_call_selected() {
            let private = ToolAnnotation {
                requires: Requires {
                    label: LabelRequirements {
                        trust_floor: None,
                        audience: vec![AudienceRequirement::Includes(RecipientSpec::Static(
                            DeclaredAudience::restricted([ReaderId::new("partner")]),
                        ))],
                    },
                    ..Requires::default()
                },
                ..plain_tool("read(path:private/*)")
            };
            let e = engine_at(
                vec![plain_tool("read(path:public/*)"), private],
                known(TRUSTED, Audience::restricted([ReaderId::new("insider")])),
            );
            let log = vec![opened(&e)];
            let proposal = e
                .resolve_call(ToolName::new("read"), br#"{"path":"private/q3.md"}"#)
                .expect("the call resolves");
            let blocked = appended_facts(proposed(&e, &log, "b1", nonce(), proposal).expect("the batch decides"));
            assert!(!blocked.iter().any(|fact| matches!(fact, Fact::DispatchOpened { .. })));
            let log = [log, blocked].concat();
            let view = e.view(&traj(), log.clone(), log.len() as u64).expect("the log replays");
            assert!(
                e.recovery_routes(
                    &view,
                    &subject("b1"),
                    &crate::audience::AudienceEvidence::default(),
                    RouteDepth::ONE
                )
                .is_ok(),
                "the standing call is blocked under the contract it selected; the first contract's verdict is not its verdict"
            );
        }
    }

    mod symbolic_audiences {
        use super::*;

        use crate::label::SymbolicAtom;
        use crate::names::GroupName;

        fn grouped(readers: &[&str], groups: &[&str]) -> DeclaredAudience {
            DeclaredAudience::Union(
                crate::label::Clause::new(
                    [],
                    groups
                        .iter()
                        .map(|group| crate::label::GroupRef::Named(GroupName::new(*group))),
                    readers.iter().map(|reader| ReaderId::new(*reader)),
                )
                .expect("literal readers beside groups"),
            )
        }

        fn readers(names: &[&str]) -> Audience {
            Audience::restricted(names.iter().map(|name| ReaderId::new(*name)))
        }

        fn team_delta(name: &str) -> ToolAnnotation {
            let mut tool = plain_tool(name);
            tool.delta = Delta {
                trust: None,
                audience: Some(grouped(&[], &["team"])),
            };
            tool
        }

        fn capped_send() -> ToolAnnotation {
            let mut send = plain_tool("send");
            send.requires = Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Cap(grouped(&["auditor"], &["team"]))],
                },
                ..Requires::default()
            };
            send
        }

        fn config(tools: Vec<ToolAnnotation>, authorities: Vec<crate::authority::Authority>) -> RegistryConfig {
            RegistryConfig {
                annotators: vec![],
                trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
                tools: declared(tools),
                authorities,
                sanitizers: vec![],
                audience: slack_groups(&["team", "board", "officers"]),
            }
        }

        fn grouped_engine(cfg: RegistryConfig, provider_run: &[&str], starting: Label) -> Engine {
            let mut declaration = crate::profile::ProfileDeclaration {
                starting_label: starting,
                ..crate::profile::covering_declaration(&cfg)
            };
            for name in provider_run {
                declaration
                    .executor_exceptions
                    .insert(ToolName::new(*name), crate::profile::ExecutorClass::ProviderRun);
                declaration.confined_results.remove(&ToolName::new(*name));
            }
            Engine::open(DeploymentPolicy {
                registry: cfg,
                planner_cap: crate::registry::PlannerCap::default(),
                dialect: PolicyDialectVersion::new(1),
                child_return: ReturnPolicy::Raw,
                profile: declaration,
            })
            .unwrap()
        }

        fn act_batch(
            id: &str,
            provider_results: Vec<crate::transition::ProviderResult>,
            proposals: Vec<crate::transition::ProposedCall>,
            audience: crate::audience::AudienceEvidence,
        ) -> EngineEvent {
            EngineEvent::Proposals(ProposalBatch {
                id: crate::transition::ProposalBatchId::new(id),
                trajectory: traj(),
                provider_results,
                proposals,
                spawn: None,
                offer_nonce: nonce(),
                evidence: Vec::new(),
                audience,
            })
        }

        fn no_answers() -> crate::audience::AudienceEvidence {
            crate::audience::AudienceEvidence::default()
        }

        fn symbolic(handle: &str) -> Audience {
            Audience::of_declared(&group_audience(handle))
        }

        #[test]
        fn a_planned_state_that_reads_an_unanswered_group_refuses_the_search_instead_of_resolving_it() {
            use crate::route::{RouteDepth, RouteError, RouteOutcome, RouteStep};

            let mut backup = plain_tool("backup");
            backup.emits = EffectSet::new([EffectKind::new("backup")]).unwrap();
            backup.delta = Delta {
                trust: None,
                audience: Some(DeclaredAudience::literal(readers(&["insider"]))),
            };
            let mut wire = plain_tool("wire");
            wire.requires = Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Includes(RecipientSpec::Static(grouped(
                        &[],
                        &["team"],
                    )))],
                },
                history: vec![HistoryRequirement::Prior(EffectKind::new("backup"))],
                ..Requires::default()
            };
            let officer = crate::authority::Authority {
                name: AuthorityName::new("officer"),
                mandate: crate::authority::Mandate {
                    reader_ceiling: Some(grouped(&[], &["board"])),
                    ..crate::authority::Mandate::default()
                },
                scope: crate::authority::Scope::default(),
                hint: None,
            };
            let e = grouped_engine(
                config(vec![backup, wire], vec![officer]),
                &[],
                known(TRUSTED, Audience::public()),
            );
            let log = vec![opened(&e)];
            let decided = e
                .handle(
                    &viewing(&e, &log),
                    act_batch("b1", vec![], vec![raw(&call("wire", json!({})))], no_answers()),
                )
                .expect("a `prior` gap alone reads no symbolic audience");
            let log = [log, appended_facts(decided)].concat();
            let view = e.view(&traj(), log.clone(), log.len() as u64).expect("the log replays");
            let subject = crate::basis::SubjectKey::Call {
                trajectory: traj(),
                batch: crate::transition::ProposalBatchId::new("b1"),
                position: 0,
            };
            let routes = |answers: &crate::audience::AudienceEvidence| {
                e.recovery_routes(&view, &subject, answers, RouteDepth::new(2).unwrap())
            };

            assert!(
                matches!(routes(&no_answers()), Err(RouteError::MembershipNeeded(_))),
                "after `backup` narrows the audience, the `includes` gap makes symbolic audiences reads: {:?}",
                routes(&no_answers())
            );
            let team = user_group("team", vec![slack_member("slack:UC", Some("carol@corp.com"))]);
            assert_eq!(
                routes(&source_evidence(vec![team.clone()])),
                Err(RouteError::MembershipNeeded(vec![SymbolicAtom::Group(
                    crate::label::GroupRef::Named(GroupName::new("board"))
                )])),
                "planning the block reads the officer's grouped ceiling"
            );
            let planned = routes(&source_evidence(vec![
                team,
                user_group("board", vec![slack_member("slack:UC", Some("carol@corp.com"))]),
            ]))
            .expect("the answers let the state plan");
            assert_eq!(planned.len(), 1);
            assert_eq!(planned[0].outcome, RouteOutcome::Complete);
            assert!(
                matches!(
                    &planned[0].steps[..],
                    [RouteStep::Precede { tool, .. }, RouteStep::Authorize { authority, .. }]
                        if tool.as_str() == "backup" && authority.as_str() == "officer"
                ),
                "{:?}",
                planned[0].steps
            );
        }

        #[test]
        fn a_route_reads_the_answers_the_block_recorded_over_any_fresh_answer() {
            use crate::route::{RouteDepth, RouteOutcome, RouteStep};

            let mut seen = plain_tool("seen");
            seen.delta = Delta {
                trust: None,
                audience: Some(grouped(&[], &["board"])),
            };
            let e = grouped_engine(
                config(vec![capped_send(), seen], vec![]),
                &[],
                known(TRUSTED, readers(&["alice"])),
            );
            let log = vec![opened(&e)];
            let recorded_answers = source_evidence(vec![user_group("board", vec![]), user_group("team", vec![])]);
            let decided = e
                .handle(
                    &viewing(&e, &log),
                    act_batch("b1", vec![], vec![raw(&call("send", json!({})))], recorded_answers),
                )
                .expect("the tool plans of the block read `board`, so the batch answers it too");
            let log = [log, appended_facts(decided)].concat();
            let view = e.view(&traj(), log.clone(), log.len() as u64).expect("the log replays");
            let subject = crate::basis::SubjectKey::Call {
                trajectory: traj(),
                batch: crate::transition::ProposalBatchId::new("b1"),
                position: 0,
            };
            let cap = crate::check::Gap::Cap {
                cap: grouped(&["auditor"], &["team"]),
            };
            let routes = |answers: crate::audience::AudienceEvidence| {
                e.recovery_routes(&view, &subject, &answers, RouteDepth::new(2).unwrap())
                    .expect("the recorded answers stand")
            };

            let narrowed = Audience::of_clauses([
                crate::label::Clause::new([], [], [ReaderId::new("alice")]).expect("a reader clause"),
                crate::label::Clause::new([], [crate::label::GroupRef::Named(GroupName::new("board"))], [])
                    .expect("a group clause"),
            ]);
            let recorded = routes(no_answers());
            assert_eq!(recorded.len(), 1);
            assert_eq!(recorded[0].outcome, RouteOutcome::Complete);
            assert!(
                matches!(
                    &recorded[0].steps[..],
                    [RouteStep::Precede { tool, clears, accepts: Some(narrowing) }]
                        if tool.as_str() == "seen"
                            && clears == &vec![cap.clone()]
                            && narrowing.to.audience == narrowed
                ),
                "`seen` narrows `alice` by the symbolic `board`, empty under the recorded answer, within \
                 the cap the recorded empty `team` leaves at `auditor`: {:?}",
                recorded[0].steps
            );
            assert_eq!(
                routes(source_evidence(vec![
                    user_group("board", vec![]),
                    user_group("team", vec![])
                ])),
                recorded,
                "restating the answers the block consumed reads them once"
            );
            assert!(
                matches!(
                    e.recovery_routes(
                        &view,
                        &subject,
                        &source_evidence(vec![user_group(
                            "team",
                            vec![slack_member("slack:UA", Some("alice@corp.com"))]
                        )]),
                        RouteDepth::ONE
                    ),
                    Err(crate::route::RouteError::Evidence(
                        crate::audience::EvidenceRefusal::ContradictedPin { .. }
                    ))
                ),
                "a fresh answer that would lift the cap contradicts the answer the block consumed"
            );
            assert!(matches!(
                e.recovery_routes(
                    &view,
                    &subject,
                    &source_evidence(vec![user_group("officers", vec![]), user_group("officers", vec![])]),
                    RouteDepth::ONE
                ),
                Err(crate::route::RouteError::Evidence(
                    crate::audience::EvidenceRefusal::DuplicateSelector { .. }
                ))
            ));
        }

        #[test]
        fn a_provider_run_delta_writing_a_group_admits_at_the_symbolic_audience() {
            let e = grouped_engine(
                config(vec![team_delta("seen")], vec![]),
                &["seen"],
                known(TRUSTED, Audience::public()),
            );
            let log = vec![opened(&e)];
            let seen = || vec![exposed("seen", "what the provider saw")];

            // The delta stays symbolic: admitting it reads no membership at all.
            let admitted = e
                .handle(&viewing(&e, &log), act_batch("b1", seen(), vec![], no_answers()))
                .expect("a symbolic delta admits without an answer");
            let facts = appended_facts(admitted);
            let Some(label) = facts.iter().find_map(|fact| match fact {
                Fact::ValueAdmitted { value, .. } => Some(value.label.clone()),
                _ => None,
            }) else {
                panic!("the exposed result admits, got {facts:?}");
            };
            assert_eq!(
                label.audience,
                symbolic("team"),
                "the durable record carries the audience symbolically, never a reader snapshot"
            );
            let log = [log, facts].concat();
            assert_eq!(e.validate_replay(&log), Ok(()));

            let repeat = e
                .handle(&viewing(&e, &log), act_batch("b1", seen(), vec![], no_answers()))
                .expect("the repeat answers from the record");
            assert_eq!(repeat.append, None);
        }

        #[test]
        fn a_cap_written_with_a_group_decides_by_the_answer_and_an_empty_answer_is_valid() {
            let e = grouped_engine(
                config(vec![capped_send()], vec![]),
                &[],
                known(TRUSTED, Audience::restricted([corp_reader("alice")])),
            );
            let log = vec![opened(&e)];
            let send = || vec![raw(&call("send", json!({})))];

            assert_eq!(
                e.handle(&viewing(&e, &log), act_batch("b1", vec![], send(), no_answers())),
                Err(TransitionError::MembershipNeeded {
                    needed: vec![group_atom("team")]
                })
            );

            let decided = e
                .handle(
                    &viewing(&e, &log),
                    act_batch("b1", vec![], send(), source_evidence(vec![user_group("team", vec![])])),
                )
                .expect("an empty answer decides");
            let (released, blocked) = answered(&decided);
            assert!(released.is_empty());
            assert_eq!(
                blocked[0].block.raw.requirement_gaps,
                vec![crate::check::Gap::Cap {
                    cap: grouped(&["auditor"], &["team"])
                }]
            );
            assert_eq!(
                e.validate_replay(&[log.clone(), appended_facts(decided)].concat()),
                Ok(())
            );

            let answer = source_evidence(vec![user_group(
                "team",
                vec![slack_member("slack:UA", Some("alice@corp.com"))],
            )]);
            let released = e
                .handle(&viewing(&e, &log), act_batch("b2", vec![], send(), answer.clone()))
                .expect("a member answer decides");
            assert_eq!(tool_names(answered(&released).0), ["send"]);
            let facts = appended_facts(released);
            assert!(facts.iter().any(|fact| matches!(
                fact,
                Fact::DispatchOpened { evidence, .. } if evidence == &answer
            )));
            let log = [log, facts].concat();
            assert_eq!(e.validate_replay(&log), Ok(()));
        }

        #[test]
        fn one_act_reads_one_answer_for_a_group_two_sites_read() {
            // A cap the current symbolic audience does not structurally derive: reading it
            // takes the group's members.
            let mut alice_capped = plain_tool("send");
            alice_capped.requires = Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Cap(DeclaredAudience::literal(
                        Audience::restricted([corp_reader("alice")]),
                    ))],
                },
                ..Requires::default()
            };
            let e = grouped_engine(
                config(vec![team_delta("seen"), alice_capped], vec![]),
                &["seen"],
                known(TRUSTED, Audience::public()),
            );
            let log = vec![opened(&e)];
            let act = |audience| {
                act_batch(
                    "b1",
                    vec![exposed("seen", "what the provider saw")],
                    vec![raw(&call("send", json!({})))],
                    audience,
                )
            };
            assert_eq!(
                e.handle(&viewing(&e, &log), act(no_answers())),
                Err(TransitionError::MembershipNeeded {
                    needed: vec![group_atom("team")]
                })
            );
            let answer = source_evidence(vec![user_group(
                "team",
                vec![slack_member("slack:UA", Some("alice@corp.com"))],
            )]);
            let decided = e
                .handle(&viewing(&e, &log), act(answer))
                .expect("the answered act decides");
            assert_eq!(tool_names(answered(&decided).0), ["send"]);
            let facts = appended_facts(decided);
            assert!(
                facts.iter().any(|fact| matches!(
                    fact,
                    Fact::ValueAdmitted { value, .. } if value.label.audience == symbolic("team")
                )),
                "the admitted label stays symbolic while the cap reads the same act's answer"
            );
            assert_eq!(e.validate_replay(&[log, facts].concat()), Ok(()));
        }

        #[test]
        fn a_reader_ceiling_writing_a_group_is_read_when_the_block_is_planned() {
            let officer = crate::authority::Authority {
                name: AuthorityName::new("officer"),
                mandate: crate::authority::Mandate {
                    reader_ceiling: Some(grouped(&[], &["officers"])),
                    ..crate::authority::Mandate::default()
                },
                scope: crate::authority::Scope::default(),
                hint: None,
            };
            let mut send = plain_tool("send");
            send.requires = Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Includes(RecipientSpec::Static(grouped(
                        &[],
                        &["team"],
                    )))],
                },
                ..Requires::default()
            };
            let e = grouped_engine(
                config(vec![send], vec![officer]),
                &[],
                known(TRUSTED, Audience::restricted([corp_reader("alice")])),
            );
            let log = vec![opened(&e)];
            let send = || vec![raw(&call("send", json!({})))];

            assert_eq!(
                e.handle(&viewing(&e, &log), act_batch("b1", vec![], send(), no_answers())),
                Err(TransitionError::MembershipNeeded {
                    needed: vec![group_atom("team")]
                })
            );
            let team = user_group("team", vec![slack_member("slack:UC", Some("carol@corp.com"))]);
            assert_eq!(
                e.handle(
                    &viewing(&e, &log),
                    act_batch("b1", vec![], send(), source_evidence(vec![team.clone()]))
                ),
                Err(TransitionError::MembershipNeeded {
                    needed: vec![group_atom("officers")]
                }),
                "the block's plans read the officer's grouped ceiling"
            );
            let uncovered = e
                .handle(
                    &viewing(&e, &log),
                    act_batch(
                        "b1",
                        vec![],
                        send(),
                        source_evidence(vec![
                            team.clone(),
                            user_group("officers", vec![slack_member("slack:UD", Some("dave@corp.com"))]),
                        ]),
                    ),
                )
                .expect("the answered act decides");
            assert!(answered(&uncovered).1[0].offers.is_empty());
            let covering = source_evidence(vec![
                team,
                user_group("officers", vec![slack_member("slack:UC", Some("carol@corp.com"))]),
            ]);
            let blocked = e
                .handle(&viewing(&e, &log), act_batch("b1", vec![], send(), covering.clone()))
                .expect("the answered act decides");
            let (offer, plan) = {
                let facts = appended_facts(blocked);
                let opened = opened_offers(&facts);
                assert_eq!(opened.len(), 1, "one ruling plan names the officer");
                (opened[0].0, opened[0].1.clone())
            };
            let blocked = e
                .handle(&viewing(&e, &log), act_batch("b1", vec![], send(), covering.clone()))
                .expect("the answered act decides");
            let log = [log, appended_facts(blocked)].concat();

            let evidence = evidence_for(
                offer,
                &plan,
                "send",
                partial(TRUSTED, Audience::restricted([corp_reader("alice")])),
            );
            let approved =
                execute_offer(&e, &log, offer, OfferOutcome::Approved(evidence)).expect("the offer approves");
            assert!(matches!(
                approved.follow_up,
                FollowUp::Offer(OfferFollowUp::Approved { .. })
            ));
            let facts = appended_facts(approved);
            assert!(facts.iter().any(|fact| matches!(
                fact,
                Fact::CallApproved { evidence, .. } if evidence.contains(&covering)
            )));
            let log = [log, facts].concat();
            assert_eq!(e.validate_replay(&log), Ok(()));

            let released = e
                .handle(&viewing(&e, &log), act_batch("b2", vec![], send(), no_answers()))
                .expect("the approved call releases");
            assert_eq!(tool_names(answered(&released).0), ["send"]);
            let log = [log, appended_facts(released)].concat();
            assert_eq!(e.validate_replay(&log), Ok(()));

            let repeated = e
                .handle(&viewing(&e, &log), act_batch("b2", vec![], send(), no_answers()))
                .expect("a decided batch repeats");
            assert!(repeated.append.is_none());
            assert_eq!(tool_names(answered(&repeated).0), ["send"]);
        }

        #[test]
        fn the_audience_configuration_is_policy_identity() {
            let identity = |cfg: &RegistryConfig| {
                let profile = crate::profile::DeploymentProfile::declare(crate::profile::covering_declaration(cfg))
                    .expect("the covering declaration declares");
                crate::profile::identity_of(cfg, &ReturnPolicy::Raw, &profile)
            };
            let forward = config(vec![capped_send(), team_delta("read")], vec![]);
            let backward = config(vec![team_delta("read"), capped_send()], vec![]);
            let mut remapped = config(vec![capped_send(), team_delta("read")], vec![]);
            remapped.audience.groups[0].from = vec![crate::audience::SelectorSpec {
                provider: "slack".to_string(),
                selector: "user-group/other".to_string(),
            }];
            assert_ne!(
                identity(&forward),
                identity(&remapped),
                "which sources feed an audience is part of what the policy means"
            );
            assert_eq!(identity(&forward), identity(&backward));
        }

        fn child_report_with(
            log: &[Fact],
            child: &TrajectoryId,
            body: &ValueBody,
            evidence: Vec<Evidence>,
            audience: crate::audience::AudienceEvidence,
        ) -> EngineEvent {
            EngineEvent::ChildReturn(ChildReport {
                child: child.clone(),
                fork: fork_in(log, child),
                submission: ChildSubmission::Value { body: body.clone() },
                evidence,
                offer_nonce: nonce(),
                audience,
            })
        }

        fn returning_with_sources(sanitizers: Vec<crate::authority::Sanitizer>) -> RegistryConfig {
            RegistryConfig {
                audience: slack_groups(&["team"]),
                ..returning_registry(sanitizers)
            }
        }

        #[test]
        fn a_sanitizer_to_writing_a_group_derives_at_the_symbolic_audience() {
            let declassify = crate::authority::Sanitizer {
                name: SanitizerName::new("declassify"),
                on: crate::authority::SanitizerPoints {
                    input: false,
                    output: true,
                },
                transition: crate::authority::DeclaredTransition::Audience {
                    from_includes: DeclaredAudience::restricted([]),
                    to: grouped(&[], &["team"]),
                },
                scope: crate::authority::Scope::default(),
                hint: None,
            };
            let e = open_engine_returning(
                returning_with_sources(vec![declassify]),
                ReturnPolicy::Sanitized(SanitizerName::new("declassify")),
            );
            let child = TrajectoryId::new("child");
            let log = spawn_family(&e, None, &child);
            let body = ValueBody::new("what I found");
            let raw_digest = RawResultDigest::of(body.as_str().as_bytes());

            let submitted = e
                .handle(
                    &viewing(&e, &log),
                    child_report_with(&log, &child, &body, vec![], no_answers()),
                )
                .expect("a symbolic transition target needs no answer to submit");
            assert!(matches!(
                submitted.follow_up,
                FollowUp::Child(ChildFollowUp::Resolve(EvidenceRequest::Sanitizer { .. }))
            ));
            let log = [log, appended_facts(submitted)].concat();
            assert_eq!(e.validate_replay(&log), Ok(()));

            let clean = ValueBody::new("clean");
            let evidence = vec![Evidence::Sanitizer {
                sanitizer: SanitizerName::new("declassify"),
                source: raw_digest,
                derived: clean.clone(),
            }];
            let derived = e
                .handle(
                    &viewing(&e, &log),
                    child_report_with(&log, &child, &body, evidence, no_answers()),
                )
                .expect("the derivation stands as the candidate");
            let stage = pending_stage_of(&derived);
            assert_eq!(
                stage.label.audience,
                symbolic("team"),
                "the derived candidate carries the transition's audience symbolically"
            );
            let acceptance = stage.offers[0].0;
            let facts = appended_facts(derived);
            let Some(via) = facts.iter().find_map(|fact| match fact {
                Fact::CandidateDerived { via, .. } => Some(via.clone()),
                _ => None,
            }) else {
                panic!("the derivation stands as the candidate")
            };
            assert_eq!(
                via,
                DerivedVia {
                    name: SanitizerName::new("declassify"),
                    transition: crate::authority::Transition::Audience {
                        from_includes: DeclaredAudience::restricted([]),
                        to: symbolic("team"),
                    },
                }
            );
            let log = [log, facts].concat();
            assert_eq!(e.validate_replay(&log), Ok(()));

            let accepted = e
                .handle(
                    &viewing(&e, &log),
                    EngineEvent::ExecuteOffer(OfferExecution {
                        trajectory: traj(),
                        offer: acceptance,
                        outcome: OfferOutcome::Approved(Vec::new()),
                        offer_nonce: nonce(),
                        audience: no_answers(),
                    }),
                )
                .expect("the parent accepts the residual");
            assert!(matches!(
                accepted.follow_up,
                FollowUp::Offer(OfferFollowUp::Admitted { .. })
            ));
            let facts = appended_facts(accepted);
            assert!(facts.iter().any(|fact| matches!(
                fact,
                Fact::ChildReturn { value, .. } if value.label.audience == symbolic("team")
            )));
            let log = [log, facts].concat();
            assert_eq!(e.validate_replay(&log), Ok(()));
        }

        #[test]
        fn an_accepted_child_return_seals_and_replays_with_its_pinned_answers() {
            // The return stage reads @team to judge the sanitizer's
            // applicability, so the submission pins the answer on its offers
            // and the accepting act inherits it: the acceptance batch must
            // seal live and replay with that evidence.
            let declassify = crate::authority::Sanitizer {
                name: SanitizerName::new("declassify"),
                on: crate::authority::SanitizerPoints {
                    input: false,
                    output: true,
                },
                transition: crate::authority::DeclaredTransition::Audience {
                    from_includes: grouped(&[], &["team"]),
                    to: DeclaredAudience::literal(Audience::public()),
                },
                scope: crate::authority::Scope::default(),
                hint: None,
            };
            let cfg = returning_with_sources(vec![declassify]);
            // Confine only the child return, so the child's own read never
            // asks the sanitizer's atom and the stage is the one reader.
            let mut declaration = crate::profile::covering_declaration(&cfg);
            declaration.confined_results = std::collections::BTreeSet::new();
            let e = Engine::open(DeploymentPolicy {
                registry: cfg,
                planner_cap: crate::registry::PlannerCap::default(),
                dialect: PolicyDialectVersion::new(1),
                child_return: ReturnPolicy::Raw,
                profile: declaration,
            })
            .expect("a return-confined deployment opens");
            let child = TrajectoryId::new("child");
            let mut log = spawn_family(&e, None, &child);
            reads(&e, &mut log, &child, "read_suspicious_internal");
            let body = ValueBody::new("what I found");
            let answers = source_evidence(vec![user_group("team", vec![slack_member("slack:U1", None)])]);

            let submitted = e
                .handle(
                    &viewing(&e, &log),
                    child_report_with(&log, &child, &body, vec![], answers.clone()),
                )
                .expect("the answered stage opens its offers");
            let acceptance = pending_stage_of(&submitted).offers[0].0;
            let log = [log, appended_facts(submitted)].concat();
            assert_eq!(e.validate_replay(&log), Ok(()));

            let accepted = e
                .handle(
                    &viewing(&e, &log),
                    EngineEvent::ExecuteOffer(OfferExecution {
                        trajectory: traj(),
                        offer: acceptance,
                        outcome: OfferOutcome::Approved(Vec::new()),
                        offer_nonce: nonce(),
                        audience: no_answers(),
                    }),
                )
                .expect("the acceptance inherits the offer's pinned answers and seals");
            assert!(matches!(
                accepted.follow_up,
                FollowUp::Offer(OfferFollowUp::Admitted { .. })
            ));
            let facts = appended_facts(accepted);
            assert!(
                facts
                    .iter()
                    .any(|fact| matches!(fact, Fact::ChildReturn { evidence, .. } if *evidence == answers)),
                "the crossing carries the inherited answers"
            );
            let log = [log, facts].concat();
            assert_eq!(e.validate_replay(&log), Ok(()));
        }
    }
}
