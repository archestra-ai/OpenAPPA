#![allow(dead_code)]

use std::path::PathBuf;

use appa_runtime_v2::api::{
    AuthorizedCall, ChildReturnDecision, ChildTask, DispatchId, EventError, OfferId, OpenError, OutcomeBody,
    ProposedCall, RemedyDecision, Runtime, Session, SessionError, ToolCallDecision, ToolOutcome, ToolResultDecision,
    TrajectoryId,
};
use appa_runtime_v2::config::Config;

fn the_declared_runtime(config: Config, db: PathBuf, id: TrajectoryId) {
    let opened: Result<Runtime, OpenError> = Runtime::open(config, db);
    let Ok(runtime) = opened else { return };
    let _created: Result<Session, SessionError> = runtime.create_session(id.clone());
    let _reopened: Result<Session, SessionError> = runtime.session(&id);
}

#[expect(
    clippy::too_many_arguments,
    reason = "the six events' parameter types are the shape under test"
)]
async fn the_declared_session_events(
    mut session: Session,
    call: ProposedCall,
    dispatch: DispatchId,
    outcome: ToolOutcome,
    offer: OfferId,
    child: TrajectoryId,
    task: ChildTask,
    value: Option<String>,
) {
    let _: Result<(), EventError> = session.on_prompt(String::new());
    let _: Result<ToolCallDecision, EventError> = session.on_tool_call(call).await;
    let _: Result<ToolResultDecision, EventError> = session.on_tool_result(dispatch, outcome).await;
    let _: Result<RemedyDecision, EventError> = session.on_remedy(offer).await;
    let _: Result<Session, EventError> = session.on_child_start(child, task);
    let _: Result<ChildReturnDecision, EventError> = session.on_child_end(value).await;
}

fn the_declared_decisions(
    call: ToolCallDecision,
    result: ToolResultDecision,
    remedy: RemedyDecision,
    child: ChildReturnDecision,
    outcome: ToolOutcome,
) {
    match call {
        ToolCallDecision::Allow {
            dispatch: DispatchId(_),
        } => {}
        ToolCallDecision::Deny { feedback } => {
            let _: String = feedback;
        }
    }
    match result {
        ToolResultDecision::Keep => {}
        ToolResultDecision::Replace { placeholder } => {
            let _: String = placeholder;
        }
    }
    match remedy {
        RemedyDecision::Authorized {
            dispatch: DispatchId(_),
            call,
        } => {
            let AuthorizedCall { tool, bytes } = call;
            let _: String = tool;
            let _: Vec<u8> = bytes;
        }
        RemedyDecision::Returned { value } => {
            let _: String = value;
        }
        RemedyDecision::Staged { feedback }
        | RemedyDecision::Declined { feedback }
        | RemedyDecision::NoAnswer { feedback } => {
            let _: String = feedback;
        }
    }
    match child {
        ChildReturnDecision::Returned { value } => {
            let _: String = value;
        }
        ChildReturnDecision::NoValue => {}
        ChildReturnDecision::Blocked { feedback } => {
            let _: String = feedback;
        }
    }
    match outcome {
        ToolOutcome::Success {
            body: OutcomeBody::Available(body),
        } => {
            let _: String = body;
        }
        ToolOutcome::Success {
            body: OutcomeBody::Unavailable,
        } => {}
        ToolOutcome::Failure { message } => {
            let _: String = message;
        }
        ToolOutcome::Indeterminate => {}
    }
}

#[test]
fn the_declared_api_compiles() {}
