//! The engine: a pure function of the log's views and the immutable registry.

use thiserror::Error;

use crate::admit::{self, AdmitError, CastAnswer, CastError, ResultAdmission};
use crate::branch::{self, BranchError, ReturnSubmission};
use crate::check::{self, CheckOutcome, Narrowing, RawBlock};
use crate::contract::ToolContract;
use crate::execute::{self, PlanError, Ruling};
use crate::fact::{Fact, FactBatch, ObservedResult, ReturnPolicy, Revision};
use crate::label::EstablishedLabel;
use crate::names::AuthorityName;
use crate::params::{ArgumentError, CanonicalArguments};
use crate::plan::{self, PlannedBlock};
use crate::profile::{self, DeploymentPolicy, DeploymentProfile, OpenVector, PolicyDialectVersion, PolicyIdentityV1};
use crate::projection::Projection;
use crate::projection::Views;
use crate::registry::{LoadError, Registry};
use crate::transition::{
    Blocked, ChildFollowUp, ChildReport, ChildSubmission, EngineDecision, EngineEvent, EngineView, Evidence,
    EvidenceRequest, FollowUp, ForkBinding, OfferExecution, OfferFollowUp, OfferOutcome, OutcomeBody, OutcomeFollowUp,
    ProposalBatch, Released, Sequence, Settled, SettledOutcome, SpawnMark, ToolOutcome, ToolReport, TransitionError,
    TransitionRefusal, ValidatedFactBatch,
};
use crate::value::{
    CanonicalDigest, ChildReturnId, DispatchId, ForkId, LabeledValue, Provenance, RawResultDigest, ResolvedCall,
    ToolName, TrajectoryId,
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
        if views.has_ended(child) {
            let recorded = views.child_return(&ChildReturnId::new(child.clone(), 0)).cloned();
            return match (&report.submission, recorded) {
                (ChildSubmission::Void, None) => Ok(EngineDecision {
                    append: None,
                    follow_up: FollowUp::Child(ChildFollowUp::Ended),
                }),
                (ChildSubmission::Value { body }, Some(crossed)) if &crossed.body == body => Ok(EngineDecision {
                    append: None,
                    follow_up: FollowUp::Child(ChildFollowUp::Merged { admitted: crossed.body }),
                }),
                _ => Err(TransitionError::BranchEnded),
            };
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
            ChildSubmission::Value { body } => body.clone(),
        };
        match branch::check_child_return(&self.registry, &views, child).map_err(branch_refusal)? {
            branch::ReturnCheck::Allow => {
                let batch = branch::submit_child_return(
                    &self.registry,
                    &views,
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
            branch::ReturnCheck::Block(branch::ReturnBlock::Narrowing { narrowing, plans }) => Ok(EngineDecision {
                append: None,
                follow_up: FollowUp::Child(ChildFollowUp::Blocked { narrowing, plans }),
            }),
            branch::ReturnCheck::Block(branch::ReturnBlock::Unestablished(facts)) => Ok(EngineDecision {
                append: None,
                follow_up: FollowUp::Child(ChildFollowUp::Unresolved(facts)),
            }),
        }
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

        let admission = match &report.outcome {
            ToolOutcome::Failure => ResultAdmission::Failure,
            ToolOutcome::Indeterminate => ResultAdmission::Indeterminate,
            ToolOutcome::Success {
                body: OutcomeBody::Unavailable,
            } => ResultAdmission::SuccessNoValue,
            ToolOutcome::Success {
                body: OutcomeBody::Available(raw),
            } => {
                let raw_digest = RawResultDigest::of(raw.as_str().as_bytes());
                match views.bound_sanitizer(dispatch) {
                    None => {
                        if self
                            .registry
                            .tool(call.tool())
                            .is_some_and(|contract| contract.pending_cast_dim().is_some())
                        {
                            return Err(TransitionError::ConfinedResult);
                        }
                        ResultAdmission::SuccessRaw { body: raw.clone() }
                    }
                    Some(sanitizer) => {
                        let derived = report.evidence.iter().find_map(|evidence| match evidence {
                            Evidence::Sanitizer {
                                sanitizer: named,
                                source,
                                derived,
                            } if named == sanitizer && source == &raw_digest => Some(derived.clone()),
                            Evidence::Sanitizer { .. } => None,
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
                                    sanitizer: sanitizer.clone(),
                                    source: raw_digest,
                                    body: raw.clone(),
                                })),
                            });
                        };
                        ResultAdmission::SuccessSanitized {
                            body: derived,
                            sanitizer: sanitizer.clone(),
                            raw_digest,
                        }
                    }
                }
            }
        };
        let batch = self
            .admit_result(&views, dispatch, &call, admission)
            .map_err(|error| match error {
                AdmitError::SanitizerTransitionUnmet | AdmitError::SanitizerBindingMismatch => {
                    TransitionError::SanitizerUnapplicable
                }
                AdmitError::OutputPendingCast => TransitionError::ConfinedResult,
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
            &batch.trajectory,
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
            let CheckOutcome::Block(raw) = check::evaluate(contract, &final_views, call) else {
                unreachable!("an in-batch release only ever adds gaps to a refused sibling's block")
            };
            let planned = plan::plan(&self.registry, &final_views, call, &raw);
            let subject = crate::basis::SubjectKey::Call {
                trajectory: batch.trajectory.clone(),
                batch: batch.id.clone(),
                position: position as u32,
            };
            let (block_id, offers, opened_offers) =
                self.open_offers(&final_views, &advance, &batch.offer_nonce, &subject, call, &planned);
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
        batch
            .proposals
            .iter()
            .enumerate()
            .map(|(position, proposed)| {
                self.resolve_call(proposed.tool.clone(), &proposed.arguments)
                    .map(|call| call.with_dynamic_resolutions(proposed.dynamic_resolutions.clone()))
                    .map_err(|error| (position, error))
            })
            .collect()
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

    fn open_offers(
        &self,
        views: &Views,
        advance: &crate::basis::BasisAdvance,
        nonce: &crate::value::OfferNonce,
        subject: &crate::basis::SubjectKey,
        call: &ResolvedCall,
        planned: &PlannedBlock,
    ) -> (
        crate::value::BlockId,
        Vec<(crate::value::OfferId, plan::PlanId)>,
        Vec<Fact>,
    ) {
        let crate::basis::SubjectKey::Call {
            trajectory,
            batch,
            position,
        } = subject
        else {
            unreachable!("offers open against a call candidate")
        };
        let digest = call.digest();
        let block_id = crate::value::BlockId::of_proposal(nonce, trajectory, batch, *position, &digest);
        let basis = views.basis_after(advance, subject);
        let mut ids = Vec::new();
        let mut facts = Vec::new();
        for (index, executable) in planned
            .plans
            .iter()
            .filter_map(plan::RemedyPlan::executable)
            .enumerate()
        {
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
                batch: batch.clone(),
                call: digest,
                subject: subject.clone(),
                plan: executable.clone(),
                basis,
            });
        }
        (block_id, ids, facts)
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
                        outcome: match views.is_open(dispatch) {
                            true => SettledOutcome::Confined,
                            false => SettledOutcome::Closed {
                                admitted: views.admitted_body(dispatch).cloned(),
                            },
                        },
                    });
                }
                None => match check::evaluate(contract, views, call) {
                    CheckOutcome::Block(raw) => {
                        let subject = crate::basis::SubjectKey::Call {
                            trajectory: batch.trajectory.clone(),
                            batch: batch.id.clone(),
                            position: position as u32,
                        };
                        let (block_id, offers) = views.pending_block(&subject).unwrap_or_else(|| {
                            let block_id = crate::value::BlockId::of_proposal(
                                &batch.offer_nonce,
                                &batch.trajectory,
                                &batch.id,
                                position as u32,
                                &call.digest(),
                            );
                            (block_id, Vec::new())
                        });
                        blocked.push(Blocked {
                            call: call.clone(),
                            block: plan::plan(&self.registry, views, call, &raw),
                            block_id,
                            offers,
                        });
                    }
                    CheckOutcome::Allow => spent.push(call.clone()),
                },
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
        let call = self.offer_call(&views, &recorded);
        let contract = self.validated_contract(&call)?;
        let live = match check::evaluate(contract, &views, &call) {
            CheckOutcome::Block(raw) => plan::plan(&self.registry, &views, &call, &raw)
                .plans
                .iter()
                .filter_map(plan::RemedyPlan::executable)
                .any(|offered| offered == &recorded.plan)
                .then_some(raw),
            // The block is gone: whatever the agent would have remedied, nothing needs it now.
            CheckOutcome::Allow => None,
        };
        let Some(raw) = live else {
            let batch = FactBatch::new(
                views.revision(),
                vec![Fact::OfferInvalidated {
                    trajectory: recorded.trajectory.clone(),
                    offer: execution.offer,
                }],
            );
            return Ok(EngineDecision {
                append: Some(self.seal(view, batch)?),
                follow_up: FollowUp::Offer(OfferFollowUp::Invalidated),
            });
        };
        match &execution.outcome {
            OfferOutcome::Approved(evidence) => {
                self.approve_offer(view, &views, execution, &recorded, contract, &raw, &call, evidence)
            }
            OfferOutcome::Denied { authority } => {
                self.deny_offer(view, &views, execution, &recorded, &call, &raw, authority)
            }
        }
    }

    fn offer_call(&self, views: &Views, recorded: &crate::projection::RecordedOffer) -> ResolvedCall {
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
        let follow_up = match (end, &execution.outcome) {
            (OfferEnd::Accepted, OfferOutcome::Approved(_)) => OfferFollowUp::Approved {
                call: views
                    .approval(&execution.offer)
                    .ok_or(TransitionRefusal::UnpreparedCallApproval)?
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
            _ => return Err(TransitionError::TerminalOffer),
        };
        Ok(EngineDecision {
            append: None,
            follow_up: FollowUp::Offer(follow_up),
        })
    }

    fn reblocked(
        &self,
        views: &Views,
        recorded: &crate::projection::RecordedOffer,
        execution: &OfferExecution,
    ) -> Result<Option<Blocked>, TransitionError> {
        let call = self.offer_call(views, recorded);
        let contract = self.validated_contract(&call)?;
        let CheckOutcome::Block(raw) = check::evaluate(contract, views, &call) else {
            return Ok(None);
        };
        let (block_id, offers) = views
            .pending_block(&recorded.subject)
            .unwrap_or((offer_block(recorded, execution, &call), Vec::new()));
        Ok(Some(Blocked {
            call: call.clone(),
            block: plan::plan(&self.registry, views, &call, &raw),
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
        let siblings = views
            .pending_block(&recorded.subject)
            .map(|(_, offers)| offers)
            .unwrap_or_default();
        facts.extend(
            siblings
                .into_iter()
                .map(|(offer, _)| offer)
                .filter(|offer| offer != &execution.offer)
                .map(|offer| Fact::OfferInvalidated {
                    trajectory: trajectory.clone(),
                    offer,
                }),
        );
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
        let planned = plan::plan(&self.registry, &after, call, raw);
        let (block_id, offers, opened) = self.open_offers(
            &after,
            &advance,
            &execution.offer_nonce,
            &recorded.subject,
            call,
            &planned,
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
        Ok(check::evaluate(contract, views, call))
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
        match check::evaluate(contract, views, call) {
            CheckOutcome::Allow => {
                let (_, fact) = opened_dispatch(contract, views, call);
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

    /// The narrowing admitting a cast-resolved value of `call` would fold into the live
    /// established bound, or `None` when it does not move it — the whole resolved label,
    /// established dimensions included (see `admit::pending_cast_narrowing`). The runtime derives
    /// the acceptance offer from this; admission re-derives it under the family lock, so a stale
    /// offer refuses by value (D2).
    pub fn cast_narrowing(
        &self,
        views: &Views,
        call: &ResolvedCall,
        resolved: &EstablishedLabel,
    ) -> Result<Option<Narrowing>, EngineError> {
        self.validated_contract(call)?;
        Ok(admit::pending_cast_narrowing(views, resolved))
    }

    /// Attach the sound remedies to a raw block: executable plans and prose recommendations. An empty
    /// result (no plans, no curative recommendation) is a proof the block is unliftable over the
    /// implemented remedy subset — see [`crate::plan`].
    pub fn plan(&self, views: &Views, call: &ResolvedCall, raw: &RawBlock) -> Result<PlannedBlock, EngineError> {
        self.validated_contract(call)?;
        Ok(plan::plan(&self.registry, views, call, raw))
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

    /// The child fold's unestablished facts — what a cast must establish before this child's
    /// return can merge. Policy-independent: the runtime drives resolution *before*
    /// the return-policy split, so raw and sanitizer-bound returns resolve alike.
    pub fn child_fold_unestablished(&self, parent: &Views, child: &TrajectoryId) -> Vec<check::UnestablishedFact> {
        branch::child_fold_unestablished(parent, child)
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

fn advance_of(engine: &Engine, view: &EngineView, batch: &FactBatch) -> crate::basis::BasisAdvance {
    Sequence::advance_of(&engine.registry, &engine.child_return, view, &batch.facts)
}

fn approved_release(
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

fn branch_refusal(error: BranchError) -> TransitionError {
    match error {
        BranchError::NotDirectParent | BranchError::NotForked => TransitionError::NotForked,
        BranchError::AlreadyEnded => TransitionError::BranchEnded,
        BranchError::ReturnPolicyMismatch => TransitionError::BoundReturnSanitizer,
        other => unreachable!("the child-return boundary refuses before reaching {other}"),
    }
}

/// Build the `DispatchOpened` fact for a call: its proposed committed label, the effects it would
/// commit on success, and its occurrence (a repeat identical call is a new dispatch). Shared by the
/// clean-allow path ([`Engine::open_dispatch`]) and atomic plan execution ([`crate::execute`]).
pub(crate) fn opened_dispatch(contract: &ToolContract, views: &Views, call: &ResolvedCall) -> (DispatchId, Fact) {
    let digest = call.digest();
    let occurrence = views.dispatch_count(&digest);
    let dispatch = DispatchId::new(views.trajectory().clone(), digest, occurrence);
    let fact = Fact::DispatchOpened {
        trajectory: views.trajectory().clone(),
        dispatch: dispatch.clone(),
        tool: call.tool().clone(),
        arguments: call.canonical_arguments().clone(),
        proposed_label: check::committed_label_for_call(contract, &views.current_label(), call)
            .bound()
            .clone(),
        proposed_effects: contract.emits.clone(),
        dynamic_resolutions: call.dynamic_resolutions().to_vec(),
    };
    (dispatch, fact)
}

pub(crate) struct SiblingRelease {
    pub(crate) dispatch: DispatchId,
    pub(crate) consumes: Option<crate::value::OfferId>,
    pub(crate) prepares_fork: Option<ForkId>,
    pub(crate) facts: Vec<Fact>,
}

/// The ordered in-batch composition, position by position: what each proposed sibling
/// does, and the records that say so. `None` at a position is a refusal.
pub(crate) fn compose_batch<'a>(
    registry: &Registry,
    child_return: &ReturnPolicy,
    working: &mut std::borrow::Cow<'a, Projection>,
    trajectory: &TrajectoryId,
    proposals: &[ResolvedCall],
    spawn: Option<SpawnMark>,
    approval: &impl Fn(&Views, &ResolvedCall) -> Option<crate::value::OfferId>,
) -> Result<Vec<Option<SiblingRelease>>, (usize, EngineError)> {
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
            let consumes = match check::evaluate(contract, &views, call) {
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
            let (dispatch, opening) = opened_dispatch(contract, &views, call);
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
                facts.extend(approved_release(trajectory, &dispatch, &prepared));
            }
            facts.push(opening);
            let prepares_fork = (spawn == Some(SpawnMark::at(position))).then(|| {
                let fork = ForkId::of(&dispatch);
                facts.push(Fact::ForkPrepared {
                    trajectory: trajectory.clone(),
                    fork: fork.clone(),
                    snapshot: views.freeze_basis(),
                    return_policy: child_return.clone(),
                });
                fork
            });
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

    fn open_engine(cfg: RegistryConfig) -> Engine {
        let profile = crate::profile::covering_declaration(&cfg);
        Engine::open(DeploymentPolicy {
            registry: cfg,
            planner_cap: crate::registry::PlannerCap::default(),
            dialect: PolicyDialectVersion::new(1),
            child_return: ReturnPolicy::Raw,
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
        assert_eq!(&appended.facts()[2..], composed.facts.as_slice());
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
                proposed_effects: crate::fact::EffectSet::default(),
                dynamic_resolutions: Vec::new(),
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
            transition: crate::authority::Transition::Trust {
                from_floor: SUSPICIOUS,
                to: TRUSTED,
            },
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
                Fact::OutputSanitizerApplied { .. },
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
                })
            ),
            Err(crate::transition::TransitionError::ContradictedSuccess)
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
        let report = |submission: ChildSubmission| {
            EngineEvent::ChildReturn(ChildReport {
                child: child.clone(),
                submission,
            })
        };
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
                EngineEvent::ChildReturn(ChildReport {
                    child: TrajectoryId::new("stranger"),
                    submission: ChildSubmission::Void,
                })
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
                EngineEvent::ChildReturn(ChildReport {
                    child: child.clone(),
                    submission: ChildSubmission::Value {
                        body: ValueBody::new("what I found"),
                    },
                }),
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
            .handle(
                &view,
                EngineEvent::ChildReturn(ChildReport {
                    child: child.clone(),
                    submission: ChildSubmission::Void,
                }),
            )
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
            crate::branch::ReturnCheck::Block(crate::branch::ReturnBlock::Narrowing { plans, .. }) => plans
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
        use crate::authority::{Authority, Mandate, Scope};
        let officer = |name: &str| Authority {
            name: AuthorityName::new(name),
            mandate: Mandate {
                trust_ceiling: Some(TRUSTED),
                ..Mandate::default()
            },
            scope: Scope::default(),
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
            Err(TransitionRefusal::UnpreparedCallApproval)
        );
        assert_eq!(
            e.view(&traj(), stopped.clone(), Revision::new(stopped.len() as u64))
                .err(),
            Some(TransitionRefusal::UnpreparedCallApproval)
        );

        let mut deferred = approval.clone();
        deferred.insert(position, user_value(known(TRUSTED, Audience::Public)));
        assert_eq!(
            e.validate_replay(&[opening.clone(), deferred].concat()),
            Err(TransitionRefusal::UnpreparedCallApproval)
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
            Err(TransitionRefusal::UnpreparedCallApproval)
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
            Err(TransitionRefusal::UnpreparedCallApproval)
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
            Err(TransitionError::Invalid(TransitionRefusal::UnpreparedCallApproval))
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
                proposed_effects: EffectSet::default(),
                dynamic_resolutions: Vec::new(),
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

    #[test]
    fn every_composed_cast_admission_replays() {
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
        let paranoid = crate::authority::Cast {
            name: crate::names::CastName::new("paranoid"),
            resolution: crate::authority::CastResolution::Resolver {
                may_cast: crate::authority::CastCeiling {
                    trust: vec![SUSPICIOUS, TRUSTED],
                    audience: Audience::Public,
                },
            },
            scope: crate::authority::Scope { tags: vec![] },
        };
        let e = open_engine(RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![scan],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![paranoid],
        });
        let call = call("scan_inbox", json!({}));
        let body = ValueBody::new("inbox");
        let cast = crate::names::CastName::new("paranoid");

        let mut open_log = vec![user_value(known(TRUSTED, Audience::Public))];
        let dispatch = open(&e, &mut open_log, &call);
        let admit = |admission: crate::admit::ResultAdmission| {
            let projection = Projection::build(&open_log, Revision::new(open_log.len() as u64));
            let batch = e
                .admit_result(&projection.view(&traj()), &dispatch, &call, admission)
                .expect("the admission is legal");
            [open_log.clone(), batch.facts].concat()
        };
        assert_eq!(
            e.validate_replay(&admit(crate::admit::ResultAdmission::SuccessCast {
                body: body.clone(),
                cast: cast.clone(),
                resolved: EstablishedLabel::new(TRUSTED, Audience::Public),
            })),
            Ok(())
        );
        let narrowing = e
            .cast_narrowing(
                &Projection::build(&open_log, Revision::new(open_log.len() as u64)).view(&traj()),
                &call,
                &EstablishedLabel::new(SUSPICIOUS, Audience::Public),
            )
            .expect("the contract is registered")
            .expect("a lower trust narrows the trajectory");
        assert_eq!(
            e.validate_replay(&admit(crate::admit::ResultAdmission::SuccessCastAccepted {
                body: body.clone(),
                cast: cast.clone(),
                resolved: EstablishedLabel::new(SUSPICIOUS, Audience::Public),
                accepted: narrowing,
            })),
            Ok(())
        );
        assert_eq!(
            e.validate_replay(&admit(crate::admit::ResultAdmission::SuccessCastLapsed {
                body,
                cast,
                resolved: EstablishedLabel::new(SUSPICIOUS, Audience::Public),
            })),
            Ok(())
        );
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
            crate::branch::ReturnCheck::Block(crate::branch::ReturnBlock::Narrowing { plans, .. }) => plans
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
                snapshot: crate::fact::ForkSnapshot::freeze(EstablishedLabel::top(), std::iter::empty()),
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
                    proposed_effects: crate::fact::EffectSet::default(),
                    dynamic_resolutions: Vec::new(),
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
        use crate::authority::{Authority, Mandate, Scope};
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
            scope: Scope::default(),
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
            proposed_effects: EffectSet::default(),
            dynamic_resolutions: vec![],
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
        use crate::authority::{Authority, Mandate, Scope};
        use crate::names::AuthorityName;
        let mut wire = strict_tool("wire");
        wire.requires.label.trust_floor = Some(TRUSTED);
        let officer = Authority {
            name: AuthorityName::new("officer"),
            mandate: Mandate {
                trust_ceiling: Some(TRUSTED),
                ..Mandate::default()
            },
            scope: Scope::default(),
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
}
