//! RP1 model-transcript view: rebuild the model-visible conversation **solely from server-held log
//! facts** (CC2), never from the north request.

use std::collections::VecDeque;

use appa_engine::fact::{Fact, ProposedCall};
use appa_engine::value::{Provenance, ToolCallId, TrajectoryId};

use crate::wire::{WireFunctionCall, WireMessage, WireToolCall};

/// Build the ordered model-visible messages for one branch: the server preamble followed by the
/// branch's turns replayed from the log. `preamble` is the server-pinned `system`/`developer` messages
/// (never client-supplied); `log` is the shared family log; `trajectory` scopes to one branch.
pub fn model_transcript(preamble: &[WireMessage], log: &[Fact], trajectory: &TrajectoryId) -> Vec<WireMessage> {
    let mut messages: Vec<WireMessage> = preamble.to_vec();
    let mut pending: VecDeque<ToolCallId> = VecDeque::new();
    let mut deferred: Vec<WireMessage> = Vec::new();

    for fact in log.iter().filter(|f| f.trajectory() == trajectory) {
        match fact {
            Fact::ValueAdmitted {
                value,
                provenance: Provenance::UserInput,
                ..
            } => messages.push(WireMessage::user(value.body.as_str())),
            Fact::ValueAdmitted {
                value,
                provenance: Provenance::ChildReturn { .. },
                ..
            } => {
                let message = WireMessage::user(value.body.as_str());
                if pending.is_empty() {
                    messages.push(message);
                } else {
                    deferred.push(message);
                }
            }
            Fact::AssistantMessage { content, calls, .. } => {
                if let Some(message) = assistant_message(content.as_deref(), calls) {
                    messages.push(message);
                }
                pending.extend(calls.iter().map(|call| call.id.clone()));
            }
            // An available tool result the model sees: its body, paired to the pending call by position.
            Fact::ValueAdmitted {
                value,
                provenance: Provenance::ToolResult { .. },
                ..
            } => {
                if let Some(call_id) = pending.pop_front() {
                    messages.push(WireMessage::tool_result(call_id.as_str(), value.body.as_str()));
                }
                flush_if_drained(&pending, &mut deferred, &mut messages);
            }
            Fact::BlockFeedback { call_id, content, .. } => {
                pending.pop_front();
                messages.push(WireMessage::tool_result(call_id.as_str(), content));
                flush_if_drained(&pending, &mut deferred, &mut messages);
            }
            // Merged / derived values, dispatch bookkeeping, rulings, boundaries: not model-visible here.
            _ => {}
        }
    }
    // A trailing (incomplete) round may leave deferred child returns; surface them at the end.
    messages.append(&mut deferred);
    messages
}

fn flush_if_drained(pending: &VecDeque<ToolCallId>, deferred: &mut Vec<WireMessage>, messages: &mut Vec<WireMessage>) {
    if pending.is_empty() && !deferred.is_empty() {
        messages.append(deferred);
    }
}

fn assistant_message(content: Option<&str>, calls: &[ProposedCall]) -> Option<WireMessage> {
    if calls.is_empty() {
        return content.map(WireMessage::assistant);
    }
    Some(WireMessage {
        role: "assistant".to_string(),
        content: content.map(str::to_string),
        tool_calls: Some(calls.iter().map(render_call).collect()),
        tool_call_id: None,
    })
}

fn render_call(call: &ProposedCall) -> WireToolCall {
    WireToolCall {
        id: call.id.as_str().to_string(),
        kind: "function".to_string(),
        function: WireFunctionCall {
            name: call.tool.as_str().to_string(),
            // OpenAI carries arguments as a JSON string; re-serialize the recorded argument tree.
            arguments: serde_json::to_string(&call.arguments).unwrap_or_else(|_| "{}".to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use appa_engine::label::{Audience, Dim, Label, Trust};
    use appa_engine::value::{ChildReturnId, DispatchId, LabeledValue, ResolvedCall, ToolName, ValueBody};
    use serde_json::json;

    fn traj() -> TrajectoryId {
        TrajectoryId::new("t")
    }

    fn public(trust: u8) -> Label {
        Label::new(Dim::Known(Trust::new(trust)), Dim::Known(Audience::Public))
    }

    fn user(text: &str) -> Fact {
        Fact::ValueAdmitted {
            trajectory: traj(),
            value: LabeledValue::new(ValueBody::new(text), public(3)),
            provenance: Provenance::UserInput,
        }
    }

    fn proposed(id: &str, tool: &str, args: serde_json::Value) -> ProposedCall {
        ProposedCall {
            id: ToolCallId::new(id),
            tool: ToolName::new(tool),
            arguments: args,
        }
    }

    fn tool_result(text: &str, tool: &str, args: serde_json::Value) -> Fact {
        let call = ResolvedCall::new(ToolName::new(tool), args, vec![]);
        Fact::ValueAdmitted {
            trajectory: traj(),
            value: LabeledValue::new(ValueBody::new(text), public(0)),
            provenance: Provenance::ToolResult {
                dispatch: DispatchId::new(traj(), call.digest(), 0),
            },
        }
    }

    #[test]
    fn preamble_then_a_bare_user_turn() {
        let preamble = vec![WireMessage::system("you are a confined agent")];
        let log = vec![user("investigate the pod")];
        let out = model_transcript(&preamble, &log, &traj());
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].role, "system");
        assert_eq!(out[1], WireMessage::user("investigate the pod"));
    }

    #[test]
    fn pairs_a_tool_call_with_its_admitted_result() {
        let log = vec![
            user("what is wrong?"),
            Fact::AssistantMessage {
                trajectory: traj(),
                content: None,
                calls: vec![proposed("call_1", "get_logs", json!({ "pod": "checkout" }))],
            },
            tool_result("CrashLoopBackOff", "get_logs", json!({ "pod": "checkout" })),
            Fact::AssistantMessage {
                trajectory: traj(),
                content: Some("the pod is crashlooping".to_string()),
                calls: vec![],
            },
        ];
        let out = model_transcript(&[], &log, &traj());
        assert_eq!(out.len(), 4);
        assert_eq!(out[1].tool_calls.as_ref().unwrap()[0].id, "call_1");
        assert_eq!(out[2].role, "tool");
        assert_eq!(out[2].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(out[2].content.as_deref(), Some("CrashLoopBackOff"));
        assert_eq!(out[3], WireMessage::assistant("the pod is crashlooping"));
    }

    #[test]
    fn blocked_and_available_responses_pair_by_position() {
        let log = vec![
            user("do two things"),
            Fact::AssistantMessage {
                trajectory: traj(),
                content: None,
                calls: vec![
                    proposed("call_1", "wire_money", json!({ "to": "stranger" })),
                    proposed("call_2", "get_logs", json!({ "pod": "api" })),
                ],
            },
            Fact::BlockFeedback {
                trajectory: traj(),
                call_id: ToolCallId::new("call_1"),
                content: "blocked: recipient not permitted".to_string(),
            },
            tool_result("OK", "get_logs", json!({ "pod": "api" })),
        ];
        let out = model_transcript(&[], &log, &traj());
        assert_eq!(out.len(), 4);
        assert_eq!(out[2].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(out[2].content.as_deref(), Some("blocked: recipient not permitted"));
        assert_eq!(out[3].tool_call_id.as_deref(), Some("call_2"));
        assert_eq!(out[3].content.as_deref(), Some("OK"));
    }

    #[test]
    fn a_child_return_is_deferred_past_pending_tool_responses() {
        let child = TrajectoryId::new("child");
        let log = vec![
            user("investigate"),
            Fact::AssistantMessage {
                trajectory: traj(),
                content: None,
                calls: vec![proposed("call_1", "get_logs", json!({}))],
            },
            Fact::ValueAdmitted {
                trajectory: traj(),
                value: LabeledValue::new(ValueBody::new("child findings"), public(0)),
                provenance: Provenance::ChildReturn {
                    child: child.clone(),
                    id: ChildReturnId::new(child, 0),
                },
            },
            tool_result("logs ok", "get_logs", json!({})),
        ];
        let out = model_transcript(&[], &log, &traj());
        let roles: Vec<&str> = out.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, vec!["user", "assistant", "tool", "user"]);
        assert_eq!(out[2].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(out[3].content.as_deref(), Some("child findings"));
    }

    #[test]
    fn a_sibling_branch_is_not_in_this_transcript() {
        let sibling = TrajectoryId::new("child");
        let mut log = vec![user("parent turn")];
        log.push(Fact::AssistantMessage {
            trajectory: sibling.clone(),
            content: Some("child-only text".to_string()),
            calls: vec![],
        });
        let out = model_transcript(&[], &log, &traj());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], WireMessage::user("parent turn"));
    }
}
