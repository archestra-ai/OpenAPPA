//! Compatibility turn entry point over [`appa_agent::Agent`].

use appa_runtime::TrajectoryId;
use appa_runtime::store::{StoreError, TenantId};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::admission::UserTurn;
use crate::runtime::Runtime;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnOutcome {
    Final(String),
    PolicyStop(String),
}

#[derive(Debug, Error)]
pub enum DriveError {
    #[error("session store fault: {0}")]
    Store(#[from] StoreError),
    #[error("dispatch identity no longer matches its call/trajectory")]
    DispatchIdentity,
    #[error("this session already ended its errand — an ended child is closed to new turns")]
    SessionEnded,
}

/// Drive an existing session through the canonical agent and runtime mediation loop.
pub async fn drive_turn(
    runtime: &Runtime,
    tenant: &TenantId,
    session: &TrajectoryId,
    _is_child: bool,
    user_turn: UserTurn,
    cancel: CancellationToken,
) -> Result<TurnOutcome, DriveError> {
    let outcome = runtime
        .agent()
        .run_existing(tenant.clone(), session.clone(), user_turn.into_string(), cancel)
        .await
        .map_err(map_agent_error)?;
    Ok(match outcome {
        appa_agent::Outcome::Final(text) => TurnOutcome::Final(text),
        appa_agent::Outcome::ChildFinished => TurnOutcome::Final(String::new()),
        appa_agent::Outcome::PolicyStop(text) => TurnOutcome::PolicyStop(text),
    })
}

fn map_agent_error(error: appa_agent::AgentError) -> DriveError {
    match error {
        appa_agent::AgentError::Begin(appa_runtime::BeginTurnError::Store(error))
        | appa_agent::AgentError::Turn(appa_runtime::TurnError::Store(error)) => DriveError::Store(error),
        appa_agent::AgentError::Begin(
            appa_runtime::BeginTurnError::Cancelled | appa_runtime::BeginTurnError::ForeignFork,
        ) => DriveError::DispatchIdentity,
        appa_agent::AgentError::Begin(appa_runtime::BeginTurnError::SessionEnded) => DriveError::SessionEnded,
        appa_agent::AgentError::Turn(
            appa_runtime::TurnError::DispatchIdentity
            | appa_runtime::TurnError::Lifecycle { .. }
            | appa_runtime::TurnError::ForkIdentity,
        ) => DriveError::DispatchIdentity,
    }
}
