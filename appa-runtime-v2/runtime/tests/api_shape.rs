#![allow(dead_code)]

use std::path::PathBuf;

use appa_runtime_api::{
    Actor, Codec, HookDecision, HookEvent, OutcomeBody, ParseRefusal, ProposedCall, ToolOutcome, TrajectoryId,
};
use appa_runtime_v2::api::{OpenError, Runtime};
use appa_runtime_v2::config::Config;
use appa_runtime_v2::hooks;

fn the_reexported_vocabulary(
    id: appa_runtime_v2::api::TrajectoryId,
    call: appa_runtime_v2::api::ProposedCall,
    outcome: appa_runtime_v2::api::ToolOutcome,
) {
    let _: TrajectoryId = id;
    let _: ProposedCall = call;
    let _: ToolOutcome = outcome;
}

fn the_declared_runtime(config: Config, db: PathBuf) {
    let opened: Result<Runtime, OpenError> = Runtime::open(config, db);
    drop(opened);
}

async fn the_declared_dispatcher(runtime: &Runtime, codec: &Codec, event: HookEvent, body: &[u8]) {
    let _: HookDecision = hooks::handle(runtime, event).await;
    let _: (u16, serde_json::Value) = hooks::answer(runtime, codec, body).await;
}

fn the_declared_vocabulary(event: HookEvent, decision: HookDecision, refusal: ParseRefusal, outcome: ToolOutcome) {
    match event {
        HookEvent::SessionStart { root } => {
            let _: TrajectoryId = root;
        }
        HookEvent::Prompt { actor, text } => {
            let Actor { root, child } = actor;
            let _: TrajectoryId = root;
            let _: Option<TrajectoryId> = child;
            let _: String = text;
        }
        HookEvent::ToolCall { actor: _, call } => {
            let ProposedCall { tool, arguments } = call;
            let _: String = tool;
            let _: serde_json::Value = arguments;
        }
        HookEvent::ToolResult {
            actor: _,
            call: _,
            outcome,
        } => {
            let _: ToolOutcome = outcome;
        }
        HookEvent::ChildStart { parent, child } => {
            let _: TrajectoryId = parent;
            let _: TrajectoryId = child;
        }
        HookEvent::ChildEnd {
            parent: _,
            child: _,
            value,
        } => {
            let _: Option<String> = value;
        }
    }
    match decision {
        HookDecision::Ack | HookDecision::AllowCall | HookDecision::PassControl => {}
        HookDecision::DenyCall { feedback } => {
            let _: String = feedback;
        }
        HookDecision::Block { reason } => {
            let _: String = reason;
        }
        HookDecision::Refuse { detail } => {
            let _: String = detail;
        }
    }
    match refusal {
        ParseRefusal::Unreadable { detail } | ParseRefusal::Malformed { detail } => {
            let _: String = detail;
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

fn the_declared_codec() {
    let codec: Codec = appa_adapter_claude_code::codec();
    let _: fn(&[u8]) -> Result<Option<HookEvent>, ParseRefusal> = codec.parse;
    let _: fn(&HookDecision) -> serde_json::Value = codec.render;
}

#[test]
fn the_declared_api_compiles() {}
