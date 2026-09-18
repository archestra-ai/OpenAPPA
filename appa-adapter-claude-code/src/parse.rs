//! Claude Code's own hook JSON read into at most one `HookEvent`, whose tool
//! spelling is still Claude Code's raw one and whose trajectory ids are derived
//! from Claude Code's own ids under the `cc:` prefix.
//!
//! Hook mapping:
//!
//! | hook | `HookEvent` |
//! |---|---|
//! | `SessionStart` | `SessionStart` |
//! | `UserPromptSubmit` | `Prompt` |
//! | `PreToolUse` | `ToolCall`; the `Agent` (`Task`) tool is the spawn |
//! | `PostToolUse` for `Agent` (`Task`) | `SpawnResult`, naming the subagent (`agentId`) and carrying its message (`content`) where the response has them |
//! | `PostToolUse`, `PostToolUseFailure` | `ToolResult` (the Q14 outcome mapping) |
//! | `SubagentStart` | `ChildStart`, naming the family's spawn in flight |
//! | `SubagentStop` | `ChildEnd` carrying `last_assistant_message` as the return; `TurnEnd` for a helper with an empty `agent_type` |
//! | `Stop`, `StopFailure` | `TurnEnd` for the actor that finished |
//!
//! Every call and result carries Claude Code's opaque `tool_use_id` as
//! its host call identity. The runtime persists that identity beside the
//! opened dispatch, so ordinary calls can run in parallel and report in
//! any order, including after a runtime restart. One exception remains:
//! only one `Agent` (`Task`) spawn may wait for binding at a time. Claude
//! Code's `SubagentStart` names the child but not the `Agent` call that
//! launched it, so two unbound spawns would make that start ambiguous.
//!
//! Subagents. Claude Code spawns a subagent through its `Agent` tool
//! (`Task` is its older name), so the codec marks that call as the
//! deployment's context-controlled spawn; the runtime holds the spawn
//! until the parent declares the child's return from the block's menu.
//! `SubagentStart` names the new subagent (`agent_id`) but not the
//! `Agent` call that started it, so the child start can echo no binding:
//! it names the family's spawn in flight, and the runtime ties it to the
//! one prepared fork still open for binding. Its answer carries the
//! child's return contract as `additionalContext`. The child's
//! `SubagentStop` is the return channel: its `last_assistant_message` is
//! the message the parent receives, and the codec reports it as
//! `ChildEnd` naming the child. A `decision: block` answer there keeps
//! the subagent running with the reason, and it stops again
//! (`stop_hook_active: true`), so a return that may not cross holds the
//! subagent until it returns an admissible message. No hook can
//! substitute what the parent receives, so a return the runtime would
//! substitute (`ChildReturn`) is rendered as a block carrying the exact
//! bytes to return: the subagent echoes them, and the next stop crosses.
//! The parent's `Agent` `PostToolUse` is the spawn outcome: in a
//! top-level session the spawn is asynchronous, the hook fires at launch
//! with `agentId` and no `content`, and its `SpawnResult` binds the fork
//! to that child before the dispatch closes, whichever of it and
//! `SubagentStart` lands first. A synchronous spawn (`claude -p`)
//! delivers the child's message in `content` after the child's stop, and
//! the same `SpawnResult` replays the crossing the stop decided; a
//! message the runtime never checked at a stop is withheld from the
//! parent. Every post-use hook of the spawn's tool is that spawn's
//! result, whatever its response carries: which lifecycle a result runs
//! is the runtime's derivation from the tool, so a response that names
//! no child and carries no message is the same event with both fields
//! empty, never another lifecycle the runtime would then contradict.
//! Claude Code's own helper agents stop with an empty
//! `agent_type`, no `SubagentStart` and no tool calls: their stop is the
//! child's `TurnEnd`, and no return is claimed.
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
//! | no outcome hook at all | nothing is reported; the dispatch stays open until the actor's `TurnEnd`, or the next `Prompt` when the turn was interrupted and sent no `Stop`, closes it as not run |
//!
//! The mapping is total over those shapes. It reads no error shape out
//! of a `tool_response` body: no recorded live example of one exists,
//! and calling a real success a `Failure` would discard effects the
//! tool had.

use serde::Deserialize;

use appa_runtime_api::{
    Actor, HookEvent, OutcomeBody, ParseRefusal, ProposedCall, SpawnRef, ToolOutcome, TrajectoryId,
};

use crate::identity::is_spawn_tool;

#[derive(Debug, Deserialize)]
pub(crate) struct WireEvent {
    hook_event_name: String,
    session_id: String,
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    tool_name: Option<String>,
    /// The JSON spelling in Claude Code's `tool_input` hook field. This is
    /// Claude Code's execution-boundary value, not Anthropic provider wire data.
    #[serde(default)]
    tool_input: Option<Box<serde_json::value::RawValue>>,
    #[serde(default)]
    tool_use_id: Option<String>,
    #[serde(default)]
    tool_response: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<serde_json::Value>,
    #[serde(default)]
    agent_type: Option<String>,
    #[serde(default)]
    last_assistant_message: Option<String>,
}

pub(crate) fn non_empty(text: Option<&str>) -> Option<&str> {
    text.filter(|text| !text.is_empty())
}

impl WireEvent {
    fn root(&self) -> TrajectoryId {
        TrajectoryId(format!("cc:{}", self.session_id))
    }

    fn child_id(&self, agent: &str) -> TrajectoryId {
        TrajectoryId(format!("cc:{}:{agent}", self.session_id))
    }

    /// The subagent a `SubagentStart` or `SubagentStop` names; an empty
    /// id names nothing.
    fn agent(&self) -> Option<&str> {
        non_empty(self.agent_id.as_deref())
    }

    fn actor(&self) -> Actor {
        Actor {
            root: self.root(),
            child: self.agent().map(|agent| self.child_id(agent)),
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

    /// The subagent the parent's `Agent` response names (`agentId`) and
    /// the message it carries (`content`), each where the response has
    /// it: a launch acknowledgement names the child and carries no
    /// message, and a response with neither fills no spawn field. Which
    /// lifecycle the runtime runs is its own derivation from the tool,
    /// never this shape, so an unrecognized response is the spawn's
    /// result with both fields empty rather than another lifecycle.
    fn spawn_return(&self) -> (Option<TrajectoryId>, Option<String>) {
        let Some(response) = self.tool_response.as_ref() else {
            return (None, None);
        };
        let child = response
            .get("agentId")
            .and_then(|id| non_empty(id.as_str()))
            .map(|agent| self.child_id(agent));
        let value = response
            .get("content")
            .filter(|content| !content.is_null())
            .map(|content| match text_blocks(content) {
                Some(texts) => texts.join("\n"),
                None => content.to_string(),
            })
            .filter(|value| !value.is_empty());
        (child, value)
    }
}

pub(crate) fn text_blocks(content: &serde_json::Value) -> Option<Vec<&str>> {
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

pub(crate) fn strip_collected_answers(arguments: Box<serde_json::value::RawValue>) -> Box<serde_json::value::RawValue> {
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

pub(crate) fn malformed(detail: &str) -> ParseRefusal {
    ParseRefusal::Malformed {
        detail: detail.to_string(),
    }
}

pub(crate) fn parse(body: &[u8]) -> Result<Option<HookEvent>, ParseRefusal> {
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
                    call_id: event.tool_use_id.clone(),
                    spawn,
                    ruling: None,
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
                    call_id: event.tool_use_id.clone(),
                    outcome: map_outcome(event.tool_response.as_ref()),
                    child,
                    value,
                }))
            }
            Some(call) => Ok(Some(HookEvent::ToolResult {
                actor: event.actor(),
                call,
                call_id: event.tool_use_id.clone(),
                outcome: map_outcome(event.tool_response.as_ref()),
            })),
            None => Err(malformed("a tool outcome without its tool call")),
        },
        "PostToolUseFailure" => match event.call() {
            Some(call) => Ok(Some(HookEvent::ToolResult {
                actor: event.actor(),
                call,
                call_id: event.tool_use_id.clone(),
                outcome: ToolOutcome::Failure {
                    message: event
                        .error
                        .as_ref()
                        .map(|error| match error {
                            serde_json::Value::String(text) => text.clone(),
                            other => other.to_string(),
                        })
                        .unwrap_or_else(|| "the tool run failed".to_string()),
                },
            })),
            None => Err(malformed("a tool outcome without its tool call")),
        },
        "SubagentStart" => match event.agent() {
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
        "SubagentStop" => match (event.agent(), non_empty(event.agent_type.as_deref())) {
            (None, _) => Err(malformed("SubagentStop without an agent id")),
            (Some(agent), Some(_)) => Ok(Some(HookEvent::ChildEnd {
                root: event.root(),
                child: event.child_id(agent),
                value: non_empty(event.last_assistant_message.as_deref()).map(str::to_string),
            })),
            // A harness-internal helper, not the deployment's spawn: its
            // stop is a turn end and claims no return.
            (Some(agent), None) => Ok(Some(HookEvent::TurnEnd {
                actor: Actor {
                    root: event.root(),
                    child: Some(event.child_id(agent)),
                },
            })),
        },
        other => {
            tracing::debug!(hook = other, "hook event outside the codec's mapping");
            Ok(None)
        }
    }
}

pub(crate) fn map_outcome(response: Option<&serde_json::Value>) -> ToolOutcome {
    match response {
        None => ToolOutcome::Indeterminate,
        Some(response) => ToolOutcome::Success {
            body: OutcomeBody::Available(response.to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::*;
    use appa_runtime_api::{
        Actor, HookEvent, OutcomeBody, ParseRefusal, ProposedCall, SpawnRef, ToolOutcome, TrajectoryId,
    };
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
        let helper = Some(TrajectoryId("cc:s1:a1".to_string()));
        for (hook, child, agent_type) in [
            ("Stop", None, None),
            ("StopFailure", None, None),
            ("SubagentStop", helper.clone(), None),
            ("SubagentStop", helper, Some("")),
        ] {
            let mut body = serde_json::json!({"hook_event_name": hook, "session_id": "s1"});
            if child.is_some() {
                body["agent_id"] = serde_json::Value::String("a1".to_string());
                body["last_assistant_message"] = serde_json::Value::String("a prompt suggestion".to_string());
            }
            if let Some(agent_type) = agent_type {
                body["agent_type"] = serde_json::Value::String(agent_type.to_string());
            }
            let parsed = parse(body.to_string().as_bytes()).expect("the turn end parses");
            assert_eq!(
                parsed,
                Some(HookEvent::TurnEnd {
                    actor: Actor { root: root(), child },
                }),
                "{hook} with agent_type {agent_type:?} did not cross as its actor's turn end",
            );
        }
    }

    #[test]
    fn a_subagent_stop_is_the_childs_return() {
        for (message, value) in [
            (Some("the summary"), Some("the summary".to_string())),
            (Some(""), None),
            (None, None),
        ] {
            let mut stop = serde_json::json!({
                "hook_event_name": "SubagentStop",
                "session_id": "s1",
                "agent_id": "a1",
                "agent_type": "general-purpose",
                "stop_hook_active": false,
            });
            if let Some(message) = message {
                stop["last_assistant_message"] = serde_json::Value::String(message.to_string());
            }
            assert_eq!(
                parse_value(&stop),
                Ok(Some(HookEvent::ChildEnd {
                    root: root(),
                    child: TrajectoryId("cc:s1:a1".to_string()),
                    value,
                })),
                "a stop with message {message:?} did not cross as the child's return",
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
                serde_json::json!({"hook_event_name": "SubagentStart", "session_id": "s1", "agent_id": ""}),
                "SubagentStart without an agent id",
            ),
            (
                serde_json::json!({"hook_event_name": "SubagentStop", "session_id": "s1"}),
                "SubagentStop without an agent id",
            ),
            (
                serde_json::json!({
                    "hook_event_name": "SubagentStop",
                    "session_id": "s1",
                    "agent_id": "",
                    "agent_type": "general-purpose",
                    "last_assistant_message": "the summary",
                }),
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
            "tool_use_id": "toolu-1",
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
                call_id: Some("toolu-1".to_string()),
                spawn: false,
                ruling: None,
            })),
        );
    }

    #[test]
    fn result_hooks_preserve_the_tool_use_id() {
        for hook in ["PostToolUse", "PostToolUseFailure"] {
            let event = serde_json::json!({
                "hook_event_name": hook,
                "session_id": "s1",
                "tool_use_id": "toolu-1",
                "tool_name": "Bash",
                "tool_input": {"command": "ls"},
                "tool_response": {"stdout": "readme.txt"},
            });
            match parse_value(&event) {
                Ok(Some(HookEvent::ToolResult { call_id, .. })) => {
                    assert_eq!(call_id.as_deref(), Some("toolu-1"));
                }
                other => panic!("expected a ToolResult event for {hook}, got {other:?}"),
            }
        }

        let mut event = agent_post_tool_use(agent_response());
        event["tool_use_id"] = serde_json::json!("toolu-agent");
        match parse_value(&event) {
            Ok(Some(HookEvent::SpawnResult { call_id, .. })) => {
                assert_eq!(call_id.as_deref(), Some("toolu-agent"));
            }
            other => panic!("expected a SpawnResult event, got {other:?}"),
        }
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
                call_id: None,
                outcome: ToolOutcome::Success {
                    body: OutcomeBody::Available(response.to_string()),
                },
                child: Some(TrajectoryId("cc:s1:a1".to_string())),
                value: Some("one file: readme.txt".to_string()),
            })),
        );
    }

    #[test]
    fn a_blank_agent_id_names_no_child() {
        let event = agent_post_tool_use(serde_json::json!({"status": "async_launched", "agentId": ""}));
        match parse_value(&event) {
            Ok(Some(HookEvent::SpawnResult { child, value, .. })) => {
                assert_eq!(child, None, "a blank agentId names no child");
                assert_eq!(value, None);
            }
            other => panic!("the spawn's post-use hook is its result: {other:?}"),
        }
        let call = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "agent_id": "",
            "tool_name": "Bash",
            "tool_input": {"command": "ls"},
        });
        match parse_value(&call) {
            Ok(Some(HookEvent::ToolCall { actor, .. })) => assert_eq!(actor.child, None),
            other => panic!("a tool call parses: {other:?}"),
        }
    }

    #[test]
    fn a_launch_acknowledgement_names_the_child_and_carries_no_message() {
        let launched = serde_json::json!({
            "isAsync": true,
            "status": "async_launched",
            "agentId": "a2",
            "description": "Compute 6*7",
            "prompt": "Compute 6*7",
            "outputFile": "/tmp/a2.md",
            "canReadOutputFile": false,
        });
        let mut without_content = launched.clone();
        without_content["content"] = serde_json::Value::Null;
        for (tool, response) in [
            ("Agent", launched.clone()),
            ("Task", launched.clone()),
            ("Agent", without_content),
        ] {
            let mut event = agent_post_tool_use(response.clone());
            event["tool_name"] = serde_json::Value::String(tool.to_string());
            assert_eq!(
                parse_value(&event),
                Ok(Some(HookEvent::SpawnResult {
                    actor: Actor {
                        root: root(),
                        child: None,
                    },
                    call: ProposedCall {
                        tool: tool.to_string(),
                        arguments: raw(serde_json::json!({"prompt": "List the files.", "subagent_type": "Explore"})),
                    },
                    call_id: None,
                    outcome: ToolOutcome::Success {
                        body: OutcomeBody::Available(response.to_string()),
                    },
                    child: Some(TrajectoryId("cc:s1:a2".to_string())),
                    value: None,
                })),
                "the {tool} launch acknowledgement names its child and crosses nothing",
            );
        }
        let mut anonymous = launched;
        anonymous.as_object_mut().expect("an object").remove("agentId");
        match parse_value(&agent_post_tool_use(anonymous)) {
            Ok(Some(HookEvent::SpawnResult { child, value, .. })) => {
                assert_eq!(child, None, "a response naming no subagent names no child");
                assert_eq!(value, None);
            }
            other => panic!("the spawn's post-use hook is its result: {other:?}"),
        }
        let mut undelivered = agent_post_tool_use(serde_json::Value::Null);
        undelivered
            .as_object_mut()
            .expect("the fixture is an object")
            .remove("tool_response");
        match parse_value(&undelivered) {
            Ok(Some(HookEvent::SpawnResult {
                outcome, child, value, ..
            })) => {
                assert_eq!(outcome, ToolOutcome::Indeterminate);
                assert_eq!((child, value), (None, None));
            }
            other => panic!("the spawn's post-use hook is its result: {other:?}"),
        }
    }

    #[test]
    fn an_anonymous_or_empty_agent_result_carries_what_it_spells() {
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
    fn a_failed_edit_preserves_the_native_error_observation() {
        let event = serde_json::json!({
            "hook_event_name": "PostToolUseFailure",
            "session_id": "s1",
            "tool_name": "Edit",
            "tool_input": {"file_path": "/work/secret.txt"},
            "error": "old_string matched private content twice",
        });
        assert!(matches!(parse_value(&event), Ok(Some(HookEvent::ToolResult {
            outcome: ToolOutcome::Failure { message }, ..
        })) if message == "old_string matched private content twice"));
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

    /// The codec's reading of a post-use hook and the derivation that decides the
    /// lifecycle agree on every response shape: under the spawn's tools the event is the
    /// spawn's result, with the child and the returned message each present only where the
    /// response has one, and under any other tool it is never one.
    #[test]
    fn every_post_use_of_the_spawns_tool_is_a_spawn_result() {
        let responses = [
            agent_response(),
            serde_json::json!({"agentId": "a1"}),
            serde_json::json!({"content": [{"type": "text", "text": "x"}]}),
            serde_json::json!({"result": "the secret"}),
            serde_json::json!({}),
            serde_json::Value::Null,
        ];
        for tool in ["Agent", "Task", "Bash"] {
            let spawn = derived(tool).expect("derives").spawn;
            for response in &responses {
                let mut event = agent_post_tool_use(response.clone());
                event["tool_name"] = serde_json::Value::String(tool.to_string());
                let parsed = parse_value(&event);
                assert_eq!(
                    matches!(parsed, Ok(Some(HookEvent::SpawnResult { .. }))),
                    spawn,
                    "{tool} on {response}: the codec and the derivation disagree ({parsed:?})",
                );
            }
        }
    }
}
