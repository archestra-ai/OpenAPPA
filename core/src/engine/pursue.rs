//! Drive one requested flow to a settled outcome: evaluate, then walk the
//! engine's first remedy plan step by step until the flow permits, blocks
//! terminally, needs an external ruling, or stalls.

use tracing::debug;

use super::PolicyEngine;
use super::capability::{Blocked, Decision, ExecutionToken, StepOutcome, StepRefused, TerminalBlock};
use crate::approval::PendingApproval;
use crate::audit::TransitionFailure;
use crate::contract::Violation;
use crate::request::ToolRequest;
use crate::turn::Trajectory;

/// How a pursuit settled. A stalled pursuit leaves no pending action behind;
/// a `NeedsApproval` pursuit deliberately keeps the slot — the held
/// [`PendingApproval`] re-enters through [`PolicyEngine::apply_approval`],
/// which requires that same action.
#[derive(Debug, PartialEq, Eq)]
#[must_use = "a dropped Pursuit loses the execution token or the pending approval"]
pub enum Pursuit {
    Permitted(ExecutionToken),
    Terminal(TerminalBlock),
    NeedsApproval(PendingApproval),
    Stalled {
        violations: Vec<Violation>,
        cause: StallCause,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum StallCause {
    BoundExhausted,
    Refused(StepRefused),
    Failed(TransitionFailure),
}

impl PolicyEngine {
    /// Evaluate `request` and walk the first remedy plan until the flow
    /// permits, blocks terminally, defers to an external authority, or
    /// stalls — applying at most `max_steps` steps. The bound is checked
    /// before each step, never after: a permit produced by the final
    /// allowed step is still returned.
    pub fn pursue(&self, trajectory: &mut Trajectory, request: ToolRequest, max_steps: usize) -> Pursuit {
        let mut decision = self.evaluate(trajectory, request);
        let mut steps = 0;
        loop {
            let (violations, plans) = match decision {
                Decision::Permitted(token) => return Pursuit::Permitted(token),
                Decision::Blocked(Blocked::Terminal(block)) => return Pursuit::Terminal(block),
                Decision::Blocked(Blocked::Remediable { violations, plans }) => (violations, plans),
            };
            if steps >= max_steps {
                debug!(steps, "pursuit stalled: step bound exhausted");
                trajectory.abandon_pending();
                return Pursuit::Stalled {
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
                    trajectory.abandon_pending();
                    return Pursuit::Stalled {
                        violations,
                        cause: StallCause::Refused(refused),
                    };
                }
            };
            match self.apply_step(trajectory, capability) {
                Ok(StepOutcome::Advanced(next)) => decision = next,
                Ok(StepOutcome::NeedsApproval(pending)) => return Pursuit::NeedsApproval(pending),
                Ok(StepOutcome::Failed(failure)) => {
                    debug!(%plan, "pursuit stalled: transition failed");
                    trajectory.abandon_pending();
                    return Pursuit::Stalled {
                        violations,
                        cause: StallCause::Failed(failure),
                    };
                }
                Err(refused) => {
                    debug!(%plan, "pursuit stalled: step refused at apply");
                    trajectory.abandon_pending();
                    return Pursuit::Stalled {
                        violations,
                        cause: StallCause::Refused(refused),
                    };
                }
            }
        }
    }
}
