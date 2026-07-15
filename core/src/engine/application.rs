use std::collections::BTreeSet;

use tracing::debug;

use crate::approval::{AncestrySnapshot, AuthorityMode, PendingApproval, Ruling, TrajectoryView};
use crate::audit::{AuditEvent, AuthorityName};
use crate::contract::{Fixability, Violation};
use crate::dimension::Effects;
use crate::plan::{Justification, TransitionKind};
use crate::request::ToolRequest;
use crate::revision::{ActionId, PlanId, ValueId};
use crate::transition::{ActionTransition, ProposedGrant, TransientWaiver};
use crate::turn::Trajectory;
use crate::value::ValueLabel;

use super::PolicyEngine;
use super::capability::{BlockReason, Decision, StepCapability, StepOutcome, StepRefused, ToolContract};
use super::planning::{SimFlow, grant_for};

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
    Terminal(Decision),
}

fn stored_plan(trajectory: &Trajectory, plan: PlanId) -> Result<&crate::plan::RemedyPlan, StepRefused> {
    trajectory
        .plans()
        .iter()
        .find(|p| p.id == plan)
        .ok_or(StepRefused::UnknownPlan { plan })
}

fn pending_action_is(trajectory: &Trajectory, action: ActionId) -> Result<&crate::request::PendingAction, StepRefused> {
    match trajectory.pending_action() {
        Some(pending) if pending.id() == action => Ok(pending),
        _ => Err(StepRefused::ActionNotPending { action }),
    }
}

fn denial_event(grant: &ProposedGrant, authority: AuthorityName, reason: String) -> AuditEvent {
    match grant {
        ProposedGrant::Accept { .. } => AuditEvent::AcceptDenied { authority, reason },
        ProposedGrant::Endorse { .. } => AuditEvent::EndorseDenied { authority, reason },
        ProposedGrant::Waive { .. } | ProposedGrant::Acknowledge { .. } => {
            AuditEvent::WaiverDenied { authority, reason }
        }
    }
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
        pending_action_is(trajectory, stored.action)?;
        Ok(StepCapability {
            plan,
            step,
            action: stored.action,
            trajectory: trajectory.id(),
            revision: trajectory.revision(),
            engine: self.id,
        })
    }

    /// Consume a step capability and apply its transition. Binding failures
    /// (foreign trajectory, stale revision) refuse without touching state;
    /// transition failures are audited and advance the revision, staling
    /// every sibling capability and plan. On success the original flow is
    /// re-evaluated — permitting, re-planning with fresh predictions, or
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
        let spec = stored
            .steps
            .get(capability.step)
            .ok_or(StepRefused::NoSuchStep {
                plan: capability.plan,
                step: capability.step,
            })?
            .clone();
        let pending = pending_action_is(trajectory, capability.action)?;
        let checked = pending.current().clone();
        let original = pending.original().clone();
        let contract = self.contracts.get(&checked.tool);
        let sim = SimFlow::of(trajectory, &checked, contract).expect("pending action dependencies stay admitted");

        if sim.violations(None) != spec.precondition.remaining {
            debug!("step refused (precondition posture no longer holds)");
            trajectory.record_event(AuditEvent::StepFailed {
                plan: capability.plan,
                step: capability.step as u64,
                failure: crate::audit::TransitionFailure::PreconditionMismatch,
            });
            return Ok(StepOutcome::Failed(
                crate::audit::TransitionFailure::PreconditionMismatch,
            ));
        }

        match spec.kind.clone() {
            TransitionKind::Derive {
                source,
                justification: Justification::Content(transformer),
            } => {
                let registered = self
                    .transformers
                    .iter()
                    .find(|t| t.descriptor.transformer == transformer)
                    .expect("plans reference only registered transformers");
                let source_value = trajectory
                    .store()
                    .get(source)
                    .expect("plans reference only admitted values");
                if let Err(failure) = registered.accepts(source_value) {
                    trajectory.fail_transform(
                        source,
                        registered.descriptor.transformer.clone(),
                        registered.descriptor.output.clone(),
                        failure.clone(),
                    );
                    return Ok(StepOutcome::Failed(failure));
                }
                let mut after = sim.clone();
                after.leaf_labels.insert(source, registered.descriptor.output.clone());
                if after.violations(None) != spec.postcondition.remaining {
                    let failure = crate::audit::TransitionFailure::PostconditionMismatch;
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
                );
                Ok(StepOutcome::Advanced(self.evaluate(trajectory, original)))
            }
            TransitionKind::ConstrainAction { transition } => {
                let registered = self
                    .action_transitions
                    .iter()
                    .find(|t| t.id == transition)
                    .expect("plans reference only registered action transitions");
                let pending = trajectory.pending_action().expect("validated above");
                let fail = |trajectory: &mut Trajectory, failure: crate::audit::TransitionFailure| {
                    trajectory.record_event(AuditEvent::StepFailed {
                        plan: capability.plan,
                        step: capability.step as u64,
                        failure: failure.clone(),
                    });
                    Ok(StepOutcome::Failed(failure))
                };
                let (target, recipients) =
                    match self.constrain_gate(registered, pending, &checked, trajectory.store(), &sim.recipients) {
                        Ok(gate) => gate,
                        Err(failure) => return fail(trajectory, failure),
                    };
                let mut after = sim.clone();
                after.tool = registered.to_tool.clone();
                after.adopt_requires(&target.requires);
                after.recipients = recipients;
                after.proposed_effects = registered.effects.clone();
                if after.violations(None) != spec.postcondition.remaining {
                    return fail(trajectory, crate::audit::TransitionFailure::PostconditionMismatch);
                }
                trajectory.apply_constraint(registered.to_tool.clone(), registered.effects.clone());
                Ok(StepOutcome::Advanced(self.evaluate(trajectory, original)))
            }
            TransitionKind::ApplyWaiver { delta } => {
                let grant = grant_for(&delta, &spec.precondition.remaining);
                Ok(
                    match self.route_step_grant(trajectory, &capability, &checked, grant, spec.precondition.remaining) {
                        RoutedStep::Approved { authority, resolved } => StepOutcome::Advanced(self.waiver_permit(
                            trajectory,
                            capability.action,
                            delta,
                            authority,
                            resolved,
                        )),
                        RoutedStep::NeedsApproval(pending) => StepOutcome::NeedsApproval(pending),
                        RoutedStep::Terminal(decision) => StepOutcome::Advanced(decision),
                    },
                )
            }
            TransitionKind::AcceptGrowth { effects } => {
                let grant = ProposedGrant::Accept {
                    effects: effects.clone(),
                };
                Ok(
                    match self.route_step_grant(trajectory, &capability, &checked, grant, spec.precondition.remaining) {
                        RoutedStep::Approved { authority, resolved } => StepOutcome::Advanced(
                            self.accept_permit(trajectory, effects, authority, resolved, original),
                        ),
                        RoutedStep::NeedsApproval(pending) => StepOutcome::NeedsApproval(pending),
                        RoutedStep::Terminal(decision) => StepOutcome::Advanced(decision),
                    },
                )
            }
            TransitionKind::Derive {
                source,
                justification: Justification::Fiat { delta, targets },
            } => {
                let grant = ProposedGrant::Endorse {
                    source,
                    delta: delta.clone(),
                };
                Ok(
                    match self.route_step_grant(trajectory, &capability, &checked, grant, targets) {
                        RoutedStep::Approved { authority, .. } => {
                            StepOutcome::Advanced(self.endorse_permit(trajectory, source, delta, authority, original))
                        }
                        RoutedStep::NeedsApproval(pending) => StepOutcome::NeedsApproval(pending),
                        RoutedStep::Terminal(decision) => StepOutcome::Advanced(decision),
                    },
                )
            }
        }
    }

    fn route_step_grant(
        &self,
        trajectory: &mut Trajectory,
        capability: &StepCapability,
        checked: &ToolRequest,
        grant: ProposedGrant,
        resolved: Vec<Violation>,
    ) -> RoutedStep {
        let routed = {
            let view = TrajectoryView::new(trajectory.store());
            self.route_grant(&grant, &resolved, &view)
        };
        match routed {
            RoutedRuling::Approved(authority) => RoutedStep::Approved { authority, resolved },
            RoutedRuling::Denied { authority, reason } => {
                trajectory.record_event(denial_event(&grant, authority.clone(), reason.clone()));
                RoutedStep::Terminal(self.terminal(
                    trajectory,
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
                let basis = checked
                    .arguments
                    .leaves()
                    .into_iter()
                    .chain(checked.control.iter().copied());
                let ancestry = AncestrySnapshot::of(trajectory.store(), basis);
                RoutedStep::NeedsApproval(PendingApproval::new(
                    capability.plan,
                    capability.action,
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
                RoutedStep::Terminal(self.terminal(trajectory, resolved, BlockReason::NoAuthorityRuled))
            }
        }
    }

    /// Consult competent authorities for `grant` in routing order and return
    /// the first resolving ruling. Inline authorities decide synchronously;
    /// an abstention (`None`) falls through to the next competent authority.
    pub(super) fn route_grant(
        &self,
        grant: &ProposedGrant,
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
    /// blocks terminally; an approval rechecks the flow fail-closed under
    /// the waiver and mints the execution token.
    pub fn apply_approval(
        &self,
        trajectory: &mut Trajectory,
        pending: PendingApproval,
        ruling: Ruling,
    ) -> Result<Decision, StepRefused> {
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
        pending_action_is(trajectory, parts.action)?;
        match ruling {
            Ruling::Approve { .. } => match parts.grant {
                ProposedGrant::Endorse { source, delta } => {
                    let original = trajectory
                        .pending_action()
                        .expect("validated pending above")
                        .original()
                        .clone();
                    Ok(self.endorse_permit(trajectory, source, delta, parts.authority, original))
                }
                ProposedGrant::Waive { waiver, .. } => {
                    Ok(self.waiver_permit(trajectory, parts.action, waiver, parts.authority, parts.resolved))
                }
                ProposedGrant::Acknowledge { .. } => Ok(self.waiver_permit(
                    trajectory,
                    parts.action,
                    TransientWaiver::empty(),
                    parts.authority,
                    parts.resolved,
                )),
                ProposedGrant::Accept { effects } => {
                    let original = trajectory
                        .pending_action()
                        .expect("validated pending above")
                        .original()
                        .clone();
                    Ok(self.accept_permit(trajectory, effects, parts.authority, parts.resolved, original))
                }
            },
            Ruling::Deny { reason } => {
                trajectory.record_event(denial_event(&parts.grant, parts.authority.clone(), reason.clone()));
                Ok(self.terminal(
                    trajectory,
                    parts.resolved,
                    BlockReason::DeniedByAuthority {
                        authority: parts.authority,
                        reason,
                    },
                ))
            }
        }
    }

    fn waiver_permit(
        &self,
        trajectory: &mut Trajectory,
        action: ActionId,
        delta: TransientWaiver,
        authority: AuthorityName,
        resolved: Vec<Violation>,
    ) -> Decision {
        let pending = trajectory
            .pending_action()
            .expect("caller validated the pending action");
        let checked = pending.current().clone();
        let original = pending.original().clone();
        // The pending action's proposed effects are the single source of truth
        // for what release commits — never re-derive them from the contract
        // (a constrain or an Accept→Waive sequence would be silently undone).
        let proposed_effects = pending.proposed_effects().clone();
        let contract = self.contracts.get(&checked.tool);
        let sim = SimFlow::of(trajectory, &checked, contract).expect("pending action dependencies stay admitted");
        let remaining = sim.violations(Some(&delta));
        if !remaining.is_empty() {
            debug!("waiver did not clear its targeted checks, failing closed");
            return self.terminal(trajectory, remaining, BlockReason::PostconditionFailed);
        }
        let mut changes = delta.kinds();
        if resolved.iter().any(|v| v.fixability() == Fixability::AcknowledgeOnly) {
            changes.insert(crate::audit::WaiverKind::Acknowledgment);
        }
        let transition = trajectory.mint_transition();
        trajectory.record_event(AuditEvent::WaiverApplied {
            transition,
            changes,
            authority,
            resolved,
        });
        let intrinsic = match contract {
            Some(c) => c.output_label.clone(),
            None => ValueLabel::unknown(),
        };
        self.permit(trajectory, Some(action), original, checked, intrinsic, proposed_effects)
    }

    fn accept_permit(
        &self,
        trajectory: &mut Trajectory,
        effects: Effects,
        authority: AuthorityName,
        resolved: Vec<Violation>,
        original: ToolRequest,
    ) -> Decision {
        let pending = trajectory
            .pending_action()
            .expect("caller validated the pending action");
        let checked = pending.current().clone();
        let contract = self.contracts.get(&checked.tool);
        let mut after = SimFlow::of(trajectory, &checked, contract).expect("pending action dependencies stay admitted");
        after.accepted_effects = after.accepted_effects.clone().combine(effects.clone());
        if after
            .violations(None)
            .iter()
            .any(|v| matches!(v, Violation::Breach(crate::contract::Breach::SurfaceGrowth { .. })))
        {
            debug!("acceptance did not clear the surface growth, failing closed");
            return self.terminal(trajectory, after.violations(None), BlockReason::PostconditionFailed);
        }
        let acquired: Vec<Violation> = resolved
            .into_iter()
            .filter(|v| matches!(v, Violation::Breach(crate::contract::Breach::SurfaceGrowth { .. })))
            .collect();
        trajectory.accept_growth(effects, authority, acquired);
        self.evaluate(trajectory, original)
    }

    fn endorse_permit(
        &self,
        trajectory: &mut Trajectory,
        source: ValueId,
        delta: crate::transition::EndorseDelta,
        authority: AuthorityName,
        original: ToolRequest,
    ) -> Decision {
        let raised = {
            let source_label = trajectory
                .store()
                .get(source)
                .expect("plans reference only admitted values")
                .label();
            delta.raise(source_label)
        };
        trajectory.endorse_value(source, authority, delta, raised);
        self.evaluate(trajectory, original)
    }

    /// The structural gate a constrain must pass, identical at planning and
    /// application: the narrowing holds, the target contract exists and
    /// declares exactly the transition's effects, and its argument schema
    /// does not widen the resolved recipient set.
    pub(super) fn constrain_gate<'a>(
        &'a self,
        transition: &ActionTransition,
        pending: &crate::request::PendingAction,
        checked: &ToolRequest,
        store: &crate::value::ValueStore,
        base_recipients: &BTreeSet<crate::dimension::UserId>,
    ) -> Result<(&'a ToolContract, BTreeSet<crate::dimension::UserId>), crate::audit::TransitionFailure> {
        transition.narrows(pending)?;
        let Some(target) = self.contracts.get(&transition.to_tool) else {
            return Err(crate::audit::TransitionFailure::PreconditionMismatch);
        };
        if target.effects != transition.effects {
            return Err(crate::audit::TransitionFailure::PreconditionMismatch);
        }
        let Ok(recipients) = target.arguments.resolve_recipients(&checked.arguments, store) else {
            return Err(crate::audit::TransitionFailure::PreconditionMismatch);
        };
        if !recipients.is_subset(base_recipients) {
            return Err(crate::audit::TransitionFailure::PreconditionMismatch);
        }
        Ok((target, recipients))
    }
}
