//! Claude Code's hook JSON read into at most one `HookEvent`.

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
