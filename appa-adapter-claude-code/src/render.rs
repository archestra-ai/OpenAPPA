//! One `HookDecision` written in the hook wire format Claude Code expects, and
//! the withholding for hook bytes [`crate::parse`] refused. What a replacement
//! carries is [`crate::redact`]'s; this module decides which channel says it —
//! a permission decision, a `block` with its reason, a replaced result, or an
//! added context.
//!
//! A `PreToolUse` release carries no slot for the spawn binding, and
//! needs none: the child start names the spawn in flight instead.

use serde::Deserialize;

use appa_runtime_api::{HookDecision, HookEvent};

use crate::redact::{Replacement, replacement, restated};

pub(crate) fn withheld(reason: &str) -> String {
    format!("[appa] the tool result was withheld: {reason}")
}

/// The stop feedback that carries the exact bytes the subagent must return for its message
/// to cross: a subagent's stop can be held, never rewritten.
pub(crate) fn echo(value: &str) -> String {
    format!(
        "[appa] what crosses to the parent is not your message as written. Return exactly this as your final \
         message, verbatim and nothing else:\n{value}"
    )
}

pub(crate) fn render(event: &HookEvent, decision: &HookDecision) -> serde_json::Value {
    match decision {
        HookDecision::Ack => serde_json::json!({}),
        HookDecision::AllowCall { .. } => allow("appa: the call is released"),
        HookDecision::PassControl => allow("appa: the runtime's own control tool"),
        HookDecision::DenyCall { feedback, .. } => deny(feedback),
        HookDecision::Block { reason } => match replacement(event, &withheld(reason)) {
            Some(replacement) => replaced(replacement, Some(reason)),
            None => block(reason),
        },
        // What stands in for the body the model asked for: an admitted value, or the
        // runtime's own words about the result. Claude Code dispatches the spellings this
        // adapter identifies from, so nothing here is spelled back and both render alike.
        HookDecision::ReplaceOutput { output } | HookDecision::DeliverValue { value: output } => {
            match replacement(event, output) {
                Some(replacement) => replaced(replacement, None),
                None => block(output),
            }
        }
        // No hook rewrites what a subagent's stop delivers, so the
        // subagent is held until it returns the crossing value itself.
        HookDecision::ChildReturn { value } => match replacement(event, value) {
            Some(replacement) => replaced(replacement, None),
            None => block(&echo(value)),
        },
        // Context reaches an actor at its start only; every other event
        // has no slot for it and is acknowledged.
        HookDecision::Context { text } => match event {
            HookEvent::SessionStart { .. } => serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "SessionStart",
                    "additionalContext": text,
                }
            }),
            HookEvent::ChildStart { .. } => serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "SubagentStart",
                    "additionalContext": text,
                }
            }),
            _ => serde_json::json!({}),
        },
        HookDecision::Refuse { detail } => match replacement(event, &withheld(detail)) {
            Some(replacement) => {
                let mut body = replaced(replacement, None);
                body["error"] = serde_json::Value::String(detail.clone());
                body
            }
            None => serde_json::json!({ "error": detail }),
        },
    }
}

pub(crate) fn replaced(replacement: Replacement, reason: Option<&str>) -> serde_json::Value {
    let mut body = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "updatedToolOutput": replacement.output,
        }
    });
    if let Some(context) = replacement.context {
        body["hookSpecificOutput"]["additionalContext"] = serde_json::Value::String(context);
    }
    if let Some(reason) = reason {
        body["decision"] = serde_json::Value::String("block".to_string());
        body["reason"] = serde_json::Value::String(reason.to_string());
    }
    body
}

pub(crate) fn block(reason: &str) -> serde_json::Value {
    serde_json::json!({
        "decision": "block",
        "reason": reason,
    })
}

pub(crate) fn allow(reason: &str) -> serde_json::Value {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "permissionDecisionReason": reason,
        }
    })
}

pub(crate) fn deny(reason: &str) -> serde_json::Value {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    })
}

/// The little of a hook event this codec still reads once [`parse`] has refused the rest:
/// the hook's name, and the tool and response a post-use hook carries. Nothing here is a
/// field a hook must be well formed to have.
#[derive(Debug, Deserialize)]
pub(crate) struct RefusedEvent {
    hook_event_name: String,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    tool_response: Option<serde_json::Value>,
}

/// The withholding for hook bytes [`parse`] refused. A post-use hook reports a result the
/// tool has already produced, and Claude Code keeps that output unless the answer replaces
/// it — so the shape those bytes still carry is read for the replacement, and a post-use
/// hook too broken to name a tool and its response is answered by the reason alone. Every
/// other hook answers `None`: nothing has run there, and the client's blocking exit is what
/// stops the action — a stop hook included, where Claude Code reads that exit as the block.
pub(crate) fn withholding(body: &[u8], reason: &str) -> Option<serde_json::Value> {
    let event: RefusedEvent = serde_json::from_slice(body).ok()?;
    let text = withheld(reason);
    match event.hook_event_name.as_str() {
        "PostToolUse" | "PostToolUseFailure" => Some(match (event.tool_name.as_deref(), event.tool_response) {
            (Some(tool), Some(response)) => replaced(restated(tool, response, &text), Some(reason)),
            _ => block(reason),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::*;
    use crate::redact::REDACTED;
    use appa_runtime_api::{Actor, HookDecision, HookEvent, ProposedCall, SpawnRef, ToolOutcome, TrajectoryId};
    #[test]
    fn every_pre_tool_decision_renders_its_exact_wire_body() {
        let event = pre_tool_use();
        assert_eq!(render(&event, &HookDecision::Ack), serde_json::json!({}));
        assert_eq!(
            render(&event, &HookDecision::AllowCall { spawn: None }),
            serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "allow",
                    "permissionDecisionReason": "appa: the call is released",
                }
            }),
        );
        assert_eq!(
            render(&event, &HookDecision::PassControl),
            serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "allow",
                    "permissionDecisionReason": "appa: the runtime's own control tool",
                }
            }),
        );
        assert_eq!(
            render(
                &event,
                &HookDecision::DenyCall {
                    feedback: "blocked: the recipient cannot read this".to_string(),
                    offers: Vec::new(),
                    review: Vec::new(),
                }
            ),
            serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": "blocked: the recipient cannot read this",
                }
            }),
        );
        assert_eq!(
            render(
                &event,
                &HookDecision::Refuse {
                    detail: "storage failure: disk full".to_string(),
                }
            ),
            serde_json::json!({"error": "storage failure: disk full"}),
        );
    }

    #[test]
    fn a_post_tool_decision_replaces_the_result_in_the_tools_own_shape() {
        let response = serde_json::json!({
            "stdout": "alpha beta",
            "stderr": "",
            "interrupted": true,
            "isImage": false,
            "mode": "files_with_matches",
            "matches": [{"type": "text", "path": "notes.txt", "line": 3}],
        });
        let event = tool_result(response);
        let swapped = |text: &str| {
            serde_json::json!({
                "stdout": text,
                "stderr": REDACTED,
                "interrupted": false,
                "isImage": false,
                "mode": "files_with_matches",
                "matches": [{"type": "text", "path": REDACTED, "line": 0}],
            })
        };
        assert_eq!(
            render(
                &event,
                &HookDecision::ReplaceOutput {
                    output: "the output is confined".to_string(),
                }
            ),
            serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PostToolUse",
                    "updatedToolOutput": swapped("the output is confined"),
                }
            }),
        );
        assert_eq!(
            render(
                &event,
                &HookDecision::DeliverValue {
                    value: "the output is confined".to_string(),
                }
            ),
            render(
                &event,
                &HookDecision::ReplaceOutput {
                    output: "the output is confined".to_string(),
                }
            ),
            "this codec spells nothing back, so an admitted value and the runtime's own words render alike",
        );
        assert_eq!(
            render(
                &event,
                &HookDecision::Block {
                    reason: "this outcome does not match the open dispatch".to_string(),
                }
            ),
            serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PostToolUse",
                    "updatedToolOutput": swapped(
                        "[appa] the tool result was withheld: this outcome does not match the open dispatch"
                    ),
                },
                "decision": "block",
                "reason": "this outcome does not match the open dispatch",
            }),
        );
        assert_eq!(
            render(
                &event,
                &HookDecision::Refuse {
                    detail: "storage failure: disk full".to_string(),
                }
            ),
            serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PostToolUse",
                    "updatedToolOutput": swapped("[appa] the tool result was withheld: storage failure: disk full"),
                },
                "error": "storage failure: disk full",
            }),
        );
        assert_eq!(
            render(
                &tool_result(serde_json::json!("plain text")),
                &HookDecision::ReplaceOutput {
                    output: "the output is confined".to_string(),
                }
            ),
            serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PostToolUse",
                    "updatedToolOutput": "the output is confined",
                }
            }),
        );
    }

    #[test]
    fn a_decision_without_a_delivered_response_blocks_instead() {
        let event = HookEvent::ToolResult {
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
            outcome: ToolOutcome::Indeterminate,
        };
        assert_eq!(
            render(
                &event,
                &HookDecision::Block {
                    reason: "the trajectory has ended".to_string(),
                }
            ),
            serde_json::json!({"decision": "block", "reason": "the trajectory has ended"}),
        );
        assert_eq!(
            render(
                &event,
                &HookDecision::ReplaceOutput {
                    output: "the output is confined".to_string(),
                }
            ),
            serde_json::json!({"decision": "block", "reason": "the output is confined"}),
        );
    }

    /// A post-use hook this codec cannot read still reports a result the tool produced, so
    /// it is answered by the same replacement a parsed one gets — built from the tool and
    /// response its bytes still carry. Here the hook misses `tool_input`, which every parse
    /// of a post-use hook requires.
    #[test]
    fn an_unreadable_post_use_hook_still_withholds_the_result_it_reports() {
        let refused = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": "s1",
            "tool_name": "Bash",
            "tool_response": {"stdout": "root:x:0:0"},
        });
        assert!(parse_value(&refused).is_err(), "the fixture is one parse refuses");
        let answer = withholding(
            &serde_json::to_vec(&refused).expect("the fixture serializes"),
            "unreadable",
        )
        .expect("a post-use hook reports a result");
        assert_eq!(
            answer,
            serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PostToolUse",
                    "updatedToolOutput": {"stdout": "[appa] the tool result was withheld: unreadable"},
                },
                "decision": "block",
                "reason": "unreadable",
            }),
        );

        let spawn = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": "s1",
            "tool_name": "Agent",
            "tool_response": agent_response(),
        });
        let answer = withholding(
            &serde_json::to_vec(&spawn).expect("the fixture serializes"),
            "unreadable",
        )
        .expect("a spawn's post-use hook reports a result");
        assert_eq!(
            answer["hookSpecificOutput"]["updatedToolOutput"]["content"],
            serde_json::json!([{"type": "text", "text": "[appa] the tool result was withheld: unreadable"}]),
            "the subagent's message is what the parent model reads: {answer}",
        );
        assert!(
            !answer.to_string().contains("one file: readme.txt"),
            "the withheld message never reaches the model: {answer}",
        );
    }

    /// Nothing has run at any other hook, and bytes that are no JSON at all report no
    /// result either: the client's blocking exit is what stops those, with nothing printed.
    #[test]
    fn only_a_post_use_hooks_bytes_report_a_result_to_withhold() {
        for hook in ["PreToolUse", "SubagentStop", "Stop", "SessionStart", "Notification"] {
            let event = serde_json::json!({"hook_event_name": hook, "session_id": "s1", "tool_name": "Bash"});
            assert_eq!(
                withholding(
                    &serde_json::to_vec(&event).expect("the fixture serializes"),
                    "unreadable"
                ),
                None,
                "{hook} reports no result the harness has already produced",
            );
        }
        assert_eq!(withholding(b"not json", "unreadable"), None);

        let broken = serde_json::json!({"hook_event_name": "PostToolUseFailure", "session_id": "s1"});
        assert_eq!(
            withholding(
                &serde_json::to_vec(&broken).expect("the fixture serializes"),
                "unreadable"
            ),
            Some(serde_json::json!({"decision": "block", "reason": "unreadable"})),
            "a post-use hook naming no response carries the reason alone",
        );
    }

    #[test]
    fn session_start_context_reaches_the_root_actor() {
        assert_eq!(
            render(
                &HookEvent::SessionStart {
                    root: root(),
                    principal: None
                },
                &HookDecision::Context {
                    text: "available file tools".into()
                },
            ),
            serde_json::json!({"hookSpecificOutput": {
                "hookEventName": "SessionStart", "additionalContext": "available file tools"
            }}),
        );
    }

    #[test]
    fn every_child_end_decision_renders_its_exact_wire_body() {
        let event = HookEvent::ChildEnd {
            root: root(),
            child: TrajectoryId("cc:s1:a1".to_string()),
            value: Some("the secret summary".to_string()),
        };
        assert_eq!(render(&event, &HookDecision::Ack), serde_json::json!({}));
        assert_eq!(
            render(
                &event,
                &HookDecision::Block {
                    reason: "nothing crossed".to_string(),
                }
            ),
            serde_json::json!({"decision": "block", "reason": "nothing crossed"}),
        );
        assert_eq!(
            render(
                &event,
                &HookDecision::Context {
                    text: "the contract".to_string(),
                }
            ),
            serde_json::json!({}),
            "a stop has no slot for context",
        );
        assert_eq!(
            render(
                &event,
                &HookDecision::ChildReturn {
                    value: "{\"status\":\"verified\"}".to_string(),
                }
            ),
            serde_json::json!({"decision": "block", "reason": echo("{\"status\":\"verified\"}")}),
            "a stop is held until the subagent returns the crossing value itself",
        );
        assert_eq!(
            render(
                &event,
                &HookDecision::ReplaceOutput {
                    output: "the output is confined".to_string(),
                }
            ),
            serde_json::json!({"decision": "block", "reason": "the output is confined"}),
        );
        assert_eq!(
            render(
                &event,
                &HookDecision::Refuse {
                    detail: "storage failure: disk full".to_string(),
                }
            ),
            serde_json::json!({"error": "storage failure: disk full"}),
        );
    }

    /// A post-use hook too broken to name both a tool and its response is answered by the
    /// reason alone. Half a shape is no shape to restate: the bytes name a tool with nothing
    /// to redact, or a response with no tool to key its restatement on, and either way the
    /// answer says so rather than inventing a replacement.
    #[test]
    fn a_post_use_hook_missing_either_half_of_its_result_is_answered_by_the_reason() {
        let reason = "unreadable";
        for (name, body) in [
            (
                "a tool with no response",
                r#"{"hook_event_name":"PostToolUse","session_id":"s1","tool_name":"Bash"}"#,
            ),
            (
                "a response with no tool",
                r#"{"hook_event_name":"PostToolUse","session_id":"s1","tool_response":{"stdout":"readme.txt"}}"#,
            ),
        ] {
            let answer = withholding(body.as_bytes(), reason).expect("a post-use hook reports a result");
            assert_eq!(
                answer,
                serde_json::json!({"decision": "block", "reason": reason}),
                "{name} is answered by the reason alone",
            );
            assert!(
                !answer.to_string().contains("readme.txt"),
                "{name} carries nothing of the response across",
            );
        }
    }

    /// Which channel every answer takes, over every event this codec produces crossed with
    /// every decision the runtime can answer it with. The four decisions that stand in for a
    /// result — block, replace, deliver, return — each render a replacement where the event
    /// carries a body to restate and fall back to a bare block where it does not, and only a
    /// start has a slot for context. The table is the whole map: a change that moves one
    /// answer onto another channel moves a cell here.
    #[test]
    fn every_event_and_decision_renders_on_one_channel() {
        /// The channel a rendered body takes, read back out of its shape.
        fn channel(body: &serde_json::Value) -> &'static str {
            let slot = &body["hookSpecificOutput"];
            let replacement = !slot["updatedToolOutput"].is_null();
            let error = !body["error"].is_null();
            match (replacement, error, body["decision"] == "block") {
                (true, true, _) => "error+replacement",
                (true, false, true) => "replacement+reason",
                (true, false, false) => "replacement",
                (false, true, _) => "error",
                (false, false, true) => "block",
                (false, false, false) => {
                    match (slot["additionalContext"].is_null(), slot["permissionDecision"].as_str()) {
                        (false, _) => "context",
                        (_, Some("allow")) => "allow",
                        (_, Some("deny")) => "deny",
                        _ => "empty",
                    }
                }
            }
        }

        let decisions = [
            HookDecision::Ack,
            HookDecision::AllowCall { spawn: None },
            HookDecision::PassControl,
            HookDecision::DenyCall {
                feedback: "denied".to_string(),
                offers: Vec::new(),
                review: Vec::new(),
            },
            HookDecision::Block {
                reason: "held".to_string(),
            },
            HookDecision::ReplaceOutput {
                output: "replaced".to_string(),
            },
            HookDecision::DeliverValue {
                value: "delivered".to_string(),
            },
            HookDecision::ChildReturn {
                value: "returned".to_string(),
            },
            HookDecision::Context {
                text: "advice".to_string(),
            },
            HookDecision::Refuse {
                detail: "refused".to_string(),
            },
        ];

        // ack, allow, control, deny, block, replace, deliver, return, context, refuse
        let table: [(&str, HookEvent, [&str; 10]); 9] = [
            (
                "session start",
                HookEvent::SessionStart {
                    root: root(),
                    principal: None,
                },
                [
                    "empty", "allow", "allow", "deny", "block", "block", "block", "block", "context", "error",
                ],
            ),
            (
                "prompt",
                HookEvent::Prompt {
                    actor: Actor {
                        root: root(),
                        child: None,
                    },
                    text: "do the thing".to_string(),
                },
                [
                    "empty", "allow", "allow", "deny", "block", "block", "block", "block", "empty", "error",
                ],
            ),
            (
                "tool call",
                pre_tool_use(),
                [
                    "empty", "allow", "allow", "deny", "block", "block", "block", "block", "empty", "error",
                ],
            ),
            (
                "tool result with a delivered body",
                tool_result(serde_json::json!({"stdout": "readme.txt"})),
                [
                    "empty",
                    "allow",
                    "allow",
                    "deny",
                    "replacement+reason",
                    "replacement",
                    "replacement",
                    "replacement",
                    "empty",
                    "error+replacement",
                ],
            ),
            (
                "tool result with no delivered body",
                HookEvent::ToolResult {
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
                    outcome: ToolOutcome::Indeterminate,
                },
                [
                    "empty", "allow", "allow", "deny", "block", "block", "block", "block", "empty", "error",
                ],
            ),
            (
                "spawn result with a delivered body",
                spawn_result(agent_response()),
                [
                    "empty",
                    "allow",
                    "allow",
                    "deny",
                    "replacement+reason",
                    "replacement",
                    "replacement",
                    "replacement",
                    "empty",
                    "error+replacement",
                ],
            ),
            (
                "child start",
                HookEvent::ChildStart {
                    root: root(),
                    child: TrajectoryId("cc:s1:a1".to_string()),
                    spawn: SpawnRef::InFlight,
                },
                [
                    "empty", "allow", "allow", "deny", "block", "block", "block", "block", "context", "error",
                ],
            ),
            (
                "child end",
                HookEvent::ChildEnd {
                    root: root(),
                    child: TrajectoryId("cc:s1:a1".to_string()),
                    value: Some("the child's return".to_string()),
                },
                [
                    "empty", "allow", "allow", "deny", "block", "block", "block", "block", "empty", "error",
                ],
            ),
            (
                "turn end",
                HookEvent::TurnEnd {
                    actor: Actor {
                        root: root(),
                        child: None,
                    },
                },
                [
                    "empty", "allow", "allow", "deny", "block", "block", "block", "block", "empty", "error",
                ],
            ),
        ];

        for (name, event, expected) in table {
            for (decision, expected) in decisions.iter().zip(expected) {
                let body = render(&event, decision);
                assert_eq!(
                    channel(&body),
                    expected,
                    "{name} answered with {decision:?} renders {body}",
                );
            }
        }
    }
}
