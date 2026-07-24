use std::sync::Arc;

use appa_runtime::store::TenantId;
use appa_runtime::wire::ChatCompletionRequest;
use appa_runtime::{
    BeginTurnError, ForkRequest, Limits, Mediator, RunBudget, Step, StopReason, TrajectoryId, Turn, TurnError,
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::provider::OpenAiCompatible;

const CANCELLED_BEFORE_BEGIN: &str = "This turn was cancelled.";
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Final(String),
    ChildFinished,
    PolicyStop(String),
}

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("the turn could not begin: {0}")]
    Begin(#[from] BeginTurnError),
    #[error("turn mediation failed: {0}")]
    Turn(#[from] TurnError),
}

pub struct Agent {
    mediator: Arc<Mediator>,
    provider: OpenAiCompatible,
    limits: Limits,
}

impl Agent {
    pub fn new(mediator: Arc<Mediator>, provider: OpenAiCompatible, limits: Limits) -> Self {
        Agent {
            mediator,
            provider,
            limits,
        }
    }

    pub fn mediator(&self) -> &Arc<Mediator> {
        &self.mediator
    }

    pub fn create_session(&self, tenant: TenantId) -> TrajectoryId {
        self.mediator.create_session(tenant)
    }

    pub async fn run_new(
        &self,
        tenant: TenantId,
        task: impl Into<String>,
        cancel: CancellationToken,
    ) -> Result<(TrajectoryId, Outcome), AgentError> {
        let session = self.create_session(tenant.clone());
        let outcome = self.run_existing(tenant, session.clone(), task, cancel).await?;
        Ok((session, outcome))
    }

    pub async fn run_existing(
        &self,
        tenant: TenantId,
        session: TrajectoryId,
        task: impl Into<String>,
        cancel: CancellationToken,
    ) -> Result<Outcome, AgentError> {
        let turn = match self
            .mediator
            .begin_turn(tenant.clone(), session.clone(), task, cancel.clone())
            .await
        {
            Ok(turn) => turn,
            Err(BeginTurnError::Cancelled) => {
                return Ok(Outcome::PolicyStop(CANCELLED_BEFORE_BEGIN.to_string()));
            }
            Err(error) => return Err(AgentError::Begin(error)),
        };
        self.drive(tenant, ActiveTurn { session, turn }, cancel).await
    }

    async fn drive(
        &self,
        tenant: TenantId,
        mut active: ActiveTurn,
        cancel: CancellationToken,
    ) -> Result<Outcome, AgentError> {
        let mut budget = RunBudget::new(self.limits);
        let mut parents = Vec::new();

        loop {
            if cancel.is_cancelled() {
                return stop_family(active, parents, StopReason::Cancelled);
            }
            if budget.charge_inference().is_err() {
                return stop_family(active, parents, StopReason::BudgetExhausted);
            }

            let request = ChatCompletionRequest {
                model: String::new(),
                messages: active.turn.transcript()?,
                tools: Some(active.turn.advertised_tools(&budget)?),
                stream: None,
            };
            let remaining = budget.remaining();
            let completion = tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    return stop_family(active, parents, StopReason::Cancelled);
                }
                result = tokio::time::timeout(remaining, self.provider.complete(request)) => {
                    match result {
                        Ok(Ok(completion)) => completion,
                        Ok(Err(_)) => return stop_family(active, parents, StopReason::InferenceFailure),
                        Err(_) => return stop_family(active, parents, StopReason::BudgetExhausted),
                    }
                }
            };

            match active.turn.mediate(completion, &mut budget).await? {
                Step::Continue => {}
                Step::Fork(request) => {
                    if cancel.is_cancelled() {
                        active.turn.fail_fork(request)?;
                        return stop_family(active, parents, StopReason::Cancelled);
                    }
                    if !budget.allows_fork_from_depth(active.turn.depth()) {
                        active.turn.fail_fork(request)?;
                        return stop_family(active, parents, StopReason::BudgetExhausted);
                    }
                    if budget.charge_fork().is_err() {
                        active.turn.fail_fork(request)?;
                        return stop_family(active, parents, StopReason::BudgetExhausted);
                    }

                    let forked = match self.mediator.fork_session_reserved(&tenant, &active.session) {
                        Ok(forked) => forked,
                        Err(_) => {
                            active.turn.fail_fork(request)?;
                            if cancel.is_cancelled() {
                                return stop_family(active, parents, StopReason::Cancelled);
                            }
                            if budget.remaining().is_zero() {
                                return stop_family(active, parents, StopReason::BudgetExhausted);
                            }
                            continue;
                        }
                    };
                    let child = forked.session().clone();
                    let child_task = request.task().to_string();
                    let child_turn =
                        match self
                            .mediator
                            .begin_forked_turn(tenant.clone(), forked, child_task, cancel.clone())
                        {
                            Ok(turn) => turn,
                            Err(error) => {
                                active.turn.fail_fork(request)?;
                                match error {
                                    BeginTurnError::Cancelled => {
                                        return stop_family(active, parents, StopReason::Cancelled);
                                    }
                                    BeginTurnError::Store(_) if budget.remaining().is_zero() => {
                                        return stop_family(active, parents, StopReason::BudgetExhausted);
                                    }
                                    BeginTurnError::Store(_) => continue,
                                    BeginTurnError::ForeignFork => {
                                        return Err(AgentError::Begin(BeginTurnError::ForeignFork));
                                    }
                                }
                            }
                        };
                    parents.push(ParentFrame { active, request });
                    active = ActiveTurn {
                        session: child,
                        turn: child_turn,
                    };
                }
                Step::Final(text) => match parents.pop() {
                    Some(mut parent) => {
                        drop(active);
                        parent.active.turn.complete_fork(parent.request)?;
                        active = parent.active;
                    }
                    None => return Ok(Outcome::Final(text)),
                },
                Step::ChildFinished => match parents.pop() {
                    Some(mut parent) => {
                        drop(active);
                        parent.active.turn.complete_fork(parent.request)?;
                        active = parent.active;
                    }
                    None => return Ok(Outcome::ChildFinished),
                },
                Step::PolicyStop(message) => {
                    let reason = if cancel.is_cancelled() {
                        StopReason::Cancelled
                    } else {
                        StopReason::BudgetExhausted
                    };
                    drop(active);
                    return unwind_parents(parents, reason, message);
                }
            }
        }
    }
}

struct ActiveTurn {
    session: TrajectoryId,
    turn: Turn,
}

struct ParentFrame {
    active: ActiveTurn,
    request: ForkRequest,
}

fn stop_family(mut active: ActiveTurn, parents: Vec<ParentFrame>, reason: StopReason) -> Result<Outcome, AgentError> {
    let message = policy_stop_message(active.turn.stop(reason)?)?;
    drop(active);
    unwind_parents(parents, reason, message)
}

fn unwind_parents(
    mut parents: Vec<ParentFrame>,
    reason: StopReason,
    mut message: String,
) -> Result<Outcome, AgentError> {
    while let Some(mut parent) = parents.pop() {
        parent.active.turn.fail_fork(parent.request)?;
        message = policy_stop_message(parent.active.turn.stop(reason)?)?;
    }
    Ok(Outcome::PolicyStop(message))
}

fn policy_stop_message(step: Step) -> Result<String, AgentError> {
    match step {
        Step::PolicyStop(message) => Ok(message),
        Step::Continue | Step::Fork(_) | Step::Final(_) | Step::ChildFinished => {
            unreachable!("Turn::stop always returns a policy stop")
        }
    }
}
