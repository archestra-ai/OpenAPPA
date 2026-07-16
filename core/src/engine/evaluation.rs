use tracing::debug;

use crate::contract::{Fixability, Violation};
use crate::dimension::Effects;
use crate::plan::NonEmptyVec;
use crate::request::{EmissionRequest, ToolRequest};
use crate::revision::ActionId;
use crate::turn::Trajectory;
use crate::value::ValueLabel;

use super::PolicyEngine;
use super::capability::{BlockReason, Emitted, ExecutionToken, FlowOutcome, FlowRefusal};
use super::planning::SimFlow;

impl PolicyEngine {
    /// Evaluate one requested tool flow against exactly its dependencies.
    #[tracing::instrument(level = "debug", skip_all, fields(tool = %request.tool))]
    pub fn evaluate(
        &self,
        trajectory: &mut Trajectory,
        request: ToolRequest,
    ) -> Result<FlowOutcome<ExecutionToken>, FlowRefusal> {
        self.freeze();
        let (checked_request, existing_action) = match trajectory.pending_action() {
            Some(pending)
                if *pending.original() == request && pending.state() == crate::request::ActionState::Released =>
            {
                debug!(action = %pending.id(), "refused (action already released, dispatch in flight)");
                return Err(FlowRefusal::ActionAlreadyPending { pending: pending.id() });
            }
            Some(pending) if *pending.original() == request => {
                debug!(action = %pending.id(), "re-entry: reusing pending action");
                (pending.current().clone(), Some(pending.id()))
            }
            Some(pending) => {
                debug!(pending = %pending.id(), "refused (another action already pending)");
                return Err(FlowRefusal::ActionAlreadyPending { pending: pending.id() });
            }
            None => (request.clone(), None),
        };

        let contract = self.contracts.get(&checked_request.tool);
        let sim = match SimFlow::of(trajectory, &checked_request, contract) {
            Ok(sim) => sim,
            Err(unknown) => {
                debug!(value = %unknown.id, "refused (unknown value referenced)");
                return Err(FlowRefusal::UnknownValueReferenced { value: unknown.id });
            }
        };
        debug!(has_contract = contract.is_some(), flow = %sim.flow_label(), "contract lookup");
        let intrinsic = contract
            .map(|c| c.output_label.clone())
            .unwrap_or_else(ValueLabel::unknown);
        let proposed_effects = sim.proposed_effects.clone();
        let violations = sim.violations(None);

        if violations.is_empty() {
            debug!("allowed (no violations)");
            return Ok(self.permit(
                trajectory,
                existing_action,
                request,
                checked_request,
                intrinsic,
                proposed_effects,
            ));
        }
        debug!(violations = ?violations, "triaging violations");

        if violations.iter().any(|v| v.fixability() == Fixability::Structural) {
            debug!("blocked (structural fix required)");
            return Ok(self.terminal(trajectory, violations, BlockReason::RequiresStructuralFix));
        }

        let action = match existing_action {
            Some(action) => action,
            None => trajectory.set_pending(request, proposed_effects),
        };
        let pending = trajectory.pending_action().expect("pending action set above");
        let flow = pending.flow();
        let drafts = self.plan_frontier(trajectory, &checked_request, contract, pending);
        match NonEmptyVec::from_vec(trajectory.store_plans(flow, Some(action), self.id, drafts)) {
            Some(plans) => {
                debug!(count = plans.len(), "blocked (remediable)");
                Ok(FlowOutcome::Remediable { violations, plans })
            }
            None => {
                debug!("blocked (no remedy)");
                Ok(self.terminal(trajectory, violations, BlockReason::NoRemedy))
            }
        }
    }

    /// Evaluate one assistant emission through the same pipeline as any tool
    /// sink, under the reserved sink name
    /// [`RESPONSE_SINK`](super::capability::RESPONSE_SINK) and the registered
    /// [`ResponsePolicy`](super::ResponsePolicy). Core never infers that a
    /// turn is "final": the caller proposes an emission whenever assistant
    /// output is about to cross the mediation boundary.
    #[tracing::instrument(level = "debug", skip_all)]
    pub fn evaluate_emission(
        &self,
        trajectory: &mut Trajectory,
        request: EmissionRequest,
    ) -> Result<FlowOutcome<Emitted>, FlowRefusal> {
        self.freeze();
        let (checked, existing_flow) = match trajectory.pending_emission() {
            Some(pending) if *pending.original() == request => {
                debug!(flow = %pending.flow(), "re-entry: reusing pending emission");
                (pending.current().clone(), Some(pending.flow()))
            }
            Some(pending) => {
                debug!(pending = %pending.flow(), "refused (another emission already pending)");
                return Err(FlowRefusal::EmissionAlreadyPending { flow: pending.flow() });
            }
            None => {
                if request.basis != trajectory.revision() {
                    debug!(composed_at = %request.basis, current = %trajectory.revision(), "refused (stale basis)");
                    return Err(FlowRefusal::StaleBasis {
                        composed_at: request.basis,
                        current: trajectory.revision(),
                    });
                }
                (request.clone(), None)
            }
        };

        let sim = match SimFlow::of_emission(trajectory, &checked, self.response_policy.as_ref()) {
            Ok(sim) => sim,
            Err(unknown) => {
                debug!(value = %unknown.id, "refused (unknown value referenced)");
                return Err(FlowRefusal::UnknownValueReferenced { value: unknown.id });
            }
        };
        debug!(has_policy = self.response_policy.is_some(), flow = %sim.flow_label(), "emission check");
        let violations = sim.violations(None);

        if violations.is_empty() {
            let (value, rendered) = trajectory
                .emit_response(&checked.body, checked.control.clone())
                .expect("emission dependencies were validated by the flow simulation above");
            debug!(%value, "emitted");
            return Ok(FlowOutcome::AllowedNow(Emitted { value, rendered }));
        }
        debug!(violations = ?violations, "triaging emission violations");

        if violations.iter().any(|v| v.fixability() == Fixability::Structural) {
            debug!("emission blocked (structural fix required)");
            return Ok(self.terminal_emission(trajectory, violations, BlockReason::RequiresStructuralFix));
        }

        let flow = match existing_flow {
            Some(flow) => flow,
            None => trajectory.set_pending_emission(request),
        };
        let drafts = self.emission_plan_frontier(trajectory, &checked, flow);
        match NonEmptyVec::from_vec(trajectory.store_plans(flow, None, self.id, drafts)) {
            Some(plans) => {
                debug!(count = plans.len(), "emission blocked (remediable)");
                Ok(FlowOutcome::Remediable { violations, plans })
            }
            None => {
                debug!("emission blocked (no remedy)");
                Ok(self.terminal_emission(trajectory, violations, BlockReason::NoRemedy))
            }
        }
    }

    /// Mint the execution token, storing the pending action first if this is
    /// a fresh proposal. Minting happens after every mutation, so the token
    /// is bound to the trajectory's final revision.
    pub(super) fn permit(
        &self,
        trajectory: &mut Trajectory,
        existing_action: Option<ActionId>,
        original: ToolRequest,
        checked_request: ToolRequest,
        intrinsic: ValueLabel,
        proposed_effects: Effects,
    ) -> FlowOutcome<ExecutionToken> {
        let action = match existing_action {
            Some(action) => action,
            None => trajectory.set_pending(original, proposed_effects.clone()),
        };
        FlowOutcome::AllowedNow(ExecutionToken {
            action,
            tool: checked_request.tool.clone(),
            intrinsic,
            arguments: checked_request.arguments.leaves(),
            control: checked_request.control,
            proposed_effects,
            trajectory: trajectory.id(),
            revision: trajectory.revision(),
        })
    }

    /// A terminal block clears the pending action slot: the flow cannot
    /// proceed, so holding the action open would only wedge the trajectory.
    pub(super) fn terminal<P>(
        &self,
        trajectory: &mut Trajectory,
        violations: Vec<Violation>,
        reason: BlockReason,
    ) -> FlowOutcome<P> {
        trajectory.clear_pending();
        FlowOutcome::Terminal { violations, reason }
    }

    /// A terminal emission block clears the pending emission slot — and only
    /// that slot: a blocked emission never clears a pending tool action.
    pub(super) fn terminal_emission<P>(
        &self,
        trajectory: &mut Trajectory,
        violations: Vec<Violation>,
        reason: BlockReason,
    ) -> FlowOutcome<P> {
        trajectory.clear_pending_emission();
        FlowOutcome::Terminal { violations, reason }
    }
}
