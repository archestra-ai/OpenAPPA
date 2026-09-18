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

use crate::redact::{replacement, restated, Replacement};

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
        // adapter derives from, so nothing here is spelled back and both render alike.
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
