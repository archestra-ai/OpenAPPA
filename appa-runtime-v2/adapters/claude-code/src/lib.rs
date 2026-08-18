//! The Claude Code codec: hook JSON to the runtime's vocabulary and
//! back.
//!
//! A pure codec — no policy, no state, no runtime calls;
//! the compiler enforces the
//! boundary, since this crate depends only on `appa-runtime-api`. It
//! derives trajectory ids from Claude Code's own ids with the `cc:`
//! prefix, maps each hook onto one `HookEvent`, and renders every
//! `HookDecision` in the hook wire format Claude Code expects. The
//! wire shapes come from recorded live hook examples
//! (`runtime/tests/fixtures/hooks.jsonl`).
//!
//! Hook mapping:
//!
//! | hook | `HookEvent` |
//! |---|---|
//! | `SessionStart` | `SessionStart` |
//! | `UserPromptSubmit` | `Prompt` |
//! | `PreToolUse` | `ToolCall`; the `Agent` (`Task`) tool is the spawn |
//! | `PostToolUse` for `Agent` (`Task`) | `SpawnResult` |
//! | `PostToolUse`, `PostToolUseFailure` | `ToolResult` (the Q14 outcome mapping) |
//! | `SubagentStart` | `ChildStart`, naming the family's spawn in flight |
//! | `Stop`, `StopFailure`, `SubagentStop` | `TurnEnd` for the actor that finished |
//!
//! Subagents. Claude Code spawns a subagent through its `Agent` tool
//! (`Task` is its older name), so the codec marks that call as the
//! deployment's context-controlled spawn. `SubagentStart`
//! names the new subagent (`agent_id`) but not the `Agent` call that
//! started it, so the child start can echo no binding: it names the
//! family's spawn in flight, and the runtime ties it to the one prepared
//! fork still open for binding. The one place Claude Code
//! links the two is the parent's `Agent` `PostToolUse`: its
//! `tool_response.agentId` is the subagent's id and its `content` is the
//! subagent's final message. That hook is therefore the return channel:
//! the codec reports it as `SpawnResult`, the runtime checks
//! the named child against the fork it bound, and the parent receives
//! what crosses. `SubagentStop` fires before it and carries the same
//! text, but a hook there cannot substitute what the parent receives —
//! it can only keep the subagent running — so it crosses as the child's
//! `TurnEnd` and never as its return.
//! A subagent started with `run_in_background: true` returns through a
//! task notification no hook observes; the shipped example policies
//! refuse that argument at the spawn call.
//!
//! Outcome mapping, which is the adapter's contract. This
//! harness runs the tools itself, so the codec observes no HTTP status,
//! no stream, no process exit and no callback — only the two outcome
//! hooks and the response one of them carries:
//!
//! | observation | `ToolOutcome` |
//! |---|---|
//! | `PostToolUseFailure` | `Failure` — the run failed; no effects commit |
//! | `PostToolUse` with a `tool_response` | `Success` carrying that response's JSON rendering |
//! | `PostToolUse` with no `tool_response` (absent or null — the wire spells them alike) | `Indeterminate` — no effects commit, the reservation stands |
//! | no outcome hook at all | nothing is reported; the dispatch stays open until the actor's `TurnEnd` closes it as not run |
//!
//! The mapping is total over those shapes. It reads no error shape out
//! of a `tool_response` body: no recorded live example of one exists,
//! and calling a real success a `Failure` would discard effects the
//! tool had.
//!
//! Replacing what the model sees. A `PostToolUse` hook replaces a tool
//! result through `hookSpecificOutput.updatedToolOutput`, and Claude
//! Code applies the replacement only when it has the tool's own output
//! shape — otherwise it silently keeps the original. So the codec never
//! answers with a bare placeholder: it restates the response it was
//! handed with its leaves redacted. For the spawn's result the swap is
//! the `content` text — the one field of the `Agent` response Claude
//! Code shows the parent model; the rest, the run's own metadata, stays
//! for the transcript. For every other builtin tool every leaf is
//! redacted: the text takes the tool's content field — `Bash` `stdout`,
//! `Read` `file.content`, `Grep` `content`, `WebFetch` `result`, `Write`
//! `content` — or, where the shape has no known one, the place of its
//! longest string; every other string becomes `[appa] redacted`, numbers
//! `0`, booleans `false`, and an array keeps one element, so a match
//! count, a line count or a result count carries nothing either and the
//! answer never grows with the leaf count. A shape with no string to
//! carry the text — counts and flags only — gets it as the hook's
//! `additionalContext` beside the redacted output. The one exception to
//! the redaction is a string under a key Claude Code validates as a
//! fixed value (`type`, `mode`, `status`) when it is one of the fixed
//! values its output shapes use; any other string there is content. An
//! MCP tool's result (`mcp__…`) is restated as one text block instead:
//! Claude Code accepts any shape there, and an MCP result's keys are
//! content too. A withheld result additionally carries the reason as
//! `decision: block`, which Claude Code shows next to the (replaced)
//! result. Verified live on Claude Code 2.1.233 for `Agent`, `Bash`,
//! `Read`, `Glob`, `Grep`, `Write`, `Edit`, `WebFetch`, and honored on a
//! non-2xx answer too — so a runtime refusal at `PostToolUse` also
//! withholds. A tool whose output shape validates another fixed-value
//! string field would keep the original; the fixed-value list is the
//! codec's to extend.
//!
//! A `PreToolUse` release carries no slot for the spawn binding, and
//! needs none: the child start names the spawn in flight instead.

use serde::Deserialize;

use appa_runtime_api::{
    Actor, Codec, HookDecision, HookEvent, OutcomeBody, ParseRefusal, ProposedCall, SpawnRef, ToolOutcome, TrajectoryId,
};

pub fn codec() -> Codec {
    Codec { parse, render }
}

fn is_spawn_tool(tool: &str) -> bool {
    tool == "Agent" || tool == "Task"
}

fn is_fixed_value(key: &str, value: &str) -> bool {
    let fixed: &[&str] = match key {
        "type" => &[
            "text",
            "image",
            "notebook",
            "pdf",
            "create",
            "update",
            "resource",
            "resource_link",
            "audio",
        ],
        "mode" => &["content", "files_with_matches", "count"],
        "status" => &[
            "completed",
            "async_launched",
            "remote_launched",
            "pending",
            "in_progress",
            "running",
            "failed",
            "killed",
            "paused",
        ],
        _ => &[],
    };
    fixed.contains(&value)
}

const REDACTED: &str = "[appa] redacted";

fn is_mcp_tool(tool: &str) -> bool {
    tool.starts_with("mcp__")
}

fn content_slot(tool: &str) -> Option<&'static str> {
    match tool {
        "Bash" => Some("/stdout"),
        "Read" => Some("/file/content"),
        "Grep" => Some("/content"),
        "WebFetch" => Some("/result"),
        "Write" => Some("/content"),
        _ => None,
    }
}

fn withheld(reason: &str) -> String {
    format!("[appa] the tool result was withheld: {reason}")
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
}

impl WireEvent {
    fn root(&self) -> TrajectoryId {
        TrajectoryId(format!("cc:{}", self.session_id))
    }

    fn child_id(&self, agent: &str) -> TrajectoryId {
        TrajectoryId(format!("cc:{}:{agent}", self.session_id))
    }

    fn actor(&self) -> Actor {
        Actor {
            root: self.root(),
            child: self.agent_id.as_deref().map(|agent| self.child_id(agent)),
        }
    }

    fn call(&self) -> Option<ProposedCall> {
        match (self.tool_name.clone(), self.tool_input.clone()) {
            (Some(tool), Some(arguments)) => {
                let arguments = match tool.as_str() {
                    "AskUserQuestion" => strip_collected_answers(arguments),
                    _ => arguments,
                };
                Some(ProposedCall { tool, arguments })
            }
            _ => None,
        }
    }

    fn spawn_return(&self) -> (Option<TrajectoryId>, Option<String>) {
        let Some(response) = self.tool_response.as_ref() else {
            return (None, None);
        };
        let child = response
            .get("agentId")
            .and_then(|id| id.as_str())
            .map(|agent| self.child_id(agent));
        let value = response
            .get("content")
            .filter(|content| !content.is_null())
            .map(|content| match text_blocks(content) {
                Some(texts) => texts.join("\n"),
                None => content.to_string(),
            });
        (child, value.filter(|text| !text.is_empty()))
    }
}

fn text_blocks(content: &serde_json::Value) -> Option<Vec<&str>> {
    content
        .as_array()?
        .iter()
        .map(|block| {
            (block.get("type")?.as_str()? == "text")
                .then(|| block.get("text")?.as_str())
                .flatten()
        })
        .collect()
}

fn strip_collected_answers(arguments: Box<serde_json::value::RawValue>) -> Box<serde_json::value::RawValue> {
    let Ok(mut parsed) = serde_json::from_str::<serde_json::Value>(arguments.get()) else {
        return arguments;
    };
    let Some(object) = parsed.as_object_mut() else {
        return arguments;
    };
    object.remove("answers");
    object.remove("annotations");
    serde_json::value::to_raw_value(&parsed).expect("a parsed value re-serializes")
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
            Some(call) => {
                let spawn = is_spawn_tool(&call.tool);
                Ok(Some(HookEvent::ToolCall {
                    actor: event.actor(),
                    call,
                    spawn,
                }))
            }
            None => Err(malformed("PreToolUse without a tool call")),
        },
        "PostToolUse" => match event.call() {
            Some(call) if is_spawn_tool(&call.tool) => {
                let (child, value) = event.spawn_return();
                Ok(Some(HookEvent::SpawnResult {
                    actor: event.actor(),
                    call,
                    outcome: map_outcome(event.tool_response.as_ref()),
                    child,
                    value,
                }))
            }
            Some(call) => Ok(Some(HookEvent::ToolResult {
                actor: event.actor(),
                call,
                outcome: map_outcome(event.tool_response.as_ref()),
            })),
            None => Err(malformed("a tool outcome without its tool call")),
        },
        "PostToolUseFailure" => match event.call() {
            Some(call) => Ok(Some(HookEvent::ToolResult {
                actor: event.actor(),
                call,
                outcome: ToolOutcome::Failure {
                    message: "the tool run failed".to_string(),
                },
            })),
            None => Err(malformed("a tool outcome without its tool call")),
        },
        "SubagentStart" => match event.agent_id.as_deref() {
            Some(agent) => Ok(Some(HookEvent::ChildStart {
                root: event.root(),
                child: event.child_id(agent),
                spawn: SpawnRef::InFlight,
            })),
            None => Err(malformed("SubagentStart without an agent id")),
        },
        "Stop" | "StopFailure" => Ok(Some(HookEvent::TurnEnd { actor: event.actor() })),
        // Without the agent id this would name the root, whose one open
        // dispatch at this point is the `Agent` spawn still in flight.
        "SubagentStop" => match event.agent_id.as_deref() {
            Some(_) => Ok(Some(HookEvent::TurnEnd { actor: event.actor() })),
            None => Err(malformed("SubagentStop without an agent id")),
        },
        other => {
            tracing::debug!(hook = other, "hook event outside the codec's mapping");
            Ok(None)
        }
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

fn render(event: &HookEvent, decision: &HookDecision) -> serde_json::Value {
    match decision {
        HookDecision::Ack => serde_json::json!({}),
        HookDecision::AllowCall { .. } => allow("appa: the call is released"),
        HookDecision::PassControl => allow("appa: the runtime's own control tool"),
        HookDecision::DenyCall { feedback } => deny(feedback),
        HookDecision::Block { reason } => match replacement(event, &withheld(reason)) {
            Some(replacement) => replaced(replacement, Some(reason)),
            None => block(reason),
        },
        // The admitted text in place of the body the model asked for.
        HookDecision::ReplaceOutput { output } => match replacement(event, output) {
            Some(replacement) => replaced(replacement, None),
            None => block(output),
        },
        HookDecision::ChildReturn { value } => match replacement(event, value) {
            Some(replacement) => replaced(replacement, None),
            None => serde_json::json!({}),
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

struct Replacement {
    output: serde_json::Value,
    context: Option<String>,
}

fn replacement(event: &HookEvent, text: &str) -> Option<Replacement> {
    match event {
        HookEvent::SpawnResult { outcome, .. } => {
            let mut response = delivered(outcome)?;
            let output = match response.as_object_mut() {
                Some(object) => {
                    object.insert(
                        "content".to_string(),
                        serde_json::json!([{ "type": "text", "text": text }]),
                    );
                    response
                }
                None => serde_json::Value::String(text.to_string()),
            };
            Some(Replacement { output, context: None })
        }
        HookEvent::ToolResult { call, outcome, .. } => {
            let response = delivered(outcome)?;
            Some(if is_mcp_tool(&call.tool) {
                Replacement {
                    output: serde_json::json!([{ "type": "text", "text": text }]),
                    context: None,
                }
            } else {
                swap_leaves(&call.tool, response, text)
            })
        }
        _ => None,
    }
}

fn delivered(outcome: &ToolOutcome) -> Option<serde_json::Value> {
    match outcome {
        ToolOutcome::Success {
            body: OutcomeBody::Available(body),
        } => serde_json::from_str(body).ok(),
        _ => None,
    }
}

fn swap_leaves(tool: &str, value: serde_json::Value, text: &str) -> Replacement {
    let slot = content_slot(tool).filter(|slot| value.pointer(slot).is_some_and(serde_json::Value::is_string));
    let longest = match slot {
        Some(_) => None,
        None => longest_content(&value, None),
    };
    let mut placed = slot.is_some();
    let mut output = redact(value, None, text, longest, &mut placed);
    if let Some(slot) = slot {
        *output.pointer_mut(slot).expect("redaction keeps the response's shape") =
            serde_json::Value::String(text.to_string());
    }
    Replacement {
        output,
        context: (!placed).then(|| text.to_string()),
    }
}

fn longest_content(value: &serde_json::Value, key: Option<&str>) -> Option<usize> {
    match value {
        serde_json::Value::String(text) => (!kept(key, text)).then_some(text.len()),
        serde_json::Value::Array(items) => items.iter().take(1).find_map(|item| longest_content(item, None)),
        serde_json::Value::Object(fields) => fields
            .iter()
            .filter_map(|(key, field)| longest_content(field, Some(key)))
            .max(),
        _ => None,
    }
}

fn redact(
    value: serde_json::Value,
    key: Option<&str>,
    text: &str,
    longest: Option<usize>,
    placed: &mut bool,
) -> serde_json::Value {
    match value {
        serde_json::Value::String(kept_text) if kept(key, &kept_text) => serde_json::Value::String(kept_text),
        serde_json::Value::String(content) => {
            if !*placed && Some(content.len()) == longest {
                *placed = true;
                serde_json::Value::String(text.to_string())
            } else {
                serde_json::Value::String(REDACTED.to_string())
            }
        }
        serde_json::Value::Number(_) => serde_json::Value::from(0),
        serde_json::Value::Bool(_) => serde_json::Value::Bool(false),
        serde_json::Value::Null => serde_json::Value::Null,
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .into_iter()
                .take(1)
                .map(|item| redact(item, None, text, longest, placed))
                .collect(),
        ),
        serde_json::Value::Object(fields) => serde_json::Value::Object(
            fields
                .into_iter()
                .map(|(key, field)| {
                    let redacted = redact(field, Some(&key), text, longest, placed);
                    (key, redacted)
                })
                .collect(),
        ),
    }
}

fn kept(key: Option<&str>, text: &str) -> bool {
    key.is_some_and(|key| is_fixed_value(key, text))
}

fn replaced(replacement: Replacement, reason: Option<&str>) -> serde_json::Value {
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

    fn raw(value: serde_json::Value) -> Box<serde_json::value::RawValue> {
        serde_json::value::to_raw_value(&value).expect("the fixture serializes")
    }

    fn root() -> TrajectoryId {
        TrajectoryId("cc:s1".to_string())
    }

    fn agent_response() -> serde_json::Value {
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

    fn agent_post_tool_use(response: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": "s1",
            "tool_name": "Agent",
            "tool_input": {"prompt": "List the files.", "subagent_type": "Explore"},
            "tool_response": response,
        })
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
    fn every_turn_end_hook_names_the_actor_that_finished() {
        for (hook, child) in [
            ("Stop", None),
            ("StopFailure", None),
            ("SubagentStop", Some(TrajectoryId("cc:s1:a1".to_string()))),
        ] {
            let mut body = serde_json::json!({"hook_event_name": hook, "session_id": "s1"});
            if child.is_some() {
                body["agent_id"] = serde_json::Value::String("a1".to_string());
            }
            let parsed = parse(body.to_string().as_bytes()).expect("the turn end parses");
            assert_eq!(
                parsed,
                Some(HookEvent::TurnEnd {
                    actor: Actor { root: root(), child },
                }),
                "{hook} did not cross as its actor's turn end",
            );
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
                serde_json::json!({"hook_event_name": "PostToolUseFailure", "session_id": "s1"}),
                "a tool outcome without its tool call",
            ),
            (
                serde_json::json!({"hook_event_name": "SubagentStart", "session_id": "s1"}),
                "SubagentStart without an agent id",
            ),
            (
                serde_json::json!({"hook_event_name": "SubagentStop", "session_id": "s1"}),
                "SubagentStop without an agent id",
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
        for name in ["PreCompact", "Notification", "SomethingNew"] {
            let event = serde_json::json!({
                "hook_event_name": name,
                "session_id": "s1",
                "agent_id": "a1",
                "last_assistant_message": "the summary",
            });
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
                    root: root(),
                    child: None,
                },
                call: ProposedCall {
                    tool: "Bash".to_string(),
                    arguments: raw(serde_json::json!({"command": "ls"})),
                },
                spawn: false,
            })),
        );
    }

    #[test]
    fn the_agent_tool_call_is_the_spawn() {
        for tool in ["Agent", "Task"] {
            let event = serde_json::json!({
                "hook_event_name": "PreToolUse",
                "session_id": "s1",
                "tool_name": tool,
                "tool_input": {"prompt": "list files", "subagent_type": "Explore"},
            });
            match parse_value(&event) {
                Ok(Some(HookEvent::ToolCall { spawn, call, .. })) => {
                    assert!(spawn, "{tool} is the spawn");
                    assert_eq!(call.tool, tool);
                }
                other => panic!("expected a ToolCall event, got {other:?}"),
            }
        }
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
                    root: root(),
                    child: Some(TrajectoryId("cc:s1:a1".to_string())),
                },
                text: "work".to_string(),
            })),
        );
    }

    #[test]
    fn a_subagent_start_names_the_spawn_in_flight() {
        let start = serde_json::json!({
            "hook_event_name": "SubagentStart",
            "session_id": "s1",
            "agent_id": "a1",
            "agent_type": "Explore",
        });
        assert_eq!(
            parse_value(&start),
            Ok(Some(HookEvent::ChildStart {
                root: root(),
                child: TrajectoryId("cc:s1:a1".to_string()),
                spawn: SpawnRef::InFlight,
            })),
        );
    }

    #[test]
    fn an_agent_result_parses_to_the_spawn_result_naming_the_child() {
        let response = agent_response();
        assert_eq!(
            parse_value(&agent_post_tool_use(response.clone())),
            Ok(Some(HookEvent::SpawnResult {
                actor: Actor {
                    root: root(),
                    child: None,
                },
                call: ProposedCall {
                    tool: "Agent".to_string(),
                    arguments: raw(serde_json::json!({"prompt": "List the files.", "subagent_type": "Explore"})),
                },
                outcome: ToolOutcome::Success {
                    body: OutcomeBody::Available(response.to_string()),
                },
                child: Some(TrajectoryId("cc:s1:a1".to_string())),
                value: Some("one file: readme.txt".to_string()),
            })),
        );
    }

    #[test]
    fn a_launch_or_anonymous_agent_result_carries_no_message() {
        let launched = serde_json::json!({
            "isAsync": true,
            "status": "async_launched",
            "agentId": "a2",
            "description": "Compute 6*7",
        });
        match parse_value(&agent_post_tool_use(launched)) {
            Ok(Some(HookEvent::SpawnResult { child, value, .. })) => {
                assert_eq!(child, Some(TrajectoryId("cc:s1:a2".to_string())));
                assert_eq!(value, None, "a launch carries no message");
            }
            other => panic!("expected a SpawnResult event, got {other:?}"),
        }
        match parse_value(&agent_post_tool_use(
            serde_json::json!({"content": [{"type": "text", "text": "x"}]}),
        )) {
            Ok(Some(HookEvent::SpawnResult { child, value, .. })) => {
                assert_eq!(child, None, "no agentId names no child");
                assert_eq!(value, Some("x".to_string()));
            }
            other => panic!("expected a SpawnResult event, got {other:?}"),
        }
        match parse_value(&agent_post_tool_use(
            serde_json::json!({"agentId": "a3", "content": []}),
        )) {
            Ok(Some(HookEvent::SpawnResult { value, .. })) => {
                assert_eq!(value, None, "empty content is no message");
            }
            other => panic!("expected a SpawnResult event, got {other:?}"),
        }
    }

    #[test]
    fn non_text_agent_content_is_the_message_as_spelled() {
        for content in [
            serde_json::json!([{"type": "image", "source": {"data": "iVBORw0"}}]),
            serde_json::json!([{"type": "text", "text": "one"}, {"type": "text", "text": 7}]),
            serde_json::json!({"text": "not an array"}),
        ] {
            match parse_value(&agent_post_tool_use(
                serde_json::json!({"agentId": "a4", "content": content}),
            )) {
                Ok(Some(HookEvent::SpawnResult { child, value, .. })) => {
                    assert_eq!(child, Some(TrajectoryId("cc:s1:a4".to_string())));
                    assert_eq!(value, Some(content.to_string()), "the content crosses as spelled");
                }
                other => panic!("expected a SpawnResult event, got {other:?}"),
            }
        }
    }

    #[test]
    fn ask_user_question_input_is_normalized_of_injected_answers() {
        let questions = serde_json::json!({"questions": [{"question": "Proceed?"}]});
        let post = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": "s1",
            "tool_name": "AskUserQuestion",
            "tool_input": {
                "questions": [{"question": "Proceed?"}],
                "answers": {"Proceed?": "Yes"},
                "annotations": {"Proceed?": {"notes": "ok"}},
            },
            "tool_response": {"answers": {"Proceed?": "Yes"}},
        });
        match parse_value(&post) {
            Ok(Some(HookEvent::ToolResult { call, .. })) => {
                let stripped: serde_json::Value =
                    serde_json::from_str(call.arguments.get()).expect("the stripped input parses");
                assert_eq!(stripped, questions, "the injected fields are stripped");
            }
            other => panic!("expected a ToolResult event, got {other:?}"),
        }
        let other = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": "s1",
            "tool_name": "SurveyTool",
            "tool_input": {"answers": {"q": "kept"}},
            "tool_response": "done",
        });
        match parse_value(&other) {
            Ok(Some(HookEvent::ToolResult { call, .. })) => {
                assert_eq!(call.arguments.get(), r#"{"answers":{"q":"kept"}}"#);
            }
            other => panic!("expected a ToolResult event, got {other:?}"),
        }
    }

    #[test]
    fn a_failed_tool_run_parses_to_a_typed_failure() {
        for tool in ["Bash", "Agent"] {
            let event = serde_json::json!({
                "hook_event_name": "PostToolUseFailure",
                "session_id": "s1",
                "tool_name": tool,
                "tool_input": {"command": "ls"},
            });
            match parse_value(&event) {
                Ok(Some(HookEvent::ToolResult { outcome, .. })) => assert_eq!(
                    outcome,
                    ToolOutcome::Failure {
                        message: "the tool run failed".to_string(),
                    },
                ),
                other => panic!("expected a ToolResult event for {tool}, got {other:?}"),
            }
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

    fn tool_result(response: serde_json::Value) -> HookEvent {
        HookEvent::ToolResult {
            actor: Actor {
                root: root(),
                child: None,
            },
            call: ProposedCall {
                tool: "Bash".to_string(),
                arguments: raw(serde_json::json!({"command": "cat notes.txt"})),
            },
            outcome: ToolOutcome::Success {
                body: OutcomeBody::Available(response.to_string()),
            },
        }
    }

    fn spawn_result(response: serde_json::Value) -> HookEvent {
        HookEvent::SpawnResult {
            actor: Actor {
                root: root(),
                child: None,
            },
            call: ProposedCall {
                tool: "Agent".to_string(),
                arguments: raw(serde_json::json!({"prompt": "List the files."})),
            },
            outcome: ToolOutcome::Success {
                body: OutcomeBody::Available(response.to_string()),
            },
            child: Some(TrajectoryId("cc:s1:a1".to_string())),
            value: Some("one file: readme.txt".to_string()),
        }
    }

    fn pre_tool_use() -> HookEvent {
        HookEvent::ToolCall {
            actor: Actor {
                root: root(),
                child: None,
            },
            call: ProposedCall {
                tool: "Bash".to_string(),
                arguments: raw(serde_json::json!({"command": "ls"})),
            },
            spawn: false,
        }
    }

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
    fn only_a_fixed_value_under_a_discriminator_key_keeps_its_text() {
        let replacement = swap_leaves(
            "Other",
            serde_json::json!({
                "type": "text",
                "mode": "count",
                "status": "the secret is under status",
                "detail": {"type": "not a fixed value", "status": {"note": "secret", "count": 4}},
                "modes": ["a"],
                "n": null,
            }),
            "x",
        );
        assert_eq!(
            replacement.output,
            serde_json::json!({
                "type": "text",
                "mode": "count",
                "status": "x",
                "detail": {"type": REDACTED, "status": {"note": REDACTED, "count": 0}},
                "modes": [REDACTED],
                "n": null,
            }),
        );
        assert_eq!(replacement.context, None);
    }

    #[test]
    fn a_known_tools_content_field_carries_the_text() {
        assert_eq!(
            swap_leaves(
                "Read",
                serde_json::json!({
                    "type": "text",
                    "file": {"filePath": "/a/very/long/path/to/notes.txt", "content": "hi\n", "numLines": 1},
                }),
                "the output is confined",
            )
            .output,
            serde_json::json!({
                "type": "text",
                "file": {"filePath": REDACTED, "content": "the output is confined", "numLines": 0},
            }),
        );
        assert_eq!(
            swap_leaves(
                "Grep",
                serde_json::json!({"mode": "files_with_matches", "filenames": ["a.rs", "src/b.rs"], "numFiles": 2}),
                "x",
            )
            .output,
            serde_json::json!({"mode": "files_with_matches", "filenames": ["x"], "numFiles": 0}),
        );
    }

    #[test]
    fn an_mcp_result_is_restated_as_one_text_block() {
        let event = HookEvent::ToolResult {
            actor: Actor {
                root: root(),
                child: None,
            },
            call: ProposedCall {
                tool: "mcp__vault__lookup".to_string(),
                arguments: raw(serde_json::json!({"key": "prod"})),
            },
            outcome: ToolOutcome::Success {
                body: OutcomeBody::Available(
                    serde_json::json!([{"type": "text", "text": "sk_live_secret"}, {"sk_live_secret": false}])
                        .to_string(),
                ),
            },
        };
        for decision in [
            HookDecision::Block {
                reason: "nothing crossed".to_string(),
            },
            HookDecision::ReplaceOutput {
                output: "the output is confined".to_string(),
            },
            HookDecision::Refuse {
                detail: "storage failure".to_string(),
            },
        ] {
            let answer = render(&event, &decision);
            assert!(!answer.to_string().contains("sk_live_secret"), "{answer}");
            assert!(answer["hookSpecificOutput"]["updatedToolOutput"].is_array(), "{answer}");
        }
    }

    #[test]
    fn the_text_takes_the_place_of_the_longest_string_once() {
        assert_eq!(
            swap_leaves(
                "Other",
                serde_json::json!({"a": "long/path/b.rs", "b": "short", "c": [{"n": "x"}, {"n": "y"}, {"n": "z"}]}),
                "the output is confined",
            )
            .output,
            serde_json::json!({"a": "the output is confined", "b": REDACTED, "c": [{"n": REDACTED}]}),
        );
        let replacement = swap_leaves(
            "Other",
            serde_json::json!({"type": "text", "count": 2, "hits": []}),
            "x",
        );
        assert_eq!(
            replacement.output,
            serde_json::json!({"type": "text", "count": 0, "hits": []})
        );
        assert_eq!(replacement.context.as_deref(), Some("x"));
        let event = tool_result(serde_json::json!({"count": 2}));
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
                    "updatedToolOutput": {"count": 0},
                    "additionalContext": "the output is confined",
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
            },
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

    #[test]
    fn a_child_return_replaces_the_subagents_message_in_the_spawn_result() {
        let event = spawn_result(agent_response());
        let mut expected = agent_response();
        expected["content"] = serde_json::json!([{"type": "text", "text": "the redacted summary"}]);
        assert_eq!(
            render(
                &event,
                &HookDecision::ChildReturn {
                    value: "the redacted summary".to_string(),
                }
            ),
            serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PostToolUse",
                    "updatedToolOutput": expected,
                }
            }),
        );
        let mut withheld = agent_response();
        withheld["content"] =
            serde_json::json!([{"type": "text", "text": "[appa] the tool result was withheld: nothing crossed"}]);
        assert_eq!(
            render(
                &event,
                &HookDecision::Block {
                    reason: "nothing crossed".to_string(),
                }
            ),
            serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PostToolUse",
                    "updatedToolOutput": withheld,
                },
                "decision": "block",
                "reason": "nothing crossed",
            }),
        );
        assert_eq!(render(&event, &HookDecision::Ack), serde_json::json!({}));
    }
}
