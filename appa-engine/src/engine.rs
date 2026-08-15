//! The engine: a pure function of the log's views and the immutable registry.

use thiserror::Error;

use crate::admit::{self, AdmitError, CastAnswer, CastError, ResultAdmission};
use crate::branch::{self, BranchError, ReturnSubmission};
use crate::candidate::{CallStage, ConfinedFrom, DerivedCandidate, DerivedVia, SanitizerLineage};
use crate::check::{self, CheckOutcome, Narrowing, RawBlock};
use crate::contract::ToolContract;
use crate::execute::{self, PlanError, Ruling};
use crate::fact::{Fact, FactBatch, ObservedResult, ReturnDerivation, ReturnPolicy, ReturnRejection, Revision};
use crate::label::{EstablishedLabel, Label, PartialLabel};
use crate::names::{AuthorityName, SanitizerName};
use crate::params::{ArgumentError, CanonicalArguments};
use crate::plan::{self, PlannedBlock};
use crate::profile::{self, DeploymentPolicy, DeploymentProfile, OpenVector, PolicyDialectVersion, PolicyIdentityV1};
use crate::projection::Projection;
use crate::projection::Views;
use crate::registry::{LoadError, Registry};
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
    #[error("the call does not pass the check as-is — remedy or accept it first")]
    NotAllowed,
    #[error("the branch already ended its errand")]
    BranchEnded,
}

/// Why a family log's durable opening record cannot be trusted on cold replay: the
/// strict verifier refuses a log whose opening is missing, displaced, duplicated, foreign, or
/// inconsistent with the supplied policy. Distinct from [`TransitionRefusal`], the per-dispatch payload
/// choke point — the complete transition validator is `T31`.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum OpeningTransitionRefusal {
    #[error("the family log carries no TrajectoryOpened record")]
    Missing,
    #[error("the TrajectoryOpened record is not the family's first record")]
    NotFirst,
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
        let identity = PolicyIdentityV1::of(&config, &child_return, &profile);
        let registry = Registry::build(config, planner_cap, profile)?;
        profile::validate_coverage(&registry, &declaration, &child_return)?;
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
            .tools()
            .map(|tool| &tool.name)
            .chain(self.registry.provider_run_contracts().map(|tool| &tool.name));
        profile::derive_open_vectors(self.profile(), tools)
    }

    /// Build the working view over a persisted family log: every record passes the one
    /// transition validator before anything reads it, so no caller decides against an untrusted
    /// stream. On cache loss the runtime rebuilds through this same call.
    pub fn view(
        &self,
        family: &TrajectoryId,
        records: Vec<Fact>,
        revision: Revision,
    ) -> Result<EngineView, TransitionRefusal> {
        let projection = self.replay(family, &records, revision)?;
        Ok(EngineView::validated(projection, self.identity, family.clone()))
    }

    /// The validator over a bare record stream, for tests that pin a refusal without holding a
    /// view. Production reaches it through [`Engine::view`].
    #[cfg(test)]
    pub(crate) fn validate_replay(&self, facts: &[Fact]) -> Result<(), TransitionRefusal> {
        let family = match facts.first() {
            Some(Fact::Boundary {
                kind: crate::fact::BoundaryKind::Fork { parent, .. },
                ..
            }) => parent.clone(),
            Some(fact) => fact.trajectory().clone(),
            None => return Ok(()),
        };
        self.replay(&family, facts, Revision::new(facts.len() as u64))
            .map(|_| ())
    }

    fn replay(
        &self,
        family: &TrajectoryId,
        records: &[Fact],
        revision: Revision,
    ) -> Result<Projection, TransitionRefusal> {
        let mut sequence = Sequence::empty(&self.registry, &self.child_return, family, revision);
        for fact in records {
            sequence.admit(fact)?;
        }
        sequence.finish()
    }

    /// Seal a candidate batch: the facts an engine operation just built pass the same validator a
    /// persisted log does, so the sealed batch is one no replay of it can refuse.
    pub(crate) fn seal(&self, view: &EngineView, batch: FactBatch) -> Result<ValidatedFactBatch, TransitionRefusal> {
        let mut sequence = Sequence::resuming(&self.registry, &self.child_return, view);
        for fact in &batch.facts {
            sequence.admit(fact)?;
        }
        sequence.finish()?;
        Ok(ValidatedFactBatch::seal(batch, self.identity, view.family().clone()))
    }

    fn declaring(
        &self,
        act: crate::basis::DecidedAct,
        advance: crate::basis::BasisAdvance,
        batch: FactBatch,
    ) -> FactBatch {
        let stamps = batch
            .facts
            .iter()
            .any(|fact| matches!(fact, Fact::OfferOpened { .. } | Fact::CallApproved { .. }));
        if advance.is_empty() && !stamps {
            return batch;
        }
        let trajectory = batch
            .facts
            .first()
            .expect("a batch that advances a basis carries the record that advanced it")
            .trajectory()
            .clone();
        let mut facts = vec![Fact::BasisAdvanced {
            trajectory,
            act,
            advance,
        }];
        facts.extend(batch.facts);
        FactBatch::new(batch.basis, facts)
    }

    /// The engine's one mutation boundary: decide one event against the view and return
    /// a sealed batch plus the typed follow-up. The engine owns semantic validation and constructs
    /// every fact; it owns no mutable state.
    pub fn handle(&self, view: &EngineView, event: EngineEvent) -> Result<EngineDecision, TransitionError> {
        if view.policy() != self.identity {
            return Err(TransitionError::ForeignView);
        }
        match event {
            EngineEvent::Proposals(batch) => self.decide_proposals(view, &batch),
            EngineEvent::Outcome(report) => self.decide_outcome(view, &report),
            EngineEvent::ChildReturn(report) => self.decide_child_return(view, &report),
            EngineEvent::BindFork(binding) => self.decide_binding(view, &binding),
            EngineEvent::ExecuteOffer(execution) => self.decide_offer(view, &execution),
        }
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
        if binding.child == parent || views.is_active(&binding.child) {
            return Err(TransitionError::ChildAlreadyUsed);
        }
        // A recorded spawn failure makes the preparation unbindable: no child ran.
        if views.dispatch_failed(binding.fork.dispatch()) {
            return Err(TransitionError::UnbindableFork);
        }
        let batch = FactBatch::new(
            views.revision(),
            vec![Fact::ForkOpened {
                trajectory: binding.child.clone(),
                fork: binding.fork.clone(),
            }],
        );
        let batch = self.declaring(
            crate::basis::DecidedAct::Binding(binding.fork.clone()),
            advance_of(self, view, &batch),
            batch,
        );
        Ok(EngineDecision {
            append: Some(self.seal(view, batch)?),
            follow_up: FollowUp::Fork {
                child: binding.child.clone(),
            },
        })
    }

    fn decide_child_return(&self, view: &EngineView, report: &ChildReport) -> Result<EngineDecision, TransitionError> {
        let child = &report.child;
        let projection = view.projection();
        let parent = projection
            .view(child)
            .parent_of(child)
            .ok_or(TransitionError::NotForked)?
            .clone();
        let views = projection.view(&parent);
        if report.fork.as_ref() != views.fork_of(child) {
            return Err(TransitionError::ReturnForkMismatch);
        }
        if views.has_ended(child) {
            return self.ended_return(view, &views, report);
        }
        let body = match &report.submission {
            ChildSubmission::Void => {
                let batch = branch::submit_void_return(&views, child).map_err(branch_refusal)?;
                let batch = self.declaring(return_act(child), advance_of(self, view, &batch), batch);
                return Ok(EngineDecision {
                    append: Some(self.seal(view, batch)?),
                    follow_up: FollowUp::Child(ChildFollowUp::Ended),
                });
            }
            ChildSubmission::Value { body } => match views.return_shape_of(child) {
                Some(shape) => match shape.validate(body.as_str()) {
                    Ok(canonical) => ValueBody::new(canonical),
                    Err(mismatch) => {
                        if let Some(fork) = &report.fork
                            && let Some(ReturnPolicy::Sanitized(name)) = views.return_policy_of(child)
                            && name.is_attest_schema()
                        {
                            return self.rejecting(
                                view,
                                &views,
                                child,
                                &ChildReturnId::new(child.clone(), 0),
                                fork,
                                RawResultDigest::of(body.as_str().as_bytes()),
                                ReturnRejection::PreconditionUnmet,
                                Vec::new(),
                            );
                        }
                        return Err(TransitionError::ReturnShapeMismatch(mismatch));
                    }
                },
                None => body.clone(),
            },
        };
        let Some(fork) = report.fork.clone() else {
            return self.legacy_child_return(view, &views, child, body);
        };
        let (working, cast_facts) = self.applied_casts(projection, child, &report.evidence)?;
        let views = working.view(&parent);
        let id = ChildReturnId::new(child.clone(), 0);
        let policy = views.return_policy_of(child).ok_or(TransitionError::NotForked)?.clone();
        match policy {
            ReturnPolicy::Raw => self.raw_return(view, &views, child, &id, &fork, body, cast_facts, report.offer_nonce),
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
            ),
        }
    }

    fn legacy_child_return(
        &self,
        view: &EngineView,
        views: &Views,
        child: &TrajectoryId,
        body: ValueBody,
    ) -> Result<EngineDecision, TransitionError> {
        match branch::check_child_return(&self.registry, views, child).map_err(branch_refusal)? {
            branch::ReturnCheck::Allow => {
                let batch = branch::submit_child_return(
                    &self.registry,
                    views,
                    child,
                    branch::ReturnSubmission::Raw { body: body.clone() },
                )
                .map_err(branch_refusal)?;
                let batch = self.declaring(return_act(child), advance_of(self, view, &batch), batch);
                Ok(EngineDecision {
                    append: Some(self.seal(view, batch)?),
                    follow_up: FollowUp::Child(ChildFollowUp::Merged { admitted: body }),
                })
            }
            branch::ReturnCheck::Block(branch::ReturnBlock { narrowing, plans }) => Ok(EngineDecision {
                append: None,
                follow_up: FollowUp::Child(ChildFollowUp::Blocked { narrowing, plans }),
            }),
        }
    }

    /// Land the cast answers a report carried, acting as the child — the
    /// branch that holds the sources a return's planning consumes. Returns the
    /// projection with each answer folded, beside the facts that carry it; an answer for a
    /// source already established is skipped, even where it disagrees with the landed one —
    /// the first admitted answer stands, and a redriven consult of a nondeterministic
    /// resolver may legitimately answer differently, so rejecting the conflict would wedge
    /// crash-redrive on an already-settled source.
    ///
    /// Deliberately not narrowed to the request the current stage owes: cast evidence binds to
    /// the cast and source identity alone — unlike membership evidence, it names no
    /// consuming operation for it — because a resolution is source-level and shared.
    /// `admit_cast` holds each answer to the basis, scope, first-answer, and ceiling rules, so
    /// an unrequested valid answer lands exactly as it would through the act that requested it.
    fn applied_casts<'p>(
        &self,
        projection: &'p Projection,
        child: &TrajectoryId,
        evidence: &[Evidence],
    ) -> Result<(std::borrow::Cow<'p, Projection>, Vec<Fact>), TransitionError> {
        let mut working = std::borrow::Cow::Borrowed(projection);
        let mut facts = Vec::new();
        for item in evidence {
            let Evidence::Cast { cast, value, resolved } = item else {
                continue;
            };
            let batch = {
                let views = working.view(child);
                if views
                    .value_label(*value)
                    .is_some_and(|prior| EstablishedLabel::from_label(prior).is_some())
                {
                    continue;
                }
                admit::admit_cast(
                    &self.registry,
                    &views,
                    *value,
                    CastAnswer {
                        cast: cast.clone(),
                        resolved: resolved.clone(),
                    },
                )
                .map_err(TransitionError::Resolution)?
            };
            for fact in &batch.facts {
                working.to_mut().fold(fact);
            }
            facts.extend(batch.facts);
        }
        Ok((working, facts))
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
    ) -> Result<EngineDecision, TransitionError> {
        match branch::check_child_return(&self.registry, views, child).map_err(branch_refusal)? {
            branch::ReturnCheck::Allow => {
                let crossing = branch::submit_child_return(
                    &self.registry,
                    views,
                    child,
                    branch::ReturnSubmission::Raw { body: body.clone() },
                )
                .map_err(branch_refusal)?;
                facts.extend(crossing.facts);
                let batch = FactBatch::new(views.revision(), facts);
                let batch = self.declaring(return_act(child), advance_of(self, view, &batch), batch);
                Ok(EngineDecision {
                    append: Some(self.seal(view, batch)?),
                    follow_up: FollowUp::Child(ChildFollowUp::Merged { admitted: body }),
                })
            }
            branch::ReturnCheck::Block(branch::ReturnBlock { narrowing, .. }) => {
                let fold = views.branch_label(child);
                let candidate = fold.bound().clone().into_label();
                let stage = match plan::return_stage(
                    &self.registry,
                    views,
                    child,
                    &fold,
                    &candidate,
                    &body,
                    &narrowing,
                    &SanitizerLineage::default(),
                ) {
                    plan::ReturnStagePlan::Resolve { cast, value } => {
                        return self.resolving(view, views, return_act(child), facts, cast, value);
                    }
                    plan::ReturnStagePlan::Stage(plans) => plans,
                };
                facts.push(Fact::ReturnSubmitted {
                    trajectory: child.clone(),
                    id: id.clone(),
                    fork: fork.clone(),
                    parent: views.trajectory().clone(),
                    label: fold,
                    digest: RawResultDigest::of(body.as_str().as_bytes()),
                    body,
                    policy: ReturnPolicy::Raw,
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
                views,
                child,
                id,
                fork,
                digest,
                ReturnRejection::PreconditionUnmet,
                facts,
            );
        }
        if !fold.is_established(registered.transition.dimension()) {
            return match plan::resolvable_source(&self.registry, views, &fold, registered.transition.dimension()) {
                Some((cast, value)) => self.resolving(view, views, return_act(child), facts, cast, value),
                None => self.rejecting(
                    view,
                    views,
                    child,
                    id,
                    fork,
                    digest,
                    ReturnRejection::ConsumedDimensionUnresolvable,
                    facts,
                ),
            };
        }
        if registered
            .derive_output(&fold.bound().clone().into_label(), &[])
            .is_none()
        {
            return self.rejecting(
                view,
                views,
                child,
                id,
                fork,
                digest,
                ReturnRejection::MandateUnmet,
                facts,
            );
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
        });
        if name.is_attest_schema() {
            let receiving = views.current_label().bound().clone();
            return self.mandatory_derivation(
                view, views, id, fork, name, &fold, receiving, digest, body, facts, nonce,
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
            let batch = FactBatch::new(views.revision(), facts);
            let batch = self.declaring(return_act(child), advance_of(self, view, &batch), batch);
            return Ok(EngineDecision {
                append: Some(self.seal(view, batch)?),
                follow_up: FollowUp::Child(ChildFollowUp::Resolve(EvidenceRequest::Sanitizer {
                    sanitizer: name.clone(),
                    source: digest,
                    body,
                })),
            });
        };
        // The receiving bound the submission pins at this same fold step.
        let receiving = views.current_label().bound().clone();
        self.mandatory_derivation(
            view, views, id, fork, name, &fold, receiving, digest, derived, facts, nonce,
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
        fold: &PartialLabel,
        receiving: EstablishedLabel,
        digest: RawResultDigest,
        derived: ValueBody,
        mut facts: Vec<Fact>,
        nonce: crate::value::OfferNonce,
    ) -> Result<EngineDecision, TransitionError> {
        let child = id.child();
        let registered = self
            .registry
            .sanitizer(name)
            .ok_or(TransitionError::SanitizerUnapplicable)?;
        let label = registered
            .derive_output(&fold.bound().clone().into_label(), &[])
            .ok_or(TransitionError::SanitizerUnapplicable)?;
        let residual = admit::confined_residual(&receiving, &label);
        let lineage = SanitizerLineage::default()
            .extend(name.clone())
            .expect("an empty lineage spends no sanitizer yet");
        facts.push(Fact::CandidateDerived {
            trajectory: views.trajectory().clone(),
            subject: crate::basis::SubjectKey::Return(id.clone()),
            via: DerivedVia::Sanitizer {
                name: name.clone(),
                transition: registered.transition.clone(),
            },
            derived: DerivedCandidate::Return {
                source: digest,
                from: ConfinedFrom::Bound,
                value: LabeledValue::new(derived.clone(), label.clone()),
                residual: residual.clone(),
            },
            lineage: lineage.clone(),
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
                    transition: registered.transition.clone(),
                },
                None,
            ));
            let batch = FactBatch::new(views.revision(), facts);
            let batch = self.declaring(return_act(child), advance_of(self, view, &batch), batch);
            return Ok(EngineDecision {
                append: Some(self.seal(view, batch)?),
                follow_up: FollowUp::Child(ChildFollowUp::Merged { admitted: derived }),
            });
        };
        let stage = match plan::return_stage(
            &self.registry,
            views,
            child,
            fold,
            &label,
            &derived,
            &residual,
            &lineage,
        ) {
            plan::ReturnStagePlan::Stage(plans) => plans,
            plan::ReturnStagePlan::Resolve { cast, value } => {
                return self.resolving(view, views, return_act(child), facts, cast, value);
            }
        };
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
        views: &Views,
        child: &TrajectoryId,
        id: &ChildReturnId,
        fork: &ForkId,
        digest: RawResultDigest,
        reason: ReturnRejection,
        mut facts: Vec<Fact>,
    ) -> Result<EngineDecision, TransitionError> {
        facts.push(Fact::ReturnRejected {
            trajectory: child.clone(),
            id: id.clone(),
            fork: fork.clone(),
            digest,
            reason: reason.clone(),
        });
        let batch = FactBatch::new(views.revision(), facts);
        let batch = self.declaring(return_act(child), advance_of(self, view, &batch), batch);
        Ok(EngineDecision {
            append: Some(self.seal(view, batch)?),
            follow_up: FollowUp::Child(ChildFollowUp::Rejected { reason }),
        })
    }

    fn resolving(
        &self,
        view: &EngineView,
        views: &Views,
        act: crate::basis::DecidedAct,
        facts: Vec<Fact>,
        cast: crate::names::CastName,
        value: crate::value::ValueId,
    ) -> Result<EngineDecision, TransitionError> {
        let body = views
            .value_body(value)
            .expect("a resolvable source retains the bytes its resolver reads")
            .clone();
        let append = match facts.is_empty() {
            true => None,
            false => {
                let batch = FactBatch::new(views.revision(), facts);
                let batch = self.declaring(act, advance_of(self, view, &batch), batch);
                Some(self.seal(view, batch)?)
            }
        };
        Ok(EngineDecision {
            append,
            follow_up: FollowUp::Child(ChildFollowUp::Resolve(EvidenceRequest::Cast { cast, value, body })),
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
    ) -> Result<(ValidatedFactBatch, PendingReturnStage), TransitionError> {
        let subject = crate::basis::SubjectKey::Return(id.clone());
        let call = views
            .dispatch_call(fork.dispatch())
            .ok_or(TransitionError::UnknownDispatch)?
            .digest();
        let advance = Sequence::advance_of(&self.registry, &self.child_return, view, &facts);
        let (_, offers, opened) = self.open_offers(views, &act, &advance, &nonce, &subject, &call, &stage);
        facts.extend(opened);
        let batch = self.declaring(act, advance, FactBatch::new(views.revision(), facts));
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
            if !comparable(body)
                .is_some_and(|canonical| RawResultDigest::of(canonical.as_str().as_bytes()) == submitted.digest)
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
            return self.continue_pending(view, report, &id);
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
    ) -> Result<EngineDecision, TransitionError> {
        let child = &report.child;
        // Cast answers land first here too.
        let (working, cast_facts) = self.applied_casts(view.projection(), child, &report.evidence)?;
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
                            false => {
                                let batch = FactBatch::new(views.revision(), cast_facts);
                                let batch = self.declaring(return_act(child), advance_of(self, view, &batch), batch);
                                Some(self.seal(view, batch)?)
                            }
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
            ),
            (_, Some(_)) => unreachable!("a settled return candidate crossed in its own batch"),
            // The submitted fold itself is the raw candidate.
            (ReturnPolicy::Raw, None) => {
                let label = fold.bound().clone().into_label();
                let to = pending.receiving.combine(&label.established_part());
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
        let fold = views.branch_label(id.child());
        let stage = match plan::return_stage(
            &self.registry,
            views,
            id.child(),
            &fold,
            &label,
            &body,
            &residual,
            &views.lineage(&subject),
        ) {
            plan::ReturnStagePlan::Stage(plans) => plans,
            plan::ReturnStagePlan::Resolve { cast, value } => {
                return self.resolving(view, views, return_act(id.child()), facts, cast, value);
            }
        };
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
        )?;
        Ok(EngineDecision {
            append: Some(batch),
            follow_up: FollowUp::Child(ChildFollowUp::Pending(Box::new(staged))),
        })
    }

    fn decide_outcome(&self, view: &EngineView, report: &ToolReport) -> Result<EngineDecision, TransitionError> {
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
            return self.restage(view, &views, dispatch, report.offer_nonce);
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
                        let contract = self.validated_contract(&call)?;
                        if contract.pending_cast_dim().is_some() {
                            return self.resolving_cast(
                                view,
                                &views,
                                dispatch,
                                &call,
                                contract,
                                raw,
                                raw_digest,
                                report,
                                checkpointed.is_some(),
                            );
                        }
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
                            Evidence::Sanitizer { .. } | Evidence::Cast { .. } | Evidence::PendingCast { .. } => None,
                        });
                        let Some(derived) = derived else {
                            let append = match checkpointed {
                                Some(_) => None,
                                None => {
                                    let checkpoint = self
                                        .observe_success(&views, dispatch, &call, ObservedResult::Available(raw_digest))
                                        .expect("an open, unreported dispatch checkpoints its observed success");
                                    let advance = advance_of(self, view, &checkpoint);
                                    let batch = self.declaring(
                                        crate::basis::DecidedAct::Outcome(dispatch.clone()),
                                        advance,
                                        checkpoint,
                                    );
                                    Some(self.seal(view, batch)?)
                                }
                            };
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
                        let (transition, candidate, lineage) = crate::admit::bound_candidate(
                            &self.registry,
                            &views,
                            dispatch,
                            contract,
                            &sanitizer,
                            raw_digest,
                            derived.clone(),
                        )
                        .map_err(|error| match error {
                            AdmitError::SanitizerTransitionUnmet | AdmitError::SanitizerBindingMismatch => {
                                TransitionError::SanitizerUnapplicable
                            }
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
                            );
                        };
                        return self.stage_candidate(
                            view,
                            &views,
                            dispatch,
                            &call,
                            checkpointed.is_some(),
                            report.offer_nonce,
                            Fact::CandidateDerived {
                                trajectory: views.trajectory().clone(),
                                subject: crate::basis::SubjectKey::ConfinedResult(dispatch.clone()),
                                via: DerivedVia::Sanitizer {
                                    name: sanitizer,
                                    transition,
                                },
                                derived: candidate,
                                lineage,
                            },
                        );
                    }
                }
            }
        };
        self.admitting_outcome(view, &views, dispatch, &call, admission)
    }

    #[allow(clippy::too_many_arguments)]
    fn resolving_cast(
        &self,
        view: &EngineView,
        views: &Views,
        dispatch: &DispatchId,
        call: &ResolvedCall,
        contract: &crate::contract::ToolContract,
        raw: &ValueBody,
        raw_digest: RawResultDigest,
        report: &ToolReport,
        checkpointed: bool,
    ) -> Result<EngineDecision, TransitionError> {
        let resolution = report.evidence.iter().find_map(|evidence| match evidence {
            Evidence::PendingCast { cast, source, resolved } if source == &raw_digest => {
                Some((cast.clone(), resolved.clone()))
            }
            _ => None,
        });
        let Some((cast, resolved)) = resolution else {
            let append = match checkpointed {
                true => None,
                false => {
                    let checkpoint = self
                        .observe_success(views, dispatch, call, ObservedResult::Available(raw_digest))
                        .expect("an open, unreported dispatch checkpoints its observed success");
                    let advance = advance_of(self, view, &checkpoint);
                    let batch =
                        self.declaring(crate::basis::DecidedAct::Outcome(dispatch.clone()), advance, checkpoint);
                    Some(self.seal(view, batch)?)
                }
            };
            let casts = self
                .registry
                .casts()
                .iter()
                .filter(|registered| registered.scope.covers(&contract.tags))
                .map(|registered| registered.name.clone())
                .collect();
            return Ok(EngineDecision {
                append,
                follow_up: FollowUp::Outcome(OutcomeFollowUp::Resolve(EvidenceRequest::PendingCast {
                    casts,
                    source: raw_digest,
                    body: raw.clone(),
                })),
            });
        };
        let candidate =
            crate::admit::cast_candidate(&self.registry, views, dispatch, contract, &cast, raw.clone(), &resolved)
                .map_err(|_| TransitionError::InadmissibleResolution)?;
        let DerivedCandidate::Result { residual: Some(_), .. } = &candidate else {
            return self.admitting_outcome(
                view,
                views,
                dispatch,
                call,
                ResultAdmission::SuccessCast {
                    body: raw.clone(),
                    cast,
                    resolved,
                },
            );
        };
        self.stage_candidate(
            view,
            views,
            dispatch,
            call,
            checkpointed,
            report.offer_nonce,
            Fact::CandidateDerived {
                trajectory: views.trajectory().clone(),
                subject: crate::basis::SubjectKey::ConfinedResult(dispatch.clone()),
                via: crate::candidate::DerivedVia::Cast { name: cast },
                derived: candidate,
                lineage: SanitizerLineage::default(),
            },
        )
    }

    fn admitting_outcome(
        &self,
        view: &EngineView,
        views: &Views,
        dispatch: &DispatchId,
        call: &ResolvedCall,
        admission: ResultAdmission,
    ) -> Result<EngineDecision, TransitionError> {
        let batch = self
            .admit_result(views, dispatch, call, admission)
            .map_err(|error| match error {
                AdmitError::SanitizerTransitionUnmet | AdmitError::SanitizerBindingMismatch => {
                    TransitionError::SanitizerUnapplicable
                }
                other => unreachable!("the outcome path admits what the log already proved: {other}"),
            })?;
        let admitted = batch.facts.iter().find_map(|fact| match fact {
            Fact::ValueAdmitted { value, .. } => Some(value.body.clone()),
            _ => None,
        });
        let batch = self.declaring(
            crate::basis::DecidedAct::Outcome(dispatch.clone()),
            advance_of(self, view, &batch),
            batch,
        );
        Ok(EngineDecision {
            append: Some(self.seal(view, batch)?),
            follow_up: FollowUp::Outcome(OutcomeFollowUp::Closed { admitted }),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn stage_candidate(
        &self,
        view: &EngineView,
        views: &Views,
        dispatch: &DispatchId,
        call: &ResolvedCall,
        checkpointed: bool,
        nonce: crate::value::OfferNonce,
        derived: Fact,
    ) -> Result<EngineDecision, TransitionError> {
        let mut facts = Vec::new();
        if !checkpointed {
            let source = match &derived {
                Fact::CandidateDerived {
                    derived: DerivedCandidate::Result { source, .. },
                    ..
                } => *source,
                _ => unreachable!("a staged record is a derived candidate"),
            };
            facts.extend(
                self.observe_success(views, dispatch, call, ObservedResult::Available(source))
                    .expect("an open, unreported dispatch checkpoints its observed success")
                    .facts,
            );
        }
        facts.push(derived);
        let (batch, confined) = self.staged(
            view,
            views,
            crate::basis::DecidedAct::Outcome(dispatch.clone()),
            nonce,
            dispatch,
            facts,
        )?;
        Ok(EngineDecision {
            append: Some(batch),
            follow_up: FollowUp::Outcome(OutcomeFollowUp::Staged(Box::new(confined))),
        })
    }

    fn staged(
        &self,
        view: &EngineView,
        views: &Views,
        act: crate::basis::DecidedAct,
        nonce: crate::value::OfferNonce,
        dispatch: &DispatchId,
        mut facts: Vec<Fact>,
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
        let stage = plan::confined_stage(&self.registry, contract, &receiving, &value.label, &residual, &lineage);
        let advance = Sequence::advance_of(&self.registry, &self.child_return, view, &facts);
        let (_, offers, opened) = self.open_offers(views, &act, &advance, &nonce, &subject, dispatch.digest(), &stage);
        facts.extend(opened);
        let batch = self.declaring(act, advance, FactBatch::new(views.revision(), facts));
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
        let stage = plan::confined_stage(&self.registry, contract, &receiving, &value.label, &residual, &lineage);
        let act = crate::basis::DecidedAct::Outcome(dispatch.clone());
        let advance = crate::basis::BasisAdvance::default();
        let (_, offers, opened) = self.open_offers(views, &act, &advance, &nonce, &subject, dispatch.digest(), &stage);
        let batch = self.declaring(act, advance, FactBatch::new(views.revision(), opened));
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

    fn decide_proposals(&self, view: &EngineView, batch: &ProposalBatch) -> Result<EngineDecision, TransitionError> {
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
        let views = view.projection().view(&batch.trajectory);
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
            if self.registry.provider_run_contract(&result.tool).is_none() {
                return Err(TransitionError::Call(match self.registry.tool(&result.tool) {
                    Some(_) => EngineError::NotProviderRun(result.tool.as_str().to_string()),
                    None => EngineError::UnknownTool(result.tool.as_str().to_string()),
                }));
            }
        }

        if let Some(decided) = views.decided_batch(&batch.id) {
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
            return Ok(EngineDecision {
                append: None,
                follow_up: self.decided_follow_up(&views, batch, &proposals, &recorded.released)?,
            });
        }

        let mut facts: Vec<Fact> = match admitted {
            0 => batch
                .provider_results
                .iter()
                .enumerate()
                .map(|(position, result)| {
                    let contract = self
                        .registry
                        .provider_run_contract(&result.tool)
                        .expect("every exposed result was classified above");
                    Fact::ValueAdmitted {
                        trajectory: batch.trajectory.clone(),
                        value: LabeledValue::new(result.body.clone(), contract.output_label()),
                        provenance: Provenance::ProviderRun {
                            tool: result.tool.clone(),
                            batch: batch.id.clone(),
                            position: position as u32,
                            effects: contract.emits.clone(),
                        },
                    }
                })
                .collect(),
            _ => Vec::new(),
        };

        let proposals = match self.resolve_proposals(batch) {
            Ok(proposals) => proposals,
            Err((position, error)) => {
                return Ok(EngineDecision {
                    append: self.seal_admissions(view, &batch.id, facts)?,
                    follow_up: FollowUp::Malformed { position, error },
                });
            }
        };

        let mut working = std::borrow::Cow::Borrowed(view.projection());
        for fact in &facts {
            working.to_mut().fold(fact);
        }
        let admissions = Sequence::advance_of(&self.registry, &self.child_return, view, &facts);
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
        )
        .map_err(|(_, error)| TransitionError::Call(error))?;

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
        });
        facts.extend(composed.iter().flatten().flat_map(|release| release.facts.clone()));
        // What this decision moves, derived before the offers that have to record where it lands.
        // The declaration prepended below re-derives it over the whole batch; an offer record
        // moves nothing, so the two agree by construction.
        let advance = Sequence::advance_of(&self.registry, &self.child_return, view, &facts);
        let final_views = working.view(&batch.trajectory);
        let mut blocked = Vec::new();
        for (position, call) in composed
            .iter()
            .enumerate()
            .filter(|(_, release)| release.is_none())
            .map(|(position, _)| (position, &proposals[position]))
        {
            let contract = self.validated_contract(call)?;
            let CheckOutcome::Block(raw) = check::evaluate(contract, &final_views, call, &CallStage::default()) else {
                unreachable!("an in-batch release only ever adds gaps to a refused sibling's block")
            };
            let planned = plan::plan(&self.registry, &final_views, call, &raw, &CallStage::default());
            let subject = crate::basis::SubjectKey::Call {
                trajectory: batch.trajectory.clone(),
                batch: batch.id.clone(),
                position: position as u32,
            };
            let (block_id, offers, opened_offers) = self.open_offers(
                &final_views,
                &crate::basis::DecidedAct::Proposals(batch.id.clone()),
                &advance,
                &batch.offer_nonce,
                &subject,
                &call.digest(),
                &Engine::executable(&planned),
            );
            facts.extend(opened_offers);
            blocked.push(Blocked {
                call: call.clone(),
                block: planned,
                block_id,
                offers,
            });
        }
        let decided = self.declaring(
            crate::basis::DecidedAct::Proposals(batch.id.clone()),
            advance,
            FactBatch::new(views.revision(), facts),
        );
        let append = self.seal(view, decided)?;
        Ok(EngineDecision {
            append: Some(append),
            follow_up: FollowUp::Proposals {
                released,
                blocked,
                forks: Vec::new(),
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
                    .map(|call| call.with_dynamic_resolutions(proposed.dynamic_resolutions.clone()))
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
        let advance = Sequence::advance_of(&self.registry, &self.child_return, view, &facts);
        let batch = self.declaring(
            crate::basis::DecidedAct::Proposals(id.clone()),
            advance,
            FactBatch::new(view.revision(), facts),
        );
        Ok(Some(self.seal(view, batch)?))
    }

    #[allow(clippy::too_many_arguments)]
    fn open_offers(
        &self,
        views: &Views,
        act: &crate::basis::DecidedAct,
        advance: &crate::basis::BasisAdvance,
        nonce: &crate::value::OfferNonce,
        subject: &crate::basis::SubjectKey,
        call: &crate::value::CanonicalDigest,
        plans: &[plan::ExecutableRemedyPlan],
    ) -> (
        crate::value::BlockId,
        Vec<(crate::value::OfferId, plan::PlanId)>,
        Vec<Fact>,
    ) {
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
    ) -> Result<FollowUp, TransitionError> {
        let mut released = Vec::new();
        let mut blocked = Vec::new();
        let mut forks = Vec::new();
        let mut spent = Vec::new();
        let mut settled = Vec::new();
        let mut next = recorded.iter().peekable();
        for (position, call) in proposals.iter().enumerate() {
            let contract = self.validated_contract(call)?;
            match next.next_if(|dispatch| views.dispatch_call(dispatch) == Some(call)) {
                // Only a dispatch still awaiting its result may be handed back for invocation.
                Some(dispatch) if views.is_open(dispatch) && !views.is_succeeded(dispatch) => released.push(Released {
                    dispatch: dispatch.clone(),
                    call: call.clone(),
                    fork: prepared_fork(views, dispatch),
                }),
                Some(dispatch) => {
                    forks.extend(prepared_fork(views, dispatch));
                    settled.push(Settled {
                        dispatch: dispatch.clone(),
                        call: call.clone(),
                        outcome: settled_outcome(views, dispatch),
                    });
                }
                None => {
                    let subject = crate::basis::SubjectKey::Call {
                        trajectory: batch.trajectory.clone(),
                        batch: batch.id.clone(),
                        position: position as u32,
                    };
                    // An input hop the agent has since run replaced this proposal, so the call
                    // this position is about now is the candidate, and the check reads its
                    // substitution. Its offers are the ones already pending on
                    // the same subject, which is why the two must be reported together.
                    let candidate = views.call_candidate(&subject).unwrap_or(call).clone();
                    let stage = views.call_stage(&subject);
                    match check::evaluate(contract, views, &candidate, &stage) {
                        CheckOutcome::Block(raw) => {
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
                                block: plan::plan(&self.registry, views, &candidate, &raw, &stage),
                                call: candidate,
                                block_id,
                                offers,
                            });
                        }
                        CheckOutcome::Allow => match dispatch_for(views, &subject, &candidate) {
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
        Ok(FollowUp::Proposals {
            released,
            blocked,
            forks,
            spent,
            settled,
        })
    }

    fn decide_offer(&self, view: &EngineView, execution: &OfferExecution) -> Result<EngineDecision, TransitionError> {
        let views = view.projection().view(&execution.trajectory);
        let recorded = views
            .offer(&execution.offer)
            .ok_or(TransitionError::UnknownOffer)?
            .clone();
        if recorded.trajectory != execution.trajectory {
            return Err(TransitionError::OfferElsewhere);
        }
        if let Some(end) = recorded.end.clone() {
            return self.ended_offer(&views, &recorded, &end, execution);
        }
        if recorded.basis != views.basis_for(&recorded.subject) {
            return Err(TransitionError::StaleOffer);
        }
        if let crate::basis::SubjectKey::ConfinedResult(dispatch) = &recorded.subject {
            let dispatch = dispatch.clone();
            return self.decide_confined(view, &views, execution, &recorded, &dispatch);
        }
        if let crate::basis::SubjectKey::Return(id) = &recorded.subject {
            let id = id.clone();
            return self.decide_return(view, &views, execution, &recorded, &id);
        }
        let call = self.offer_call(&views, &recorded);
        let contract = self.validated_contract(&call)?;
        let stage = views.call_stage(&recorded.subject);
        let live = match check::evaluate(contract, &views, &call, &stage) {
            CheckOutcome::Block(raw) => plan::plan(&self.registry, &views, &call, &raw, &stage)
                .plans
                .iter()
                .filter_map(plan::RemedyPlan::executable)
                .any(|offered| offered == &recorded.plan)
                .then_some(raw),
            // The block is gone: whatever the agent would have remedied, nothing needs it now.
            CheckOutcome::Allow => None,
        };
        let Some(raw) = live else {
            return self.invalidated(view, &views, execution, &recorded);
        };
        match (&execution.outcome, recorded.plan.hop()) {
            (OfferOutcome::Derived(evidence), Some(sanitizer)) => self.hop_call(
                view, &views, execution, &recorded, contract, &raw, &call, &stage, sanitizer, evidence,
            ),
            (OfferOutcome::Approved(evidence), None) => {
                self.approve_offer(view, &views, execution, &recorded, contract, &raw, &call, evidence)
            }
            (OfferOutcome::Denied { authority }, None) => {
                self.deny_offer(view, &views, execution, &recorded, &call, &raw, &stage, authority)
            }
            _ => Err(TransitionError::PlanOutcomeMismatch),
        }
    }

    fn invalidated(
        &self,
        view: &EngineView,
        views: &Views,
        execution: &OfferExecution,
        recorded: &crate::projection::RecordedOffer,
    ) -> Result<EngineDecision, TransitionError> {
        let batch = FactBatch::new(
            views.revision(),
            vec![Fact::OfferInvalidated {
                trajectory: recorded.trajectory.clone(),
                offer: execution.offer,
            }],
        );
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
            return self.invalidated(view, views, execution, recorded);
        };
        let lineage = views.lineage(&subject);
        let contract = self.dispatch_contract(views, dispatch)?;
        let stage = plan::confined_stage(&self.registry, contract, &receiving, &value.label, &residual, &lineage);
        if !stage.contains(&recorded.plan) {
            return self.invalidated(view, views, execution, recorded);
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
                view, views, execution, dispatch, &call, &receiving, &value, &lineage, sanitizer, evidence, facts,
            ),
            (OfferOutcome::Approved(evidence), None) if evidence.is_empty() => {
                self.accept_candidate(view, views, execution, dispatch, &call, facts)
            }
            _ => Err(TransitionError::PlanOutcomeMismatch),
        }
    }

    fn accept_candidate(
        &self,
        view: &EngineView,
        views: &Views,
        execution: &OfferExecution,
        dispatch: &DispatchId,
        call: &ResolvedCall,
        mut facts: Vec<Fact>,
    ) -> Result<EngineDecision, TransitionError> {
        let admitted = self
            .admit_result(
                views,
                dispatch,
                call,
                ResultAdmission::CandidateAccepted { offer: execution.offer },
            )
            .unwrap_or_else(|error| unreachable!("the confined stage admits what the log already proved: {error}"));
        facts.extend(admitted.facts);
        let value = crossed(&facts);
        let advance = Sequence::advance_of(&self.registry, &self.child_return, view, &facts);
        let batch = self.declaring(
            crate::basis::DecidedAct::Offer(execution.offer),
            advance,
            FactBatch::new(views.revision(), facts),
        );
        Ok(EngineDecision {
            append: Some(self.seal(view, batch)?),
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
        receiving: &EstablishedLabel,
        predecessor: &crate::value::LabeledValue,
        lineage: &SanitizerLineage,
        sanitizer: &SanitizerName,
        evidence: &Evidence,
        mut facts: Vec<Fact>,
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
            .derive_output(&predecessor.label, &self.validated_contract(call)?.tags)
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
            via: crate::candidate::DerivedVia::Sanitizer {
                name: sanitizer.clone(),
                transition: registered.transition.clone(),
            },
            derived: DerivedCandidate::Result {
                dispatch: dispatch.clone(),
                source: source_digest,
                from: ConfinedFrom::Offer(execution.offer),
                value: crate::value::LabeledValue::new(body.clone(), label),
                residual,
            },
            lineage,
        });
        if staged {
            let (batch, confined) = self.staged(
                view,
                views,
                crate::basis::DecidedAct::Offer(execution.offer),
                execution.offer_nonce,
                dispatch,
                facts,
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
        let admitted = self
            .admit_result(
                &after.view(views.trajectory()),
                dispatch,
                call,
                ResultAdmission::CandidateAdmissible,
            )
            .unwrap_or_else(|error| unreachable!("the confined stage admits what this act just derived: {error}"));
        facts.extend(admitted.facts);
        let value = crossed(&facts);
        let advance = Sequence::advance_of(&self.registry, &self.child_return, view, &facts);
        let batch = self.declaring(
            crate::basis::DecidedAct::Offer(execution.offer),
            advance,
            FactBatch::new(views.revision(), facts),
        );
        Ok(EngineDecision {
            append: Some(self.seal(view, batch)?),
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
    ) -> Result<EngineDecision, TransitionError> {
        let subject = crate::basis::SubjectKey::Return(id.clone());
        let Some(pending) = views.pending_return(id).cloned() else {
            return self.invalidated(view, views, execution, recorded);
        };
        let fold = views.branch_label(id.child());
        let lineage = views.lineage(&subject);
        let standing = match views.candidate(&subject).cloned() {
            Some(DerivedCandidate::Return {
                value,
                residual: Some(residual),
                ..
            }) => Some((value, residual)),
            Some(_) => return self.invalidated(view, views, execution, recorded),
            None if pending.policy == ReturnPolicy::Raw => None,
            None => return self.invalidated(view, views, execution, recorded),
        };
        let (label, body, residual) = match &standing {
            Some((value, residual)) => (value.label.clone(), value.body.clone(), residual.clone()),
            None => (
                fold.bound().clone().into_label(),
                pending.body().clone(),
                Narrowing {
                    from: pending.receiving.clone(),
                    to: pending.receiving.combine(fold.bound()),
                },
            ),
        };
        let stage = match plan::return_stage(
            &self.registry,
            views,
            id.child(),
            &fold,
            &label,
            &body,
            &residual,
            &lineage,
        ) {
            plan::ReturnStagePlan::Stage(plans) => plans,
            plan::ReturnStagePlan::Resolve { .. } => return self.invalidated(view, views, execution, recorded),
        };
        if !stage.contains(&recorded.plan) {
            return self.invalidated(view, views, execution, recorded);
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
        fold: &PartialLabel,
        lineage: &SanitizerLineage,
        residual: Narrowing,
        mut facts: Vec<Fact>,
    ) -> Result<EngineDecision, TransitionError> {
        let (value, derivation) = match candidate {
            Some(value) => {
                let sanitizer = lineage
                    .names()
                    .last()
                    .expect("a return candidate's lineage names the sanitizer that derived it")
                    .clone();
                let transition = self
                    .registry
                    .sanitizer(&sanitizer)
                    .ok_or(TransitionError::SanitizerUnapplicable)?
                    .transition
                    .clone();
                (
                    value,
                    ReturnDerivation::Sanitized {
                        sanitizer,
                        raw_digest: pending.digest,
                        transition,
                    },
                )
            }
            None => (
                crate::value::LabeledValue::new(pending.body().clone(), fold.bound().clone().into_label()),
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
        ));
        let advance = Sequence::advance_of(&self.registry, &self.child_return, view, &facts);
        let batch = self.declaring(
            crate::basis::DecidedAct::Offer(execution.offer),
            advance,
            FactBatch::new(views.revision(), facts),
        );
        Ok(EngineDecision {
            append: Some(self.seal(view, batch)?),
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
        fold: &PartialLabel,
        lineage: &SanitizerLineage,
        sanitizer: &SanitizerName,
        evidence: &Evidence,
        mut facts: Vec<Fact>,
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
            None => (fold.bound().clone().into_label(), pending.digest),
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
            .derive_output(&from_label, &[])
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
            via: DerivedVia::Sanitizer {
                name: sanitizer.clone(),
                transition: registered.transition.clone(),
            },
            derived: DerivedCandidate::Return {
                source: source_digest,
                from: ConfinedFrom::Offer(execution.offer),
                value: value.clone(),
                residual: residual.clone(),
            },
            lineage: lineage.clone(),
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
                    transition: registered.transition.clone(),
                },
                None,
            ));
            let advance = Sequence::advance_of(&self.registry, &self.child_return, view, &facts);
            let batch = self.declaring(
                crate::basis::DecidedAct::Offer(execution.offer),
                advance,
                FactBatch::new(views.revision(), facts),
            );
            return Ok(EngineDecision {
                append: Some(self.seal(view, batch)?),
                follow_up: FollowUp::Offer(OfferFollowUp::Admitted { value: body.clone() }),
            });
        };
        // The stage the successor leaves. A hop lands only while its offer's basis is current, so
        // a consumed dimension resolvable now was resolvable when this stage's predecessor
        // opened; a fresh `Resolve` has no live path, and the empty stage it would leave
        // re-plans on the child's next report.
        let stage = match plan::return_stage(
            &self.registry,
            views,
            id.child(),
            fold,
            &label,
            body,
            &residual,
            &lineage,
        ) {
            plan::ReturnStagePlan::Stage(plans) => plans,
            plan::ReturnStagePlan::Resolve { .. } => Vec::new(),
        };
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
        )?;
        Ok(EngineDecision {
            append: Some(batch),
            follow_up: FollowUp::Offer(OfferFollowUp::ReturnStaged(Box::new(staged))),
        })
    }

    /// One input-substitution progress hop.
    ///
    /// The sanitizer read the engine's own canonical argument bytes and returned one complete
    /// replacement object. Its bytes are untrusted: the engine strictly parses them, schema-checks
    /// them against the callee's declared parameters, constructs the canonical arguments itself,
    /// and only then has a call to measure. Nothing about the replacement is taken on the runtime's
    /// word, and the tool is never replaced.
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
        contract: &ToolContract,
        raw: &crate::check::RawBlock,
        call: &ResolvedCall,
        stage: &CallStage,
        sanitizer: &SanitizerName,
        evidence: &Evidence,
    ) -> Result<EngineDecision, TransitionError> {
        if !raw.unestablished.is_empty() {
            return Err(PlanError::Unestablished(raw.unestablished.clone()).into());
        }
        let Evidence::Sanitizer {
            sanitizer: named,
            source,
            derived: body,
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
            .derive_input(&stage.released(&views.current_label()), &contract.tags)
            .ok_or(TransitionError::SanitizerUnapplicable)?;
        let lineage = stage
            .lineage()
            .extend(sanitizer.clone())
            .ok_or(TransitionError::SanitizerUnapplicable)?;
        let substituted = substituted_call(contract, call, body)?;
        let next = CallStage::substituting(label.clone(), lineage.clone());
        let after = check::evaluate(contract, views, &substituted, &next);
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
            via: DerivedVia::Sanitizer {
                name: sanitizer.clone(),
                transition: registered.transition.clone(),
            },
            derived,
            lineage,
        });
        let staged = Sequence::advance_of(&self.registry, &self.child_return, view, &facts);
        let act = crate::basis::DecidedAct::Offer(execution.offer);
        let follow_up = match after {
            CheckOutcome::Allow => {
                let (dispatch, opening) =
                    opened_dispatch(contract, views, &substituted, Some(recorded.subject.clone()));
                facts.push(opening);
                OfferFollowUp::Released(Box::new(Released {
                    dispatch,
                    call: substituted,
                    fork: None,
                }))
            }
            CheckOutcome::Block(raw) => {
                let planned = plan::plan(&self.registry, views, &substituted, &raw, &next);
                let (block_id, offers, opened) = self.open_offers(
                    views,
                    &act,
                    &staged,
                    &execution.offer_nonce,
                    &recorded.subject,
                    &substituted.digest(),
                    &Engine::executable(&planned),
                );
                facts.extend(opened);
                OfferFollowUp::Substituted {
                    block: Box::new(Blocked {
                        call: substituted,
                        block: planned,
                        block_id,
                        offers,
                    }),
                }
            }
        };
        let advance = Sequence::advance_of(&self.registry, &self.child_return, view, &facts);
        let batch = self.declaring(act, advance, FactBatch::new(views.revision(), facts));
        Ok(EngineDecision {
            append: Some(self.seal(view, batch)?),
            follow_up: FollowUp::Offer(follow_up),
        })
    }

    fn substituted_repeat(
        &self,
        views: &Views,
        recorded: &crate::projection::RecordedOffer,
        execution: &OfferExecution,
    ) -> OfferFollowUp {
        let Some(candidate) = views.call_candidate(&recorded.subject).cloned() else {
            return OfferFollowUp::Invalidated;
        };
        if views.pending_block(&recorded.subject).is_some() {
            return match self.reblocked(views, recorded, execution) {
                Ok(Some(block)) => OfferFollowUp::Substituted { block: Box::new(block) },
                _ => OfferFollowUp::Invalidated,
            };
        }
        match dispatch_for(views, &recorded.subject, &candidate) {
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
        }
    }

    fn offer_call(&self, views: &Views, recorded: &crate::projection::RecordedOffer) -> ResolvedCall {
        if let Some(candidate) = views.call_candidate(&recorded.subject) {
            return candidate.clone();
        }
        let crate::basis::SubjectKey::Call { batch, position, .. } = &recorded.subject else {
            unreachable!("an opened offer's subject is a call candidate")
        };
        views
            .decided_batch(batch)
            .and_then(|decided| decided.proposals.get(*position as usize))
            .expect("an opened offer names a proposal of a decided batch")
            .clone()
    }

    fn ended_offer(
        &self,
        views: &Views,
        recorded: &crate::projection::RecordedOffer,
        end: &crate::projection::OfferEnd,
        execution: &OfferExecution,
    ) -> Result<EngineDecision, TransitionError> {
        use crate::projection::OfferEnd;
        if let crate::basis::SubjectKey::ConfinedResult(dispatch) = &recorded.subject {
            return match (end, &execution.outcome, recorded.plan.hop()) {
                (OfferEnd::Accepted, OfferOutcome::Derived(_), Some(_))
                | (OfferEnd::Accepted, OfferOutcome::Approved(_), None) => Ok(EngineDecision {
                    append: None,
                    follow_up: FollowUp::Offer(self.confined_repeat(views, dispatch)),
                }),
                (OfferEnd::Invalidated, _, _) => Ok(EngineDecision {
                    append: None,
                    follow_up: FollowUp::Offer(OfferFollowUp::Invalidated),
                }),
                (_, OfferOutcome::Derived(_), None) | (_, OfferOutcome::Approved(_), Some(_)) => {
                    Err(TransitionError::PlanOutcomeMismatch)
                }
                _ => Err(TransitionError::TerminalOffer),
            };
        }
        if let crate::basis::SubjectKey::Return(id) = &recorded.subject {
            return match (end, &execution.outcome, recorded.plan.hop()) {
                (OfferEnd::Accepted, OfferOutcome::Derived(_), Some(name)) if !name.is_attest_schema() => {
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
                (OfferEnd::Accepted, OfferOutcome::Approved(_), None) => Ok(EngineDecision {
                    append: None,
                    follow_up: FollowUp::Offer(self.return_repeat(views, id)),
                }),
                (OfferEnd::Invalidated, _, _) => Ok(EngineDecision {
                    append: None,
                    follow_up: FollowUp::Offer(OfferFollowUp::Invalidated),
                }),
                (OfferEnd::Accepted, OfferOutcome::Derived(_), Some(_)) => Err(TransitionError::PlanOutcomeMismatch),
                (_, OfferOutcome::Derived(_), None) | (_, OfferOutcome::Approved(_), Some(_)) => {
                    Err(TransitionError::PlanOutcomeMismatch)
                }
                _ => Err(TransitionError::TerminalOffer),
            };
        }
        if recorded.plan.hop().is_some() {
            return match (end, &execution.outcome) {
                (OfferEnd::Accepted, OfferOutcome::Derived(_)) => Ok(EngineDecision {
                    append: None,
                    follow_up: FollowUp::Offer(self.substituted_repeat(views, recorded, execution)),
                }),
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
                call: views
                    .approval(&execution.offer)
                    .ok_or(TransitionRefusal::UndischargedAcceptance)?
                    .call
                    .clone(),
            },
            (OfferEnd::Denied(recorded_authority), OfferOutcome::Denied { authority })
                if recorded_authority == authority =>
            {
                match self.reblocked(views, recorded, execution)? {
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
    ) -> Result<Option<Blocked>, TransitionError> {
        let call = self.offer_call(views, recorded);
        let contract = self.validated_contract(&call)?;
        let stage = views.call_stage(&recorded.subject);
        let CheckOutcome::Block(raw) = check::evaluate(contract, views, &call, &stage) else {
            return Ok(None);
        };
        let (block_id, offers) = views
            .pending_block(&recorded.subject)
            .unwrap_or((offer_block(recorded, execution, &call), Vec::new()));
        Ok(Some(Blocked {
            block: plan::plan(&self.registry, views, &call, &raw, &stage),
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
        contract: &ToolContract,
        raw: &crate::check::RawBlock,
        call: &ResolvedCall,
        evidence: &[execute::AuthorityEvidence],
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
        if !raw.unestablished.is_empty() {
            return Err(PlanError::Unestablished(raw.unestablished.clone()).into());
        }
        execute::rulings_cover(
            &self.registry,
            contract,
            raw,
            evidence.iter().map(|given| (&given.authority, given.covers.as_slice())),
        )?;
        if evidence.iter().any(|given| given.offer != execution.offer) {
            return Err(PlanError::EvidenceOfferMismatch.into());
        }
        // And each reviewed exactly this call at the fold the release will run against.
        let live = views.current_label();
        if evidence
            .iter()
            .any(|given| given.reviewed.tool != contract.name || given.reviewed.trajectory_label != live)
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
        let advance = Sequence::advance_of(&self.registry, &self.child_return, view, &facts);
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
        });
        let batch = self.declaring(
            crate::basis::DecidedAct::Offer(execution.offer),
            advance,
            FactBatch::new(views.revision(), facts),
        );
        Ok(EngineDecision {
            append: Some(self.seal(view, batch)?),
            follow_up: FollowUp::Offer(OfferFollowUp::Approved { call: call.clone() }),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn deny_offer(
        &self,
        view: &EngineView,
        views: &Views,
        execution: &OfferExecution,
        recorded: &crate::projection::RecordedOffer,
        call: &ResolvedCall,
        raw: &crate::check::RawBlock,
        stage: &CallStage,
        authority: &AuthorityName,
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
        let advance = Sequence::advance_of(&self.registry, &self.child_return, view, &facts);
        let planned = plan::plan(&self.registry, &after, call, raw, stage);
        let (block_id, offers, opened) = self.open_offers(
            &after,
            &crate::basis::DecidedAct::Offer(execution.offer),
            &advance,
            &execution.offer_nonce,
            &recorded.subject,
            &call.digest(),
            &Engine::executable(&planned),
        );
        facts.extend(opened);
        let batch = self.declaring(
            crate::basis::DecidedAct::Offer(execution.offer),
            advance,
            FactBatch::new(views.revision(), facts),
        );
        Ok(EngineDecision {
            append: Some(self.seal(view, batch)?),
            follow_up: FollowUp::Offer(OfferFollowUp::Denied {
                block: Box::new(Blocked {
                    call: call.clone(),
                    block: planned,
                    block_id,
                    offers,
                }),
            }),
        })
    }

    /// The opening batch of a fresh root trajectory family: one `TrajectoryOpened`
    /// record against the empty log. The runtime appends it before any other family event.
    pub fn open_trajectory(&self, trajectory: &TrajectoryId) -> FactBatch {
        FactBatch::new(
            Revision::ZERO,
            vec![Fact::TrajectoryOpened {
                trajectory: trajectory.clone(),
                dialect: self.dialect,
                profile: self.profile().clone(),
                policy_digest: self.identity,
                open_vectors: self.open_vectors(),
            }],
        )
    }

    /// The strict cold-replay verifier of the durable opening: exactly one
    /// `TrajectoryOpened`, first in the family log, naming the replayed root, at a supported
    /// dialect, carrying the supplied policy's digest, declaration, and derived vectors. The
    /// recorded declaration must equal this engine's validated profile byte for byte — which
    /// subsumes re-running the coverage matrix over it — and the recorded vectors must rederive
    /// from it exactly.
    pub fn verify_opening(&self, facts: &[Fact], trajectory: &TrajectoryId) -> Result<(), OpeningTransitionRefusal> {
        let mut openings = facts.iter().enumerate().filter_map(|(index, fact)| match fact {
            Fact::TrajectoryOpened {
                trajectory,
                dialect,
                profile,
                policy_digest,
                open_vectors,
            } => Some((index, trajectory, dialect, profile, policy_digest, open_vectors)),
            _ => None,
        });
        let Some((index, recorded_trajectory, dialect, recorded_profile, policy_digest, open_vectors)) =
            openings.next()
        else {
            return Err(OpeningTransitionRefusal::Missing);
        };
        if openings.next().is_some() {
            return Err(OpeningTransitionRefusal::Duplicate);
        }
        if index != 0 {
            return Err(OpeningTransitionRefusal::NotFirst);
        }
        if recorded_trajectory != trajectory {
            return Err(OpeningTransitionRefusal::WrongTrajectory {
                found: recorded_trajectory.as_str().to_string(),
            });
        }
        if *dialect != self.dialect {
            return Err(OpeningTransitionRefusal::UnsupportedDialect { found: dialect.value() });
        }
        if policy_digest != &self.identity {
            return Err(OpeningTransitionRefusal::DigestMismatch);
        }
        if recorded_profile != self.profile() {
            return Err(OpeningTransitionRefusal::ProfileMismatch);
        }
        if open_vectors != &self.open_vectors() {
            return Err(OpeningTransitionRefusal::VectorMismatch);
        }
        Ok(())
    }

    /// Convert untrusted provider bytes into the only call representation accepted by this
    /// engine. Tool lookup, strict JSON scanning, schema validation, and RFC 8785 rendering
    /// happen together, so outer runtimes cannot construct a call under a different schema.
    pub fn resolve_call(&self, tool: ToolName, raw_arguments: &[u8]) -> Result<ResolvedCall, EngineError> {
        let contract = self.checkable_contract(&tool)?;
        let arguments =
            CanonicalArguments::from_raw(raw_arguments, &contract.parameters).map_err(EngineError::InvalidCall)?;
        Ok(ResolvedCall::new(tool, arguments))
    }

    /// Evaluate a proposed call: allow, or block carrying everything that stopped it at once —
    /// the requirement gaps, the narrowing where one fired, and the values whose consumed
    /// dimension no cast has established. Resolution is the runtime's job;
    /// the runtime re-checks after each landed cast, so a surfaced block is the residual.
    pub fn check(&self, views: &Views, call: &ResolvedCall) -> Result<CheckOutcome, EngineError> {
        let contract = self.validated_contract(call)?;
        Ok(check::evaluate(contract, views, call, &CallStage::default()))
    }

    /// Open a dispatch for a call that **passes the check as-is**. Re-checks and refuses any
    /// block — unestablished values included (a narrowing is accepted through
    /// [`Engine::execute_remedy_plan`], not here), so
    /// the engine never emits an appendable dispatch for a call it would not allow. Folds nothing —
    /// the label folds only when the result value is admitted.
    pub fn open_dispatch(&self, views: &Views, call: &ResolvedCall) -> Result<FactBatch, EngineError> {
        let contract = self.validated_contract(call)?;
        if views.has_ended(views.trajectory()) {
            return Err(EngineError::BranchEnded);
        }
        match check::evaluate(contract, views, call, &CallStage::default()) {
            CheckOutcome::Allow => {
                let (_, fact) = opened_dispatch(contract, views, call, None);
                Ok(FactBatch::new(views.revision(), vec![fact]))
            }
            _ => Err(EngineError::NotAllowed),
        }
    }

    /// Execute a remedy plan: land the covering rulings, the narrowing acceptance, and the dispatch
    /// as one atomic batch, enforcing the plan's exact grouped assignment and mandate coverage. The
    /// chosen plan is matched by value against the live offers — the return-path staleness story.
    pub fn execute_remedy_plan(
        &self,
        views: &Views,
        chosen: &plan::ExecutableRemedyPlan,
        call: &ResolvedCall,
        rulings: &[Ruling],
    ) -> Result<FactBatch, PlanError> {
        if self.registry.provider_run_contract(call.tool()).is_some() {
            return Err(PlanError::ProviderRunTool(call.tool().as_str().to_string()));
        }
        // An ended branch releases nothing more, whichever path reaches the dispatch.
        if views.has_ended(views.trajectory()) {
            return Err(PlanError::BranchEnded);
        }
        execute::execute_remedy_plan(&self.registry, views, chosen, call, rulings)
    }

    /// Close a dispatch and admit its result — raw, cast-resolved, or withheld. The label folds only
    /// from an admitted value, never from the close.
    pub fn admit_result(
        &self,
        views: &Views,
        dispatch: &DispatchId,
        call: &ResolvedCall,
        admission: ResultAdmission,
    ) -> Result<FactBatch, AdmitError> {
        admit::admit_result(&self.registry, views, dispatch, call, admission)
    }

    /// Record observed success for a still-open dispatch: its declared effects commit now, at the
    /// one append point the spec puts at success, while any value finalization — an
    /// output sanitizer derivation, a pending-cast resolution — is still in flight. See
    /// [`crate::admit::observe_success`].
    pub fn observe_success(
        &self,
        views: &Views,
        dispatch: &DispatchId,
        call: &ResolvedCall,
        observed: crate::fact::ObservedResult,
    ) -> Result<FactBatch, AdmitError> {
        admit::observe_success(&self.registry, views, dispatch, call, observed)
    }

    /// Attach the sound remedies to a raw block: executable plans and prose recommendations. An empty
    /// result (no plans, no curative recommendation) is a proof the block is unliftable over the
    /// implemented remedy subset — see [`crate::plan`].
    pub fn plan(&self, views: &Views, call: &ResolvedCall, raw: &RawBlock) -> Result<PlannedBlock, EngineError> {
        self.validated_contract(call)?;
        Ok(plan::plan(&self.registry, views, call, raw, &CallStage::default()))
    }

    /// Establish an admitted Unknown value's complete label by a validated whole-source cast
    /// answer: one `CastApplied` fact or nothing.
    pub fn admit_cast(
        &self,
        views: &Views,
        value: crate::value::ValueId,
        answer: CastAnswer,
    ) -> Result<FactBatch, CastError> {
        admit::admit_cast(&self.registry, views, value, answer)
    }

    /// Seed a child branch at the parent's current label with an immutable fork binding carrying
    /// the deployment's `[child]` return policy — the binding is the engine's validated state,
    /// never a caller-supplied per-fork choice. Branching exists only where the
    /// deployment declares context control. See [`crate::branch`].
    pub fn seed_child(&self, parent: &Views, child: &TrajectoryId) -> Result<FactBatch, BranchError> {
        branch::seed_child(&self.registry, parent, child, self.child_return.clone())
    }

    /// Record a child's returned value at an engine-derived label AND merge it into the direct
    /// parent — one atomic batch, no orphanable intermediate state. A raw crossing that would
    /// narrow the parent is refused (`ReturnNarrowsParent`): it exists only through an executed
    /// return plan. See [`crate::branch`].
    pub fn submit_child_return(
        &self,
        parent: &Views,
        child: &TrajectoryId,
        ret: ReturnSubmission,
    ) -> Result<FactBatch, BranchError> {
        branch::submit_child_return(&self.registry, parent, child, ret)
    }

    /// Decide whether a raw return by `child` may merge silently, and if not, which return plans
    /// could cross it. Both folds and the linkage come from the parent's one projection snapshot.
    /// See [`crate::branch`].
    pub fn check_child_return(&self, parent: &Views, child: &TrajectoryId) -> Result<branch::ReturnCheck, BranchError> {
        branch::check_child_return(&self.registry, parent, child)
    }

    /// Record a child's void return: the child-attributed terminal that ends the branch and
    /// crosses no value — no merge, no label contribution. A branch ends at most once.
    /// See [`crate::branch`].
    pub fn submit_void_return(&self, parent: &Views, child: &TrajectoryId) -> Result<FactBatch, BranchError> {
        branch::submit_void_return(parent, child)
    }

    /// Execute one offered return plan as a single atomic batch: crossing, acceptance where the
    /// plan carries one, and merge. Re-derives the block from the live views and refuses a chosen
    /// plan the fresh offers no longer contain. See [`crate::branch`].
    pub fn execute_child_return_plan(
        &self,
        parent: &Views,
        child: &TrajectoryId,
        chosen: branch::ReturnPlan,
        submission: ReturnSubmission,
    ) -> Result<FactBatch, BranchError> {
        branch::execute_child_return_plan(&self.registry, parent, child, chosen, submission)
    }

    fn dispatch_contract(&self, views: &Views, dispatch: &DispatchId) -> Result<&ToolContract, TransitionError> {
        let tool = views.dispatch_tool(dispatch).ok_or(TransitionError::UnknownDispatch)?;
        self.checkable_contract(tool).map_err(TransitionError::Call)
    }

    fn checkable_contract(&self, tool: &ToolName) -> Result<&ToolContract, EngineError> {
        contract_for(&self.registry, tool)
    }

    fn contract(&self, call: &ResolvedCall) -> Result<&ToolContract, EngineError> {
        self.checkable_contract(call.tool())
    }

    fn validated_contract(&self, call: &ResolvedCall) -> Result<&ToolContract, EngineError> {
        let contract = self.contract(call)?;
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

fn substituted_call(
    contract: &ToolContract,
    call: &ResolvedCall,
    body: &ValueBody,
) -> Result<ResolvedCall, TransitionError> {
    let arguments = crate::params::CanonicalArguments::from_raw(body.as_str().as_bytes(), &contract.parameters)
        .map_err(|error| TransitionError::Call(EngineError::InvalidCall(error)))?;
    Ok(call.substituting(arguments))
}

fn dispatch_for(views: &Views, subject: &crate::basis::SubjectKey, candidate: &ResolvedCall) -> Option<DispatchId> {
    views
        .subject_dispatch(subject)
        .cloned()
        .or_else(|| views.dispatch_of(candidate))
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

fn advance_of(engine: &Engine, view: &EngineView, batch: &FactBatch) -> crate::basis::BasisAdvance {
    Sequence::advance_of(&engine.registry, &engine.child_return, view, &batch.facts)
}

fn approved_release(
    registry: &Registry,
    contract: &ToolContract,
    call: &ResolvedCall,
    trajectory: &TrajectoryId,
    dispatch: &DispatchId,
    approval: &crate::projection::PreparedApproval,
) -> Vec<Fact> {
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
    }));
    if let Some(sanitizer) = &approval.sanitizer {
        facts.push(Fact::OutputSanitizerBound {
            trajectory: trajectory.clone(),
            dispatch: dispatch.clone(),
            plan: approval.plan,
            sanitizer: sanitizer.clone(),
            contribution: crate::plan::bound_contribution(registry, contract, call, sanitizer)
                .expect("a prepared approval binds an output sanitizer enumeration found applicable"),
        });
    }
    facts
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
        BranchError::ReturnPolicyMismatch => TransitionError::BoundReturnSanitizer,
        other => unreachable!("the child-return boundary refuses before reaching {other}"),
    }
}

/// Build the `DispatchOpened` fact for a call: its proposed committed label, the effects it would
/// commit on success, its occurrence (a repeat identical call is a new dispatch), and the subject
/// whose decision released it where one did. Shared by the clean-allow path
/// ([`Engine::open_dispatch`]) and atomic plan execution ([`crate::execute`]).
pub(crate) fn opened_dispatch(
    contract: &ToolContract,
    views: &Views,
    call: &ResolvedCall,
    subject: Option<crate::basis::SubjectKey>,
) -> (DispatchId, Fact) {
    let digest = call.digest();
    let occurrence = views.dispatch_count(&digest);
    let dispatch = DispatchId::new(views.trajectory().clone(), digest, occurrence);
    let current = views.current_label();
    let fact = Fact::DispatchOpened {
        trajectory: views.trajectory().clone(),
        dispatch: dispatch.clone(),
        tool: call.tool().clone(),
        arguments: call.canonical_arguments().clone(),
        proposed_label: check::committed_label_for_call(contract, &current, call)
            .bound()
            .clone(),
        receiving: current.bound().clone(),
        proposed_effects: contract.emits.clone(),
        dynamic_resolutions: call.dynamic_resolutions().to_vec(),
        subject,
    };
    (dispatch, fact)
}

pub(crate) struct SiblingRelease {
    pub(crate) dispatch: DispatchId,
    pub(crate) consumes: Option<crate::value::OfferId>,
    pub(crate) prepares_fork: Option<ForkId>,
    pub(crate) facts: Vec<Fact>,
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

/// The ordered in-batch composition, position by position: what each proposed sibling
/// does, and the records that say so. `None` at a position is a refusal.
pub(crate) fn compose_batch<'a>(
    registry: &Registry,
    child_return: &ReturnPolicy,
    working: &mut std::borrow::Cow<'a, Projection>,
    batch: ComposingBatch<'_>,
    proposals: &[ResolvedCall],
    spawn: Option<SpawnMark>,
    approval: &impl Fn(&Views, &ResolvedCall) -> Option<crate::value::OfferId>,
) -> Result<Vec<Option<SiblingRelease>>, (usize, EngineError)> {
    let trajectory = batch.trajectory;
    let singleton = proposals.len() == 1;
    let mut composed = Vec::with_capacity(proposals.len());
    // Whether any earlier sibling was refused, and so will be re-planned against the final state.
    let mut refused = false;
    for (position, call) in proposals.iter().enumerate() {
        let release = {
            let views = working.view(trajectory);
            let contract = contract_for(registry, call.tool()).map_err(|error| (position, error))?;
            contract
                .parameters
                .validate(call.arguments())
                .map_err(|error| (position, EngineError::InvalidCall(error)))?;
            let consumes = match check::evaluate(contract, &views, call, &CallStage::default()) {
                CheckOutcome::Allow => None,
                CheckOutcome::Block(_) if singleton => match approval(&views, call) {
                    Some(offer) => Some(offer),
                    None => {
                        refused = true;
                        composed.push(None);
                        continue;
                    }
                },
                CheckOutcome::Block(_) => {
                    refused = true;
                    composed.push(None);
                    continue;
                }
            };
            let subject = batch.subject(position);
            let (dispatch, opening) = opened_dispatch(contract, &views, call, Some(subject));
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
                    registry, contract, call, trajectory, &dispatch, &prepared,
                ));
            }
            facts.push(opening);
            let prepares_fork = if spawn == Some(SpawnMark::at(position)) {
                let shape = marked_return_shape(call).map_err(|error| (position, error))?;
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
            }
        };
        if position + 1 < proposals.len() || refused {
            for fact in &release.facts {
                working.to_mut().fold(fact);
            }
        }
        composed.push(Some(release));
    }
    Ok(composed)
}

fn contract_for<'a>(registry: &'a Registry, tool: &ToolName) -> Result<&'a ToolContract, EngineError> {
    registry.tool(tool).ok_or_else(|| {
        if registry.provider_run_contract(tool).is_some() {
            EngineError::ProviderRunTool(tool.as_str().to_string())
        } else {
            EngineError::UnknownTool(tool.as_str().to_string())
        }
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::check::Gap;
    use crate::contract::{
        AudienceRequirement, Delta, HistoryRequirement, LabelRequirements, RecipientSpec, Requires, ToolContract,
    };
    use crate::fact::{EffectKind, EffectSet, Fact, Revision};
    use crate::label::PartialLabel;
    use crate::label::{Audience, Dim, Dimension, Label, ReaderId, Trust};
    use crate::names::MarkName;
    use crate::projection::Projection;
    use crate::registry::{RegistryConfig, TrustChain};
    use crate::value::{LabeledValue, Provenance, ToolName, TrajectoryId, ValueBody, ValueId};
    use serde_json::json;

    const SUSPICIOUS: Trust = Trust::new(0);
    const TRUSTED: Trust = Trust::new(1);

    fn nonce() -> crate::value::OfferNonce {
        crate::value::OfferNonce::new([7u8; 32])
    }

    fn traj() -> TrajectoryId {
        TrajectoryId::new("t")
    }

    fn engine(tools: Vec<ToolContract>) -> Engine {
        let cfg = RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools,
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![],
        };
        open_engine(cfg)
    }

    fn forked_child(e: &Engine, log: &[Fact], child: &TrajectoryId) -> Vec<Fact> {
        e.seed_child(
            &Projection::build(log, Revision::new(log.len() as u64)).view(&traj()),
            child,
        )
        .expect("the parent may fork")
        .facts
    }

    fn child_report(log: &[Fact], child: &TrajectoryId, submission: ChildSubmission) -> EngineEvent {
        EngineEvent::ChildReturn(ChildReport {
            child: child.clone(),
            fork: log.iter().find_map(|fact| match fact {
                Fact::ForkOpened { trajectory, fork } if trajectory == child => Some(fork.clone()),
                _ => None,
            }),
            submission,
            evidence: Vec::new(),
            offer_nonce: nonce(),
        })
    }

    fn open_engine(cfg: RegistryConfig) -> Engine {
        open_engine_returning(cfg, ReturnPolicy::Raw)
    }

    fn open_engine_returning(cfg: RegistryConfig, child_return: ReturnPolicy) -> Engine {
        let profile = crate::profile::covering_declaration(&cfg);
        Engine::open(DeploymentPolicy {
            registry: cfg,
            planner_cap: crate::registry::PlannerCap::default(),
            dialect: PolicyDialectVersion::new(1),
            child_return,
            profile,
        })
        .unwrap()
    }

    fn user_value(label: Label) -> Fact {
        Fact::ValueAdmitted {
            trajectory: traj(),
            value: LabeledValue::new(ValueBody::new("body"), label),
            provenance: Provenance::UserInput,
        }
    }

    fn known(trust: Trust, audience: Audience) -> Label {
        Label::new(Dim::Known(trust), Dim::Known(audience))
    }

    fn established(trust: Trust, audience: Audience) -> EstablishedLabel {
        EstablishedLabel::new(trust, audience)
    }

    fn partial(trust: Trust, audience: Audience) -> PartialLabel {
        PartialLabel::established(EstablishedLabel::new(trust, audience))
    }

    fn call(tool: &str, args: serde_json::Value) -> ResolvedCall {
        ResolvedCall::new(ToolName::new(tool), crate::params::test_arguments(&args))
    }

    fn raw(call: &ResolvedCall) -> crate::transition::ProposedCall {
        crate::transition::ProposedCall {
            tool: call.tool().clone(),
            arguments: call.canonical_arguments().canonical_bytes().to_vec(),
            dynamic_resolutions: call.dynamic_resolutions().to_vec(),
        }
    }

    fn check(engine: &Engine, log: &[Fact], call: &ResolvedCall) -> CheckOutcome {
        let p = Projection::build(log, Revision::new(log.len() as u64));
        let t = traj();
        engine.check(&p.view(&t), call).unwrap()
    }

    fn crm_tool() -> ToolContract {
        ToolContract {
            name: ToolName::new("get_ticket"),
            tags: vec![],
            delta: Some(Delta {
                trust: None,
                audience: Some(Dim::Known(Audience::restricted([ReaderId::new("internal")])).into()),
            }),
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
        let pay = |emits: [&str; 2]| ToolContract {
            name: ToolName::new("pay"),
            tags: vec![],
            delta: Some(Delta::NONE),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new(emits.map(EffectKind::new)).unwrap(),
            requires: Requires::default(),
        };
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let p = Projection::build(&log, Revision::new(1));
        let open = |contract: ToolContract| {
            engine(vec![contract])
                .open_dispatch(&p.view(&traj()), &call("pay", json!({})))
                .unwrap()
        };
        let ab = open(pay(["spend", "audit"]));
        let ba = open(pay(["audit", "spend"]));
        assert_eq!(
            serde_json::to_string(&ab.facts).unwrap(),
            serde_json::to_string(&ba.facts).unwrap()
        );
        let mut log_ab = log.clone();
        log_ab.extend(ab.facts);
        let mut log_ba = log;
        log_ba.extend(ba.facts);
        assert_eq!(
            Projection::build(&log_ab, Revision::new(2)),
            Projection::build(&log_ba, Revision::new(2))
        );
    }

    #[test]
    fn clean_call_allows() {
        let e = engine(vec![crm_tool()]);
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
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
        let e = engine(vec![crm_tool()]);
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let records = vec![user_value(known(TRUSTED, internal))];
        let view = e.view(&traj(), records, Revision::new(1)).unwrap();
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
                }),
            )
            .unwrap();

        let released = match &decision.follow_up {
            FollowUp::Proposals { released, blocked, .. } if blocked.is_empty() => released.clone(),
            other => panic!("expected a release, got {other:?}"),
        };
        assert_eq!(released.len(), 1);
        assert_eq!(released[0].call, call);
        let composed = e.open_dispatch(&view.projection().view(&traj()), &call).unwrap();
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
        let mut decided = appended.facts()[2..].to_vec();
        let subject = match &mut decided[0] {
            Fact::DispatchOpened { subject, .. } => subject.take(),
            other => panic!("the decision's first release record is its opening, got {other:?}"),
        };
        assert_eq!(
            subject,
            Some(crate::basis::SubjectKey::Call {
                trajectory: traj(),
                batch: crate::transition::ProposalBatchId::new("b1"),
                position: 0,
            })
        );
        assert_eq!(decided.as_slice(), composed.facts.as_slice());
        assert!(matches!(
            &appended.facts()[2],
            Fact::DispatchOpened { dispatch, .. } if dispatch == &released[0].dispatch
        ));
    }

    #[test]
    fn a_repeated_batch_identity_returns_its_recorded_decision_and_a_reused_one_is_refused() {
        let e = engine(vec![crm_tool()]);
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let records = vec![user_value(known(TRUSTED, internal))];
        let view = e.view(&traj(), records.clone(), Revision::new(1)).unwrap();
        let batch = |proposals: Vec<ResolvedCall>| {
            EngineEvent::Proposals(ProposalBatch {
                id: crate::transition::ProposalBatchId::new("b1"),
                trajectory: traj(),
                provider_results: Vec::new(),
                proposals: proposals.iter().map(raw).collect(),
                spawn: None,
                offer_nonce: nonce(),
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
            .into_unsealed()
            .facts;
        let decided = [records, appended_facts.clone()].concat();
        let after = e.view(&traj(), decided, Revision::new(2)).unwrap();

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
        };
        let allowed = |records: Vec<Fact>| {
            [
                vec![user_value(known(
                    TRUSTED,
                    Audience::restricted([ReaderId::new("internal")]),
                ))],
                records,
            ]
            .concat()
        };
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
        let public = user_value(known(TRUSTED, Audience::Public));
        let dispatch = DispatchId::new(traj(), call.digest(), 0);
        let forged = vec![
            public,
            Fact::ProposalBatchDecided {
                trajectory: traj(),
                batch: crate::transition::ProposalBatchId::new("b1"),
                proposals: vec![call.clone()],
                spawn: None,
                released: vec![dispatch.clone()],
            },
            Fact::DispatchOpened {
                trajectory: traj(),
                dispatch,
                tool: call.tool().clone(),
                arguments: call.canonical_arguments().clone(),
                proposed_label: EstablishedLabel::new(TRUSTED, Audience::restricted([ReaderId::new("internal")])),
                receiving: EstablishedLabel::new(TRUSTED, Audience::restricted([ReaderId::new("internal")])),
                proposed_effects: crate::fact::EffectSet::default(),
                dynamic_resolutions: Vec::new(),
                subject: None,
            },
        ];
        assert_eq!(e.validate_replay(&forged), Err(TransitionRefusal::MisdecidedBatch));

        assert_eq!(
            e.validate_replay(&[forged[0].clone(), forged[2].clone()]),
            Err(TransitionRefusal::UnreleasedDispatch)
        );
    }

    #[test]
    fn a_repeat_of_a_block_that_has_lifted_reports_a_spent_identity() {
        let e = engine(vec![crm_tool()]);
        let public = vec![user_value(known(TRUSTED, Audience::Public))];
        let call = call("get_ticket", json!({}));
        let event = EngineEvent::Proposals(ProposalBatch {
            id: crate::transition::ProposalBatchId::new("b1"),
            trajectory: traj(),
            provider_results: Vec::new(),
            proposals: vec![raw(&call)],
            spawn: None,
            offer_nonce: nonce(),
        });

        let view = e.view(&traj(), public.clone(), Revision::new(1)).unwrap();
        let decision = e.handle(&view, event.clone()).unwrap();
        let decided = decision.append.expect("the block records its decision").into_unsealed();

        let internal = Audience::restricted([ReaderId::new("internal")]);
        let later = [public, decided.facts, vec![user_value(known(TRUSTED, internal))]].concat();
        let revision = Revision::new(later.len() as u64);
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
        let records = vec![user_value(known(TRUSTED, Audience::Public))];
        let view = e.view(&traj(), records, Revision::new(1)).unwrap();
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
        let e = engine(vec![crm_tool()]);
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let log = vec![user_value(known(TRUSTED, internal))];
        assert_eq!(check(&e, &log, &call("get_ticket", json!({}))), CheckOutcome::Allow);
    }

    #[test]
    fn pending_cast_output_dispatches_before_resolution() {
        let scan = ToolContract {
            name: ToolName::new("scan_inbox"),
            tags: vec![],
            delta: Some(Delta {
                trust: Some(Dim::Unknown),
                audience: None,
            }),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires::default(),
        };
        let e = engine(vec![scan]);
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        assert_eq!(check(&e, &log, &call("scan_inbox", json!({}))), CheckOutcome::Allow);
    }

    #[test]
    fn trust_floor_gap_when_suspicious() {
        let e = engine(vec![crm_tool()]);
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let log = vec![user_value(known(SUSPICIOUS, internal))];
        match check(&e, &log, &call("get_ticket", json!({}))) {
            CheckOutcome::Block(b) => assert!(b.requirement_gaps.contains(&Gap::TrustFloor {
                required: TRUSTED,
                actual: SUSPICIOUS,
            })),
            other => panic!("expected trust gap, got {other:?}"),
        }
    }

    #[test]
    fn includes_placeholder_resolves_from_arguments() {
        let send = ToolContract {
            name: ToolName::new("send_email"),
            tags: vec![],
            delta: Some(Delta::NONE),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new([EffectKind::new("egress")]).unwrap(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Includes(RecipientSpec::Placeholder("to".into()))],
                },
                ..Requires::default()
            },
        };
        let e = engine(vec![send]);
        let internal = Audience::restricted([ReaderId::new("auditor")]);
        let log = vec![user_value(known(TRUSTED, internal))];
        assert_eq!(
            check(&e, &log, &call("send_email", json!({ "to": "auditor" }))),
            CheckOutcome::Allow
        );
        match check(&e, &log, &call("send_email", json!({ "to": "stranger" }))) {
            CheckOutcome::Block(b) => assert!(matches!(
                b.requirement_gaps.as_slice(),
                [crate::check::Gap::Includes { .. }]
            )),
            other => panic!("expected includes gap, got {other:?}"),
        }
    }

    #[test]
    fn history_prior_and_no_prior() {
        let del = ToolContract {
            name: ToolName::new("delete_db"),
            tags: vec![],
            delta: Some(Delta::NONE),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                history: vec![
                    HistoryRequirement::Prior(EffectKind::new("backup.done")),
                    HistoryRequirement::NoPrior(EffectKind::new("db.deleted")),
                ],
                ..Requires::default()
            },
        };
        let e = engine(vec![del]);
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        match check(&e, &log, &call("delete_db", json!({}))) {
            CheckOutcome::Block(b) => {
                assert!(b.requirement_gaps.contains(&Gap::Prior(EffectKind::new("backup.done"))))
            }
            other => panic!("expected prior gap, got {other:?}"),
        }
    }

    #[test]
    fn an_includes_requirement_reads_the_committed_label() {
        let b_reader = Audience::restricted([ReaderId::new("b")]);
        let share = ToolContract {
            name: ToolName::new("share"),
            tags: vec![],
            delta: Some(Delta {
                trust: None,
                audience: Some(Dim::Known(Audience::restricted([ReaderId::new("a")])).into()),
            }),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Includes(RecipientSpec::Static(b_reader.clone()))],
                },
                ..Requires::default()
            },
        };
        let e = engine(vec![share]);
        let both = Audience::restricted([ReaderId::new("a"), ReaderId::new("b")]);
        let log = vec![user_value(known(TRUSTED, both.clone()))];
        match check(&e, &log, &call("share", json!({}))) {
            CheckOutcome::Block(block) => {
                assert_eq!(block.requirement_gaps, vec![Gap::Includes { recipients: b_reader }]);
                assert_eq!(
                    block.narrowing,
                    Some(crate::check::Narrowing {
                        from: established(TRUSTED, both),
                        to: established(TRUSTED, Audience::restricted([ReaderId::new("a")])),
                    })
                );
                assert!(block.unestablished.is_empty());
            }
            other => panic!("expected the committed-label includes gap, got {other:?}"),
        }
    }

    #[test]
    fn a_trust_floor_reads_the_committed_label() {
        let risky = ToolContract {
            name: ToolName::new("risky"),
            tags: vec![],
            delta: Some(Delta {
                trust: Some(Dim::Known(SUSPICIOUS)),
                audience: None,
            }),
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
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
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
        let scoped = ToolContract {
            name: ToolName::new("scoped"),
            tags: vec![],
            delta: Some(Delta {
                trust: None,
                audience: Some(Dim::Known(a_reader.clone()).into()),
            }),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Cap(a_reader)],
                },
                ..Requires::default()
            },
        };
        let e = engine(vec![scoped]);
        let both = Audience::restricted([ReaderId::new("a"), ReaderId::new("b")]);
        let log = vec![user_value(known(TRUSTED, both))];
        match check(&e, &log, &call("scoped", json!({}))) {
            CheckOutcome::Block(block) => {
                assert!(block.requirement_gaps.is_empty(), "narrowing into the cap is not a gap");
                assert!(block.narrowing.is_some());
            }
            other => panic!("expected a narrowing-only soft block, got {other:?}"),
        }
    }

    fn emitting(name: &str, kind: &str) -> ToolContract {
        ToolContract {
            name: ToolName::new(name),
            tags: vec![],
            delta: Some(Delta::NONE),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new([EffectKind::new(kind)]).unwrap(),
            requires: Requires::default(),
        }
    }

    fn history_guarded(name: &str, requirement: HistoryRequirement) -> ToolContract {
        ToolContract {
            name: ToolName::new(name),
            tags: vec![],
            delta: Some(Delta::NONE),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                history: vec![requirement],
                ..Requires::default()
            },
        }
    }

    fn open(e: &Engine, log: &mut Vec<Fact>, c: &ResolvedCall) -> crate::value::DispatchId {
        let p = Projection::build(log, Revision::new(log.len() as u64));
        let batch = e.open_dispatch(&p.view(&traj()), c).unwrap();
        let dispatch = batch
            .facts
            .iter()
            .find_map(|fact| match fact {
                Fact::DispatchOpened { dispatch, .. } => Some(dispatch.clone()),
                _ => None,
            })
            .expect("open_dispatch appends the open fact");
        log.extend(batch.facts);
        dispatch
    }

    fn close(
        e: &Engine,
        log: &mut Vec<Fact>,
        dispatch: &crate::value::DispatchId,
        c: &ResolvedCall,
        admission: crate::admit::ResultAdmission,
    ) {
        let p = Projection::build(log, Revision::new(log.len() as u64));
        let batch = e.admit_result(&p.view(&traj()), dispatch, c, admission).unwrap();
        log.extend(batch.facts);
    }

    #[test]
    fn a_rewritten_admitted_label_is_refused_at_every_provenance() {
        let e = engine(vec![crm_tool()]);
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let call = call("get_ticket", json!({}));
        let mut log = vec![user_value(known(TRUSTED, internal.clone()))];
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
            widened(known(TRUSTED, Audience::Public)),
            Err(TransitionRefusal::ForgedLabel)
        );
        let admitted = match log.last() {
            Some(Fact::ValueAdmitted { value, .. }) => value.label.clone(),
            other => panic!("the raw result admits a value, got {other:?}"),
        };
        assert_eq!(widened(admitted), Ok(()));

        let child = TrajectoryId::new("child");
        let mut branched = vec![user_value(known(SUSPICIOUS, internal.clone()))];
        branched.extend(forked_child(&e, &branched.clone(), &child));
        let crossing = e
            .submit_child_return(
                &Projection::build(&branched, Revision::new(branched.len() as u64)).view(&traj()),
                &child,
                crate::branch::ReturnSubmission::Raw {
                    body: ValueBody::new("done"),
                },
            )
            .expect("a non-narrowing return crosses");
        branched.extend(crossing.facts);
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
            forge(crossing_at, known(TRUSTED, Audience::Public)),
            Err(TransitionRefusal::ForgedLabel)
        );
        assert_eq!(
            forge(crossing_at + 1, known(TRUSTED, Audience::Public)),
            Err(TransitionRefusal::ForgedLabel)
        );
    }

    #[test]
    fn a_reported_outcome_closes_once_and_repeats_answer_from_the_record() {
        let e = engine(vec![crm_tool()]);
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let call = call("get_ticket", json!({}));
        let records = vec![user_value(known(TRUSTED, internal))];
        let view = e.view(&traj(), records.clone(), Revision::new(1)).unwrap();
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
                }),
            )
            .unwrap();
        let FollowUp::Proposals { released, .. } = decision.follow_up else {
            panic!("a proposal batch answers with proposals")
        };
        let dispatch = released[0].dispatch.clone();
        let log = [records, decision.append.unwrap().facts().to_vec()].concat();
        let released_view = e.view(&traj(), log.clone(), Revision::new(2)).unwrap();

        let report = |outcome: ToolOutcome| ToolReport {
            dispatch: dispatch.clone(),
            outcome,
            evidence: Vec::new(),
            offer_nonce: nonce(),
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
        assert!(matches!(
            facts.as_slice(),
            [
                Fact::BasisAdvanced { .. },
                Fact::DispatchClosed { .. },
                Fact::ValueAdmitted { .. }
            ]
        ));

        let after = e.view(&traj(), [log, facts].concat(), Revision::new(3)).unwrap();
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
                })
            ),
            Err(crate::transition::TransitionError::UnknownDispatch)
        );
    }

    #[test]
    fn the_composed_admission_of_an_accepted_residual_replays() {
        let redactor = crate::authority::Sanitizer {
            name: crate::names::SanitizerName::new("redactor"),
            on: crate::authority::SanitizerPoints {
                input: false,
                output: true,
            },
            transition: crate::authority::Transition::Trust {
                from_floor: SUSPICIOUS,
                to: Trust::new(1),
            },
            scope: crate::authority::Scope::default(),
            hint: None,
        };
        let fetch = ToolContract {
            name: ToolName::new("fetch"),
            tags: vec![],
            delta: Some(Delta {
                trust: Some(Dim::Known(SUSPICIOUS)),
                audience: None,
            }),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new([EffectKind::new("web.read")]).unwrap(),
            requires: Requires::default(),
        };
        let e = open_engine(RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "limited".into(), "trusted".into()]),
            tools: vec![fetch],
            authorities: vec![],
            sanitizers: vec![redactor],
            casts: vec![],
        });
        let call = call("fetch", json!({}));
        let mut log = vec![user_value(known(Trust::new(2), Audience::Public))];
        let projection = Projection::build(&log, Revision::new(log.len() as u64));
        let trajectory = traj();
        let views = projection.view(&trajectory);
        let block = match e.check(&views, &call).unwrap() {
            CheckOutcome::Block(raw) => raw,
            other => panic!("the narrowing call blocks, got {other:?}"),
        };
        let sanitize = e
            .plan(&views, &call, &block)
            .unwrap()
            .plans
            .iter()
            .filter_map(crate::plan::RemedyPlan::executable)
            .find(|plan| {
                plan.steps
                    .iter()
                    .any(|step| matches!(step, crate::plan::RemedyStep::Sanitize(_)))
            })
            .expect("a confined result point offers the sanitize plan")
            .clone();
        let executed = e.execute_remedy_plan(&views, &sanitize, &call, &[]).unwrap();
        let dispatch = executed
            .facts
            .iter()
            .find_map(|fact| match fact {
                Fact::DispatchOpened { dispatch, .. } => Some(dispatch.clone()),
                _ => None,
            })
            .expect("the executed plan opens the dispatch");
        log.extend(executed.facts);
        assert_eq!(e.validate_replay(&log), Ok(()));

        let raw = ValueBody::new("page bytes");
        let view = e
            .view(&trajectory, log.clone(), Revision::new(log.len() as u64))
            .unwrap();
        let admitted = e
            .admit_result(
                &view.projection().view(&trajectory),
                &dispatch,
                &call,
                crate::admit::ResultAdmission::SuccessSanitized {
                    body: ValueBody::new("page bytes, redacted"),
                    sanitizer: crate::names::SanitizerName::new("redactor"),
                    raw_digest: crate::value::RawResultDigest::of(raw.as_str().as_bytes()),
                },
            )
            .expect("the release accepted exactly this residual");
        let log = [log, admitted.facts].concat();
        assert_eq!(e.validate_replay(&log), Ok(()));
    }

    #[test]
    fn a_bound_sanitizer_checkpoints_before_it_asks_for_the_derivation() {
        let redactor = crate::authority::Sanitizer {
            name: crate::names::SanitizerName::new("redactor"),
            on: crate::authority::SanitizerPoints {
                input: false,
                output: true,
            },
            transition: crate::authority::Transition::Trust {
                from_floor: SUSPICIOUS,
                to: TRUSTED,
            },
            scope: crate::authority::Scope::default(),
            hint: None,
        };
        let fetch = ToolContract {
            name: ToolName::new("fetch"),
            tags: vec![],
            delta: Some(Delta {
                trust: Some(Dim::Known(SUSPICIOUS)),
                audience: None,
            }),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new([EffectKind::new("web.read")]).unwrap(),
            requires: Requires::default(),
        };
        let e = open_engine(RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![fetch],
            authorities: vec![],
            sanitizers: vec![redactor],
            casts: vec![],
        });
        let call = call("fetch", json!({}));
        let mut log = vec![user_value(known(TRUSTED, Audience::Public))];
        let projection = Projection::build(&log, Revision::new(log.len() as u64));
        let trajectory = traj();
        let views = projection.view(&trajectory);
        let block = match e.check(&views, &call).unwrap() {
            CheckOutcome::Block(raw) => raw,
            other => panic!("the narrowing call blocks, got {other:?}"),
        };
        let sanitize = e
            .plan(&views, &call, &block)
            .unwrap()
            .plans
            .iter()
            .filter_map(crate::plan::RemedyPlan::executable)
            .find(|plan| {
                plan.steps
                    .iter()
                    .any(|step| matches!(step, crate::plan::RemedyStep::Sanitize(_)))
            })
            .expect("a confined result point offers the sanitize plan")
            .clone();
        let executed = e.execute_remedy_plan(&views, &sanitize, &call, &[]).unwrap();
        let dispatch = executed
            .facts
            .iter()
            .find_map(|fact| match fact {
                Fact::DispatchOpened { dispatch, .. } => Some(dispatch.clone()),
                _ => None,
            })
            .expect("the executed plan opens the dispatch");
        log.extend(executed.facts);
        let view = e.view(&traj(), log.clone(), Revision::new(2)).unwrap();

        let raw = ValueBody::new("page bytes");
        let source = crate::value::RawResultDigest::of(raw.as_str().as_bytes());
        let report = |evidence: Vec<crate::transition::Evidence>| ToolReport {
            dispatch: dispatch.clone(),
            outcome: ToolOutcome::Success {
                body: OutcomeBody::Available(raw.clone()),
            },
            evidence,
            offer_nonce: nonce(),
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
            .view(
                &traj(),
                [log.clone(), checkpoint.facts().to_vec()].concat(),
                Revision::new(3),
            )
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
        assert_eq!(e.view(&traj(), whole.clone(), Revision::new(4)).map(|_| ()), Ok(()));
        assert_eq!(
            e.view(&traj(), whole[..whole.len() - 1].to_vec(), Revision::new(4))
                .map(|_| ())
                .unwrap_err(),
            crate::transition::TransitionRefusal::UnadmittedDerivation
        );

        let settled = e.view(&traj(), whole, Revision::new(4)).unwrap();
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
            transition: crate::authority::Transition::Trust { from_floor, to },
            scope: crate::authority::Scope::default(),
            hint: None,
        };
        open_engine(RegistryConfig {
            trust_chain: TrustChain::new(vec!["secret".into(), "suspicious".into(), "trusted".into()]),
            tools: vec![
                ToolContract {
                    name: ToolName::new("fetch"),
                    tags: vec![],
                    delta: Some(Delta {
                        trust: Some(Dim::Known(Trust::new(0))),
                        audience: None,
                    }),
                    parameters: crate::params::ToolParameters::open(),
                    emits: EffectSet::new([EffectKind::new("web.read")]).unwrap(),
                    requires: Requires::default(),
                },
                open_tool("ping"),
            ],
            authorities: vec![],
            sanitizers: vec![
                sanitizer("redactor", Trust::new(0), Trust::new(1)),
                sanitizer("scrubber", Trust::new(1), Trust::new(2)),
            ],
            casts: vec![],
        })
    }

    fn staged_candidate(e: &Engine) -> (Vec<Fact>, DispatchId, ValueBody, EngineDecision) {
        let call = call("fetch", json!({}));
        let mut log = vec![user_value(known(Trust::new(2), Audience::Public))];
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
        };
        let view = |log: &[Fact]| {
            e.view(&traj(), log.to_vec(), Revision::new(log.len() as u64))
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
                from: established(Trust::new(2), Audience::Public),
                to: established(Trust::new(1), Audience::Public),
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
                &e.view(&traj(), log.clone(), Revision::new(log.len() as u64)).unwrap(),
                EngineEvent::Outcome(ToolReport {
                    dispatch,
                    outcome: ToolOutcome::Success {
                        body: OutcomeBody::Available(ValueBody::new("page bytes")),
                    },
                    evidence: Vec::new(),
                    offer_nonce: nonce(),
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

        let e = substituting_engine();
        let proposal = call("post", json!({ "body": "ssn 123" }));
        let log = internal_log(TRUSTED);
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
            .view(
                &traj(),
                [log.clone(), hopped].concat(),
                Revision::new((log.len() + 8) as u64),
            )
            .expect("the hop's batch replays");
        assert_eq!(
            after.projection().view(&traj()).current_label().bound(),
            &established(Trust::new(2), Audience::Public),
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
                Revision::new((log.len() + 8) as u64),
            )
            .expect("the acceptance's batch replays");
        assert_eq!(
            after.projection().view(&traj()).current_label().bound(),
            &established(Trust::new(1), Audience::Public)
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
                Revision::new((log.len() + 8) as u64),
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
                &e.view(&traj(), log.clone(), Revision::new(log.len() as u64)).unwrap(),
                EngineEvent::Outcome(ToolReport {
                    dispatch,
                    outcome: ToolOutcome::Success {
                        body: OutcomeBody::Available(ValueBody::new("page bytes")),
                    },
                    evidence: Vec::new(),
                    offer_nonce: crate::value::OfferNonce::new([13u8; 32]),
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

    fn substituting_engine() -> Engine {
        let partner = Audience::restricted([ReaderId::new("partner")]);
        let post = |name: &str, tags: Vec<crate::names::TagName>| ToolContract {
            name: ToolName::new(name),
            tags,
            delta: Some(Delta::NONE),
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
                    audience: vec![AudienceRequirement::Includes(RecipientSpec::Static(partner.clone()))],
                },
                ..Requires::default()
            },
        };
        open_engine(RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![
                post("post", vec![crate::names::TagName::new("outbound")]),
                post("post_untagged", vec![]),
                open_tool("ping"),
            ],
            authorities: vec![crate::authority::Authority {
                name: AuthorityName::new("officer"),
                mandate: crate::authority::Mandate {
                    trust_ceiling: Some(TRUSTED),
                    reader_ceiling: Some(partner),
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
                transition: crate::authority::Transition::Audience {
                    from_includes: Audience::restricted([ReaderId::new("internal")]),
                    to: Audience::restricted([ReaderId::new("internal"), ReaderId::new("partner")]),
                },
                scope: crate::authority::Scope {
                    tags: vec![crate::names::TagName::new("outbound")],
                },
                hint: None,
            }],
            casts: vec![],
        })
    }

    fn internal_log(trust: Trust) -> Vec<Fact> {
        vec![user_value(known(
            trust,
            Audience::restricted([ReaderId::new("internal")]),
        ))]
    }

    fn substitution(call: &ResolvedCall, replacement: &str) -> OfferOutcome {
        OfferOutcome::Derived(crate::transition::Evidence::Sanitizer {
            sanitizer: crate::names::SanitizerName::new("redact"),
            source: crate::value::RawResultDigest::of(call.canonical_arguments().canonical_bytes()),
            derived: ValueBody::new(replacement),
        })
    }

    const REDACTED: &str = r#"{"body":"[redacted]"}"#;

    #[test]
    fn an_input_hop_is_offered_before_the_ruling_that_covers_the_same_gap() {
        let e = substituting_engine();
        let log = internal_log(TRUSTED);
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
    fn a_substitution_that_clears_the_last_gap_dispatches_in_the_hops_own_batch() {
        let e = substituting_engine();
        let proposal = call("post", json!({ "body": "ssn 123" }));
        let log = internal_log(TRUSTED);
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
                        recipients: Audience::restricted([ReaderId::new("partner")])
                    }]
                );
            }
            other => panic!("a fresh proposal decides as proposals, got {other:?}"),
        }
    }

    #[test]
    fn a_forged_answer_neither_persists_on_a_candidate_nor_releases_one() {
        let e = substituting_engine();
        let proposal = call("post", json!({ "body": "ssn 123" }));
        let log = internal_log(TRUSTED);
        let facts = appended_facts(proposed(&e, &log, "b1", nonce(), proposal.clone()).expect("the batch decides"));
        let offers = opened_offers(&facts);
        let log = [log, facts].concat();
        let facts = appended_facts(
            execute_offer(&e, &log, offers[0].0, substitution(&proposal, REDACTED)).expect("the hop runs"),
        );
        let log = [log, facts].concat();
        assert_eq!(e.validate_replay(&log), Ok(()));

        let answer = || {
            crate::contract::PinnedDynamicResolution::from_answer(
                crate::contract::DynamicAudienceBinding {
                    resolver: crate::names::DynamicResolverName::new("acl"),
                    argument: "body".to_string(),
                },
                Some(Audience::restricted([ReaderId::new("partner")])),
            )
        };

        let mut forged = log.clone();
        let candidate = forged.len() - 2;
        let Fact::CandidateDerived {
            derived: DerivedCandidate::Call { call, .. },
            ..
        } = &mut forged[candidate]
        else {
            panic!("the hop's batch records its candidate before the opening")
        };
        *call = call.clone().with_dynamic_resolutions(vec![answer()]);
        assert_eq!(
            e.validate_replay(&forged),
            Err(crate::transition::TransitionRefusal::ForgedLabel),
            "the candidate is the call the predecessor's own substitution renders, answers included"
        );

        let mut forged = log.clone();
        let Fact::DispatchOpened {
            dynamic_resolutions, ..
        } = forged.last_mut().expect("the opening is the batch's last record")
        else {
            panic!("the hop's batch ends with its opening")
        };
        *dynamic_resolutions = vec![answer()];
        assert_eq!(
            e.validate_replay(&forged),
            Err(crate::transition::TransitionRefusal::UnbackedDecision),
            "the release a candidate earned is that exact call; an opening of any other claims a \
             subject this decision never released"
        );
    }

    #[test]
    fn a_repeat_of_a_hop_names_the_dispatch_that_hop_opened() {
        let e = substituting_engine();
        let proposal = call("post", json!({ "body": "ssn 123" }));
        let log = internal_log(TRUSTED);

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
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
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
        assert_eq!(openings[0].1, Some(subject(0)));
        assert_eq!(openings[1].1, Some(subject(1)));

        let mut forged = log.clone();
        let second = forged
            .iter_mut()
            .filter_map(|fact| match fact {
                Fact::DispatchOpened { subject, .. } => Some(subject),
                _ => None,
            })
            .nth(1)
            .expect("the batch opened two dispatches");
        *second = Some(subject(0));
        assert_eq!(
            e.validate_replay(&forged),
            Err(crate::transition::TransitionRefusal::UnbackedDecision)
        );
    }

    #[test]
    fn a_substitution_that_leaves_a_gap_re_plans_over_the_derived_call() {
        let e = substituting_engine();
        let proposal = call("post", json!({ "body": "ssn 123" }));
        let log = internal_log(SUSPICIOUS);
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
            partial(SUSPICIOUS, Audience::restricted([ReaderId::new("internal")])),
        );
        let approved = execute_offer(&e, &log, offer, OfferOutcome::Approved(evidence)).expect("the officer answers");
        assert_eq!(
            offer_answer(&approved),
            &OfferFollowUp::Approved {
                call: block.call.clone()
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
        let e = substituting_engine();
        let proposal = call("post", json!({ "body": "ssn 123" }));
        let log = internal_log(TRUSTED);
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
    fn a_hop_goes_stale_with_its_basis_and_a_spent_one_answers_from_the_record() {
        let e = substituting_engine();
        let proposal = call("post", json!({ "body": "ssn 123" }));
        let log = internal_log(TRUSTED);
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
                &e.view(&traj(), log.clone(), Revision::new(log.len() as u64)).unwrap(),
                EngineEvent::Outcome(ToolReport {
                    dispatch,
                    outcome: ToolOutcome::Success {
                        body: OutcomeBody::Available(body.clone()),
                    },
                    evidence: Vec::new(),
                    offer_nonce: nonce(),
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
        let e = engine(vec![]);
        let child = TrajectoryId::new("child");
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let mut log = vec![user_value(known(SUSPICIOUS, internal.clone()))];
        log.extend(forked_child(&e, &log.clone(), &child));
        let view = e.view(&traj(), log.clone(), Revision::new(log.len() as u64)).unwrap();

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
        let after = e
            .view(&traj(), [log.clone(), facts].concat(), Revision::new(9))
            .unwrap();

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
                child_report(&log, &TrajectoryId::new("stranger"), ChildSubmission::Void)
            ),
            Err(crate::transition::TransitionError::NotForked)
        );
    }

    #[test]
    fn a_narrowing_child_return_blocks_and_a_void_ends_the_branch() {
        let e = engine(vec![]);
        let child = TrajectoryId::new("child");
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let mut log = vec![user_value(known(TRUSTED, Audience::Public))];
        log.extend(forked_child(&e, &log.clone(), &child));
        log.push(Fact::ValueAdmitted {
            trajectory: child.clone(),
            value: LabeledValue::new(ValueBody::new("read"), known(SUSPICIOUS, internal)),
            provenance: Provenance::UserInput,
        });
        let view = e.view(&traj(), log.clone(), Revision::new(log.len() as u64)).unwrap();

        let blocked = e
            .handle(
                &view,
                child_report(
                    &log,
                    &child,
                    ChildSubmission::Value {
                        body: ValueBody::new("what I found"),
                    },
                ),
            )
            .expect("a narrowing crossing blocks");
        assert_eq!(blocked.append, None);
        match blocked.follow_up {
            FollowUp::Child(ChildFollowUp::Blocked { plans, .. }) => {
                assert!(
                    plans
                        .iter()
                        .any(|plan| matches!(plan, crate::branch::ReturnPlan::Accept(_)))
                );
            }
            other => panic!("expected a blocked crossing, got {other:?}"),
        }

        let ended = e
            .handle(&view, child_report(&log, &child, ChildSubmission::Void))
            .expect("a void return ends the branch");
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
    fn a_marked_spawn_prepares_its_fork_and_the_child_binds_to_it() {
        let e = engine(vec![plain_tool("spawn")]);
        let call = call("spawn", json!({}));
        let records = vec![user_value(known(TRUSTED, Audience::Public))];
        let view = e.view(&traj(), records.clone(), Revision::new(1)).unwrap();
        let batch = |spawn: Option<crate::transition::SpawnMark>| {
            EngineEvent::Proposals(ProposalBatch {
                id: crate::transition::ProposalBatchId::new("b1"),
                trajectory: traj(),
                provider_results: Vec::new(),
                proposals: vec![raw(&call)],
                spawn,
                offer_nonce: nonce(),
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
        let prepared = e.view(&traj(), log.clone(), Revision::new(2)).unwrap();
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

        let after = e
            .view(&traj(), [log.clone(), opened].concat(), Revision::new(3))
            .unwrap();
        assert_eq!(after.views(&child).current_label(), partial(TRUSTED, Audience::Public));
        assert_eq!(after.views(&child).parent_of(&child), Some(&traj()));

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
        let after_run = e.view(&traj(), ran, Revision::new(3)).unwrap();
        let repeat = e
            .handle(&after_run, batch(Some(crate::transition::SpawnMark::at(0))))
            .expect("the repeat answers from the record");
        assert_eq!(repeat.append, None);
        match repeat.follow_up {
            FollowUp::Proposals { released, forks, .. } => {
                assert!(released.is_empty(), "an invoked call is not re-released");
                assert_eq!(forks, vec![fork.clone()], "its fork still awaits a child");
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
        let after_failure = e.view(&traj(), failed, Revision::new(3)).unwrap();
        assert_eq!(
            e.handle(&after_failure, bind(fork, child)),
            Err(crate::transition::TransitionError::UnbindableFork)
        );
    }

    #[test]
    fn a_fork_preparation_replays_only_as_its_marked_release() {
        let e = engine(vec![plain_tool("spawn")]);
        let call = call("spawn", json!({}));
        let records = vec![user_value(known(TRUSTED, Audience::Public))];
        let view = e.view(&traj(), records.clone(), Revision::new(1)).unwrap();
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
                user_value(known(SUSPICIOUS, Audience::Public)),
                batch[2].clone(),
            ],
        ]
        .concat();
        assert_eq!(e.validate_replay(&displaced), Err(TransitionRefusal::UnbackedDecision));
    }

    #[test]
    fn a_spawn_mark_takes_declared_context_control() {
        let config = RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![plain_tool("spawn")],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![],
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
        let view = e
            .view(
                &traj(),
                vec![user_value(known(TRUSTED, Audience::Public))],
                Revision::new(1),
            )
            .unwrap();
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
        let records = vec![user_value(known(TRUSTED, Audience::Public))];
        let view = e.view(&traj(), records.clone(), Revision::new(1)).unwrap();
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
                }),
            )
            .expect("the marked spawn releases and prepares");
        let FollowUp::Proposals { released, .. } = &decision.follow_up else {
            panic!("a proposal batch answers with proposals")
        };
        let fork = released[0].fork.clone().expect("the release carries its fork");
        let log = [records, decision.append.expect("the release appends").facts().to_vec()].concat();
        let prepared = e.view(&traj(), log.clone(), Revision::new(log.len() as u64)).unwrap();
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

        let view = e.view(&traj(), log.clone(), Revision::new(log.len() as u64)).unwrap();
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
        let after = e
            .view(&traj(), ended.clone(), Revision::new(ended.len() as u64))
            .unwrap();

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
        let view = e.view(&traj(), log.clone(), Revision::new(log.len() as u64)).unwrap();
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
        let view = e
            .view(
                &traj(),
                vec![user_value(known(TRUSTED, Audience::Public))],
                Revision::new(1),
            )
            .unwrap();
        let batch = |id: &str, spawn: Option<crate::transition::SpawnMark>| {
            EngineEvent::Proposals(ProposalBatch {
                id: crate::transition::ProposalBatchId::new(id),
                trajectory: traj(),
                provider_results: Vec::new(),
                proposals: vec![raw(&call)],
                spawn,
                offer_nonce: nonce(),
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

        let view = e.view(&traj(), log.clone(), Revision::new(log.len() as u64)).unwrap();
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

    #[test]
    fn a_merge_that_restricts_the_parent_replays_only_with_its_acceptance() {
        let e = engine(vec![]);
        let child = TrajectoryId::new("child");
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let mut log = vec![user_value(known(TRUSTED, Audience::Public))];
        log.extend(forked_child(&e, &log.clone(), &child));
        log.push(Fact::ValueAdmitted {
            trajectory: child.clone(),
            value: LabeledValue::new(ValueBody::new("read"), known(SUSPICIOUS, internal)),
            provenance: Provenance::UserInput,
        });

        let projection = Projection::build(&log, Revision::new(log.len() as u64));
        let trajectory = traj();
        let views = projection.view(&trajectory);
        let accept = match e.check_child_return(&views, &child).unwrap() {
            crate::branch::ReturnCheck::Block(crate::branch::ReturnBlock { plans, .. }) => plans
                .into_iter()
                .find(|plan| matches!(plan, crate::branch::ReturnPlan::Accept(_)))
                .expect("acceptance is always offered"),
            other => panic!("expected a narrowing block, got {other:?}"),
        };
        let executed = e
            .execute_child_return_plan(
                &views,
                &child,
                accept,
                crate::branch::ReturnSubmission::Raw {
                    body: ValueBody::new("what I found"),
                },
            )
            .expect("the accepted crossing merges");
        let merged = [log, executed.facts].concat();
        assert_eq!(e.validate_replay(&merged), Ok(()));

        let forged: Vec<Fact> = merged
            .into_iter()
            .filter(|fact| !matches!(fact, Fact::ChildReturnAcceptance { .. }))
            .collect();
        assert_eq!(e.validate_replay(&forged), Err(TransitionRefusal::ReturnNarrowsParent));
    }

    fn neutral_tool() -> ToolContract {
        ToolContract {
            name: ToolName::new("read_note"),
            tags: vec![],
            delta: Some(Delta::NONE),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires::default(),
        }
    }

    fn emitting_tool() -> ToolContract {
        ToolContract {
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
        e.view(&traj(), log.to_vec(), Revision::new(log.len() as u64))
            .expect("the log replays")
            .projection()
            .view(&traj())
            .basis_for(&quiet_subject())
    }

    fn decide(e: &Engine, log: &[Fact], id: &str, call: &ResolvedCall) -> EngineDecision {
        let view = e
            .view(&traj(), log.to_vec(), Revision::new(log.len() as u64))
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
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let before = basis_of(&e, &log);
        let facts = appended_facts(decide(&e, &log, "b1", &call("read_note", json!({}))));
        assert!(!facts.iter().any(|fact| matches!(fact, Fact::BasisAdvanced { .. })));
        assert_eq!(basis_of(&e, &[log, facts].concat()), before);
    }

    #[test]
    fn a_release_advances_the_components_its_contract_can_move() {
        let effects = engine(vec![emitting_tool()]);
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let before = basis_of(&effects, &log);
        let facts = appended_facts(decide(&effects, &log, "b1", &call("send_note", json!({}))));
        let after = basis_of(&effects, &[log.clone(), facts].concat());
        assert_eq!(after.family, before.family.next());
        assert_eq!(after.flow, before.flow, "a `delta = {{}}` result restricts nothing");

        let restricting = engine(vec![crm_tool()]);
        let internal = vec![user_value(known(
            TRUSTED,
            Audience::restricted([ReaderId::new("internal")]),
        ))];
        let before = basis_of(&restricting, &internal);
        let facts = appended_facts(decide(&restricting, &internal, "b1", &call("get_ticket", json!({}))));
        let after = basis_of(&restricting, &[internal, facts].concat());
        assert_eq!(after.flow, before.flow.next());
        assert_eq!(after.family, before.family, "it reserves no effect");
    }

    #[test]
    fn a_blocked_proposal_leaves_every_basis_component_where_it_was() {
        let e = engine(vec![crm_tool()]);
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
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
        let opening = vec![user_value(known(TRUSTED, Audience::Public))];
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
            .view(&traj(), log.to_vec(), Revision::new(log.len() as u64))
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
            }),
        )
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
        open_engine(RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![ToolContract {
                name: ToolName::new("wire"),
                tags: vec![],
                delta: Some(Delta::NONE),
                parameters: crate::params::ToolParameters::open(),
                emits: EffectSet::default(),
                requires: Requires {
                    label: LabelRequirements {
                        trust_floor: Some(TRUSTED),
                        audience: vec![],
                    },
                    ..Requires::default()
                },
            }],
            authorities: vec![officer("officer-a"), officer("officer-b")],
            sanitizers: vec![],
            casts: vec![],
        })
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
        fold: PartialLabel,
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
        let view = e
            .view(&traj(), log.to_vec(), Revision::new(log.len() as u64))
            .expect("the log replays");
        e.handle(
            &view,
            EngineEvent::ExecuteOffer(OfferExecution {
                trajectory: traj(),
                offer,
                outcome,
                offer_nonce: crate::value::OfferNonce::new([11u8; 32]),
            }),
        )
    }

    fn offer_answer(decision: &EngineDecision) -> &OfferFollowUp {
        match &decision.follow_up {
            FollowUp::Offer(answer) => answer,
            other => panic!("an offer execution answers with an offer follow-up, not {other:?}"),
        }
    }

    fn open_tool(name: &str) -> ToolContract {
        ToolContract {
            name: ToolName::new(name),
            tags: vec![],
            delta: None,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires::default(),
        }
    }

    #[test]
    fn one_call_carries_one_current_approval() {
        let e = engine(vec![crm_tool()]);
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
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
        let opening = vec![user_value(known(SUSPICIOUS, Audience::Public))];
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
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
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
            transition: crate::authority::Transition::Audience {
                from_includes: Audience::restricted([ReaderId::new("internal")]),
                to: Audience::Public,
            },
            scope: crate::authority::Scope::default(),
            hint: None,
        };
        let e = open_engine(RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![crm_tool()],
            authorities: vec![],
            sanitizers: vec![declassify],
            casts: vec![],
        });
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
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
        let opening = vec![user_value(known(SUSPICIOUS, Audience::Public))];
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
    fn evidence_gathered_for_one_offer_cannot_approve_another() {
        let e = two_officer_engine();
        let log = vec![user_value(known(SUSPICIOUS, Audience::Public))];
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
        let fold = partial(SUSPICIOUS, Audience::Public);
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
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let note = call("note", json!({}));
        let release = proposed(&e, &log, "b1", nonce(), note.clone()).expect("the note releases");
        let dispatch = match &release.follow_up {
            FollowUp::Proposals { released, .. } => released[0].dispatch.clone(),
            other => panic!("a proposal batch answers with proposals, not {other:?}"),
        };
        let log = [log, appended_facts(release)].concat();
        let view = e
            .view(&traj(), log.clone(), Revision::new(log.len() as u64))
            .expect("the log replays");
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
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
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
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
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
        let log = prepared(&e, vec![user_value(known(TRUSTED, Audience::Public))]);
        let neutral = appended_facts(
            proposed(&e, &log, "b2", nonce(), call("read_note", json!({}))).expect("the neutral call releases"),
        );
        assert!(
            releases(&e, &[log, neutral].concat(), "b3"),
            "a neutral release stales nothing"
        );

        let e = engine(vec![crm_tool(), strict_tool("send")]);
        let log = prepared(&e, vec![user_value(known(TRUSTED, Audience::Public))]);
        let elsewhere = appended_facts(
            proposed(&e, &log, "b2", nonce(), call("get_ticket", json!({ "id": "other" })))
                .expect("the other proposal decides"),
        );
        assert!(releases(&e, &[log, elsewhere].concat(), "b3"), "a block stales nothing");

        let e = engine(vec![crm_tool(), open_tool("note")]);
        let log = prepared(&e, vec![user_value(known(TRUSTED, Audience::Public))]);
        let restricting =
            appended_facts(proposed(&e, &log, "b2", nonce(), call("note", json!({}))).expect("the note releases"));
        assert!(
            !releases(&e, &[log, restricting].concat(), "b3"),
            "a release that can restrict the trajectory stales the approval it did not belong to"
        );
    }

    #[test]
    fn a_later_basis_change_does_not_revoke_an_open_dispatch() {
        let e = engine(vec![crm_tool(), open_tool("note")]);
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
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

        let view = e
            .view(&traj(), log.clone(), Revision::new(log.len() as u64))
            .expect("the log replays");
        let closed = e
            .handle(
                &view,
                EngineEvent::Outcome(ToolReport {
                    dispatch,
                    outcome: ToolOutcome::Failure,
                    evidence: Vec::new(),
                    offer_nonce: nonce(),
                }),
            )
            .expect("the dispatch is still the engine's to close");
        assert_eq!(e.validate_replay(&[log, appended_facts(closed)].concat()), Ok(()));
    }

    #[test]
    fn a_forged_release_cannot_depart_from_the_approval_it_spends() {
        let e = engine(vec![crm_tool()]);
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
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
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let decision = blocked_batch(&e, &log, "b1", nonce());
        let opened = appended_facts(decision);
        let (offer, plan) = opened_offers(&opened)[0].clone();
        let log = [log, opened].concat();

        let done = execute_offer(&e, &log, offer, OfferOutcome::Approved(Vec::new())).expect("the offer executes");
        let proposal = match offer_answer(&done) {
            OfferFollowUp::Approved { call } => call.clone(),
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
        let log = vec![user_value(known(SUSPICIOUS, Audience::Public))];
        let wire = call("wire", json!({}));
        let decision = proposed(&e, &log, "b1", nonce(), wire.clone()).expect("the batch decides");
        let opened = appended_facts(decision);
        let offers = opened_offers(&opened);
        assert_eq!(offers.len(), 2, "each officer's grouped assignment is its own offer");
        let (chosen, plan) = offers[0].clone();
        let sibling = offers[1].0;
        let log = [log, opened].concat();

        let evidence = evidence_for(chosen, &plan, "wire", partial(SUSPICIOUS, Audience::Public));
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
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
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
        let e = engine(vec![crm_tool(), open_tool("note")]);
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
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
        let log = vec![user_value(known(SUSPICIOUS, Audience::Public))];
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
        let evidence = evidence_for(fresh_offer, &fresh_plan, "wire", partial(SUSPICIOUS, Audience::Public));
        let executed =
            execute_offer(&e, &log, fresh_offer, OfferOutcome::Approved(evidence)).expect("the fresh offer executes");
        assert!(matches!(offer_answer(&executed), OfferFollowUp::Approved { .. }));
    }

    #[test]
    fn a_denial_by_an_unassigned_authority_is_refused() {
        let e = two_officer_engine();
        let log = vec![user_value(known(SUSPICIOUS, Audience::Public))];
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
        let log = vec![user_value(known(SUSPICIOUS, Audience::Public))];
        let decision = proposed(&e, &log, "b1", nonce(), call("wire", json!({}))).expect("the batch decides");
        let opened = appended_facts(decision);
        let (offer, plan) = opened_offers(&opened)[0].clone();
        let log = [log, opened].concat();
        let complete = evidence_for(offer, &plan, "wire", partial(SUSPICIOUS, Audience::Public));

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
        moved_fold[0].reviewed.trajectory_label = partial(TRUSTED, Audience::Public);
        assert!(matches!(
            execute_offer(&e, &log, offer, OfferOutcome::Approved(moved_fold)),
            Err(TransitionError::Plan(PlanError::ReviewMismatch))
        ));
    }

    #[test]
    fn an_unknown_or_foreign_offer_is_refused() {
        let e = engine(vec![crm_tool()]);
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
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
        let view = e
            .view(&traj(), log.clone(), Revision::new(log.len() as u64))
            .expect("the log replays");
        assert_eq!(
            e.handle(
                &view,
                EngineEvent::ExecuteOffer(OfferExecution {
                    trajectory: TrajectoryId::new("elsewhere"),
                    offer,
                    outcome: OfferOutcome::Approved(Vec::new()),
                    offer_nonce: nonce(),
                }),
            ),
            Err(TransitionError::OfferElsewhere)
        );
    }

    #[test]
    fn a_forged_approval_record_is_refused() {
        let e = engine(vec![crm_tool()]);
        let opening = vec![user_value(known(TRUSTED, Audience::Public))];
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
        let opening = vec![user_value(known(SUSPICIOUS, Audience::Public))];
        let wire = call("wire", json!({}));
        let opened = appended_facts(proposed(&e, &opening, "b1", nonce(), wire.clone()).expect("the batch decides"));
        let offers = opened_offers(&opened);
        let (chosen, plan) = offers[0].clone();
        let opening = [opening, opened].concat();
        let elsewhere = appended_facts(proposed(&e, &opening, "b2", nonce(), wire).expect("the batch decides"));
        let unrelated = opened_offers(&elsewhere)[0].0;
        let opening = [opening, elsewhere].concat();

        let evidence = evidence_for(chosen, &plan, "wire", partial(SUSPICIOUS, Audience::Public));
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
            e.view(&traj(), stopped.clone(), Revision::new(stopped.len() as u64))
                .err(),
            Some(TransitionRefusal::UndischargedAcceptance)
        );

        let mut deferred = approval.clone();
        deferred.insert(position, user_value(known(TRUSTED, Audience::Public)));
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
        let log = vec![user_value(known(SUSPICIOUS, Audience::Public))];
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

        let evidence = evidence_for(fresh, &fresh_plan, "wire", partial(SUSPICIOUS, Audience::Public));
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
        let opening = vec![user_value(known(TRUSTED, Audience::Public))];
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

        let mut projection = Projection::empty(Revision::new(unpaired.len() as u64));
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
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
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
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
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
        let opening = vec![user_value(known(TRUSTED, Audience::Public))];
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
        let e = engine(vec![crm_tool()]);
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let call = call("get_ticket", json!({}));
        let records = vec![user_value(known(TRUSTED, internal))];
        let view = e.view(&traj(), records.clone(), Revision::new(1)).unwrap();
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
                user_value(known(SUSPICIOUS, Audience::Public)),
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
                if let Fact::DispatchOpened {
                    dynamic_resolutions, ..
                } = fact
                {
                    dynamic_resolutions.push(crate::contract::PinnedDynamicResolution::from_answer(
                        crate::contract::DynamicAudienceBinding {
                            resolver: crate::names::DynamicResolverName::new("acl"),
                            argument: "who".to_string(),
                        },
                        Some(Audience::restricted([ReaderId::new("anyone")])),
                    ));
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
                    *proposed_label = EstablishedLabel::top();
                }
            }),
            Err(TransitionRefusal::ForgedLabel)
        );
    }

    #[test]
    fn a_crossing_replays_only_with_its_admission_and_its_merge() {
        let e = engine(vec![]);
        let child = TrajectoryId::new("child");
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let mut log = vec![user_value(known(SUSPICIOUS, internal.clone()))];
        log.extend(forked_child(&e, &log.clone(), &child));
        let projection = Projection::build(&log, Revision::new(log.len() as u64));
        let trajectory = traj();
        let crossing = e
            .submit_child_return(
                &projection.view(&trajectory),
                &child,
                crate::branch::ReturnSubmission::Raw {
                    body: ValueBody::new("the answer"),
                },
            )
            .expect("a non-narrowing crossing merges");
        let whole = [log.clone(), crossing.facts.clone()].concat();
        assert_eq!(e.validate_replay(&whole), Ok(()));

        assert_eq!(
            e.validate_replay(&[log.clone(), vec![crossing.facts[0].clone()]].concat()),
            Err(TransitionRefusal::UnmergedCrossing)
        );
        assert_eq!(
            e.validate_replay(&[log.clone(), vec![crossing.facts[0].clone(), crossing.facts[2].clone()]].concat()),
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
        let e = engine(vec![crm_tool()]);
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let call = call("get_ticket", json!({}));
        let mut log = vec![user_value(known(TRUSTED, internal))];
        let dispatch = open(&e, &mut log, &call);
        let admitted = Fact::ValueAdmitted {
            trajectory: traj(),
            value: LabeledValue::new(
                ValueBody::new("ticket"),
                e.registry().tool(call.tool()).unwrap().output_label(),
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
        let e = engine(vec![crm_tool()]);
        let child = TrajectoryId::new("child");
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let mut log = vec![user_value(known(TRUSTED, internal))];
        log.extend(forked_child(&e, &log.clone(), &child));
        let ended = e
            .submit_void_return(
                &Projection::build(&log, Revision::new(log.len() as u64)).view(&traj()),
                &child,
            )
            .expect("the child ends with no value");
        log.extend(ended.facts);
        let view = e.view(&traj(), log.clone(), Revision::new(log.len() as u64)).unwrap();

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
                })
            ),
            Err(crate::transition::TransitionError::BranchEnded)
        );
        let ended_views = view.views(&child);
        assert_eq!(e.open_dispatch(&ended_views, &call), Err(EngineError::BranchEnded));
        assert_eq!(
            e.execute_remedy_plan(
                &ended_views,
                &crate::plan::ExecutableRemedyPlan {
                    id: crate::plan::PlanId::new(0),
                    steps: vec![],
                    required: vec![],
                },
                &call,
                &[],
            ),
            Err(crate::execute::PlanError::BranchEnded)
        );
        let forged = [
            log,
            vec![Fact::DispatchOpened {
                trajectory: child.clone(),
                dispatch: DispatchId::new(child, call.digest(), 0),
                tool: call.tool().clone(),
                arguments: call.canonical_arguments().clone(),
                proposed_label: EstablishedLabel::new(TRUSTED, Audience::restricted([ReaderId::new("internal")])),
                receiving: EstablishedLabel::new(TRUSTED, Audience::restricted([ReaderId::new("internal")])),
                proposed_effects: EffectSet::default(),
                dynamic_resolutions: Vec::new(),
                subject: None,
            }],
        ]
        .concat();
        assert_eq!(e.validate_replay(&forged), Err(TransitionRefusal::BranchEnded));
    }

    #[test]
    fn an_admission_after_a_checkpoint_carries_the_bytes_it_observed() {
        let e = engine(vec![crm_tool()]);
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let call = call("get_ticket", json!({}));
        let mut log = vec![user_value(known(TRUSTED, internal))];
        let dispatch = open(&e, &mut log, &call);
        let body = ValueBody::new("the ticket");
        let checkpoint = e
            .observe_success(
                &Projection::build(&log, Revision::new(log.len() as u64)).view(&traj()),
                &dispatch,
                &call,
                crate::fact::ObservedResult::Available(crate::value::RawResultDigest::of(body.as_str().as_bytes())),
            )
            .expect("an open dispatch checkpoints");
        log.extend(checkpoint.facts);
        let views = Projection::build(&log, Revision::new(log.len() as u64));

        assert_eq!(
            e.admit_result(
                &views.view(&traj()),
                &dispatch,
                &call,
                crate::admit::ResultAdmission::SuccessRaw {
                    body: ValueBody::new("other bytes"),
                },
            )
            .expect_err("other bytes are another observation"),
            crate::admit::AdmitError::ObservationMismatch
        );
        assert!(
            e.admit_result(
                &views.view(&traj()),
                &dispatch,
                &call,
                crate::admit::ResultAdmission::SuccessRaw { body },
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
            transition: crate::authority::Transition::Audience {
                from_includes: Audience::restricted([ReaderId::new("internal")]),
                to: Audience::Public,
            },
            scope: crate::authority::Scope::default(),
            hint: None,
        };
        let config = RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![crm_tool()],
            authorities: vec![],
            sanitizers: vec![declassify],
            casts: vec![],
        };
        let call = call("get_ticket", json!({}));
        let records = vec![user_value(known(TRUSTED, Audience::Public))];

        let confining = open_engine(config.clone());
        let projection = Projection::build(&records, Revision::new(1));
        let trajectory = traj();
        let views = projection.view(&trajectory);
        let block = match confining.check(&views, &call).unwrap() {
            CheckOutcome::Block(raw) => raw,
            other => panic!("the narrowing call blocks, got {other:?}"),
        };
        let sanitize = confining
            .plan(&views, &call, &block)
            .unwrap()
            .plans
            .iter()
            .filter_map(crate::plan::RemedyPlan::executable)
            .find(|plan| {
                plan.steps
                    .iter()
                    .any(|step| matches!(step, crate::plan::RemedyStep::Sanitize(_)))
            })
            .expect("a confined result point offers the sanitize settlement")
            .clone();
        let released = confining
            .execute_remedy_plan(&views, &sanitize, &call, &[])
            .expect("the sanitize plan executes");
        let log = [records.clone(), released.facts].concat();
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
        assert_eq!(
            unconfined.validate_replay(&log),
            Err(TransitionRefusal::UnreleasedDispatch)
        );
    }

    fn pending_cast_tool(name: &str, tag: &str) -> ToolContract {
        ToolContract {
            name: ToolName::new(name),
            tags: vec![crate::names::TagName::new(tag)],
            delta: Some(Delta {
                trust: Some(Dim::Unknown),
                audience: None,
            }),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new([EffectKind::new(format!("{tag}.read").as_str())]).unwrap(),
            requires: Requires::default(),
        }
    }

    fn resolver_cast(name: &str, trust: Vec<Trust>, tags: Vec<crate::names::TagName>) -> crate::authority::Cast {
        crate::authority::Cast {
            name: crate::names::CastName::new(name),
            resolution: crate::authority::CastResolution::Resolver {
                may_cast: crate::authority::CastCeiling {
                    trust,
                    audience: Audience::Public,
                },
            },
            scope: crate::authority::Scope { tags },
        }
    }

    fn cast_engine() -> Engine {
        open_engine(RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![
                pending_cast_tool("scan_inbox", "mail"),
                pending_cast_tool("browse", "web"),
                open_tool("ping"),
            ],
            authorities: vec![],
            sanitizers: vec![crate::authority::Sanitizer {
                name: crate::names::SanitizerName::new("launder"),
                on: crate::authority::SanitizerPoints {
                    input: false,
                    output: true,
                },
                transition: crate::authority::Transition::Trust {
                    from_floor: SUSPICIOUS,
                    to: TRUSTED,
                },
                scope: crate::authority::Scope::default(),
                hint: None,
            }],
            casts: vec![
                resolver_cast("paranoid", vec![SUSPICIOUS, TRUSTED], vec![]),
                resolver_cast("stingy", vec![TRUSTED], vec![]),
                resolver_cast(
                    "elsewhere",
                    vec![SUSPICIOUS, TRUSTED],
                    vec![crate::names::TagName::new("web")],
                ),
            ],
        })
    }

    fn cast_report(
        dispatch: &crate::value::DispatchId,
        raw: &ValueBody,
        evidence: Vec<crate::transition::Evidence>,
    ) -> ToolReport {
        ToolReport {
            dispatch: dispatch.clone(),
            outcome: ToolOutcome::Success {
                body: OutcomeBody::Available(raw.clone()),
            },
            evidence,
            offer_nonce: nonce(),
        }
    }

    fn replayed(e: &Engine, log: &[Fact]) -> EngineView {
        e.view(&traj(), log.to_vec(), Revision::new(log.len() as u64))
            .expect("the log replays")
    }

    fn resolving_dispatch(
        e: &Engine,
    ) -> (
        Vec<Fact>,
        crate::value::DispatchId,
        ValueBody,
        crate::value::RawResultDigest,
    ) {
        let call = call("scan_inbox", json!({}));
        let mut log = vec![user_value(known(TRUSTED, Audience::Public))];
        let released = proposed(e, &log, "b1", nonce(), call).expect("the open call releases");
        let dispatch = match &released.follow_up {
            FollowUp::Proposals { released, .. } => released[0].dispatch.clone(),
            other => panic!("the proposal releases, got {other:?}"),
        };
        log = [log, appended_facts(released)].concat();
        let raw = ValueBody::new("inbox bytes");
        let source = crate::value::RawResultDigest::of(raw.as_str().as_bytes());
        let asked = e
            .handle(
                &replayed(e, &log),
                EngineEvent::Outcome(cast_report(&dispatch, &raw, Vec::new())),
            )
            .expect("the confined result asks for its resolution");
        log = [log, appended_facts(asked)].concat();
        (log, dispatch, raw, source)
    }

    #[test]
    fn a_pending_cast_report_asks_for_the_applicable_casts_and_stays_repeatable() {
        let e = cast_engine();
        let call = call("scan_inbox", json!({}));
        let mut log = vec![user_value(known(TRUSTED, Audience::Public))];
        let released = proposed(&e, &log, "b1", nonce(), call).expect("the open call releases");
        let dispatch = match &released.follow_up {
            FollowUp::Proposals { released, .. } => released[0].dispatch.clone(),
            other => panic!("the proposal releases, got {other:?}"),
        };
        log = [log, appended_facts(released)].concat();
        let raw = ValueBody::new("inbox bytes");
        let source = crate::value::RawResultDigest::of(raw.as_str().as_bytes());
        let asked = e
            .handle(
                &replayed(&e, &log),
                EngineEvent::Outcome(cast_report(&dispatch, &raw, Vec::new())),
            )
            .expect("the confined result asks for its resolution");
        assert_eq!(
            asked.follow_up,
            FollowUp::Outcome(OutcomeFollowUp::Resolve(
                crate::transition::EvidenceRequest::PendingCast {
                    casts: vec![
                        crate::names::CastName::new("paranoid"),
                        crate::names::CastName::new("stingy"),
                    ],
                    source,
                    body: raw.clone(),
                }
            )),
            "the ask names the casts whose scope covers the tool, in registration order"
        );
        let log = [log, appended_facts(asked.clone())].concat();
        let view = replayed(&e, &log);
        let trajectory = traj();
        let views = view.projection().view(&trajectory);
        assert!(
            views.has_effect(&EffectKind::new("mail.read")),
            "the effects commit at the checkpoint, before the external step"
        );
        assert!(
            views.is_open(&dispatch),
            "the dispatch stays open awaiting its resolution"
        );
        let again = e
            .handle(&view, EngineEvent::Outcome(cast_report(&dispatch, &raw, Vec::new())))
            .expect("the report stays repeatable");
        assert_eq!(again.append, None);
        assert_eq!(again.follow_up, asked.follow_up);

        let alone = open_engine(RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![
                pending_cast_tool("scan_inbox", "mail"),
                pending_cast_tool("browse", "web"),
            ],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![resolver_cast(
                "elsewhere",
                vec![SUSPICIOUS, TRUSTED],
                vec![crate::names::TagName::new("web")],
            )],
        });
        let (log, dispatch, raw, _) = resolving_dispatch(&alone);
        let again = alone
            .handle(
                &replayed(&alone, &log),
                EngineEvent::Outcome(cast_report(&dispatch, &raw, Vec::new())),
            )
            .expect("the unresolvable result stays pending");
        assert!(matches!(
            again.follow_up,
            FollowUp::Outcome(OutcomeFollowUp::Resolve(crate::transition::EvidenceRequest::PendingCast { casts, .. }))
                if casts.is_empty()
        ));
    }

    #[test]
    fn a_non_narrowing_resolution_admits_directly_with_its_cast_record() {
        let e = cast_engine();
        let (log, dispatch, raw, source) = resolving_dispatch(&e);
        let resolved = established(TRUSTED, Audience::Public);
        let crossed = e
            .handle(
                &replayed(&e, &log),
                EngineEvent::Outcome(cast_report(
                    &dispatch,
                    &raw,
                    vec![crate::transition::Evidence::PendingCast {
                        cast: crate::names::CastName::new("paranoid"),
                        source,
                        resolved: resolved.clone(),
                    }],
                )),
            )
            .expect("the non-narrowing resolution admits");
        assert_eq!(
            crossed.follow_up,
            FollowUp::Outcome(OutcomeFollowUp::Closed {
                admitted: Some(raw.clone())
            })
        );
        let facts = crossed.append.expect("the admission appends").facts().to_vec();
        assert!(
            matches!(
                facts.as_slice(),
                [
                    Fact::BasisAdvanced { .. },
                    Fact::DispatchClosed {
                        outcome: crate::fact::CloseOutcome::Success { effects },
                        ..
                    },
                    Fact::OutputCastApplied {
                        cast,
                        resolved: restated,
                        raw_digest,
                        ..
                    },
                    Fact::ValueAdmitted { .. },
                ] if effects.is_empty() && cast.as_str() == "paranoid" && restated == &resolved && raw_digest == &source
            ),
            "close (no duplicate effects), the cast record, and the admitted value share one batch: {facts:?}"
        );
        let whole = [log, facts].concat();
        assert_eq!(e.validate_replay(&whole), Ok(()));
        assert_eq!(
            e.validate_replay(&whole[..whole.len() - 1]),
            Err(crate::transition::TransitionRefusal::UnadmittedDerivation)
        );
    }

    #[test]
    fn a_narrowing_resolution_stages_an_acceptance_only_candidate() {
        let e = cast_engine();
        let (log, dispatch, raw, source) = resolving_dispatch(&e);
        let resolved = established(SUSPICIOUS, Audience::Public);
        let staged = e
            .handle(
                &replayed(&e, &log),
                EngineEvent::Outcome(cast_report(
                    &dispatch,
                    &raw,
                    vec![crate::transition::Evidence::PendingCast {
                        cast: crate::names::CastName::new("paranoid"),
                        source,
                        resolved: resolved.clone(),
                    }],
                )),
            )
            .expect("the narrowing resolution stages");
        let confined = confined_of(&staged.follow_up);
        assert_eq!(confined.dispatch, dispatch);
        assert_eq!(
            confined.candidate.body, raw,
            "the candidate is the raw itself, confined"
        );
        assert_eq!(
            confined.residual,
            crate::check::Narrowing {
                from: established(TRUSTED, Audience::Public),
                to: established(SUSPICIOUS, Audience::Public),
            }
        );
        let facts = appended_facts(staged.clone());
        assert!(
            !facts
                .iter()
                .any(|fact| matches!(fact, Fact::ValueAdmitted { .. } | Fact::DispatchClosed { .. })),
            "a staged candidate admits nothing and closes nothing: {facts:?}"
        );
        assert!(facts.iter().any(|fact| matches!(
            fact,
            Fact::CandidateDerived {
                via: DerivedVia::Cast { name },
                ..
            } if name.as_str() == "paranoid"
        )));
        let stage: Vec<_> = opened_offers(&facts).into_iter().map(|(_, plan)| plan).collect();
        assert_eq!(
            stage.iter().filter_map(plan::ExecutableRemedyPlan::hop).count(),
            0,
            "no sanitizer hop is offered on a pending-cast stage, applicable or not"
        );
        assert_eq!(
            stage
                .iter()
                .filter_map(plan::ExecutableRemedyPlan::narrowing)
                .collect::<Vec<_>>(),
            vec![&confined.residual],
            "acceptance of exactly the pinned residual"
        );
        assert_eq!(stage.len(), 1, "the stage is the acceptance alone");
        let log = [log, facts].concat();
        assert_eq!(e.validate_replay(&log), Ok(()));

        let again = e
            .handle(
                &replayed(&e, &log),
                EngineEvent::Outcome(cast_report(
                    &dispatch,
                    &raw,
                    vec![crate::transition::Evidence::PendingCast {
                        cast: crate::names::CastName::new("paranoid"),
                        source,
                        resolved: established(TRUSTED, Audience::Public),
                    }],
                )),
            )
            .expect("the repeat hears the stage");
        assert_eq!(again.append, None);
        assert_eq!(confined_of(&again.follow_up), confined);
    }

    #[test]
    fn an_accepted_cast_candidate_crosses_atomically_and_survives_replay() {
        let e = cast_engine();
        let (log, dispatch, raw, source) = resolving_dispatch(&e);
        let resolved = established(SUSPICIOUS, Audience::Public);
        let staged = e
            .handle(
                &replayed(&e, &log),
                EngineEvent::Outcome(cast_report(
                    &dispatch,
                    &raw,
                    vec![crate::transition::Evidence::PendingCast {
                        cast: crate::names::CastName::new("paranoid"),
                        source,
                        resolved: resolved.clone(),
                    }],
                )),
            )
            .expect("the narrowing resolution stages");
        let confined = confined_of(&staged.follow_up).clone();
        let log = [log, appended_facts(staged)].concat();
        let accept = confined.offers[0].0;
        let accepted =
            execute_offer(&e, &log, accept, OfferOutcome::Approved(Vec::new())).expect("the acceptance runs");
        assert_eq!(offer_answer(&accepted), &OfferFollowUp::Admitted { value: raw.clone() });
        let facts = appended_facts(accepted);
        assert!(
            matches!(
                facts.as_slice(),
                [
                    Fact::BasisAdvanced { .. },
                    Fact::OfferAccepted { .. },
                    Fact::CandidateAccepted { narrowing, .. },
                    Fact::DispatchClosed {
                        outcome: crate::fact::CloseOutcome::Success { effects },
                        ..
                    },
                    Fact::OutputCastApplied {
                        cast,
                        resolved: restated,
                        raw_digest,
                        ..
                    },
                    Fact::ValueAdmitted { .. },
                ] if narrowing == &confined.residual
                    && effects.is_empty()
                    && cast.as_str() == "paranoid"
                    && restated == &resolved
                    && raw_digest == &source
            ),
            "one atomic commit: acceptance, close, cast record, admitted value: {facts:?}"
        );
        let whole = [log, facts].concat();
        let after = replayed(&e, &whole);
        assert_eq!(
            after.projection().view(&traj()).current_label().bound(),
            &established(SUSPICIOUS, Audience::Public),
            "the accepted narrowing is the admission's whole fold move"
        );
        assert!(
            after
                .projection()
                .view(&traj())
                .candidate(&crate::basis::SubjectKey::ConfinedResult(dispatch.clone()))
                .is_none(),
            "the crossing ends the stage"
        );
        assert_eq!(
            e.validate_replay(&whole[..whole.len() - 1]),
            Err(crate::transition::TransitionRefusal::UnadmittedDerivation)
        );
        assert_eq!(
            offer_answer(
                &execute_offer(&e, &whole, accept, OfferOutcome::Approved(Vec::new())).expect("the repeat answers")
            ),
            &OfferFollowUp::Admitted { value: raw }
        );
    }

    #[test]
    fn a_cast_stage_goes_stale_with_its_basis_and_the_durable_candidate_is_planned_again() {
        let e = cast_engine();
        let (log, dispatch, raw, source) = resolving_dispatch(&e);
        let staged = e
            .handle(
                &replayed(&e, &log),
                EngineEvent::Outcome(cast_report(
                    &dispatch,
                    &raw,
                    vec![crate::transition::Evidence::PendingCast {
                        cast: crate::names::CastName::new("paranoid"),
                        source,
                        resolved: established(SUSPICIOUS, Audience::Public),
                    }],
                )),
            )
            .expect("the narrowing resolution stages");
        let confined = confined_of(&staged.follow_up).clone();
        let log = [log, appended_facts(staged)].concat();
        let log = [
            log.clone(),
            appended_facts(proposed(&e, &log, "b2", nonce(), call("ping", json!({}))).expect("the open call releases")),
        ]
        .concat();
        assert_eq!(
            execute_offer(&e, &log, confined.offers[0].0, OfferOutcome::Approved(Vec::new())),
            Err(TransitionError::StaleOffer)
        );

        let replanned = e
            .handle(
                &replayed(&e, &log),
                EngineEvent::Outcome(ToolReport {
                    dispatch,
                    outcome: ToolOutcome::Success {
                        body: OutcomeBody::Available(raw.clone()),
                    },
                    evidence: Vec::new(),
                    offer_nonce: crate::value::OfferNonce::new([13u8; 32]),
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
        assert_eq!(
            fresh.offers.len(),
            1,
            "the replanned stage is still the acceptance alone"
        );
        let fresh = fresh.offers[0].0;
        let log = [log, appended_facts(replanned)].concat();
        assert_eq!(
            offer_answer(
                &execute_offer(&e, &log, fresh, OfferOutcome::Approved(Vec::new())).expect("the fresh offer runs")
            ),
            &OfferFollowUp::Admitted { value: raw }
        );
    }

    #[test]
    fn a_resolution_is_measured_against_the_pinned_bound_not_the_live_fold() {
        let e = cast_engine();
        let (log, dispatch, raw, source) = resolving_dispatch(&e);
        let log = [log, vec![user_value(known(SUSPICIOUS, Audience::Public))]].concat();
        let crossed = e
            .handle(
                &replayed(&e, &log),
                EngineEvent::Outcome(cast_report(
                    &dispatch,
                    &raw,
                    vec![crate::transition::Evidence::PendingCast {
                        cast: crate::names::CastName::new("paranoid"),
                        source,
                        resolved: established(TRUSTED, Audience::Public),
                    }],
                )),
            )
            .expect("the pinned bound, not the live fold, measures the resolution");
        assert_eq!(
            crossed.follow_up,
            FollowUp::Outcome(OutcomeFollowUp::Closed { admitted: Some(raw) })
        );
        let whole = [log, crossed.append.expect("the admission appends").facts().to_vec()].concat();
        assert_eq!(e.validate_replay(&whole), Ok(()));
    }

    #[test]
    fn a_forged_cast_record_is_refused_on_replay() {
        let e = cast_engine();
        let (log, dispatch, raw, source) = resolving_dispatch(&e);
        let staged = e
            .handle(
                &replayed(&e, &log),
                EngineEvent::Outcome(cast_report(
                    &dispatch,
                    &raw,
                    vec![crate::transition::Evidence::PendingCast {
                        cast: crate::names::CastName::new("paranoid"),
                        source,
                        resolved: established(SUSPICIOUS, Audience::Public),
                    }],
                )),
            )
            .expect("the narrowing resolution stages");
        let confined = confined_of(&staged.follow_up).clone();
        let mut forged = appended_facts(staged.clone());
        for fact in &mut forged {
            if let Fact::CandidateDerived {
                via: DerivedVia::Cast { name },
                ..
            } = fact
            {
                *name = crate::names::CastName::new("bogus");
            }
        }
        assert_eq!(
            e.validate_replay(&[log.clone(), forged].concat()),
            Err(crate::transition::TransitionRefusal::InadmissibleResolution),
            "a candidate claiming an unregistered cast is refused"
        );

        let log = [log, appended_facts(staged)].concat();
        let closed_past_the_stage = vec![Fact::DispatchClosed {
            trajectory: traj(),
            dispatch: dispatch.clone(),
            outcome: crate::fact::CloseOutcome::Success {
                effects: EffectSet::default(),
            },
        }];
        assert_eq!(
            e.validate_replay(&[log.clone(), closed_past_the_stage].concat()),
            Err(crate::transition::TransitionRefusal::StagedClose),
            "a staged confined result's dispatch closes only with its settlement"
        );
        let accepted = execute_offer(&e, &log, confined.offers[0].0, OfferOutcome::Approved(Vec::new()))
            .expect("the acceptance runs");
        let crossing = appended_facts(accepted);
        let mut forged = crossing.clone();
        for fact in &mut forged {
            if let Fact::OutputCastApplied { resolved, .. } = fact {
                *resolved = established(TRUSTED, Audience::Public);
            }
        }
        assert_eq!(
            e.validate_replay(&[log.clone(), forged].concat()),
            Err(crate::transition::TransitionRefusal::InadmissibleResolution),
            "the atomic commit's cast record must restate exactly the candidate's resolution"
        );
        let unaccepted: Vec<Fact> = crossing
            .iter()
            .filter(|fact| !matches!(fact, Fact::CandidateAccepted { .. }))
            .cloned()
            .collect();
        assert_eq!(
            e.validate_replay(&[log.clone(), unaccepted].concat()),
            Err(crate::transition::TransitionRefusal::UndischargedAcceptance),
            "a crossing without its acceptance never closes the stage"
        );
        let dropped: Vec<Fact> = crossing
            .iter()
            .filter(|fact| !matches!(fact, Fact::OutputCastApplied { .. }))
            .cloned()
            .collect();
        assert_eq!(
            e.validate_replay(&[log.clone(), dropped].concat()),
            Err(crate::transition::TransitionRefusal::ForgedLabel),
            "a cast candidate crosses only beside its cast record"
        );
        let mut doubled = crossing.clone();
        let record = crossing
            .iter()
            .find(|fact| matches!(fact, Fact::OutputCastApplied { .. }))
            .expect("the crossing carries its cast record")
            .clone();
        let admission = doubled.len() - 1;
        doubled.insert(admission, record);
        assert_eq!(
            e.validate_replay(&[log, doubled].concat()),
            Err(crate::transition::TransitionRefusal::RepeatAdmission),
            "one crossing restates its resolution once"
        );
    }

    #[test]
    fn an_out_of_scope_or_out_of_ceiling_resolution_is_refused() {
        let e = cast_engine();
        let (log, dispatch, raw, source) = resolving_dispatch(&e);
        let view = replayed(&e, &log);
        assert_eq!(
            e.handle(
                &view,
                EngineEvent::Outcome(cast_report(
                    &dispatch,
                    &raw,
                    vec![crate::transition::Evidence::PendingCast {
                        cast: crate::names::CastName::new("elsewhere"),
                        source,
                        resolved: established(TRUSTED, Audience::Public),
                    }],
                )),
            ),
            Err(TransitionError::InadmissibleResolution)
        );
        assert_eq!(
            e.handle(
                &view,
                EngineEvent::Outcome(cast_report(
                    &dispatch,
                    &raw,
                    vec![crate::transition::Evidence::PendingCast {
                        cast: crate::names::CastName::new("stingy"),
                        source,
                        resolved: established(SUSPICIOUS, Audience::Public),
                    }],
                )),
            ),
            Err(TransitionError::InadmissibleResolution)
        );
        assert!(matches!(
            e.handle(&view, EngineEvent::Outcome(cast_report(&dispatch, &raw, Vec::new())))
                .expect("the ask stands")
                .follow_up,
            FollowUp::Outcome(OutcomeFollowUp::Resolve(_))
        ));
    }

    #[test]
    fn an_executed_sanitize_return_plan_replays() {
        let declassify = crate::authority::Sanitizer {
            name: crate::names::SanitizerName::new("declassify"),
            on: crate::authority::SanitizerPoints {
                input: false,
                output: true,
            },
            transition: crate::authority::Transition::Audience {
                from_includes: Audience::restricted([ReaderId::new("internal")]),
                to: Audience::Public,
            },
            scope: crate::authority::Scope::default(),
            hint: None,
        };
        let e = open_engine(RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![],
            authorities: vec![],
            sanitizers: vec![declassify],
            casts: vec![],
        });
        let child = TrajectoryId::new("child");
        let mut log = vec![user_value(known(TRUSTED, Audience::Public))];
        log.extend(forked_child(&e, &log.clone(), &child));
        log.push(Fact::ValueAdmitted {
            trajectory: child.clone(),
            value: LabeledValue::new(
                ValueBody::new("read"),
                known(TRUSTED, Audience::restricted([ReaderId::new("internal")])),
            ),
            provenance: Provenance::UserInput,
        });

        let projection = Projection::build(&log, Revision::new(log.len() as u64));
        let trajectory = traj();
        let views = projection.view(&trajectory);
        let sanitize = match e.check_child_return(&views, &child).unwrap() {
            crate::branch::ReturnCheck::Block(crate::branch::ReturnBlock { plans, .. }) => plans
                .into_iter()
                .find(|plan| matches!(plan, crate::branch::ReturnPlan::Sanitize { .. }))
                .expect("the declassifier clears the narrowing"),
            other => panic!("expected a narrowing block, got {other:?}"),
        };
        let raw = ValueBody::new("what I found");
        let executed = e
            .execute_child_return_plan(
                &views,
                &child,
                sanitize,
                crate::branch::ReturnSubmission::Derived {
                    body: ValueBody::new("what I found, cleaned"),
                    raw_digest: crate::value::RawResultDigest::of(raw.as_str().as_bytes()),
                },
            )
            .expect("the offered plan executes");
        assert_eq!(e.validate_replay(&[log, executed.facts].concat()), Ok(()));
    }

    fn reservation_tools() -> Vec<ToolContract> {
        vec![
            emitting("send", "email.sent"),
            history_guarded("guard", HistoryRequirement::NoPrior(EffectKind::new("email.sent"))),
            history_guarded("wants", HistoryRequirement::Prior(EffectKind::new("email.sent"))),
        ]
    }

    #[test]
    fn an_open_dispatch_reserves_its_emits_for_no_prior_only() {
        let e = engine(reservation_tools());
        let mut log = vec![user_value(known(TRUSTED, Audience::Public))];
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
        let mut log = vec![user_value(known(TRUSTED, Audience::Public))];
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
        let mut log = vec![user_value(known(TRUSTED, Audience::Public))];
        let send = call("send", json!({}));
        let dispatch = open(&e, &mut log, &send);
        close(
            &e,
            &mut log,
            &dispatch,
            &send,
            crate::admit::ResultAdmission::Indeterminate,
        );
        let p = Projection::build(&log, Revision::new(log.len() as u64));
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
        let mut log = vec![user_value(known(TRUSTED, Audience::Public))];
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
        let selfguard = ToolContract {
            name: ToolName::new("selfguard"),
            tags: vec![],
            delta: Some(Delta::NONE),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new([EffectKind::new("email.sent")]).unwrap(),
            requires: Requires {
                history: vec![HistoryRequirement::NoPrior(EffectKind::new("email.sent"))],
                ..Requires::default()
            },
        };
        let e = engine(vec![selfguard]);
        let mut log = vec![user_value(known(TRUSTED, Audience::Public))];
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
        let scan = ToolContract {
            name: ToolName::new("scan"),
            tags: vec![],
            delta: Some(Delta {
                trust: Some(Dim::Unknown),
                audience: None,
            }),
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
        let mut log = vec![user_value(known(TRUSTED, Audience::Public))];
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
        let p = Projection::build(&log, Revision::new(log.len() as u64));
        let batch = e
            .observe_success(&p.view(&traj()), &dispatch, &scan_call, ObservedResult::Unavailable)
            .unwrap();
        log.extend(batch.facts);
        let p = Projection::build(&log, Revision::new(log.len() as u64));
        assert!(p.view(&traj()).is_open(&dispatch));
        assert_eq!(check(&e, &log, &call("wants_read", json!({}))), CheckOutcome::Allow);
        assert!(matches!(
            check(&e, &log, &call("guard_read", json!({}))),
            CheckOutcome::Block(_)
        ));
    }

    #[test]
    fn attention_is_always_a_gap() {
        let tool = ToolContract {
            name: ToolName::new("wire"),
            tags: vec![],
            delta: Some(Delta::NONE),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                attention: vec![MarkName::new("signoff")],
                ..Requires::default()
            },
        };
        let e = engine(vec![tool]);
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        match check(&e, &log, &call("wire", json!({}))) {
            CheckOutcome::Block(b) => {
                assert!(b.requirement_gaps.contains(&Gap::Attention(MarkName::new("signoff"))))
            }
            other => panic!("expected attention gap, got {other:?}"),
        }
    }

    #[test]
    fn unknown_label_is_unestablished_not_a_gap() {
        let e = engine(vec![crm_tool()]);
        let log = vec![user_value(Label::new(Dim::Unknown, Dim::Known(Audience::Public)))];
        match check(&e, &log, &call("get_ticket", json!({}))) {
            CheckOutcome::Block(b) => {
                assert!(b.requirement_gaps.is_empty());
                assert!(b.narrowing.is_some(), "the audience narrowing reports alongside");
                assert_eq!(b.unestablished.len(), 1);
                assert_eq!(b.unestablished[0].dimensions, BTreeSet::from([Dimension::Trust]));
            }
            other => panic!("expected an unestablished block, got {other:?}"),
        }
    }

    #[test]
    fn all_three_block_slots_coexist() {
        let vault = ToolContract {
            name: ToolName::new("vault"),
            tags: vec![],
            delta: Some(Delta {
                trust: None,
                audience: Some(Dim::Known(Audience::restricted([ReaderId::new("internal")])).into()),
            }),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(TRUSTED),
                    audience: vec![],
                },
                attention: vec![MarkName::new("signoff")],
                ..Requires::default()
            },
        };
        let e = engine(vec![vault]);
        let log = vec![user_value(Label::new(Dim::Unknown, Dim::Known(Audience::Public)))];
        match check(&e, &log, &call("vault", json!({}))) {
            CheckOutcome::Block(b) => {
                assert_eq!(b.requirement_gaps, vec![Gap::Attention(MarkName::new("signoff"))]);
                assert!(b.narrowing.is_some());
                assert_eq!(b.unestablished.len(), 1);
                assert_eq!(b.unestablished[0].dimensions, BTreeSet::from([Dimension::Trust]));
            }
            other => panic!("expected a three-slot block, got {other:?}"),
        }
    }

    #[test]
    fn a_gap_and_an_unestablished_source_split_by_dimension() {
        let vault = ToolContract {
            name: ToolName::new("vault"),
            tags: vec![],
            delta: Some(Delta {
                trust: None,
                audience: None,
            }),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(TRUSTED),
                    audience: vec![crate::contract::AudienceRequirement::Cap(Audience::restricted([
                        ReaderId::new("internal"),
                    ]))],
                },
                ..Requires::default()
            },
        };
        let e = engine(vec![vault]);
        let log = vec![user_value(Label::new(Dim::Known(SUSPICIOUS), Dim::Unknown))];
        match check(&e, &log, &call("vault", json!({}))) {
            CheckOutcome::Block(b) => {
                assert_eq!(
                    b.requirement_gaps,
                    vec![Gap::TrustFloor {
                        required: TRUSTED,
                        actual: SUSPICIOUS,
                    }]
                );
                assert_eq!(b.unestablished.len(), 1);
                assert_eq!(b.unestablished[0].dimensions, BTreeSet::from([Dimension::Audience]));
            }
            other => panic!("expected a gap+unestablished block, got {other:?}"),
        }
    }

    #[test]
    fn replay_refuses_malformed_cast_history() {
        let classifier = crate::authority::Cast {
            name: crate::names::CastName::new("classifier"),
            resolution: crate::authority::CastResolution::Resolver {
                may_cast: crate::authority::CastCeiling {
                    trust: vec![SUSPICIOUS],
                    audience: Audience::Public,
                },
            },
            scope: crate::authority::Scope::default(),
        };
        let cfg = RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![classifier],
        };
        let e = open_engine(cfg);
        let cast_fact = |value: u64, resolved: EstablishedLabel, cast: &str| Fact::CastApplied {
            trajectory: traj(),
            value: crate::value::ValueId::new(value),
            resolved,
            cast: crate::names::CastName::new(cast),
        };
        let unknown_source = user_value(Label::new(Dim::Unknown, Dim::Known(Audience::Public)));
        let good = cast_fact(0, established(SUSPICIOUS, Audience::Public), "classifier");

        assert_eq!(e.validate_replay(&[unknown_source.clone(), good.clone()]), Ok(()));
        assert_eq!(
            e.validate_replay(std::slice::from_ref(&good)),
            Err(TransitionRefusal::CastBeforeSource)
        );
        assert_eq!(
            e.validate_replay(&[unknown_source.clone(), good.clone(), good.clone()]),
            Err(TransitionRefusal::RepeatResolution)
        );
        assert_eq!(
            e.validate_replay(&[user_value(known(TRUSTED, Audience::Public)), good.clone()]),
            Err(TransitionRefusal::RepeatResolution)
        );
        assert!(matches!(
            e.validate_replay(&[
                unknown_source.clone(),
                cast_fact(0, established(SUSPICIOUS, Audience::Public), "bogus")
            ]),
            Err(TransitionRefusal::UnknownCast(name)) if name == "bogus"
        ));
        let sibling = TrajectoryId::new("sibling");
        let sibling_resolves = [
            forked_child(&e, &[], &sibling),
            vec![
                unknown_source.clone(),
                Fact::CastApplied {
                    trajectory: sibling.clone(),
                    value: crate::value::ValueId::new(0),
                    resolved: established(SUSPICIOUS, Audience::Public),
                    cast: crate::names::CastName::new("classifier"),
                },
            ],
        ]
        .concat();
        assert_eq!(
            e.validate_replay(&sibling_resolves),
            Err(TransitionRefusal::ForeignResolution)
        );
        assert_eq!(
            e.validate_replay(&[
                unknown_source.clone(),
                Fact::CastApplied {
                    trajectory: TrajectoryId::new("stranger"),
                    value: crate::value::ValueId::new(0),
                    resolved: established(SUSPICIOUS, Audience::Public),
                    cast: crate::names::CastName::new("classifier"),
                }
            ]),
            Err(TransitionRefusal::ForeignTrajectory)
        );
        assert_eq!(
            e.validate_replay(&[
                unknown_source,
                cast_fact(0, established(TRUSTED, Audience::Public), "classifier")
            ]),
            Err(TransitionRefusal::InadmissibleResolution)
        );
    }

    #[test]
    fn replay_holds_a_fork_to_its_parents_frozen_basis() {
        let classifier = crate::authority::Cast {
            name: crate::names::CastName::new("classifier"),
            resolution: crate::authority::CastResolution::Resolver {
                may_cast: crate::authority::CastCeiling {
                    trust: vec![SUSPICIOUS],
                    audience: Audience::Public,
                },
            },
            scope: crate::authority::Scope::default(),
        };
        let e = open_engine(RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![classifier],
        });
        let child = TrajectoryId::new("child");
        let unknown_source = user_value(Label::new(Dim::Unknown, Dim::Known(Audience::Public)));
        let fork = |snapshot: crate::fact::ForkSnapshot| Fact::Boundary {
            trajectory: child.clone(),
            kind: crate::fact::BoundaryKind::Fork {
                parent: traj(),
                snapshot,
                return_policy: crate::fact::ReturnPolicy::Raw,
            },
        };
        let basis_after = |log: &[Fact]| {
            Projection::build(log, crate::fact::Revision::new(log.len() as u64))
                .view(&traj())
                .freeze_basis()
        };
        let resolve = |trajectory: TrajectoryId, value: u64| Fact::CastApplied {
            trajectory,
            value: crate::value::ValueId::new(value),
            resolved: established(SUSPICIOUS, Audience::Public),
            cast: crate::names::CastName::new("classifier"),
        };

        let opened = vec![unknown_source.clone()];
        let snapshot = basis_after(&opened);
        assert_eq!(
            e.validate_replay(&[unknown_source.clone(), fork(snapshot.clone())]),
            Ok(())
        );
        assert_eq!(
            e.validate_replay(&[
                unknown_source.clone(),
                fork(snapshot.clone()),
                resolve(child.clone(), 0)
            ]),
            Ok(())
        );

        assert_eq!(
            e.validate_replay(&[unknown_source.clone(), fork(basis_after(&[]))]),
            Err(TransitionRefusal::ForkBasisMismatch)
        );
        let late = vec![unknown_source.clone(), unknown_source.clone()];
        assert_eq!(
            e.validate_replay(&[unknown_source.clone(), fork(basis_after(&late))]),
            Err(TransitionRefusal::ForkBasisMismatch)
        );
        assert_eq!(
            e.validate_replay(&[
                unknown_source.clone(),
                fork(snapshot.clone()),
                unknown_source.clone(),
                resolve(child.clone(), 1)
            ]),
            Err(TransitionRefusal::ForeignResolution)
        );
        assert_eq!(
            e.validate_replay(&[
                unknown_source.clone(),
                Fact::ValueAdmitted {
                    trajectory: child.clone(),
                    value: LabeledValue::new(ValueBody::new("early"), known(SUSPICIOUS, Audience::Public)),
                    provenance: Provenance::UserInput,
                },
                fork(snapshot.clone())
            ]),
            Err(TransitionRefusal::ForeignTrajectory)
        );
        assert_eq!(
            e.validate_replay(&[unknown_source.clone(), fork(snapshot.clone()), fork(snapshot.clone())]),
            Err(TransitionRefusal::ChildActiveBeforeFork)
        );
        let refork = vec![unknown_source.clone(), fork(snapshot.clone()), unknown_source.clone()];
        let widened = basis_after(&refork[..3]);
        assert_eq!(
            e.validate_replay(&[refork.clone(), vec![fork(widened)]].concat()),
            Err(TransitionRefusal::ChildActiveBeforeFork)
        );
        assert_eq!(
            e.validate_replay(&[
                unknown_source.clone(),
                Fact::Boundary {
                    trajectory: child.clone(),
                    kind: crate::fact::BoundaryKind::Fork {
                        parent: traj(),
                        snapshot: snapshot.clone(),
                        return_policy: crate::fact::ReturnPolicy::Sanitized(crate::names::SanitizerName::new("redact")),
                    },
                }
            ]),
            Err(TransitionRefusal::ForkReturnPolicyMismatch)
        );

        let grandchild = Fact::Boundary {
            trajectory: TrajectoryId::new("grandchild"),
            kind: crate::fact::BoundaryKind::Fork {
                parent: child.clone(),
                snapshot: crate::fact::ForkSnapshot::freeze(
                    EstablishedLabel::top(),
                    std::iter::empty(),
                    std::iter::empty(),
                ),
                return_policy: crate::fact::ReturnPolicy::Raw,
            },
        };
        assert_eq!(
            e.validate_replay(&[unknown_source.clone(), grandchild.clone(), fork(snapshot.clone())]),
            Err(TransitionRefusal::ForeignTrajectory)
        );
        assert_eq!(
            e.validate_replay(&[unknown_source, fork(snapshot), grandchild]),
            Err(TransitionRefusal::ForkBasisMismatch)
        );
    }

    #[test]
    fn replay_holds_a_later_fork_to_the_absorbed_basis() {
        let e = engine(vec![]);
        let child = TrajectoryId::new("child");
        let mut log = vec![user_value(known(TRUSTED, Audience::Public))];
        log.extend(forked_child(&e, &log.clone(), &child));
        log.push(Fact::ValueAdmitted {
            trajectory: child.clone(),
            value: LabeledValue::new(
                ValueBody::new("read"),
                Label::new(Dim::Unknown, Dim::Known(Audience::Public)),
            ),
            provenance: Provenance::UserInput,
        });
        let crossing = e
            .submit_child_return(
                &Projection::build(&log, Revision::new(log.len() as u64)).view(&traj()),
                &child,
                crate::branch::ReturnSubmission::Raw {
                    body: ValueBody::new("found"),
                },
            )
            .expect("an unresolved-identity crossing merges");
        log.extend(crossing.facts);
        assert_eq!(e.validate_replay(&log), Ok(()));

        let sibling = TrajectoryId::new("sibling");
        log.extend(forked_child(&e, &log.clone(), &sibling));
        assert_eq!(e.validate_replay(&log), Ok(()));
        let honest = match log.last() {
            Some(Fact::Boundary {
                kind: crate::fact::BoundaryKind::Fork { snapshot, .. },
                ..
            }) => snapshot.clone(),
            other => panic!("the fork boundary was just appended, got {other:?}"),
        };
        assert!(
            !honest.seed().is_established(crate::label::Dimension::Trust),
            "the seed pin carries the absorbed unresolved identity"
        );

        let sources = [
            (crate::value::ValueId::new(0), known(TRUSTED, Audience::Public)),
            (crate::value::ValueId::new(2), known(TRUSTED, Audience::Public)),
        ];
        let Some(Fact::Boundary {
            kind: crate::fact::BoundaryKind::Fork { snapshot, .. },
            ..
        }) = log.last_mut()
        else {
            unreachable!("the fork boundary position was just read")
        };
        *snapshot = crate::fact::ForkSnapshot::freeze(
            EstablishedLabel::top(),
            sources.iter().map(|(id, label)| (*id, label)),
            std::iter::empty(),
        );
        assert_eq!(e.validate_replay(&log), Err(TransitionRefusal::ForkBasisMismatch));
    }

    #[test]
    fn replay_refuses_an_out_of_scope_resolution() {
        let fetch = crate::contract::ToolContract {
            name: ToolName::new("fetch"),
            tags: vec![crate::names::TagName::new("web")],
            delta: Some(crate::contract::Delta {
                trust: Some(Dim::Unknown),
                audience: Some(Dim::Known(Audience::Public).into()),
            }),
            parameters: crate::params::ToolParameters::open(),
            emits: crate::fact::EffectSet::default(),
            requires: Default::default(),
        };
        let webby = crate::authority::Cast {
            name: crate::names::CastName::new("webby"),
            resolution: crate::authority::CastResolution::Constant(established(SUSPICIOUS, Audience::Public)),
            scope: crate::authority::Scope {
                tags: vec![crate::names::TagName::new("web")],
            },
        };
        let cfg = RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![fetch],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![webby],
        };
        let e = open_engine(cfg);
        assert_eq!(
            e.validate_replay(&[
                user_value(Label::new(Dim::Unknown, Dim::Known(Audience::Public))),
                Fact::CastApplied {
                    trajectory: traj(),
                    value: crate::value::ValueId::new(0),
                    resolved: established(SUSPICIOUS, Audience::Public),
                    cast: crate::names::CastName::new("webby"),
                }
            ]),
            Err(TransitionRefusal::OutOfScopeResolution)
        );
        let fetch_call = crate::value::ResolvedCall::new(
            ToolName::new("fetch"),
            crate::params::test_arguments(&serde_json::json!({})),
        );
        let dispatch = DispatchId::new(traj(), fetch_call.digest(), 0);
        let sibling = TrajectoryId::new("sibling");
        let foreign_dispatch = [
            forked_child(&e, &[], &sibling),
            vec![
                Fact::DispatchOpened {
                    trajectory: traj(),
                    dispatch: dispatch.clone(),
                    tool: fetch_call.tool().clone(),
                    arguments: fetch_call.canonical_arguments().clone(),
                    proposed_label: EstablishedLabel::top(),
                    receiving: EstablishedLabel::top(),
                    proposed_effects: crate::fact::EffectSet::default(),
                    dynamic_resolutions: Vec::new(),
                    subject: None,
                },
                Fact::ValueAdmitted {
                    trajectory: sibling.clone(),
                    value: crate::value::LabeledValue::new(
                        crate::value::ValueBody::new("page"),
                        Label::new(Dim::Unknown, Dim::Known(Audience::Public)),
                    ),
                    provenance: crate::value::Provenance::ToolResult { dispatch },
                },
                Fact::CastApplied {
                    trajectory: sibling,
                    value: crate::value::ValueId::new(0),
                    resolved: established(SUSPICIOUS, Audience::Public),
                    cast: crate::names::CastName::new("webby"),
                },
            ],
        ]
        .concat();
        assert_eq!(
            e.validate_replay(&foreign_dispatch),
            Err(TransitionRefusal::ForeignDispatch)
        );
        assert_eq!(
            e.validate_replay(&[Fact::ValueAdmitted {
                trajectory: traj(),
                value: crate::value::LabeledValue::new(
                    crate::value::ValueBody::new("page"),
                    Label::new(Dim::Unknown, Dim::Known(Audience::Public)),
                ),
                provenance: crate::value::Provenance::ToolResult {
                    dispatch: DispatchId::new(traj(), fetch_call.digest(), 7),
                },
            }]),
            Err(TransitionRefusal::UnknownDispatch)
        );
    }

    fn unannotated_tool(name: &str) -> ToolContract {
        ToolContract {
            name: ToolName::new(name),
            tags: vec![],
            delta: None,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires::default(),
        }
    }

    #[test]
    fn an_unannotated_tool_dispatches_and_its_result_admits_unknown() {
        let e = engine(vec![unannotated_tool("probe")]);
        let mut log = vec![user_value(known(TRUSTED, Audience::Public))];
        let proposed = call("probe", json!({}));
        assert_eq!(check(&e, &log, &proposed), CheckOutcome::Allow);

        let t = traj();
        let p = Projection::build(&log, Revision::new(log.len() as u64));
        let batch = e.open_dispatch(&p.view(&t), &proposed).unwrap();
        log.extend(batch.facts);
        let p = Projection::build(&log, Revision::new(log.len() as u64));
        let dispatch = DispatchId::new(t.clone(), proposed.digest(), 0);
        let batch = e
            .admit_result(
                &p.view(&t),
                &dispatch,
                &proposed,
                ResultAdmission::SuccessRaw {
                    body: ValueBody::new("raw"),
                },
            )
            .unwrap();
        log.extend(batch.facts);
        let p = Projection::build(&log, Revision::new(log.len() as u64));
        let current = p.view(&t).current_label();
        assert_eq!(current.bound(), &EstablishedLabel::new(TRUSTED, Audience::Public));
        assert!(!current.is_established(Dimension::Trust));
        assert!(!current.is_established(Dimension::Audience));
        assert!(current.unresolved(Dimension::Trust).any(|id| id == ValueId::new(1)));
        assert!(current.unresolved(Dimension::Audience).any(|id| id == ValueId::new(1)));
    }

    #[test]
    fn an_unknown_trajectory_blocks_only_requirement_consuming_calls() {
        let e = engine(vec![unannotated_tool("noop"), crm_tool()]);
        let log = vec![user_value(Label::new(Dim::Unknown, Dim::Unknown))];
        assert_eq!(check(&e, &log, &call("noop", json!({}))), CheckOutcome::Allow);
        match check(&e, &log, &call("get_ticket", json!({}))) {
            CheckOutcome::Block(b) => {
                assert!(b.requirement_gaps.is_empty());
                assert_eq!(
                    b.unestablished,
                    vec![crate::check::UnestablishedFact {
                        value: ValueId::new(0),
                        dimensions: BTreeSet::from([Dimension::Trust, Dimension::Audience]),
                    }]
                );
            }
            other => panic!("expected an unestablished block, got {other:?}"),
        }
    }

    #[test]
    fn unknown_tool_errors() {
        let e = engine(vec![]);
        let p = Projection::build(&[], Revision::ZERO);
        let t = traj();
        assert!(matches!(
            e.check(&p.view(&t), &call("ghost", json!({}))),
            Err(EngineError::UnknownTool(name)) if name == "ghost"
        ));
    }

    #[test]
    fn open_dispatch_refuses_a_blocked_call() {
        let e = engine(vec![crm_tool()]);
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let log = vec![user_value(known(SUSPICIOUS, internal))];
        let p = Projection::build(&log, Revision::new(log.len() as u64));
        let t = traj();
        assert_eq!(
            e.open_dispatch(&p.view(&t), &call("get_ticket", json!({}))),
            Err(EngineError::NotAllowed)
        );
    }

    #[test]
    fn includes_missing_placeholder_fails_closed_on_public() {
        let send = ToolContract {
            name: ToolName::new("send_email"),
            tags: vec![],
            delta: Some(Delta::NONE),
            parameters: crate::params::ToolParameters::open(),
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
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        match check(&e, &log, &call("send_email", json!({}))) {
            CheckOutcome::Block(b) => assert!(matches!(b.requirement_gaps.as_slice(), [Gap::Includes { .. }])),
            other => panic!("expected includes gap on a malformed call, got {other:?}"),
        }

        let log = vec![user_value(Label::new(Dim::Known(TRUSTED), Dim::Unknown))];
        match check(&e, &log, &call("send_email", json!({}))) {
            CheckOutcome::Block(b) => {
                assert!(b.requirement_gaps.is_empty(), "the sentinel gap must be masked");
                assert_eq!(b.unestablished.len(), 1);
                assert_eq!(b.unestablished[0].dimensions, BTreeSet::from([Dimension::Audience]));
            }
            other => panic!("expected an unestablished block on an Unknown audience, got {other:?}"),
        }
    }

    #[test]
    fn required_rulings_route_each_gap_to_its_authority() {
        use crate::authority::{Authority, Mandate};
        use crate::names::AuthorityName;

        let wire = ToolContract {
            name: ToolName::new("wire"),
            tags: vec![],
            delta: Some(Delta::NONE),
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
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![wire],
            authorities: vec![officer],
            sanitizers: vec![],
            casts: vec![],
        };
        let e = open_engine(cfg);
        let log = vec![user_value(known(SUSPICIOUS, Audience::Public))];
        let p = Projection::build(&log, Revision::new(log.len() as u64));
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

    fn strict_tool(name: &str) -> ToolContract {
        ToolContract {
            name: ToolName::new(name),
            tags: vec![],
            parameters: crate::params::ToolParameters::compile(&json!({
                "type": "object",
                "properties": { "to": { "type": "string" } },
                "required": ["to"],
            }))
            .unwrap(),
            delta: Some(Delta::NONE),
            emits: EffectSet::default(),
            requires: Requires::default(),
        }
    }

    #[test]
    fn schema_invalid_arguments_are_an_invalid_call_at_every_fresh_entry_point() {
        let e = engine(vec![strict_tool("send")]);
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let p = Projection::build(&log, Revision::new(1));
        let t = traj();
        let views = p.view(&t);
        let bogus = call("send", json!({ "bogus": 1 }));
        assert!(matches!(e.check(&views, &bogus), Err(EngineError::InvalidCall(_))));
        assert!(matches!(
            e.open_dispatch(&views, &bogus),
            Err(EngineError::InvalidCall(_))
        ));
        let raw = crate::check::RawBlock {
            requirement_gaps: vec![],
            narrowing: None,
            unestablished: vec![],
        };
        assert!(matches!(e.plan(&views, &bogus, &raw), Err(EngineError::InvalidCall(_))));
        let fabricated = plan::ExecutableRemedyPlan {
            id: plan::PlanId::new(0),
            steps: vec![],
            required: vec![],
        };
        assert!(matches!(
            e.execute_remedy_plan(&views, &fabricated, &bogus, &[]),
            Err(PlanError::InvalidCall(_))
        ));
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
        assert_eq!(
            e.resolve_call(ToolName::new("ghost"), br#"{}"#),
            Err(EngineError::UnknownTool("ghost".to_string()))
        );
    }

    #[test]
    fn replay_refuses_a_corrupt_dispatched_call() {
        let e = engine(vec![strict_tool("send")]);
        let mut log = vec![user_value(known(TRUSTED, Audience::Public))];
        let good = call("send", json!({ "to": "hr" }));
        let p = Projection::build(&log, Revision::new(1));
        let batch = e.open_dispatch(&p.view(&traj()), &good).unwrap();
        log.extend(batch.facts);
        assert_eq!(e.validate_replay(&log), Ok(()));

        let opened = |tool: &str, payload: serde_json::Value, minted_from: &ResolvedCall| Fact::DispatchOpened {
            trajectory: traj(),
            dispatch: DispatchId::new(traj(), minted_from.digest(), 0),
            tool: ToolName::new(tool),
            arguments: crate::params::test_arguments(&payload),
            proposed_label: established(TRUSTED, Audience::Public),
            receiving: established(TRUSTED, Audience::Public),
            proposed_effects: EffectSet::default(),
            dynamic_resolutions: vec![],
            subject: None,
        };
        let ghost_call = call("ghost", json!({}));
        assert!(matches!(
            e.validate_replay(&[opened("ghost", json!({}), &ghost_call)]),
            Err(TransitionRefusal::UnknownTool(name)) if name == "ghost"
        ));
        let smuggled = call("send", json!({ "bogus": 1 }));
        assert!(matches!(
            e.validate_replay(&[opened("send", json!({ "bogus": 1 }), &smuggled)]),
            Err(TransitionRefusal::InvalidPayload(_))
        ));
        assert!(matches!(
            e.validate_replay(&[opened("send", json!({ "to": "hr" }), &smuggled)]),
            Err(TransitionRefusal::DigestMismatch)
        ));
    }

    #[test]
    fn the_dispatched_payload_is_persisted_exactly_once() {
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
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![wire],
            authorities: vec![officer],
            sanitizers: vec![],
            casts: vec![],
        };
        let e = open_engine(cfg);
        let log = vec![user_value(known(SUSPICIOUS, Audience::Public))];
        let p = Projection::build(&log, Revision::new(1));
        let t = traj();
        let views = p.view(&t);
        let wire_call = call("wire", json!({ "to": "distinctive-recipient-hr" }));
        let raw = match e.check(&views, &wire_call).unwrap() {
            CheckOutcome::Block(raw) => raw,
            other => panic!("expected a block, got {other:?}"),
        };
        let planned = e.plan(&views, &wire_call, &raw).unwrap();
        let chosen = planned.plans[0].executable().expect("an authority plan").clone();
        let ruling = crate::execute::Ruling {
            dispatch: DispatchId::new(t.clone(), wire_call.digest(), 0),
            authority: AuthorityName::new("officer"),
            covers: chosen.required[0].covers.clone(),
            reviewed: crate::execute::AuthorityReview {
                tool: ToolName::new("wire"),
                trajectory_label: partial(SUSPICIOUS, Audience::Public),
            },
        };
        let batch = e.execute_remedy_plan(&views, &chosen, &wire_call, &[ruling]).unwrap();
        let serialized = serde_json::to_string(&batch.facts).unwrap();
        assert_eq!(serialized.matches("distinctive-recipient-hr").count(), 1);
        assert!(matches!(batch.facts.last().unwrap(), Fact::DispatchOpened { .. }));
        let restored: Vec<Fact> = serde_json::from_str(&serialized).unwrap();
        assert_eq!(restored, batch.facts);
    }

    #[test]
    fn open_dispatch_records_proposed_label_and_effects() {
        let e = engine(vec![crm_tool()]);
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let log = vec![user_value(known(TRUSTED, internal.clone()))];
        let p = Projection::build(&log, Revision::new(log.len() as u64));
        let t = traj();
        let batch = e.open_dispatch(&p.view(&t), &call("get_ticket", json!({}))).unwrap();
        match &batch.facts[0] {
            Fact::DispatchOpened { proposed_label, .. } => {
                assert_eq!(*proposed_label, established(TRUSTED, internal));
            }
            other => panic!("expected DispatchOpened, got {other:?}"),
        }
    }

    fn plain_tool(name: &str) -> ToolContract {
        ToolContract {
            name: ToolName::new(name),
            tags: vec![],
            delta: Some(Delta::NONE),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires::default(),
        }
    }

    fn engine_with_provider_run(tools: Vec<ToolContract>, provider_run: &[&str]) -> Engine {
        let cfg = RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools,
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![],
        };
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
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let p = Projection::build(&log, Revision::new(1));
        let t = traj();
        let views = p.view(&t);
        let proposed = call("search", json!({}));
        assert!(matches!(
            e.check(&views, &proposed),
            Err(EngineError::ProviderRunTool(name)) if name == "search"
        ));
        assert!(matches!(
            e.open_dispatch(&views, &proposed),
            Err(EngineError::ProviderRunTool(_))
        ));
        let raw = crate::check::RawBlock {
            requirement_gaps: vec![],
            narrowing: None,
            unestablished: vec![],
        };
        assert!(matches!(
            e.plan(&views, &proposed, &raw),
            Err(EngineError::ProviderRunTool(_))
        ));
        assert!(matches!(
            e.resolve_call(ToolName::new("search"), b"{}"),
            Err(EngineError::ProviderRunTool(_))
        ));
        let fabricated = plan::ExecutableRemedyPlan {
            id: plan::PlanId::new(0),
            steps: vec![],
            required: vec![],
        };
        assert!(matches!(
            e.execute_remedy_plan(&views, &fabricated, &proposed, &[]),
            Err(PlanError::ProviderRunTool(name)) if name == "search"
        ));
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
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let offered_tools = |e: &Engine| -> Vec<String> {
            let p = Projection::build(&log, Revision::new(1));
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
        let batch = e.open_trajectory(&t);
        assert_eq!(batch.basis, Revision::ZERO);
        match batch.facts.as_slice() {
            [
                Fact::TrajectoryOpened {
                    trajectory,
                    dialect,
                    profile,
                    policy_digest,
                    open_vectors,
                },
            ] => {
                assert_eq!(trajectory, &t);
                assert_eq!(*dialect, PolicyDialectVersion::new(1));
                assert_eq!(profile, e.profile());
                assert_eq!(*policy_digest, e.identity());
                assert_eq!(open_vectors, &e.open_vectors());
                assert_eq!(open_vectors.len(), 1);
            }
            other => panic!("expected exactly the opening record, got {other:?}"),
        }
        let wire = serde_json::to_string(&batch.facts).unwrap();
        assert_eq!(serde_json::from_str::<Vec<Fact>>(&wire).unwrap(), batch.facts);
    }

    #[test]
    fn cold_replay_verifies_the_opening_strictly() {
        let e = engine_with_provider_run(vec![plain_tool("send"), plain_tool("search")], &["search"]);
        let t = traj();
        let opening = e.open_trajectory(&t).facts.remove(0);
        let admitted = user_value(known(TRUSTED, Audience::Public));

        assert_eq!(e.verify_opening(&[opening.clone(), admitted.clone()], &t), Ok(()));
        assert_eq!(
            e.verify_opening(std::slice::from_ref(&admitted), &t),
            Err(OpeningTransitionRefusal::Missing)
        );
        assert_eq!(
            e.verify_opening(&[admitted.clone(), opening.clone()], &t),
            Err(OpeningTransitionRefusal::NotFirst)
        );
        assert_eq!(
            e.verify_opening(&[opening.clone(), opening.clone()], &t),
            Err(OpeningTransitionRefusal::Duplicate)
        );
        assert_eq!(
            e.verify_opening(std::slice::from_ref(&opening), &TrajectoryId::new("other")),
            Err(OpeningTransitionRefusal::WrongTrajectory { found: "t".to_string() })
        );

        let mutated = |mutate: &dyn Fn(&mut Fact)| {
            let mut fact = opening.clone();
            mutate(&mut fact);
            e.verify_opening(&[fact], &t)
        };
        assert_eq!(
            mutated(&|fact| {
                if let Fact::TrajectoryOpened { dialect, .. } = fact {
                    *dialect = PolicyDialectVersion::new(9);
                }
            }),
            Err(OpeningTransitionRefusal::UnsupportedDialect { found: 9 })
        );
        assert_eq!(
            mutated(&|fact| {
                if let Fact::TrajectoryOpened { policy_digest, .. } = fact {
                    let other = engine(vec![plain_tool("send")]);
                    *policy_digest = other.identity();
                }
            }),
            Err(OpeningTransitionRefusal::DigestMismatch)
        );
        assert_eq!(
            mutated(&|fact| {
                if let Fact::TrajectoryOpened { profile, .. } = fact {
                    let other = engine(vec![plain_tool("send")]);
                    *profile = other.profile().clone();
                }
            }),
            Err(OpeningTransitionRefusal::ProfileMismatch)
        );
        assert_eq!(
            mutated(&|fact| {
                if let Fact::TrajectoryOpened { open_vectors, .. } = fact {
                    open_vectors.clear();
                }
            }),
            Err(OpeningTransitionRefusal::VectorMismatch)
        );
    }

    #[test]
    fn branching_takes_declared_context_control() {
        let cfg = RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![plain_tool("send")],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![],
        };
        let mut declaration = crate::profile::covering_declaration(&cfg);
        declaration.context_control = false;
        let e = Engine::open(DeploymentPolicy {
            registry: cfg,
            planner_cap: crate::registry::PlannerCap::default(),
            dialect: PolicyDialectVersion::new(1),
            child_return: ReturnPolicy::Raw,
            profile: declaration,
        })
        .unwrap();
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let p = Projection::build(&log, Revision::new(1));
        let t = traj();
        assert_eq!(
            e.seed_child(&p.view(&t), &TrajectoryId::new("t:child")),
            Err(crate::branch::BranchError::ContextUncontrolled)
        );
    }

    #[test]
    fn a_fork_carries_the_deployments_child_return_binding() {
        let mut cfg = RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![plain_tool("send")],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![],
        };
        cfg.sanitizers = vec![crate::authority::Sanitizer {
            name: crate::names::SanitizerName::new("redactor"),
            on: crate::authority::SanitizerPoints {
                input: false,
                output: true,
            },
            transition: crate::authority::Transition::Trust {
                from_floor: SUSPICIOUS,
                to: TRUSTED,
            },
            scope: crate::authority::Scope::default(),
            hint: None,
        }];
        let bound = ReturnPolicy::Sanitized(crate::names::SanitizerName::new("redactor"));
        let e = Engine::open(DeploymentPolicy {
            registry: cfg.clone(),
            planner_cap: crate::registry::PlannerCap::default(),
            dialect: PolicyDialectVersion::new(1),
            child_return: bound.clone(),
            profile: crate::profile::covering_declaration(&cfg),
        })
        .unwrap();
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let p = Projection::build(&log, Revision::new(1));
        let t = traj();
        let batch = e.seed_child(&p.view(&t), &TrajectoryId::new("t:child")).unwrap();
        match batch.facts.as_slice() {
            [
                Fact::Boundary {
                    kind: crate::fact::BoundaryKind::Fork { return_policy, .. },
                    ..
                },
            ] => assert_eq!(return_policy, &bound),
            other => panic!("expected the fork binding, got {other:?}"),
        }
    }

    #[test]
    fn an_opening_record_is_inert_in_projection_and_replay_validation() {
        let e = engine(vec![plain_tool("send")]);
        let t = traj();
        let opening = e.open_trajectory(&t).facts.remove(0);
        let admitted = user_value(known(SUSPICIOUS, Audience::Public));
        let with = [opening.clone(), admitted.clone()];
        let without = [admitted];
        let p_with = Projection::build(&with, Revision::new(2));
        let p_without = Projection::build(&without, Revision::new(1));
        assert_eq!(p_with.view(&t).current_label(), p_without.view(&t).current_label());
        assert_eq!(p_with.view(&t).boundary_count(), p_without.view(&t).boundary_count());
        assert_eq!(e.validate_replay(&with), Ok(()));
    }

    fn raw_call(tool: &str, arguments: &[u8]) -> crate::transition::ProposedCall {
        crate::transition::ProposedCall {
            tool: ToolName::new(tool),
            arguments: arguments.to_vec(),
            dynamic_resolutions: Vec::new(),
        }
    }

    fn exposed(tool: &str, body: &str) -> crate::transition::ProviderResult {
        crate::transition::ProviderResult {
            tool: ToolName::new(tool),
            body: ValueBody::new(body),
        }
    }

    fn viewing(e: &Engine, log: &[Fact]) -> EngineView {
        e.view(&traj(), log.to_vec(), Revision::new(log.len() as u64))
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
        engine_with_provider_run(vec![seen, wire, guard, emit, plain_tool("quiet")], &["seen"])
    }

    fn opening_log() -> Vec<Fact> {
        vec![user_value(known(TRUSTED, Audience::Public))]
    }

    #[test]
    fn an_exposed_provider_run_result_is_history_for_every_sibling() {
        let e = batch_engine();
        let log = opening_log();
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
        let log = opening_log();
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
        let log = opening_log();
        for (position, malformed) in [
            (1, raw_call("nowhere", b"{}")),
            (1, raw_call("seen", b"{}")),
            (1, raw_call("quiet", b"not json")),
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
        let log = opening_log();
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
        let log = opening_log();
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

        let elsewhere = e.handle(
            &viewing(&e, &log),
            batch_on(
                &TrajectoryId::new("other"),
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
        let log = opening_log();
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
    fn a_provider_admission_advances_flow_and_the_family_whose_effects_it_records() {
        let e = engine_with_provider_run(
            vec![plain_tool("quiet"), {
                let mut emitting = plain_tool("loud");
                emitting.emits = EffectSet::new([EffectKind::new("k")]).unwrap();
                emitting
            }],
            &["quiet", "loud"],
        );
        let log = opening_log();
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
        assert!(quiet.flows.contains(&traj()));
        assert!(
            !quiet.family,
            "an observation with no declared effects moves no family state"
        );
        assert!(declared("loud").family);
    }

    #[test]
    fn an_admission_only_batch_decides_and_an_empty_one_is_no_event() {
        let e = batch_engine();
        let log = opening_log();
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
        let log = opening_log();
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
        let e = engine(vec![strict, emit]);
        let log = vec![user_value(known(SUSPICIOUS, Audience::Public))];
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
        let log = opening_log();
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
        let log = opening_log();
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
    fn an_exposed_admission_stales_the_approval_its_own_batch_would_spend() {
        let e = engine_with_provider_run(vec![crm_tool(), plain_tool("seen")], &["seen"]);
        let log = opening_log();
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
        assert!(released.is_empty(), "the approval was stale before the proposal");
        assert_eq!(blocked_names(blocked), ["get_ticket"]);
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
                },
            },
        );
        assert_eq!(
            e.validate_replay(&[log, spliced].concat()),
            Err(TransitionRefusal::MisdecidedBatch)
        );
    }

    #[test]
    fn a_repeat_of_a_multi_sibling_batch_answers_each_position_from_the_record() {
        let e = batch_engine();
        let log = opening_log();
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
    fn a_repeat_matches_each_sibling_to_the_dispatch_its_own_answers_opened() {
        let binding = crate::contract::DynamicAudienceBinding {
            resolver: crate::names::DynamicResolverName::new("acl"),
            argument: "room".to_string(),
        };
        let mut notify = plain_tool("notify");
        notify.requires = Requires {
            label: LabelRequirements {
                trust_floor: None,
                audience: vec![AudienceRequirement::Includes(RecipientSpec::Dynamic(binding.clone()))],
            },
            ..Requires::default()
        };
        let e = engine(vec![notify]);
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let log = vec![user_value(known(TRUSTED, internal.clone()))];
        let pinned = |audience: &Audience| {
            raw(&call("notify", json!({})).with_dynamic_resolutions(vec![
                crate::contract::PinnedDynamicResolution::from_answer(binding.clone(), Some(audience.clone())),
            ]))
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
        assert_eq!(released[0].call.dynamic_resolution(&binding), Some(&internal));
        let log = [log, appended_facts(first)].concat();

        let repeat = e
            .handle(&viewing(&e, &log), batch("b1", Vec::new(), proposals()))
            .expect("the repeat answers");
        assert_eq!(repeat.append, None);
        let (released, blocked) = answered(&repeat);
        assert_eq!(released.len(), 1);
        assert_eq!(released[0].dispatch, ran);
        assert_eq!(
            released[0].call.dynamic_resolution(&binding),
            Some(&internal),
            "the repeat names the sibling that actually ran"
        );
        assert_eq!(blocked.len(), 1);
        assert_eq!(blocked[0].call.dynamic_resolution(&binding), Some(&outsider));
    }

    #[test]
    fn replay_refuses_a_provider_admission_no_act_declared() {
        let e = batch_engine();
        let log = opening_log();
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
        let parent = Projection::build(&log, Revision::new(log.len() as u64));
        let seeded = e
            .seed_child(&parent.view(&traj()), &child)
            .expect("the child seeds")
            .facts;
        let ended = e
            .submit_void_return(
                &Projection::build(&[log.clone(), seeded.clone()].concat(), Revision::new(2)).view(&traj()),
                &child,
            )
            .expect("the child ends its errand")
            .facts;
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
        let log = opening_log();
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
        let opening = opening_log();
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
        let log = opening_log();
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
        let log = opening_log();
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
                    value.label = Label::new(Dim::Known(TRUSTED), Dim::Unknown);
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
        let log = opening_log();
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
        let parent = Projection::build(&log, Revision::new(log.len() as u64));
        let seeded = e
            .seed_child(&parent.view(&traj()), &child)
            .expect("the child seeds")
            .facts;
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

    fn returning_registry(
        sanitizers: Vec<crate::authority::Sanitizer>,
        casts: Vec<crate::authority::Cast>,
    ) -> RegistryConfig {
        RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![plain_tool("spawn"), open_tool("fetch")],
            authorities: vec![],
            sanitizers,
            casts,
        }
    }

    fn lifting_sanitizer(name: &str) -> crate::authority::Sanitizer {
        crate::authority::Sanitizer {
            name: SanitizerName::new(name),
            on: crate::authority::SanitizerPoints {
                input: false,
                output: true,
            },
            transition: crate::authority::Transition::Trust {
                from_floor: SUSPICIOUS,
                to: TRUSTED,
            },
            scope: crate::authority::Scope::default(),
            hint: None,
        }
    }

    fn classifier_cast() -> crate::authority::Cast {
        crate::authority::Cast {
            name: crate::names::CastName::new("classifier"),
            resolution: crate::authority::CastResolution::Resolver {
                may_cast: crate::authority::CastCeiling {
                    trust: vec![SUSPICIOUS],
                    audience: Audience::Public,
                },
            },
            scope: crate::authority::Scope::default(),
        }
    }

    fn pending_stage_of(decision: &EngineDecision) -> &PendingReturnStage {
        match &decision.follow_up {
            FollowUp::Child(ChildFollowUp::Pending(stage)) => stage,
            other => panic!("expected a pending return stage, got {other:?}"),
        }
    }

    fn child_read(child: &TrajectoryId, label: Label) -> Fact {
        Fact::ValueAdmitted {
            trajectory: child.clone(),
            value: LabeledValue::new(ValueBody::new("read"), label),
            provenance: Provenance::UserInput,
        }
    }

    fn with_unresolved_fetch(e: &Engine, log: Vec<Fact>, child: &TrajectoryId) -> Vec<Fact> {
        let fetch = call("fetch", json!({}));
        let released = e
            .handle(
                &viewing(e, &log),
                batch_on(child, "bf", Vec::new(), vec![raw(&fetch)], None),
            )
            .expect("the child's fetch releases");
        let opened = [log, appended_facts(released)].concat();
        let dispatch = opened
            .iter()
            .find_map(|fact| match fact {
                Fact::DispatchOpened {
                    trajectory, dispatch, ..
                } if trajectory == child => Some(dispatch.clone()),
                _ => None,
            })
            .expect("the release opens the dispatch");
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
                }),
            )
            .expect("the unannotated result admits at unknown");
        [opened, appended_facts(admitted)].concat()
    }

    fn fork_in(log: &[Fact], child: &TrajectoryId) -> ForkId {
        log.iter()
            .find_map(|fact| match fact {
                Fact::ForkOpened { trajectory, fork } if trajectory == child => Some(fork.clone()),
                _ => None,
            })
            .expect("the fork opened")
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
            fork: Some(fork_in(log, child)),
            submission: ChildSubmission::Value { body: body.clone() },
            evidence,
            offer_nonce: nonce(),
        })
    }

    #[test]
    fn a_narrowing_fork_return_transfers_custody_and_opens_the_parents_stage() {
        let e = engine(vec![plain_tool("spawn")]);
        let child = TrajectoryId::new("child");
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let mut log = spawn_family(&e, None, &child);
        log.push(child_read(&child, known(SUSPICIOUS, internal.clone())));
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
                from: established(TRUSTED, Audience::Public),
                to: established(SUSPICIOUS, internal.clone()),
            }
        );
        assert_eq!(stage.offers.len(), 1);

        let ended = [log.clone(), appended_facts(decision)].concat();
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
        assert!(
            ended
                .iter()
                .all(|fact| !matches!(fact, Fact::OfferOpened { trajectory, .. } if trajectory != &traj()))
        );

        let after = Projection::build(&ended, Revision::new(ended.len() as u64));
        let parent = traj();
        let views = after.view(&parent);
        assert!(views.has_ended(&child));
        assert!(views.child_return(&ChildReturnId::new(child.clone(), 0)).is_none());
        assert_eq!(views.current_label().bound(), &established(TRUSTED, Audience::Public));

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
        let e = engine(vec![plain_tool("spawn")]);
        let child = TrajectoryId::new("child");
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let mut log = spawn_family(&e, None, &child);
        log.push(child_read(&child, known(SUSPICIOUS, internal.clone())));
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
        let views = Projection::build(&merged, Revision::new(merged.len() as u64));
        assert_eq!(
            views.view(&traj()).current_label().bound(),
            &established(SUSPICIOUS, internal)
        );

        let repeat = execute_offer(&e, &merged, accept, OfferOutcome::Approved(vec![])).expect("the repeat answers");
        assert_eq!(repeat.append, None);
        assert!(matches!(
            offer_answer(&repeat),
            OfferFollowUp::Admitted { value } if value == &body
        ));
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

    #[test]
    fn an_inapplicable_sanitizer_charges_the_return_no_resolution() {
        let mut scoped = lifting_sanitizer("scoped-lifter");
        scoped.scope = crate::authority::Scope {
            tags: vec![crate::names::TagName::new("web")],
        };
        let e = open_engine(returning_registry(vec![scoped], vec![classifier_cast()]));
        let child = TrajectoryId::new("child");
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let mut log = spawn_family(&e, None, &child);
        log.push(child_read(&child, known(SUSPICIOUS, internal)));
        let log = with_unresolved_fetch(&e, log, &child);
        let body = ValueBody::new("what I found");
        let decision = e
            .handle(
                &viewing(&e, &log),
                child_report(&log, &child, ChildSubmission::Value { body: body.clone() }),
            )
            .expect("the submission transfers custody without cast IO");
        let stage = pending_stage_of(&decision);
        assert_eq!(stage.offers.len(), 1, "acceptance alone — no hop, no resolution");
        let appended = appended_facts(decision);
        assert!(appended.iter().any(|fact| matches!(fact, Fact::ReturnSubmitted { .. })));
        assert!(!appended.iter().any(|fact| matches!(fact, Fact::CastApplied { .. })));
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
            transition: crate::authority::Transition::Audience {
                from_includes: Audience::restricted([ReaderId::new("external")]),
                to: Audience::Public,
            },
            scope: crate::authority::Scope::default(),
            hint: None,
        };
        let e = open_engine(returning_registry(vec![unreachable_from], vec![classifier_cast()]));
        let child = TrajectoryId::new("child");
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let mut log = spawn_family(&e, None, &child);
        log.push(child_read(&child, known(SUSPICIOUS, internal)));
        let log = with_unresolved_fetch(&e, log, &child);
        let body = ValueBody::new("what I found");
        let decision = e
            .handle(
                &viewing(&e, &log),
                child_report(&log, &child, ChildSubmission::Value { body: body.clone() }),
            )
            .expect("the submission transfers custody without cast IO");
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
        let e = engine(vec![plain_tool("spawn")]);
        let child = TrajectoryId::new("child");
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let mut log = spawn_family(&e, None, &child);
        log.push(child_read(&child, known(SUSPICIOUS, internal.clone())));
        let body = ValueBody::new("what I found");
        let decision = e
            .handle(
                &viewing(&e, &log),
                child_report(&log, &child, ChildSubmission::Value { body: body.clone() }),
            )
            .expect("the submission transfers custody");
        let stage = pending_stage_of(&decision).clone();
        let mut ended = [log, appended_facts(decision)].concat();
        ended.push(child_read(&traj(), known(SUSPICIOUS, Audience::Public)));

        let accept = return_offer(&ended, false);
        let crossed = execute_offer(&e, &ended, accept, OfferOutcome::Approved(vec![]))
            .expect("the acceptance crosses the pinned residual over the moved fold");
        let crossing = appended_facts(crossed);
        assert!(crossing.iter().any(|fact| matches!(
            fact,
            Fact::ChildReturnAcceptance { narrowing, .. } if narrowing == &stage.residual
        )));
        let merged = [ended, crossing].concat();
        assert_eq!(e.validate_replay(&merged), Ok(()));
        let views = Projection::build(&merged, Revision::new(merged.len() as u64));
        assert_eq!(
            views.view(&traj()).current_label().bound(),
            &established(SUSPICIOUS, internal)
        );
    }

    #[test]
    fn a_resolution_stales_a_return_stage_and_the_redrive_replans_it() {
        let e = open_engine(returning_registry(vec![], vec![classifier_cast()]));
        let child = TrajectoryId::new("child");
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let mut log = spawn_family(&e, None, &child);
        log.push(child_read(&child, known(SUSPICIOUS, internal.clone())));
        let log = with_unresolved_fetch(&e, log, &child);
        let body = ValueBody::new("what I found");
        let decision = e
            .handle(
                &viewing(&e, &log),
                child_report(&log, &child, ChildSubmission::Value { body: body.clone() }),
            )
            .expect("the submission transfers custody");
        let ended = [log, appended_facts(decision)].concat();
        let stale = return_offer(&ended, false);

        let redriven = e
            .handle(
                &viewing(&e, &ended),
                EngineEvent::ChildReturn(ChildReport {
                    child: child.clone(),
                    fork: Some(fork_in(&ended, &child)),
                    submission: ChildSubmission::Value { body: body.clone() },
                    evidence: vec![Evidence::Cast {
                        cast: crate::names::CastName::new("classifier"),
                        value: ValueId::new(2),
                        resolved: established(SUSPICIOUS, Audience::Public),
                    }],
                    offer_nonce: crate::value::OfferNonce::new([9u8; 32]),
                }),
            )
            .expect("the re-drive lands the resolution and re-plans the stage");
        let restage = pending_stage_of(&redriven).clone();
        let replanned = [ended, appended_facts(redriven)].concat();
        assert_eq!(e.validate_replay(&replanned), Ok(()));
        assert!(replanned.iter().any(|fact| matches!(fact, Fact::CastApplied { .. })));
        assert_eq!(
            execute_offer(&e, &replanned, stale, OfferOutcome::Approved(vec![])),
            Err(TransitionError::StaleOffer)
        );
        assert_eq!(
            replanned
                .iter()
                .filter(|fact| matches!(fact, Fact::ReturnSubmitted { .. }))
                .count(),
            1
        );
        assert_ne!(restage.offers[0].0, stale);

        let crossed = execute_offer(&e, &replanned, restage.offers[0].0, OfferOutcome::Approved(vec![]))
            .expect("the fresh acceptance crosses");
        assert!(matches!(
            offer_answer(&crossed),
            OfferFollowUp::Admitted { value } if value == &body
        ));
        assert_eq!(
            e.validate_replay(&[replanned, appended_facts(crossed)].concat()),
            Ok(())
        );
    }

    #[test]
    fn a_staged_sanitizer_hop_replaces_the_candidate_and_replans() {
        let e = open_engine(returning_registry(vec![lifting_sanitizer("redactor")], vec![]));
        let child = TrajectoryId::new("child");
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let mut log = spawn_family(&e, None, &child);
        log.push(child_read(&child, known(SUSPICIOUS, internal.clone())));
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
                from: established(TRUSTED, Audience::Public),
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
            Projection::build(&merged, Revision::new(merged.len() as u64))
                .view(&traj())
                .current_label()
                .bound(),
            &established(TRUSTED, internal)
        );
    }

    #[test]
    fn a_hop_that_settles_the_residual_crosses_in_its_own_batch() {
        let e = open_engine(returning_registry(vec![lifting_sanitizer("redactor")], vec![]));
        let child = TrajectoryId::new("child");
        let mut log = spawn_family(&e, None, &child);
        log.push(child_read(&child, known(SUSPICIOUS, Audience::Public)));
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
        let e = open_engine_returning(
            returning_registry(vec![lifting_sanitizer("quarantine")], vec![]),
            quarantine,
        );
        let child = TrajectoryId::new("child");
        let mut log = spawn_family(&e, None, &child);
        log.push(child_read(&child, known(SUSPICIOUS, Audience::Public)));
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
            Projection::build(&ended, Revision::new(ended.len() as u64))
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
        let merged = [ended, crossing].concat();
        assert_eq!(e.validate_replay(&merged), Ok(()));

        let mut forged = merged.clone();
        for fact in &mut forged {
            if let Fact::ChildReturn { value, derivation, .. } = fact {
                *derivation = ReturnDerivation::Raw;
                *value = LabeledValue::new(body.clone(), known(SUSPICIOUS, Audience::Public));
            }
        }
        assert_eq!(e.validate_replay(&forged), Err(TransitionRefusal::ReturnPolicyMismatch));
    }

    #[test]
    fn a_narrowing_mandatory_derivation_enters_the_staged_pipeline() {
        let e = open_engine_returning(
            returning_registry(vec![lifting_sanitizer("quarantine")], vec![]),
            ReturnPolicy::Sanitized(SanitizerName::new("quarantine")),
        );
        let child = TrajectoryId::new("child");
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let mut log = spawn_family(&e, None, &child);
        log.push(child_read(&child, known(SUSPICIOUS, internal.clone())));
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
            Projection::build(&merged, Revision::new(merged.len() as u64))
                .view(&traj())
                .current_label()
                .bound(),
            &established(TRUSTED, internal)
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
            transition: crate::authority::Transition::Trust {
                from_floor: TRUSTED,
                to: TRUSTED,
            },
            scope: crate::authority::Scope::default(),
            hint: None,
        };
        let e = open_engine_returning(
            returning_registry(vec![picky], vec![]),
            ReturnPolicy::Sanitized(SanitizerName::new("picky")),
        );
        let child = TrajectoryId::new("child");
        let mut log = spawn_family(&e, None, &child);
        log.push(child_read(&child, known(SUSPICIOUS, Audience::Public)));
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
        let views = Projection::build(&ended, Revision::new(ended.len() as u64));
        assert!(views.view(&traj()).has_ended(&child));
        assert_eq!(
            views.view(&traj()).current_label().bound(),
            &established(TRUSTED, Audience::Public)
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
                *reason = ReturnRejection::ConsumedDimensionUnresolvable;
            }
        }
        assert_eq!(
            e.validate_replay(&flipped),
            Err(TransitionRefusal::ReturnRecordMismatch)
        );
    }

    #[test]
    fn a_consumed_dimension_resolves_first_or_rejects_or_excludes() {
        let e = open_engine_returning(
            returning_registry(vec![lifting_sanitizer("quarantine")], vec![classifier_cast()]),
            ReturnPolicy::Sanitized(SanitizerName::new("quarantine")),
        );
        let child = TrajectoryId::new("child");
        let log = with_unresolved_fetch(&e, spawn_family(&e, None, &child), &child);
        let body = ValueBody::new("what I found");
        let asked = e
            .handle(
                &viewing(&e, &log),
                child_report(&log, &child, ChildSubmission::Value { body: body.clone() }),
            )
            .expect("the unresolved dimension asks for its cast");
        assert_eq!(asked.append, None);
        assert_eq!(
            asked.follow_up,
            FollowUp::Child(ChildFollowUp::Resolve(EvidenceRequest::Cast {
                cast: crate::names::CastName::new("classifier"),
                value: ValueId::new(1),
                body: ValueBody::new("page"),
            }))
        );

        let answer = Evidence::Cast {
            cast: crate::names::CastName::new("classifier"),
            value: ValueId::new(1),
            resolved: established(SUSPICIOUS, Audience::Public),
        };
        let resolved = e
            .handle(
                &viewing(&e, &log),
                evidenced_report(&log, &child, &body, vec![answer.clone()]),
            )
            .expect("the resolved dimension lets custody transfer");
        assert!(matches!(
            resolved.follow_up,
            FollowUp::Child(ChildFollowUp::Resolve(EvidenceRequest::Sanitizer { .. }))
        ));
        let appended = appended_facts(resolved);
        assert!(appended.iter().any(|fact| matches!(fact, Fact::CastApplied { .. })));
        assert!(appended.iter().any(|fact| matches!(fact, Fact::ReturnSubmitted { .. })));
        let ended = [log, appended].concat();
        assert_eq!(e.validate_replay(&ended), Ok(()));

        let repeat = e
            .handle(
                &viewing(&e, &ended),
                evidenced_report(&ended, &child, &body, vec![answer]),
            )
            .expect("the landed resolution repeats cleanly");
        assert_eq!(repeat.append, None);
        assert!(matches!(
            repeat.follow_up,
            FollowUp::Child(ChildFollowUp::Resolve(EvidenceRequest::Sanitizer { .. }))
        ));

        let e = open_engine_returning(
            returning_registry(vec![lifting_sanitizer("quarantine")], vec![]),
            ReturnPolicy::Sanitized(SanitizerName::new("quarantine")),
        );
        let mut log = spawn_family(&e, None, &child);
        log.push(child_read(
            &child,
            Label::new(Dim::Unknown, Dim::Known(Audience::Public)),
        ));
        let rejected = e
            .handle(
                &viewing(&e, &log),
                child_report(&log, &child, ChildSubmission::Value { body: body.clone() }),
            )
            .expect("the unresolvable dimension rejects terminally");
        assert_eq!(
            rejected.follow_up,
            FollowUp::Child(ChildFollowUp::Rejected {
                reason: ReturnRejection::ConsumedDimensionUnresolvable,
            })
        );
        assert_eq!(e.validate_replay(&[log, appended_facts(rejected)].concat()), Ok(()));

        let e = open_engine(returning_registry(vec![lifting_sanitizer("redactor")], vec![]));
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let mut log = spawn_family(&e, None, &child);
        log.push(child_read(&child, Label::new(Dim::Unknown, Dim::Known(internal))));
        let staged = e
            .handle(
                &viewing(&e, &log),
                child_report(&log, &child, ChildSubmission::Value { body: body.clone() }),
            )
            .expect("the narrowing submission stages");
        assert_eq!(pending_stage_of(&staged).offers.len(), 1);
        assert_eq!(e.validate_replay(&[log, appended_facts(staged)].concat()), Ok(()));
    }

    #[test]
    fn return_custody_records_replay_only_as_produced() {
        let e = engine(vec![plain_tool("spawn")]);
        let child = TrajectoryId::new("child");
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let mut log = spawn_family(&e, None, &child);
        log.push(child_read(&child, known(SUSPICIOUS, internal.clone())));
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
                *label = partial(TRUSTED, Audience::Public);
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
            returning_registry(vec![lifting_sanitizer("quarantine")], vec![]),
            ReturnPolicy::Sanitized(SanitizerName::new("quarantine")),
        );
        let child = TrajectoryId::new("child");
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let mut log = spawn_family(&e, None, &child);
        log.push(child_read(&child, known(SUSPICIOUS, internal.clone())));
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
        let e = engine(vec![plain_tool("spawn")]);
        let child = TrajectoryId::new("child");
        let mut log = spawn_family(&e, Some(&verdict_schema()), &child);
        log.push(child_read(&child, known(SUSPICIOUS, Audience::Public)));
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
            } if value.label == known(SUSPICIOUS, Audience::Public) && value.body == body
        )));
        let merged = [ended, crossing].concat();
        assert_eq!(e.validate_replay(&merged), Ok(()));
        assert_eq!(
            Projection::build(&merged, Revision::new(merged.len() as u64))
                .view(&traj())
                .current_label()
                .bound(),
            &established(SUSPICIOUS, Audience::Public)
        );
    }

    #[test]
    fn an_unshaped_fork_offers_no_attest_hop() {
        let e = open_engine(returning_registry(vec![lifting_sanitizer("attest-schema")], vec![]));
        let child = TrajectoryId::new("child");
        let mut log = spawn_family(&e, None, &child);
        log.push(child_read(&child, known(SUSPICIOUS, Audience::Public)));
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
        let e = open_engine(returning_registry(vec![lifting_sanitizer("redactor")], vec![]));
        let child = TrajectoryId::new("child");
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let mut log = spawn_family(&e, Some(&verdict_schema()), &child);
        log.push(child_read(&child, known(SUSPICIOUS, internal)));
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
        let e = open_engine(returning_registry(vec![lifting_sanitizer("attest-schema")], vec![]));
        let child = TrajectoryId::new("child");
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let mut log = spawn_family(&e, Some(&verdict_schema()), &child);
        log.push(child_read(&child, known(SUSPICIOUS, internal.clone())));
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
            Projection::build(&merged, Revision::new(merged.len() as u64))
                .view(&traj())
                .current_label()
                .bound(),
            &established(TRUSTED, internal)
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
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![ToolContract {
                name: ToolName::new("fetch"),
                tags: vec![],
                delta: Some(Delta {
                    trust: Some(Dim::Known(SUSPICIOUS)),
                    audience: None,
                }),
                parameters: crate::params::ToolParameters::open(),
                emits: EffectSet::new([EffectKind::new("web.read")]).unwrap(),
                requires: Requires::default(),
            }],
            authorities: vec![],
            sanitizers: vec![lifting_sanitizer("redactor"), lifting_sanitizer("attest-schema")],
            casts: vec![],
        });
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
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
            transition: crate::authority::Transition::Trust {
                from_floor: SUSPICIOUS,
                to: Trust::new(2),
            },
            scope: crate::authority::Scope::default(),
            hint: None,
        };
        let e = open_engine(RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into(), "gold".into()]),
            tools: vec![plain_tool("spawn")],
            authorities: vec![],
            sanitizers: vec![attest],
            casts: vec![],
        });
        let child = TrajectoryId::new("child");
        let mut log = spawn_family(&e, Some(&verdict_schema()), &child);
        log.push(child_read(&child, known(SUSPICIOUS, Audience::Public)));
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
            returning_registry(vec![lifting_sanitizer("attest-schema")], vec![]),
            ReturnPolicy::Sanitized(SanitizerName::new("attest-schema")),
        );
        let child = TrajectoryId::new("child");
        let mut log = spawn_family(&e, Some(&verdict_schema()), &child);
        log.push(child_read(&child, known(SUSPICIOUS, Audience::Public)));
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
        let views = Projection::build(&merged, Revision::new(merged.len() as u64));
        assert!(views.view(&traj()).has_ended(&child));
        assert_eq!(
            views.view(&traj()).current_label().bound(),
            &established(TRUSTED, Audience::Public)
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
            returning_registry(vec![lifting_sanitizer("attest-schema")], vec![]),
            ReturnPolicy::Sanitized(SanitizerName::new("attest-schema")),
        );
        let child = TrajectoryId::new("child");
        let mut log = spawn_family(&e, Some(&verdict_schema()), &child);
        log.push(child_read(&child, known(SUSPICIOUS, Audience::Public)));
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
            Projection::build(&ended, Revision::new(ended.len() as u64))
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
    fn selection_skips_a_cast_that_cannot_establish_the_consumed_dimension() {
        let deaf = crate::authority::Cast {
            name: crate::names::CastName::new("deaf"),
            resolution: crate::authority::CastResolution::Resolver {
                may_cast: crate::authority::CastCeiling {
                    trust: vec![],
                    audience: Audience::Public,
                },
            },
            scope: crate::authority::Scope::default(),
        };
        let e = open_engine_returning(
            returning_registry(vec![lifting_sanitizer("quarantine")], vec![deaf, classifier_cast()]),
            ReturnPolicy::Sanitized(SanitizerName::new("quarantine")),
        );
        let child = TrajectoryId::new("child");
        let mut log = spawn_family(&e, None, &child);
        log.push(child_read(&child, known(SUSPICIOUS, Audience::Public)));
        let log = with_unresolved_fetch(&e, log, &child);
        let asked = e
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
            .expect("the unresolved dimension asks for its capable cast");
        assert!(matches!(
            &asked.follow_up,
            FollowUp::Child(ChildFollowUp::Resolve(EvidenceRequest::Cast { cast, body, .. }))
                if cast.as_str() == "classifier" && body.as_str() == "page"
        ));
    }

    #[test]
    fn an_unshaped_fork_under_a_child_attest_binding_rejects_the_return() {
        let e = open_engine_returning(
            returning_registry(vec![lifting_sanitizer("attest-schema")], vec![]),
            ReturnPolicy::Sanitized(SanitizerName::new("attest-schema")),
        );
        let child = TrajectoryId::new("child");
        let mut log = spawn_family(&e, None, &child);
        log.push(child_read(&child, known(SUSPICIOUS, Audience::Public)));
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
}
