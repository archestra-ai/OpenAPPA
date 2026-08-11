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
    tool_input: Option<serde_json::Value>,
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
        "PostToolUse" | "PostToolUseFailure" => {
            let Some(call) = event.call() else {
                return Err(malformed("a tool outcome without its tool call"));
            };
            let outcome = match event.hook_event_name.as_str() {
                "PostToolUseFailure" => ToolOutcome::Failure {
                    message: "the tool run failed".to_string(),
                },
                _ => map_outcome(event.tool_response.as_ref()),
            };
            Ok(Some(HookEvent::ToolResult {
                actor: event.actor(),
                call,
                outcome,
            }))
        }
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

fn map_outcome(response: Option<&serde_json::Value>) -> ToolOutcome {
    let Some(response) = response else {
        return ToolOutcome::Indeterminate;
    };
    let Ok(body) = serde_json::to_string(response) else {
        return ToolOutcome::Indeterminate;
    };
    ToolOutcome::Success {
        body: OutcomeBody::Available(body),
    }
}

fn render(decision: &HookDecision) -> serde_json::Value {
    match decision {
        HookDecision::Ack => serde_json::json!({}),
        HookDecision::AllowCall => allow("appa: the call is released"),
        HookDecision::PassControl => allow("appa: the runtime's own control tool"),
        HookDecision::DenyCall { feedback } => deny(feedback),
        HookDecision::Block { reason } => serde_json::json!({
            "decision": "block",
            "reason": reason,
        }),
        HookDecision::Refuse { detail } => serde_json::json!({ "error": detail }),
    }
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
                    arguments: serde_json::json!({"command": "ls"}),
                },
            })),
        );
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
    fn the_outcome_mapping_carries_what_it_saw() {
        let available = map_outcome(Some(&serde_json::json!({"ok": true})));
        assert_eq!(
            available,
            ToolOutcome::Success {
                body: OutcomeBody::Available("{\"ok\":true}".to_string()),
            },
        );
        let big = serde_json::json!("x".repeat(5000));
        assert_eq!(
            map_outcome(Some(&big)),
            ToolOutcome::Success {
                body: OutcomeBody::Available(serde_json::to_string(&big).expect("the big body serializes")),
            },
        );
        assert_eq!(map_outcome(None), ToolOutcome::Indeterminate);
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
