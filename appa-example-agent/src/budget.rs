//! What one run may spend. The budget is the agent's, not the
//! runtime's: it bounds cost and recursion, and it decides nothing
//! about a flow.

use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_inference_rounds: u32,
    /// Rounds held back from tool-using execution so the root can finish
    /// without tools when an execution ceiling is reached.
    pub finalization_rounds: u32,
    /// Tool calls answered across the whole run. One completion can
    /// propose any number of them, so the round ceiling does not bound
    /// this: without it a single response could keep the loop dispatching
    /// until the deadline.
    pub max_tool_calls: u32,
    pub run_deadline: Duration,
    pub max_forks: u32,
    pub max_fork_depth: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_inference_rounds: 24,
            finalization_rounds: 1,
            max_tool_calls: 64,
            run_deadline: Duration::from_secs(240),
            max_forks: 4,
            max_fork_depth: 2,
        }
    }
}

pub(crate) struct RunBudget {
    limits: Limits,
    started: Instant,
    rounds: u32,
    calls: u32,
    forks: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ForkUnavailable {
    DepthLimit,
    RunLimit,
}

impl RunBudget {
    pub(crate) fn new(limits: Limits) -> Self {
        RunBudget {
            limits,
            started: Instant::now(),
            rounds: 0,
            calls: 0,
            forks: 0,
        }
    }

    /// Charge one tool-using provider call while preserving the configured
    /// finalization reserve.
    pub(crate) fn charge_inference(&mut self) -> Result<(), Exhausted> {
        let reserve = self.finalization_reserve();
        if self.rounds >= self.limits.max_inference_rounds.saturating_sub(reserve) {
            return Err(Exhausted);
        }
        self.rounds += 1;
        Ok(())
    }

    /// Charge the final tool-free completion. It may spend the reserve but
    /// can never exceed the run's total inference ceiling.
    pub(crate) fn charge_finalization(&mut self) -> Result<(), Exhausted> {
        if self.rounds >= self.limits.max_inference_rounds || self.finalization_reserve() == 0 {
            return Err(Exhausted);
        }
        self.rounds += 1;
        Ok(())
    }

    fn finalization_reserve(&self) -> u32 {
        self.limits.finalization_rounds.min(self.limits.max_inference_rounds)
    }

    /// Charge one tool call, before it is checked. `Err` means the
    /// call ceiling is reached and nothing may be proposed.
    pub(crate) fn charge_tool_call(&mut self) -> Result<(), Exhausted> {
        if self.calls >= self.limits.max_tool_calls {
            return Err(Exhausted);
        }
        self.calls += 1;
        Ok(())
    }

    /// Charge one child opened from `depth`. Both ceilings answer here,
    /// so no caller can consult one and forget the other; a refusal
    /// spends nothing.
    pub(crate) fn charge_fork(&mut self, depth: u32) -> Result<(), ForkUnavailable> {
        self.fork_availability(depth)?;
        self.forks += 1;
        Ok(())
    }

    /// Whether the next inference at `depth` may open a child. The model's
    /// catalogue uses the same predicate as the eventual charge, so a spent
    /// spawn is not advertised as an action that can only fail.
    pub(crate) fn fork_availability(&self, depth: u32) -> Result<(), ForkUnavailable> {
        if depth >= self.limits.max_fork_depth {
            Err(ForkUnavailable::DepthLimit)
        } else if self.forks >= self.limits.max_forks {
            Err(ForkUnavailable::RunLimit)
        } else {
            Ok(())
        }
    }

    /// How many children this run has opened. Also the child ids'
    /// discriminator, which is why it never decreases.
    pub(crate) fn forks(&self) -> u32 {
        self.forks
    }

    pub(crate) fn remaining(&self) -> Duration {
        self.limits.run_deadline.saturating_sub(self.started.elapsed())
    }

    pub(crate) fn deadline_elapsed(&self) -> bool {
        self.remaining().is_zero()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Exhausted;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_ceiling_stops_its_own_charge() {
        let limits = Limits {
            max_inference_rounds: 2,
            finalization_rounds: 0,
            max_tool_calls: 1,
            max_forks: 1,
            max_fork_depth: 2,
            ..Limits::default()
        };
        let mut budget = RunBudget::new(limits);
        assert_eq!(budget.fork_availability(0), Ok(()));
        assert_eq!(budget.charge_inference(), Ok(()));
        assert_eq!(budget.charge_inference(), Ok(()));
        assert_eq!(budget.charge_inference(), Err(Exhausted));

        assert_eq!(budget.charge_tool_call(), Ok(()));
        assert_eq!(
            budget.charge_tool_call(),
            Err(Exhausted),
            "one completion can propose any number of calls, so they are counted too",
        );

        assert_eq!(budget.charge_fork(0), Ok(()));
        assert_eq!(budget.fork_availability(0), Err(ForkUnavailable::RunLimit));
        assert_eq!(
            budget.charge_fork(0),
            Err(ForkUnavailable::RunLimit),
            "the count ceiling"
        );
        assert_eq!(budget.forks(), 1, "a refused charge does not consume a child id");
    }

    #[test]
    fn execution_preserves_one_round_for_finalization() {
        let mut budget = RunBudget::new(Limits {
            max_inference_rounds: 3,
            finalization_rounds: 1,
            ..Limits::default()
        });
        assert_eq!(budget.charge_inference(), Ok(()));
        assert_eq!(budget.charge_inference(), Ok(()));
        assert_eq!(budget.charge_inference(), Err(Exhausted));
        assert_eq!(budget.charge_finalization(), Ok(()));
        assert_eq!(budget.charge_finalization(), Err(Exhausted));
    }

    #[test]
    fn the_depth_ceiling_refuses_without_spending_a_child_id() {
        let mut budget = RunBudget::new(Limits {
            max_forks: 8,
            max_fork_depth: 2,
            ..Limits::default()
        });
        assert_eq!(budget.fork_availability(1), Ok(()));
        assert_eq!(budget.fork_availability(2), Err(ForkUnavailable::DepthLimit));
        assert_eq!(budget.charge_fork(1), Ok(()));
        assert_eq!(
            budget.charge_fork(2),
            Err(ForkUnavailable::DepthLimit),
            "the ceiling is exclusive"
        );
        assert_eq!(budget.forks(), 1);
    }

    #[test]
    fn a_zero_deadline_is_elapsed_from_the_start() {
        let budget = RunBudget::new(Limits {
            run_deadline: Duration::ZERO,
            ..Limits::default()
        });
        assert!(budget.deadline_elapsed());
        assert!(budget.remaining().is_zero());
    }
}
