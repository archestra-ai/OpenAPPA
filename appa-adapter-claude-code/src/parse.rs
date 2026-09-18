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
