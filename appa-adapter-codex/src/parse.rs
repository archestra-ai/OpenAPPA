use appa_runtime_api::{
    Actor, HookEvent, OutcomeBody, ParseRefusal, ProposedCall, SpawnRef, ToolOutcome, TrajectoryId,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Input {
    hook_event_name: String,
    session_id: String,
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    tool_use_id: Option<String>,
    #[serde(default)]
    tool_input: Option<Box<serde_json::value::RawValue>>,
    #[serde(default)]
    tool_response: Option<serde_json::Value>,
    #[serde(default)]
    last_assistant_message: Option<String>,
}

fn malformed(detail: &str) -> ParseRefusal {
    ParseRefusal::Malformed { detail: detail.into() }
}

impl Input {
    fn root(&self) -> TrajectoryId {
        TrajectoryId(format!("codex:{}", self.session_id))
    }

    fn child(&self) -> Option<TrajectoryId> {
        self.agent_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .map(|id| TrajectoryId(format!("codex:{}:{id}", self.session_id)))
    }

    fn actor(&self) -> Actor {
        Actor {
            root: self.root(),
            child: self.child(),
        }
    }

    fn call(&self) -> Result<ProposedCall, ParseRefusal> {
        let raw = self
            .tool_name
            .as_deref()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| malformed("tool hook has no tool_name"))?;
        Ok(ProposedCall {
            tool: if raw == "Bash" { "appa_exec" } else { raw }.to_owned(),
            arguments: self
                .tool_input
                .clone()
                .ok_or_else(|| malformed("tool hook has no tool_input"))?,
            cwd: self.cwd.clone().filter(|cwd| !cwd.is_empty()),
        })
    }

    fn call_id(&self) -> Result<Option<String>, ParseRefusal> {
        Ok(Some(
            self.tool_use_id
                .clone()
                .filter(|id| !id.is_empty())
                .ok_or_else(|| malformed("tool hook has no tool_use_id"))?,
        ))
    }
}

pub(crate) fn parse(body: &[u8]) -> Result<Option<HookEvent>, ParseRefusal> {
    let input: Input = serde_json::from_slice(body).map_err(|error| ParseRefusal::Unreadable {
        detail: error.to_string(),
    })?;
    if input.session_id.is_empty() {
        return Err(malformed("hook has no session_id"));
    }
    let event = match input.hook_event_name.as_str() {
        "SessionStart" => HookEvent::SessionStart {
            root: input.root(),
            principal: None,
        },
        "UserPromptSubmit" => HookEvent::Prompt {
            actor: input.actor(),
            text: input
                .prompt
                .clone()
                .ok_or_else(|| malformed("prompt hook has no prompt"))?,
        },
        "PreToolUse" => {
            let call = input.call()?;
            let spawn = call.tool == "spawn_agent";
            HookEvent::ToolCall {
                actor: input.actor(),
                call,
                call_id: input.call_id()?,
                spawn,
                ruling: None,
            }
        }
        "PostToolUse" => {
            let call = input.call()?;
            let outcome = match input.tool_response.as_ref() {
                None => ToolOutcome::Indeterminate,
                Some(response) if call.tool.starts_with("mcp__") && mcp_error(response) => ToolOutcome::Indeterminate,
                Some(response) => ToolOutcome::Success {
                    body: OutcomeBody::Available(
                        response
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| response.to_string()),
                    ),
                },
            };
            if call.tool == "spawn_agent" {
                let child = input
                    .tool_response
                    .as_ref()
                    .and_then(|response| response.get("agent_id").or_else(|| response.get("agentId")))
                    .and_then(serde_json::Value::as_str)
                    .filter(|id| !id.is_empty())
                    .map(|id| TrajectoryId(format!("codex:{}:{id}", input.session_id)));
                HookEvent::SpawnResult {
                    actor: input.actor(),
                    call,
                    call_id: input.call_id()?,
                    outcome,
                    child,
                    value: None,
                }
            } else {
                HookEvent::ToolResult {
                    actor: input.actor(),
                    call,
                    call_id: input.call_id()?,
                    outcome,
                }
            }
        }
        "SubagentStart" => HookEvent::ChildStart {
            root: input.root(),
            child: input
                .child()
                .ok_or_else(|| malformed("subagent start has no agent_id"))?,
            spawn: SpawnRef::InFlight,
        },
        "SubagentStop" => HookEvent::ChildEnd {
            root: input.root(),
            child: input
                .child()
                .ok_or_else(|| malformed("subagent stop has no agent_id"))?,
            value: input.last_assistant_message.clone(),
        },
        "Stop" => HookEvent::TurnEnd { actor: input.actor() },
        "SessionEnd" | "Interrupt" | "PreCompact" | "PostCompact" | "PermissionRequest" => return Ok(None),
        _ => return Err(malformed("unsupported Codex hook event")),
    };
    Ok(Some(event))
}

fn mcp_error(response: &serde_json::Value) -> bool {
    response
        .get("isError")
        .or_else(|| response.get("is_error"))
        .and_then(serde_json::Value::as_bool)
        == Some(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_keep_host_identity_and_arguments() {
        let body = br#"{"hook_event_name":"PreToolUse","session_id":"s1","tool_name":"Bash","tool_use_id":"c1","tool_input":{"command":"echo hi"},"cwd":"/tmp"}"#;
        let event = parse(body).unwrap().unwrap();
        let HookEvent::ToolCall {
            actor, call, call_id, ..
        } = event
        else {
            panic!("tool call")
        };
        assert_eq!(actor.root.0, "codex:s1");
        assert_eq!(call_id.as_deref(), Some("c1"));
        assert_eq!(call.cwd.as_deref(), Some("/tmp"));
        assert_eq!(call.tool, "appa_exec");
        assert_eq!(call.arguments.get(), r#"{"command":"echo hi"}"#);
    }

    #[test]
    fn a_codex_string_result_is_not_json_quoted() {
        let body = br#"{"hook_event_name":"PostToolUse","session_id":"s1","tool_name":"Bash","tool_use_id":"c1","tool_input":{"command":"echo hi"},"tool_response":"hello\n"}"#;
        let event = parse(body).unwrap().unwrap();
        let HookEvent::ToolResult { outcome, .. } = event else {
            panic!("tool result")
        };
        assert_eq!(
            outcome,
            ToolOutcome::Success {
                body: OutcomeBody::Available("hello\n".into())
            }
        );
    }

    #[test]
    fn an_observed_mcp_error_keeps_effects_unsettled() {
        for spelling in ["isError", "is_error"] {
            let mut response = serde_json::json!({"content": [{"type":"text", "text":"partial write"}]});
            response[spelling] = serde_json::json!(true);
            let body = serde_json::json!({
                "hook_event_name": "PostToolUse", "session_id": "s1", "tool_name": "mcp__fs__write",
                "tool_use_id": "c1", "tool_input": {"path": "x"},
                "tool_response": response,
            });
            let event = parse(&serde_json::to_vec(&body).unwrap()).unwrap().unwrap();
            let HookEvent::ToolResult { outcome, .. } = event else {
                panic!("tool result")
            };
            assert_eq!(outcome, ToolOutcome::Indeterminate);
        }
    }

    #[test]
    fn invalid_required_fields_refuse() {
        for body in [
            br#"{"hook_event_name":"PreToolUse","session_id":"s1","tool_name":"Bash","tool_input":{}}"#.as_slice(),
            br#"{"hook_event_name":"PostToolUse","session_id":"s1","tool_use_id":"c1","tool_name":"Bash"}"#,
        ] {
            assert!(parse(body).is_err());
        }
    }
}
