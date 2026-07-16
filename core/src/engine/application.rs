use tracing::debug;

use crate::approval::{AncestrySnapshot, AuthorityMode, PendingApproval, Ruling, TrajectoryView};
use crate::audit::{AuditEvent, AuthorityName};
use crate::contract::Violation;
use crate::dimension::Effects;
use crate::remedy::{
    Authorization, AuthorizationScope, DeltaCoordinate, LabelRaise, Lift, PlannedRemedy, ReductionTarget,
};
use crate::revision::{FlowId, PlanId, ValueId};
use crate::turn::{ReductionSite, Trajectory};
use crate::value::ValueLabel;

use super::PolicyEngine;
use super::capability::{BlockReason, Emitted, FlowOutcome, FlowPermit, StepCapability, StepOutcome, StepRefused};
use super::planning::SimFlow;

/// The result of routing a grant through the competent authorities: the first
/// resolving inline ruling, a deferral to an external authority, or no ruling
/// at all (every competent authority was inline and abstained).
pub(super) enum RoutedRuling {
    Approved(AuthorityName),
    Denied { authority: AuthorityName, reason: String },
    External(AuthorityName),
    NoRuling,
}

enum RoutedStep {
    Approved {
        authority: AuthorityName,
        resolved: Vec<Violation>,
    },
    NeedsApproval(PendingApproval),
    Terminal(FlowOutcome<FlowPermit>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlowKind {
    Action,
    Emission,
}

impl FlowKind {
    fn site(self) -> ReductionSite {
        match self {
            Self::Action => ReductionSite::Action,
            Self::Emission => ReductionSite::Emission,
        }
    }
}

fn stored_plan(trajectory: &Trajectory, plan: PlanId) -> Result<&crate::plan::RemedyPlan, StepRefused> {
    trajectory
        .plans()
        .iter()
        .find(|p| p.id == plan)
        .ok_or(StepRefused::UnknownPlan { plan })
}

fn pending_flow_kind(trajectory: &Trajectory, flow: FlowId) -> Result<FlowKind, StepRefused> {
    match (trajectory.pending_action(), trajectory.pending_emission()) {
        (Some(pending), _) if pending.flow() == flow => Ok(FlowKind::Action),
        (_, Some(pending)) if pending.flow() == flow => Ok(FlowKind::Emission),
        _ => Err(StepRefused::FlowNotPending { flow }),
    }
}

fn lift_of(ask: &Authorization) -> Lift {
    let mut lift = Lift::empty();
    for coordinate in ask.delta().coordinates() {
        match coordinate {
            DeltaCoordinate::ExceptPriorEffects(effects) => lift.prior_effects = Some(effects.clone()),
            DeltaCoordinate::StandInConfirmation => lift.confirms = true,
            DeltaCoordinate::ReleaseControl(deps) => lift.control_release = deps.clone(),
            DeltaCoordinate::RaiseLabel(_)
            | DeltaCoordinate::AcquireEffects(_)
            | DeltaCoordinate::AcknowledgeUnknown(_) => {}
        }
    }
    lift
}

fn raise_of(ask: &Authorization) -> Option<LabelRaise> {
    ask.delta().coordinates().find_map(|coordinate| match coordinate {
        DeltaCoordinate::RaiseLabel(raise) => Some(raise.clone()),
        _ => None,
    })
}

fn acquisition_of(ask: &Authorization) -> Option<Effects> {
    ask.delta().coordinates().find_map(|coordinate| match coordinate {
        DeltaCoordinate::AcquireEffects(effects) => Some(effects.clone()),
        _ => None,
    })
}

impl PolicyEngine {
    /// Mint the linear capability for one stored plan step. Pure — binding
    /// happens against the current revision; any later state change stales
    /// the capability.
    pub fn mint_step(&self, trajectory: &Trajectory, plan: PlanId, step: usize) -> Result<StepCapability, StepRefused> {
        let stored = stored_plan(trajectory, plan)?;
        if stored.basis != trajectory.revision() {
            return Err(StepRefused::StalePlan {
                basis: stored.basis,
                current: trajectory.revision(),
            });
        }
        if stored.engine != self.id {
            return Err(StepRefused::ForeignEngine {
                minted_by: stored.engine,
                this: self.id,
            });
        }
        stored.steps.get(step).ok_or(StepRefused::NoSuchStep { plan, step })?;
        if step != 0 {
            return Err(StepRefused::NotNextStep { step });
        }
        pending_flow_kind(trajectory, stored.flow)?;
        Ok(StepCapability {
            plan,
            step,
            flow: stored.flow,
            trajectory: trajectory.id(),
            revision: trajectory.revision(),
            engine: self.id,
        })
    }

    /// Consume a step capability and apply its remedy. Binding failures
    /// (foreign trajectory, stale revision) refuse without touching state;
    /// reduction failures are audited and advance the revision, staling
    /// every sibling capability and plan. On success the original flow is
    /// re-evaluated — allowing, re-planning with fresh predictions, or
    /// blocking terminally.
    #[tracing::instrument(level = "debug", skip_all, fields(plan = %capability.plan, step = capability.step))]
    pub fn apply_step(
        &self,
        trajectory: &mut Trajectory,
        capability: StepCapability,
    ) -> Result<StepOutcome, StepRefused> {
        if capability.engine != self.id {
            return Err(StepRefused::ForeignEngine {
                minted_by: capability.engine,
                this: self.id,
            });
        }
        if capability.trajectory != trajectory.id() {
            return Err(StepRefused::ForeignTrajectory {
                minted_for: capability.trajectory,
                this: trajectory.id(),
            });
        }
        if capability.revision != trajectory.revision() {
            return Err(StepRefused::StalePlan {
                basis: capability.revision,
                current: trajectory.revision(),
            });
        }
        let stored = stored_plan(trajectory, capability.plan)?;
        let step = stored
            .steps
            .get(capability.step)
            .ok_or(StepRefused::NoSuchStep {
                plan: capability.plan,
                step: capability.step,
            })?
            .clone();
        if capability.step != 0 {
            return Err(StepRefused::NotNextStep { step: capability.step });
        }
        let kind = pending_flow_kind(trajectory, capability.flow)?;

        match step {
            PlannedRemedy::Reduce(ReductionTarget::DeriveValue { source, transformer }) => {
                let registered = self
                    .transformers
                    .iter()
                    .find(|t| t.descriptor.transformer == transformer)
                    .expect("plans reference only registered transformers");
                let source_value = trajectory.value(source).expect("plans reference only admitted values");
                if let Err(failure) = registered.accepts(&source_value) {
                    trajectory.fail_transform(
                        source,
                        registered.descriptor.transformer.clone(),
                        registered.descriptor.output.clone(),
                        failure.clone(),
                    );
                    return Ok(StepOutcome::Failed(failure));
                }
                let body = match (registered.run)(source_value.body()) {
                    Ok(body) => body,
                    Err(error) => {
                        let failure = crate::audit::TransitionFailure::TransformerError { message: error.message };
                        trajectory.fail_transform(
                            source,
                            registered.descriptor.transformer.clone(),
                            registered.descriptor.output.clone(),
                            failure.clone(),
                        );
                        return Ok(StepOutcome::Failed(failure));
                    }
                };
                trajectory.apply_transform(
                    source,
                    registered.descriptor.transformer.clone(),
                    registered.descriptor.output.clone(),
                    body,
                    kind.site(),
                );
                Ok(StepOutcome::Advanced(self.recheck(trajectory, kind)))
            }
            PlannedRemedy::Reduce(ReductionTarget::NarrowAction { transition }) => {
                debug_assert_eq!(kind, FlowKind::Action, "narrowing is enumerated only for tool flows");
                let registered = self
                    .action_transitions
                    .iter()
                    .find(|t| t.id == transition)
                    .expect("plans reference only registered action transitions");
                let pending = trajectory
                    .pending_action()
                    .expect("a tool flow's pending action was resolved above");
                let checked = pending.current().clone();
                let sim = SimFlow::of(trajectory, &checked, self.contracts.get(&checked.tool))
                    .expect("pending action dependencies stay admitted");
                match self.constrain_gate(&sim, registered, &checked.arguments, trajectory.store()) {
                    Ok(_) => {}
                    Err(failure) => {
                        trajectory.record_event(AuditEvent::StepFailed {
                            plan: capability.plan,
                            step: capability.step as u64,
                            failure: failure.clone(),
                        });
                        return Ok(StepOutcome::Failed(failure));
                    }
                }
                trajectory.apply_constraint(registered.to_tool.clone(), registered.effects.clone());
                Ok(StepOutcome::Advanced(self.recheck(trajectory, kind)))
            }
            PlannedRemedy::Authorize {
                authorization, targets, ..
            } => {
                Ok(
                    match self.route_step_grant(trajectory, &capability, kind, authorization.clone(), targets) {
                        RoutedStep::Approved { authority, resolved } => match &authorization.scope() {
                            AuthorizationScope::DerivedValue { source } => {
                                let raise =
                                    raise_of(&authorization).expect("a derived-value authorization carries a raise");
                                StepOutcome::Advanced(self.endorse_permit(trajectory, *source, raise, authority, kind))
                            }
                            AuthorizationScope::PendingAction { .. } => {
                                debug_assert_eq!(
                                    kind,
                                    FlowKind::Action,
                                    "acquisition is enumerated only for tool flows"
                                );
                                let effects = acquisition_of(&authorization)
                                    .expect("an action-scoped authorization carries an acquisition");
                                StepOutcome::Advanced(self.accept_permit(trajectory, effects, authority, resolved))
                            }
                            AuthorizationScope::PolicyCheck { .. } => StepOutcome::Advanced(self.lift_permit(
                                trajectory,
                                kind,
                                lift_of(&authorization),
                                authorization.clone(),
                                authority,
                                resolved,
                            )),
                        },
                        RoutedStep::NeedsApproval(pending) => StepOutcome::NeedsApproval(pending),
                        RoutedStep::Terminal(outcome) => StepOutcome::Advanced(outcome),
                    },
                )
            }
        }
    }

    fn recheck(&self, trajectory: &mut Trajectory, kind: FlowKind) -> FlowOutcome<FlowPermit> {
        match kind {
            FlowKind::Action => {
                let original = trajectory
                    .pending_action()
                    .expect("the applied remedy's action stays pending")
                    .original()
                    .clone();
                self.evaluate(trajectory, original)
                    .expect("re-entry of the pending action is never a refusal")
                    .map_allowed(FlowPermit::Execute)
            }
            FlowKind::Emission => {
                let original = trajectory
                    .pending_emission()
                    .expect("the applied remedy's emission stays pending")
                    .original()
                    .clone();
                self.evaluate_emission(trajectory, original)
                    .expect("re-entry of the pending emission is never a refusal")
                    .map_allowed(FlowPermit::Emit)
            }
        }
    }

    fn terminal_for(
        &self,
        trajectory: &mut Trajectory,
        kind: FlowKind,
        violations: Vec<Violation>,
        reason: BlockReason,
    ) -> FlowOutcome<FlowPermit> {
        match kind {
            FlowKind::Action => self.terminal(trajectory, violations, reason),
            FlowKind::Emission => self.terminal_emission(trajectory, violations, reason),
        }
    }

    fn route_step_grant(
        &self,
        trajectory: &mut Trajectory,
        capability: &StepCapability,
        kind: FlowKind,
        grant: Authorization,
        resolved: Vec<Violation>,
    ) -> RoutedStep {
        let routed = {
            let view = TrajectoryView::new(trajectory.view());
            self.route_grant(&grant, &resolved, &view)
        };
        match routed {
            RoutedRuling::Approved(authority) => RoutedStep::Approved { authority, resolved },
            RoutedRuling::Denied { authority, reason } => {
                trajectory.record_denied_authorization(grant.clone(), authority.clone(), reason.clone());
                RoutedStep::Terminal(self.terminal_for(
                    trajectory,
                    kind,
                    resolved,
                    BlockReason::DeniedByAuthority { authority, reason },
                ))
            }
            RoutedRuling::External(authority) => {
                trajectory.record_event(AuditEvent::ApprovalRequested {
                    plan: capability.plan,
                    authority: authority.clone(),
                    resolved: resolved.clone(),
                });
                let basis = self.flow_basis(trajectory, kind);
                let ancestry = AncestrySnapshot::of(trajectory.view(), basis);
                RoutedStep::NeedsApproval(PendingApproval::new(
                    capability.plan,
                    capability.flow,
                    grant,
                    authority,
                    resolved,
                    ancestry,
                    trajectory.id(),
                    trajectory.revision(),
                    self.id,
                ))
            }
            RoutedRuling::NoRuling => {
                RoutedStep::Terminal(self.terminal_for(trajectory, kind, resolved, BlockReason::NoAuthorityRuled))
            }
        }
    }

    fn flow_basis(&self, trajectory: &Trajectory, kind: FlowKind) -> Vec<ValueId> {
        match kind {
            FlowKind::Action => {
                let checked = trajectory
                    .pending_action()
                    .expect("a tool flow's pending action was resolved by the caller")
                    .current();
                checked
                    .arguments
                    .leaves()
                    .into_iter()
                    .chain(checked.control.iter().copied())
                    .collect()
            }
            FlowKind::Emission => {
                let checked = trajectory
                    .pending_emission()
                    .expect("an emission flow's pending emission was resolved by the caller")
                    .current();
                checked
                    .body
                    .leaves()
                    .into_iter()
                    .chain(checked.control.iter().copied())
                    .collect()
            }
        }
    }

    /// Consult competent authorities for `grant` in routing order and return
    /// the first resolving ruling. Inline authorities decide synchronously;
    /// an abstention (`None`) falls through to the next competent authority.
    pub(super) fn route_grant(
        &self,
        grant: &Authorization,
        resolved: &[Violation],
        view: &TrajectoryView,
    ) -> RoutedRuling {
        for authority in self.competent_authorities(grant) {
            match &authority.mode {
                AuthorityMode::Inline(decide) => match decide(grant, resolved, view) {
                    Some(Ruling::Approve { .. }) => return RoutedRuling::Approved(authority.name.clone()),
                    Some(Ruling::Deny { reason }) => {
                        return RoutedRuling::Denied {
                            authority: authority.name.clone(),
                            reason,
                        };
                    }
                    None => continue,
                },
                AuthorityMode::External => return RoutedRuling::External(authority.name.clone()),
            }
        }
        RoutedRuling::NoRuling
    }

    /// Consume a pending approval with the authority's ruling. Binding
    /// failures refuse without touching state. A denial is audited and
    /// blocks terminally; an approval advances the granted authorization's
    /// state machine and rechecks the flow fail-closed.
    pub fn apply_approval(
        &self,
        trajectory: &mut Trajectory,
        pending: PendingApproval,
        ruling: Ruling,
    ) -> Result<FlowOutcome<FlowPermit>, StepRefused> {
        let parts = pending.into_parts();
        if parts.engine != self.id {
            return Err(StepRefused::ForeignEngine {
                minted_by: parts.engine,
                this: self.id,
            });
        }
        if parts.trajectory != trajectory.id() {
            return Err(StepRefused::ForeignTrajectory {
                minted_for: parts.trajectory,
                this: trajectory.id(),
            });
        }
        if parts.revision != trajectory.revision() {
            return Err(StepRefused::StalePlan {
                basis: parts.revision,
                current: trajectory.revision(),
            });
        }
        let kind = pending_flow_kind(trajectory, parts.flow)?;
        match ruling {
            Ruling::Approve { .. } => match &parts.grant.scope() {
                AuthorizationScope::DerivedValue { source } => {
                    let raise = raise_of(&parts.grant).expect("a derived-value grant carries a raise");
                    Ok(self.endorse_permit(trajectory, *source, raise, parts.authority, kind))
                }
                AuthorizationScope::PendingAction { .. } => {
                    debug_assert_eq!(kind, FlowKind::Action, "acquisition is enumerated only for tool flows");
                    let effects = acquisition_of(&parts.grant).expect("an action-scoped grant carries an acquisition");
                    Ok(self.accept_permit(trajectory, effects, parts.authority, parts.resolved))
                }
                AuthorizationScope::PolicyCheck { .. } => {
                    let lift = lift_of(&parts.grant);
                    Ok(self.lift_permit(trajectory, kind, lift, parts.grant, parts.authority, parts.resolved))
                }
            },
            Ruling::Deny { reason } => {
                trajectory.record_denied_authorization(parts.grant.clone(), parts.authority.clone(), reason.clone());
                Ok(self.terminal_for(
                    trajectory,
                    kind,
                    parts.resolved,
                    BlockReason::DeniedByAuthority {
                        authority: parts.authority,
                        reason,
                    },
                ))
            }
        }
    }

    fn lift_permit(
        &self,
        trajectory: &mut Trajectory,
        kind: FlowKind,
        delta: Lift,
        authorization: Authorization,
        authority: AuthorityName,
        resolved: Vec<Violation>,
    ) -> FlowOutcome<FlowPermit> {
        match kind {
            FlowKind::Action => {
                let pending = trajectory
                    .pending_action()
                    .expect("a tool flow's pending action was resolved by the caller");
                let action = pending.id();
                let checked = pending.current().clone();
                let original = pending.original().clone();
                // The pending action's proposed effects are the single source of truth
                // for what release commits — never re-derive them from the contract
                // (a constrain or an Accept→Waive sequence would be silently undone).
                let proposed_effects = pending.proposed_effects().clone();
                let contract = self.contracts.get(&checked.tool);
                let sim =
                    SimFlow::of(trajectory, &checked, contract).expect("pending action dependencies stay admitted");
                let remaining = sim.violations(Some(&delta));
                if !remaining.is_empty() {
                    debug!("lift did not clear its targeted checks, failing closed");
                    return self.terminal(trajectory, remaining, BlockReason::PostconditionFailed);
                }
                let transition = trajectory.mint_transition();
                trajectory.record_applied_authorization(transition, authorization, authority, resolved);
                let intrinsic = match contract {
                    Some(c) => c.output_label.clone(),
                    None => ValueLabel::unknown(),
                };
                self.permit(trajectory, Some(action), original, checked, intrinsic, proposed_effects)
                    .map_allowed(FlowPermit::Execute)
            }
            FlowKind::Emission => {
                let checked = trajectory
                    .pending_emission()
                    .expect("an emission flow's pending emission was resolved by the caller")
                    .current()
                    .clone();
                let sim = SimFlow::of_emission(trajectory, &checked, self.response_policy.as_ref())
                    .expect("pending emission dependencies stay admitted");
                let remaining = sim.violations(Some(&delta));
                if !remaining.is_empty() {
                    debug!("lift did not clear its targeted checks, failing closed");
                    return self.terminal_emission(trajectory, remaining, BlockReason::PostconditionFailed);
                }
                let transition = trajectory.mint_transition();
                trajectory.record_applied_authorization(transition, authorization, authority, resolved);
                let (value, rendered) = trajectory
                    .emit_response(&checked.body, checked.control)
                    .expect("pending emission dependencies stay admitted");
                FlowOutcome::AllowedNow(FlowPermit::Emit(Emitted { value, rendered }))
            }
        }
    }

    fn accept_permit(
        &self,
        trajectory: &mut Trajectory,
        effects: Effects,
        authority: AuthorityName,
        resolved: Vec<Violation>,
    ) -> FlowOutcome<FlowPermit> {
        let pending = trajectory
            .pending_action()
            .expect("caller validated the pending action");
        let checked = pending.current().clone();
        let contract = self.contracts.get(&checked.tool);
        let mut after = SimFlow::of(trajectory, &checked, contract).expect("pending action dependencies stay admitted");
        after.accepted_effects = after.accepted_effects.clone().combine(effects.clone());
        let remaining = after.violations(None);
        if remaining
            .iter()
            .any(|v| matches!(v, Violation::Breach(crate::contract::Breach::SurfaceGrowth { .. })))
        {
            debug!("acceptance did not clear the surface growth, failing closed");
            return self.terminal(trajectory, remaining, BlockReason::PostconditionFailed);
        }
        let acquired: Vec<Violation> = resolved
            .into_iter()
            .filter(|v| matches!(v, Violation::Breach(crate::contract::Breach::SurfaceGrowth { .. })))
            .collect();
        trajectory.accept_growth(effects, authority, acquired);
        self.recheck(trajectory, FlowKind::Action)
    }

    fn endorse_permit(
        &self,
        trajectory: &mut Trajectory,
        source: ValueId,
        delta: LabelRaise,
        authority: AuthorityName,
        kind: FlowKind,
    ) -> FlowOutcome<FlowPermit> {
        let raised = {
            let source_label = trajectory.label(source).expect("plans reference only admitted values");
            delta.raise(source_label)
        };
        trajectory.endorse_value(source, authority, delta, raised, kind.site());
        self.recheck(trajectory, kind)
    }
}
