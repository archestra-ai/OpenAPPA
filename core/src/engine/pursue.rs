//! Drive one requested flow to a settled outcome: evaluate, then walk the
//! frontier's first remedy plan step by step until the flow is allowed,
//! blocks terminally, needs an external ruling, or stalls.

use tracing::debug;

use super::PolicyEngine;
use super::capability::{
    BlockReason, Emitted, ExecutionToken, FlowOutcome, FlowPermit, FlowRefusal, StepOutcome, StepRefused,
};
use crate::approval::PendingApproval;
use crate::audit::TransitionFailure;
use crate::contract::Violation;
use crate::request::{EmissionRequest, ToolRequest};
use crate::turn::Trajectory;

/// How a tool-flow pursuit settled. A stalled pursuit leaves no pending
/// action behind; a `NeedsApproval` pursuit deliberately keeps the slot —
/// the held [`PendingApproval`] re-enters through
/// [`PolicyEngine::apply_approval`], which requires that same flow.
#[derive(Debug, PartialEq, Eq)]
#[must_use = "a dropped Pursuit loses the execution token or the pending approval"]
pub enum Pursuit {
    Permitted(ExecutionToken),
    Terminal {
        violations: Vec<Violation>,
        reason: BlockReason,
    },
    NeedsApproval(PendingApproval),
    Stalled {
        violations: Vec<Violation>,
        cause: StallCause,
    },
    Refused(FlowRefusal),
}

/// How an emission pursuit settled. Mirrors [`Pursuit`] with the emission
/// permit payload: an allowed emission was already emitted atomically.
#[derive(Debug, PartialEq, Eq)]
#[must_use = "a dropped EmissionPursuit loses the emitted bytes or the pending approval"]
pub enum EmissionPursuit {
    Emitted(Emitted),
    Terminal {
        violations: Vec<Violation>,
        reason: BlockReason,
    },
    NeedsApproval(PendingApproval),
    Stalled {
        violations: Vec<Violation>,
        cause: StallCause,
    },
    Refused(FlowRefusal),
}

#[derive(Debug, PartialEq, Eq)]
pub enum StallCause {
    BoundExhausted,
    Refused(StepRefused),
    Failed(TransitionFailure),
}

enum Driven {
    Allowed(FlowPermit),
    Terminal {
        violations: Vec<Violation>,
        reason: BlockReason,
    },
    NeedsApproval(PendingApproval),
    Stalled {
        violations: Vec<Violation>,
        cause: StallCause,
    },
}

impl PolicyEngine {
    /// Evaluate `request` and walk the first remedy plan until the flow is
    /// allowed, blocks terminally, defers to an external authority, or
    /// stalls — applying at most `max_steps` steps. The bound is checked
    /// before each step, never after: a permit produced by the final
    /// allowed step is still returned.
    pub fn pursue(&self, trajectory: &mut Trajectory, request: ToolRequest, max_steps: usize) -> Pursuit {
        let first = match self.evaluate(trajectory, request) {
            Ok(outcome) => outcome.map_allowed(FlowPermit::Execute),
            Err(refusal) => return Pursuit::Refused(refusal),
        };
        let abandon = |trajectory: &mut Trajectory| {
            trajectory
                .abandon_pending()
                .expect("a stalled pursuit abandons an open action");
        };
        match self.drive(trajectory, first, max_steps, abandon) {
            Driven::Allowed(FlowPermit::Execute(token)) => Pursuit::Permitted(token),
            Driven::Allowed(FlowPermit::Emit(_)) => {
                unreachable!("a tool flow settles in an execution permit")
            }
            Driven::Terminal { violations, reason } => Pursuit::Terminal { violations, reason },
            Driven::NeedsApproval(pending) => Pursuit::NeedsApproval(pending),
            Driven::Stalled { violations, cause } => Pursuit::Stalled { violations, cause },
        }
    }

    /// Evaluate an emission and walk the first remedy plan, exactly like
    /// [`PolicyEngine::pursue`] over the emission sink.
    pub fn pursue_emission(
        &self,
        trajectory: &mut Trajectory,
        request: EmissionRequest,
        max_steps: usize,
    ) -> EmissionPursuit {
        let first = match self.evaluate_emission(trajectory, request) {
            Ok(outcome) => outcome.map_allowed(FlowPermit::Emit),
            Err(refusal) => return EmissionPursuit::Refused(refusal),
        };
        match self.drive(trajectory, first, max_steps, Trajectory::abandon_pending_emission) {
            Driven::Allowed(FlowPermit::Emit(emitted)) => EmissionPursuit::Emitted(emitted),
            Driven::Allowed(FlowPermit::Execute(_)) => {
                unreachable!("an emission flow settles in an emitted response")
            }
            Driven::Terminal { violations, reason } => EmissionPursuit::Terminal { violations, reason },
            Driven::NeedsApproval(pending) => EmissionPursuit::NeedsApproval(pending),
            Driven::Stalled { violations, cause } => EmissionPursuit::Stalled { violations, cause },
        }
    }

    fn drive(
        &self,
        trajectory: &mut Trajectory,
        first: FlowOutcome<FlowPermit>,
        max_steps: usize,
        abandon: fn(&mut Trajectory),
    ) -> Driven {
        let mut outcome = first;
        let mut steps = 0;
        loop {
            let (violations, plans) = match outcome {
                FlowOutcome::AllowedNow(permit) => return Driven::Allowed(permit),
                FlowOutcome::Terminal { violations, reason } => return Driven::Terminal { violations, reason },
                FlowOutcome::Remediable { violations, plans } => (violations, plans),
            };
            if steps >= max_steps {
                debug!(steps, "pursuit stalled: step bound exhausted");
                abandon(trajectory);
                return Driven::Stalled {
                    violations,
                    cause: StallCause::BoundExhausted,
                };
            }
            steps += 1;
            let plan = plans.first().id;
            let capability = match self.mint_step(trajectory, plan, 0) {
                Ok(capability) => capability,
                Err(refused) => {
                    debug!(%plan, "pursuit stalled: step refused at mint");
                    abandon(trajectory);
                    return Driven::Stalled {
                        violations,
                        cause: StallCause::Refused(refused),
                    };
                }
            };
            match self.apply_step(trajectory, capability) {
                Ok(StepOutcome::Advanced(next)) => outcome = next,
                Ok(StepOutcome::NeedsApproval(pending)) => return Driven::NeedsApproval(pending),
                Ok(StepOutcome::Failed(failure)) => {
                    debug!(%plan, "pursuit stalled: reduction failed");
                    abandon(trajectory);
                    return Driven::Stalled {
                        violations,
                        cause: StallCause::Failed(failure),
                    };
                }
                Err(refused) => {
                    debug!(%plan, "pursuit stalled: step refused at apply");
                    abandon(trajectory);
                    return Driven::Stalled {
                        violations,
                        cause: StallCause::Refused(refused),
                    };
                }
            }
        }
    }
}
