//! The Claude Code codec: hook JSON to the runtime's vocabulary and
//! back.

use serde::Deserialize;

use appa_runtime_api::{
    Actor, Codec, HookDecision, HookEvent, OutcomeBody, ParseRefusal, ProposedCall, ToolOutcome, TrajectoryId,
};

pub fn codec() -> Codec {
    Codec { parse, render }
}

#[derive(Debug, Deserialize)]
struct WireEvent {
    hook_event_name: String,
    session_id: String,
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    tool_input: Option<Box<serde_json::value::RawValue>>,
    #[serde(default)]
    tool_response: Option<serde_json::Value>,
    #[serde(default)]
    last_assistant_message: Option<String>,
}

impl WireEvent {
    fn root(&self) -> TrajectoryId {
        TrajectoryId(format!("cc:{}", self.session_id))
    }

    fn trajectory(&self) -> TrajectoryId {
        match &self.agent_id {
            Some(agent) => TrajectoryId(format!("cc:{}:{agent}", self.session_id)),
            None => self.root(),
        }
    }

    fn actor(&self) -> Actor {
        Actor {
            root: self.root(),
            child: self
                .agent_id
                .as_ref()
                .map(|agent| TrajectoryId(format!("cc:{}:{agent}", self.session_id))),
        }
    }

    fn call(&self) -> Option<ProposedCall> {
        match (self.tool_name.clone(), self.tool_input.clone()) {
            (Some(tool), Some(arguments)) => Some(ProposedCall { tool, arguments }),
            _ => None,
        }
    }
}

fn malformed(detail: &str) -> ParseRefusal {
    ParseRefusal::Malformed {
        detail: detail.to_string(),
    }
}

fn parse(body: &[u8]) -> Result<Option<HookEvent>, ParseRefusal> {
    let event: WireEvent = serde_json::from_slice(body).map_err(|error| ParseRefusal::Unreadable {
        detail: format!("unreadable hook event: {error}"),
    })?;
    tracing::debug!(hook = %event.hook_event_name, session = %event.session_id, "hook event");
    match event.hook_event_name.as_str() {
        "SessionStart" => Ok(Some(HookEvent::SessionStart { root: event.root() })),
        "UserPromptSubmit" => match event.prompt.clone() {
            Some(text) => Ok(Some(HookEvent::Prompt {
                actor: event.actor(),
                text,
            })),
            None => Err(malformed("UserPromptSubmit without a prompt")),
        },
        "PreToolUse" => match event.call() {
            Some(call) => Ok(Some(HookEvent::ToolCall {
                actor: event.actor(),
                call,
            })),
            None => Err(malformed("PreToolUse without a tool call")),
        },
        "PostToolUse" => tool_result(&event, map_outcome(event.tool_response.as_ref())),
        "PostToolUseFailure" => tool_result(
            &event,
            ToolOutcome::Failure {
                message: "the tool run failed".to_string(),
            },
        ),
        "SubagentStart" => match event.agent_id.clone() {
            Some(agent) => Ok(Some(HookEvent::ChildStart {
                parent: event.root(),
                child: TrajectoryId(format!("cc:{}:{agent}", event.session_id)),
            })),
            None => Err(malformed("SubagentStart without an agent id")),
        },
        "SubagentStop" => Ok(Some(HookEvent::ChildEnd {
            parent: event.root(),
            child: event.trajectory(),
            value: event.last_assistant_message.clone(),
        })),
        other => {
            tracing::debug!(hook = other, "hook event outside the codec's mapping");
            Ok(None)
        }
    }
}

fn tool_result(event: &WireEvent, outcome: ToolOutcome) -> Result<Option<HookEvent>, ParseRefusal> {
    match event.call() {
        Some(call) => Ok(Some(HookEvent::ToolResult {
            actor: event.actor(),
            call,
            outcome,
        })),
        None => Err(malformed("a tool outcome without its tool call")),
    }
}

fn map_outcome(response: Option<&serde_json::Value>) -> ToolOutcome {
    match response {
        None => ToolOutcome::Indeterminate,
        Some(response) => ToolOutcome::Success {
            body: OutcomeBody::Available(response.to_string()),
        },
    }
}

fn render(decision: &HookDecision) -> serde_json::Value {
    match decision {
        HookDecision::Ack => serde_json::json!({}),
        HookDecision::AllowCall => allow("appa: the call is released"),
        HookDecision::PassControl => allow("appa: the runtime's own control tool"),
        HookDecision::DenyCall { feedback } => deny(feedback),
        HookDecision::Block { reason } => block(reason),
        HookDecision::ReplaceOutput { output } => block(output),
        HookDecision::ChildReturn { .. } => serde_json::json!({}),
        HookDecision::Refuse { detail } => serde_json::json!({ "error": detail }),
    }
}

fn block(reason: &str) -> serde_json::Value {
    serde_json::json!({
        "decision": "block",
        "reason": reason,
    })
}

fn allow(reason: &str) -> serde_json::Value {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "permissionDecisionReason": reason,
        }
    })
}

fn deny(reason: &str) -> serde_json::Value {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_value(event: &serde_json::Value) -> Result<Option<HookEvent>, ParseRefusal> {
        parse(&serde_json::to_vec(event).expect("the fixture serializes"))
    }

    #[test]
    fn an_unreadable_body_is_refused_with_the_wire_detail() {
        match parse(b"not json") {
            Err(ParseRefusal::Unreadable { detail }) => {
                assert!(
                    detail.starts_with("unreadable hook event: "),
                    "the detail must carry the wire prefix, got {detail:?}",
                );
            }
            other => panic!("expected an Unreadable refusal, got {other:?}"),
        }
    }

    #[test]
    fn missing_required_fields_are_named_refusals() {
        for (event, detail) in [
            (
                serde_json::json!({"hook_event_name": "UserPromptSubmit", "session_id": "s1"}),
                "UserPromptSubmit without a prompt",
            ),
            (
                serde_json::json!({"hook_event_name": "PreToolUse", "session_id": "s1"}),
                "PreToolUse without a tool call",
            ),
            (
                serde_json::json!({"hook_event_name": "PostToolUse", "session_id": "s1"}),
                "a tool outcome without its tool call",
            ),
            (
                serde_json::json!({"hook_event_name": "SubagentStart", "session_id": "s1"}),
                "SubagentStart without an agent id",
            ),
        ] {
            assert_eq!(
                parse_value(&event),
                Err(ParseRefusal::Malformed {
                    detail: detail.to_string()
                }),
                "the {} refusal drifted",
                event["hook_event_name"],
            );
        }
    }

    #[test]
    fn unmapped_hooks_parse_to_no_event() {
        for name in ["Stop", "SomethingNew"] {
            let event = serde_json::json!({"hook_event_name": name, "session_id": "s1"});
            assert_eq!(parse_value(&event), Ok(None), "the {name} hook maps to no event");
        }
    }

    #[test]
    fn a_pre_tool_use_parses_to_a_tool_call_with_cc_ids() {
        let event = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "tool_name": "Bash",
            "tool_input": {"command": "ls"},
        });
        assert_eq!(
            parse_value(&event),
            Ok(Some(HookEvent::ToolCall {
                actor: Actor {
                    root: TrajectoryId("cc:s1".to_string()),
                    child: None,
                },
                call: ProposedCall {
                    tool: "Bash".to_string(),
                    arguments: serde_json::value::to_raw_value(&serde_json::json!({"command": "ls"}))
                        .expect("the fixture serializes"),
                },
            })),
        );
    }

    #[test]
    fn duplicate_argument_members_reach_the_runtime_unresolved() {
        let body =
            br#"{"hook_event_name":"PreToolUse","session_id":"s1","tool_name":"Bash","tool_input":{"a":1,"a":2}}"#;
        let Ok(Some(HookEvent::ToolCall { call, .. })) = parse(body) else {
            panic!("the hook parses to a tool call");
        };
        assert_eq!(call.arguments.get(), r#"{"a":1,"a":2}"#);
    }

    #[test]
    fn an_agent_id_attributes_the_event_to_the_child() {
        let event = serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "s1",
            "agent_id": "a1",
            "prompt": "work",
        });
        assert_eq!(
            parse_value(&event),
            Ok(Some(HookEvent::Prompt {
                actor: Actor {
                    root: TrajectoryId("cc:s1".to_string()),
                    child: Some(TrajectoryId("cc:s1:a1".to_string())),
                },
                text: "work".to_string(),
            })),
        );
    }

    #[test]
    fn subagent_hooks_parse_to_child_events() {
        let start = serde_json::json!({
            "hook_event_name": "SubagentStart",
            "session_id": "s1",
            "agent_id": "a1",
        });
        assert_eq!(
            parse_value(&start),
            Ok(Some(HookEvent::ChildStart {
                parent: TrajectoryId("cc:s1".to_string()),
                child: TrajectoryId("cc:s1:a1".to_string()),
            })),
        );
        let stop = serde_json::json!({
            "hook_event_name": "SubagentStop",
            "session_id": "s1",
            "agent_id": "a1",
            "last_assistant_message": "the summary",
        });
        assert_eq!(
            parse_value(&stop),
            Ok(Some(HookEvent::ChildEnd {
                parent: TrajectoryId("cc:s1".to_string()),
                child: TrajectoryId("cc:s1:a1".to_string()),
                value: Some("the summary".to_string()),
            })),
        );
    }

    #[test]
    fn a_failed_tool_run_parses_to_a_typed_failure() {
        let event = serde_json::json!({
            "hook_event_name": "PostToolUseFailure",
            "session_id": "s1",
            "tool_name": "Bash",
            "tool_input": {"command": "ls"},
        });
        match parse_value(&event) {
            Ok(Some(HookEvent::ToolResult { outcome, .. })) => assert_eq!(
                outcome,
                ToolOutcome::Failure {
                    message: "the tool run failed".to_string(),
                },
            ),
            other => panic!("expected a ToolResult event, got {other:?}"),
        }
    }

    #[test]
    fn a_post_tool_use_maps_its_response_shape_onto_one_outcome() {
        let post = |response: Option<serde_json::Value>| {
            let mut event = serde_json::json!({
                "hook_event_name": "PostToolUse",
                "session_id": "s1",
                "tool_name": "Bash",
                "tool_input": {"command": "ls"},
            });
            if let Some(response) = response {
                event["tool_response"] = response;
            }
            match parse_value(&event) {
                Ok(Some(HookEvent::ToolResult { outcome, .. })) => outcome,
                other => panic!("expected a ToolResult event, got {other:?}"),
            }
        };
        assert_eq!(post(None), ToolOutcome::Indeterminate, "no response key at all");
        assert_eq!(
            post(Some(serde_json::Value::Null)),
            ToolOutcome::Indeterminate,
            "an explicit null response carries no result either",
        );
        assert_eq!(
            post(Some(serde_json::json!({"stdout": "readme.txt"}))),
            ToolOutcome::Success {
                body: OutcomeBody::Available("{\"stdout\":\"readme.txt\"}".to_string()),
            },
        );
        assert_eq!(
            post(Some(serde_json::json!("plain text"))),
            ToolOutcome::Success {
                body: OutcomeBody::Available("\"plain text\"".to_string()),
            },
            "a scalar response is carried as its JSON rendering, like every other shape",
        );
        let big = serde_json::json!("x".repeat(5000));
        assert_eq!(
            post(Some(big.clone())),
            ToolOutcome::Success {
                body: OutcomeBody::Available(big.to_string()),
            },
        );
    }

    #[test]
    fn every_decision_renders_its_exact_wire_body() {
        assert_eq!(render(&HookDecision::Ack), serde_json::json!({}));
        assert_eq!(
            render(&HookDecision::AllowCall),
            serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "allow",
                    "permissionDecisionReason": "appa: the call is released",
                }
            }),
        );
        assert_eq!(
            render(&HookDecision::PassControl),
            serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "allow",
                    "permissionDecisionReason": "appa: the runtime's own control tool",
                }
            }),
        );
        assert_eq!(
            render(&HookDecision::DenyCall {
                feedback: "blocked: the recipient cannot read this".to_string(),
            }),
            serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": "blocked: the recipient cannot read this",
                }
            }),
        );
        assert_eq!(
            render(&HookDecision::Block {
                reason: "the output is confined".to_string(),
            }),
            serde_json::json!({"decision": "block", "reason": "the output is confined"}),
        );
        assert_eq!(
            render(&HookDecision::ReplaceOutput {
                output: "the output is confined".to_string(),
            }),
            serde_json::json!({"decision": "block", "reason": "the output is confined"}),
            "this harness cannot rewrite a delivered output, so the admitted text blocks instead",
        );
        assert_eq!(
            render(&HookDecision::ChildReturn {
                value: "the redacted summary".to_string(),
            }),
            serde_json::json!({}),
            "SubagentStop cannot substitute a finished child's return, and the branch has already ended",
        );
        assert_eq!(
            render(&HookDecision::Refuse {
                detail: "storage failure: disk full".to_string(),
            }),
            serde_json::json!({"error": "storage failure: disk full"}),
        );
    }

    #[test]
    fn the_codec_carries_parse_and_render() {
        let codec = codec();
        assert_eq!(
            (codec.parse)(b"{\"hook_event_name\":\"Stop\",\"session_id\":\"s1\"}"),
            Ok(None),
        );
        assert_eq!((codec.render)(&HookDecision::Ack), serde_json::json!({}));
    }
}
