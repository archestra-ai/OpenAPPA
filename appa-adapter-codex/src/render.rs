use appa_runtime_api::{HookDecision, HookEvent, ToolOutcome};

fn deny(reason: &str) -> serde_json::Value {
    serde_json::json!({"hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": "deny",
        "permissionDecisionReason": reason,
    }})
}

fn block(reason: &str) -> serde_json::Value {
    serde_json::json!({"decision": "block", "reason": reason})
}

fn stop_turn(reason: &str) -> serde_json::Value {
    serde_json::json!({"continue": false, "stopReason": reason})
}

fn block_event(event: &HookEvent, reason: &str) -> serde_json::Value {
    match event {
        HookEvent::SessionStart { .. } | HookEvent::Prompt { .. } => stop_turn(reason),
        _ => block(reason),
    }
}

fn feedback(event: &HookEvent, value: &str) -> serde_json::Value {
    match event {
        HookEvent::ToolResult { .. } | HookEvent::SpawnResult { .. } => block(value),
        HookEvent::ChildEnd { .. } => block(value),
        _ => block(value),
    }
}

pub(crate) fn render(event: &HookEvent, decision: &HookDecision) -> serde_json::Value {
    match decision {
        HookDecision::Ack => match event {
            HookEvent::ToolResult { outcome, .. } | HookEvent::SpawnResult { outcome, .. }
                if !matches!(outcome, ToolOutcome::Success { .. }) =>
            {
                block("OpenAPPA could not establish a complete admitted result")
            }
            _ => serde_json::json!({}),
        },
        HookDecision::AllowCall { .. } | HookDecision::PassControl => serde_json::json!({}),
        HookDecision::DenyCall { feedback, .. } => deny(feedback),
        HookDecision::Block { reason } => block_event(event, reason),
        HookDecision::ReplaceOutput { output } => feedback(event, output),
        HookDecision::DeliverValue { value } => feedback(event, value),
        HookDecision::ChildReturn { value } => block(&format!("Return exactly this text:\n{value}")),
        HookDecision::Context { text } => match event {
            HookEvent::SessionStart { .. } => serde_json::json!({"hookSpecificOutput": {
                "hookEventName": "SessionStart", "additionalContext": text
            }}),
            HookEvent::ChildStart { .. } => serde_json::json!({"hookSpecificOutput": {
                "hookEventName": "SubagentStart", "additionalContext": text
            }}),
            _ => serde_json::json!({}),
        },
        HookDecision::Refuse { detail } => match event {
            HookEvent::ToolCall { .. } => deny(detail),
            _ => block_event(event, detail),
        },
    }
}

pub(crate) fn withholding(body: &[u8], reason: &str) -> Option<serde_json::Value> {
    let event: serde_json::Value = serde_json::from_slice(body).ok()?;
    (event.get("hook_event_name")?.as_str()? == "PostToolUse").then(|| block(reason))
}

#[cfg(test)]
mod tests {
    use super::*;
    use appa_runtime_api::{Actor, ProposedCall, ToolOutcome, TrajectoryId};

    #[test]
    fn ordinary_admission_is_a_valid_no_op_for_codex() {
        let event = HookEvent::ToolCall {
            actor: Actor {
                root: TrajectoryId("codex:s1".into()),
                child: None,
            },
            call: ProposedCall {
                tool: "apply_patch".into(),
                arguments: serde_json::value::to_raw_value(&serde_json::json!({"patch":"x"})).unwrap(),
                cwd: None,
            },
            call_id: Some("c1".into()),
            spawn: false,
            ruling: None,
        };
        for decision in [HookDecision::AllowCall { spawn: None }, HookDecision::PassControl] {
            let value = render(&event, &decision);
            assert_eq!(value, serde_json::json!({}));
            assert!(value.get("permissionDecision").is_none());
        }
    }

    #[test]
    fn a_failed_session_start_stops_the_codex_turn() {
        let event = HookEvent::SessionStart {
            root: TrajectoryId("codex:s1".into()),
            principal: None,
        };
        for decision in [
            HookDecision::Block {
                reason: "runtime unavailable".into(),
            },
            HookDecision::Refuse {
                detail: "storage failure".into(),
            },
        ] {
            let value = render(&event, &decision);
            assert_eq!(value["continue"], false);
            assert!(value["stopReason"].as_str().is_some_and(|reason| !reason.is_empty()));
            assert!(value.get("decision").is_none());
        }
    }

    #[test]
    fn post_result_is_never_rendered_with_unsupported_replacement_field() {
        let event = HookEvent::ToolResult {
            actor: Actor {
                root: TrajectoryId("codex:s1".into()),
                child: None,
            },
            call: ProposedCall {
                tool: "Bash".into(),
                arguments: serde_json::value::to_raw_value(&serde_json::json!({})).unwrap(),
                cwd: None,
            },
            call_id: Some("c1".into()),
            outcome: ToolOutcome::Indeterminate,
        };
        let value = render(
            &event,
            &HookDecision::DeliverValue {
                value: "admitted".into(),
            },
        );
        assert_eq!(value, serde_json::json!({"decision":"block","reason":"admitted"}));
        assert!(!value.to_string().contains("updatedToolOutput"));
    }

    #[test]
    fn indeterminate_result_is_withheld_even_when_runtime_keeps_it() {
        let event = HookEvent::ToolResult {
            actor: Actor {
                root: TrajectoryId("codex:s1".into()),
                child: None,
            },
            call: ProposedCall {
                tool: "mcp__fs__write".into(),
                arguments: serde_json::value::to_raw_value(&serde_json::json!({})).unwrap(),
                cwd: None,
            },
            call_id: Some("c1".into()),
            outcome: ToolOutcome::Indeterminate,
        };
        let rendered = render(&event, &HookDecision::Ack);
        assert_eq!(rendered["decision"], "block");
    }
}
