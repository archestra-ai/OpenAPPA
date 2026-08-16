#![allow(dead_code)]

use std::path::PathBuf;

use appa_runtime_api::{
    Actor, Codec, HookDecision, HookEvent, OutcomeBody, ParseRefusal, ProposedCall, SpawnBinding, ToolOutcome,
    TrajectoryId,
};
use appa_runtime_v2::api::{
    AuditEvent, AuditLabel, DispatchOutcome, OfferId, OpenError, RemedyOutcome, Runtime, TrajectoryStatus,
};
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

fn the_declared_runtime(config: Config, db: PathBuf, modules: Option<PathBuf>) {
    let opened: Result<Runtime, OpenError> = Runtime::open(config, db, modules);
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
        HookEvent::ToolCall { actor: _, call, spawn } => {
            let ProposedCall { tool, arguments } = call;
            let _: String = tool;
            let _: Box<serde_json::value::RawValue> = arguments;
            let _: bool = spawn;
        }
        HookEvent::ToolResult {
            actor: _,
            call: _,
            outcome,
        } => {
            let _: ToolOutcome = outcome;
        }
        HookEvent::ChildStart {
            root: _,
            parent,
            child,
            spawn,
        } => {
            let _: TrajectoryId = parent;
            let _: TrajectoryId = child;
            let _: Option<SpawnBinding> = spawn;
        }
        HookEvent::ChildEnd {
            root: _,
            child: _,
            value,
        } => {
            let _: Option<String> = value;
        }
    }
    match decision {
        HookDecision::Ack | HookDecision::PassControl => {}
        HookDecision::AllowCall { spawn } => {
            let _: Option<SpawnBinding> = spawn;
        }
        HookDecision::DenyCall { feedback } => {
            let _: String = feedback;
        }
        HookDecision::Block { reason } => {
            let _: String = reason;
        }
        HookDecision::ReplaceOutput { output } => {
            let _: String = output;
        }
        HookDecision::ChildReturn { value } => {
            let _: String = value;
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

fn the_declared_status(runtime: &Runtime, id: &TrajectoryId) {
    let status: Option<TrajectoryStatus> = runtime.status(id);
    if let Some(status) = status {
        let _: (String, String, String) = (status.trajectory, status.trust, status.audience);
    }
}

fn the_declared_audit(runtime: &Runtime, id: &TrajectoryId) {
    let Some(entries) = runtime.audit(id) else {
        return;
    };
    for entry in entries {
        let _: String = entry.trajectory;
        match entry.event {
            AuditEvent::Forked { parent, seed } => {
                let _: (String, AuditLabel) = (parent, seed);
            }
            AuditEvent::Released { tool, label, effects } => {
                let _: (String, AuditLabel, Vec<String>) = (tool, label, effects);
            }
            AuditEvent::EffectsCommitted { effects } => {
                let _: Vec<String> = effects;
            }
            AuditEvent::Closed { outcome } => match outcome {
                DispatchOutcome::Ran { effects } => {
                    let _: Vec<String> = effects;
                }
                DispatchOutcome::Failed | DispatchOutcome::Unknown => {}
            },
            AuditEvent::Admitted { label } => {
                let _: (String, String) = (label.trust, label.audience);
            }
            AuditEvent::Ruled { authority } | AuditEvent::Denied { authority } => {
                let _: String = authority;
            }
            AuditEvent::Narrowed { from, to } => {
                let _: (AuditLabel, AuditLabel) = (from, to);
            }
            AuditEvent::Cast { cast, resolved } => {
                let _: (String, AuditLabel) = (cast, resolved);
            }
            AuditEvent::SanitizerBound { sanitizer } | AuditEvent::Sanitized { sanitizer } => {
                let _: String = sanitizer;
            }
            AuditEvent::ChildReturn { sanitizer, label } => {
                let _: (Option<String>, AuditLabel) = (sanitizer, label);
            }
            AuditEvent::Merged | AuditEvent::VoidReturn => {}
        }
    }
}

async fn the_declared_remedy_entry(runtime: &Runtime, offer: OfferId) {
    match runtime.execute_remedy(offer).await {
        RemedyOutcome::Authorized { call } | RemedyOutcome::Substituted { call } => {
            let _: ProposedCall = call;
        }
        RemedyOutcome::Returned { value } => {
            let _: String = value;
        }
        RemedyOutcome::Declined { feedback } | RemedyOutcome::NoAnswer { feedback } => {
            let _: String = feedback;
        }
        RemedyOutcome::Refused { detail } => {
            let _: String = detail;
        }
    }
}

fn the_declared_codec() {
    let codec: Codec = appa_adapter_claude_code::codec();
    let _: fn(&[u8]) -> Result<Option<HookEvent>, ParseRefusal> = codec.parse;
    let _: fn(&HookDecision) -> serde_json::Value = codec.render;
}

#[test]
fn the_declared_api_compiles() {}
