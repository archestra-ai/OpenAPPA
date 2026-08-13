//! RP1 model-transcript view: rebuild the model-visible conversation **solely from server-held log
//! facts** (CC2), never from the north request.

use std::collections::VecDeque;

use appa_engine::fact::{BoundaryKind, Fact, ProposedCall};
use appa_engine::value::{Provenance, ToolCallId, TrajectoryId};

use crate::wire::{WireFunctionCall, WireMessage, WireToolCall};

/// Build the ordered model-visible messages for one branch: the host transcript head followed by its
/// inherited ancestor snapshots and branch-local turns. Each ancestor snapshot ends at the last
/// complete message before the descendant's fork, so a pending tool-call round never enters a child
/// context and later ancestor activity cannot become a cross-branch channel.
pub fn model_transcript(head: &[WireMessage], log: &[Fact], trajectory: &TrajectoryId) -> Vec<WireMessage> {
    let mut messages: Vec<WireMessage> = head.to_vec();
    let mut pending: VecDeque<ToolCallId> = VecDeque::new();
    let mut deferred: Vec<WireMessage> = Vec::new();

    let segments = ancestry_segments(log, trajectory);
    for fact in log.iter().enumerate().filter_map(|(index, fact)| {
        segments
            .iter()
            .any(|(member, end)| fact.trajectory() == member && index < *end)
            .then_some(fact)
    }) {
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

fn ancestry_segments(log: &[Fact], target: &TrajectoryId) -> Vec<(TrajectoryId, usize)> {
    let mut lineage = vec![target.clone()];
    let mut child = target;
    while let Some(parent) = fork_parent(log, child) {
        if lineage.contains(&parent) {
            break;
        }
        lineage.push(parent);
        child = lineage.last().expect("a parent was just appended");
    }
    lineage.reverse();

    lineage
        .iter()
        .enumerate()
        .map(|(index, member)| {
            let end = lineage
                .get(index + 1)
                .and_then(|descendant| fork_index(log, member, descendant))
                .map(|fork| completed_prefix_end(log, member, fork))
                .unwrap_or(log.len());
            (member.clone(), end)
        })
        .collect()
}

fn fork_parent(log: &[Fact], child: &TrajectoryId) -> Option<TrajectoryId> {
    log.iter().find_map(|fact| match fact {
        Fact::Boundary {
            trajectory,
            kind: BoundaryKind::Fork { parent, .. },
        } if trajectory == child => Some(parent.clone()),
        _ => None,
    })
}

fn fork_index(log: &[Fact], parent: &TrajectoryId, child: &TrajectoryId) -> Option<usize> {
    log.iter().position(|fact| {
        matches!(
            fact,
            Fact::Boundary {
                trajectory,
                kind: BoundaryKind::Fork { parent: fork_parent, .. },
            } if trajectory == child && fork_parent == parent
        )
    })
}

fn completed_prefix_end(log: &[Fact], trajectory: &TrajectoryId, limit: usize) -> usize {
    let mut pending = 0usize;
    let mut safe_end = 0usize;
    for (index, fact) in log.iter().enumerate().take(limit) {
        if fact.trajectory() != trajectory {
            continue;
        }
        match fact {
            Fact::AssistantMessage { calls, .. } => pending += calls.len(),
            Fact::ValueAdmitted {
                provenance: Provenance::ToolResult { .. },
                ..
            }
            | Fact::BlockFeedback { .. } => pending = pending.saturating_sub(1),
            _ => {}
        }
        if pending == 0 {
            safe_end = index + 1;
        }
    }
    safe_end
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
    use appa_engine::fact::{ForkSnapshot, ReturnPolicy};
    use appa_engine::label::{Audience, Dim, EstablishedLabel, Label, Trust};
    use appa_engine::value::{ChildReturnId, DispatchId, LabeledValue, ToolName, ValueBody};
    use serde_json::json;

    fn traj() -> TrajectoryId {
        TrajectoryId::new("t")
    }

    fn public(trust: u8) -> Label {
        Label::new(Dim::Known(Trust::new(trust)), Dim::Known(Audience::Public))
    }

    fn user(text: &str) -> Fact {
        user_for(traj(), text)
    }

    fn user_for(trajectory: TrajectoryId, text: &str) -> Fact {
        Fact::ValueAdmitted {
            trajectory,
            value: LabeledValue::new(ValueBody::new(text), public(3)),
            provenance: Provenance::UserInput,
        }
    }

    fn fork(parent: &TrajectoryId, child: &TrajectoryId) -> Fact {
        Fact::Boundary {
            trajectory: child.clone(),
            kind: BoundaryKind::Fork {
                parent: parent.clone(),
                snapshot: ForkSnapshot::freeze(EstablishedLabel::top(), std::iter::empty()),
                return_policy: ReturnPolicy::Raw,
            },
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
        let call = crate::common::test_call(tool, args);
        Fact::ValueAdmitted {
            trajectory: traj(),
            value: LabeledValue::new(ValueBody::new(text), public(0)),
            provenance: Provenance::ToolResult {
                dispatch: DispatchId::new(traj(), call.digest(), 0),
            },
        }
    }

    #[test]
    fn transcript_head_then_a_bare_user_turn() {
        let head = vec![WireMessage::system("you are a confined agent")];
        let log = vec![user("investigate the pod")];
        let out = model_transcript(&head, &log, &traj());
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

    #[test]
    fn a_child_inherits_only_the_parents_completed_prefix() {
        let parent = traj();
        let child = TrajectoryId::new("child");
        let log = vec![
            user_for(parent.clone(), "root task"),
            Fact::AssistantMessage {
                trajectory: parent.clone(),
                content: Some("working".to_string()),
                calls: vec![],
            },
            Fact::AssistantMessage {
                trajectory: parent.clone(),
                content: None,
                calls: vec![proposed("fork_1", "fork", json!({ "task": "inspect" }))],
            },
            fork(&parent, &child),
            user_for(child.clone(), "inspect"),
            user_for(parent.clone(), "post-fork parent turn"),
        ];

        let out = model_transcript(&[], &log, &child);
        assert_eq!(
            out,
            vec![
                WireMessage::user("root task"),
                WireMessage::assistant("working"),
                WireMessage::user("inspect"),
            ]
        );
    }

    #[test]
    fn a_grandchild_inherits_each_ancestor_snapshot_without_siblings() {
        let root = traj();
        let child = TrajectoryId::new("child");
        let sibling = TrajectoryId::new("sibling");
        let grandchild = TrajectoryId::new("grandchild");
        let log = vec![
            user_for(root.clone(), "root task"),
            fork(&root, &child),
            user_for(child.clone(), "child task"),
            fork(&root, &sibling),
            user_for(sibling, "sibling task"),
            fork(&child, &grandchild),
            user_for(grandchild.clone(), "grandchild task"),
            user_for(child, "late child turn"),
        ];

        let out = model_transcript(&[], &log, &grandchild);
        assert_eq!(
            out,
            vec![
                WireMessage::user("root task"),
                WireMessage::user("child task"),
                WireMessage::user("grandchild task"),
            ]
        );
    }
}
