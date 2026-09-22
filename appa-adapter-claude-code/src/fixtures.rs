//! The hook bodies and parsed events every module's tests are written against.
//! One fixture per shape, so a shape that changes changes in one place.

use appa_runtime_api::{Actor, Derived, HookEvent, OutcomeBody, ParseRefusal, ProposedCall, ToolOutcome, TrajectoryId};

use crate::adapter;
use crate::parse::parse;

pub(crate) fn parse_value(event: &serde_json::Value) -> Result<Option<HookEvent>, ParseRefusal> {
    parse(&serde_json::to_vec(event).expect("the fixture serializes"))
}

pub(crate) fn raw(value: serde_json::Value) -> Box<serde_json::value::RawValue> {
    serde_json::value::to_raw_value(&value).expect("the fixture serializes")
}

pub(crate) fn root() -> TrajectoryId {
    TrajectoryId("cc:s1".to_string())
}

pub(crate) fn agent_response() -> serde_json::Value {
    serde_json::json!({
        "status": "completed",
        "prompt": "List the files.",
        "agentId": "a1",
        "agentType": "Explore",
        "content": [{"type": "text", "text": "one file: readme.txt"}],
        "totalDurationMs": 15484,
        "toolStats": {"readCount": 1},
    })
}

pub(crate) fn agent_post_tool_use(response: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "hook_event_name": "PostToolUse",
        "session_id": "s1",
        "tool_name": "Agent",
        "tool_input": {"prompt": "List the files.", "subagent_type": "Explore"},
        "tool_response": response,
    })
}

pub(crate) fn proposed(tool: &str, arguments: serde_json::Value) -> (Actor, ProposedCall) {
    (
        Actor {
            root: root(),
            child: None,
        },
        ProposedCall {
            tool: tool.to_string(),
            arguments: raw(arguments),
            cwd: None,
        },
    )
}

pub(crate) fn derived(tool: &str) -> Result<Derived, ParseRefusal> {
    (adapter().derive)(tool)
}

pub(crate) fn named_children(tool: &str, arguments: serde_json::Value) -> Vec<TrajectoryId> {
    let (actor, call) = proposed(tool, arguments);
    (adapter().names_children)(&actor, &call)
}

pub(crate) fn tool_result(response: serde_json::Value) -> HookEvent {
    HookEvent::ToolResult {
        actor: Actor {
            root: root(),
            child: None,
        },
        call: ProposedCall {
            tool: "Bash".to_string(),
            arguments: raw(serde_json::json!({"command": "cat notes.txt"})),
            cwd: None,
        },
        call_id: None,
        outcome: ToolOutcome::Success {
            body: OutcomeBody::Available(response.to_string()),
        },
    }
}

pub(crate) fn spawn_result(response: serde_json::Value) -> HookEvent {
    HookEvent::SpawnResult {
        actor: Actor {
            root: root(),
            child: None,
        },
        call: ProposedCall {
            tool: "Agent".to_string(),
            arguments: raw(serde_json::json!({"prompt": "List the files."})),
            cwd: None,
        },
        call_id: None,
        outcome: ToolOutcome::Success {
            body: OutcomeBody::Available(response.to_string()),
        },
        child: Some(TrajectoryId("cc:s1:a1".to_string())),
        value: Some("one file: readme.txt".to_string()),
    }
}

pub(crate) fn pre_tool_use() -> HookEvent {
    HookEvent::ToolCall {
        actor: Actor {
            root: root(),
            child: None,
        },
        call: ProposedCall {
            tool: "Bash".to_string(),
            arguments: raw(serde_json::json!({"command": "ls"})),
            cwd: None,
        },
        call_id: None,
        spawn: false,
        ruling: None,
    }
}
