//! The hook dispatcher: one typed `HookEvent` in, one `HookDecision`
//! out.

use appa_runtime_api::{Actor, Codec, HookDecision, HookEvent, ParseRefusal};

use crate::api::{
    ChildReturnDecision, EventError, Runtime, Session, SessionError, ToolCallDecision, ToolResultDecision,
    is_control_tool,
};

/// One hook call, wire to wire: parse through the codec, dispatch,
/// render back, with the HTTP status the answer travels under. A
/// non-2xx status makes the hook command exit 2, which blocks the
/// action — hooks fail closed.
pub async fn answer(runtime: &Runtime, codec: &Codec, body: &[u8]) -> (u16, serde_json::Value) {
    let event = match (codec.parse)(body) {
        Ok(Some(event)) => event,
        Ok(None) => return (200, serde_json::json!({})),
        Err(ParseRefusal::Unreadable { detail }) => return (400, serde_json::json!({ "error": detail })),
        Err(ParseRefusal::Malformed { detail }) => return (409, serde_json::json!({ "error": detail })),
    };
    let decision = handle(runtime, event).await;
    let status = match decision {
        HookDecision::Refuse { .. } => 409,
        _ => 200,
    };
    (status, (codec.render)(&decision))
}

/// Dispatch one typed event to its session and fold the outcome into
/// one decision. The dispatcher holds nothing between calls; every id
/// it needs is in the event or in the runtime's persistence.
pub async fn handle(runtime: &Runtime, event: HookEvent) -> HookDecision {
    match event {
        HookEvent::SessionStart { root } => match open_or_reopen(runtime, &root) {
            Ok(_) => HookDecision::Ack,
            Err(error) => refuse(error.to_string()),
        },
        HookEvent::Prompt { actor, text } => {
            let mut session = match event_session(runtime, &actor) {
                Ok(session) => session,
                Err(error) => return fold(error, block),
            };
            match session.on_prompt(text) {
                Ok(()) => HookDecision::Ack,
                Err(error) => fold(error, block),
            }
        }
        HookEvent::ToolCall { actor, call } => {
            if is_control_tool(&call.tool) {
                tracing::debug!(trajectory = %actor.root.0, "control tool passes unchecked");
                return HookDecision::PassControl;
            }
            let mut session = match event_session(runtime, &actor) {
                Ok(session) => session,
                Err(error) => return fold(error, deny),
            };
            match session.on_tool_call(call).await {
                Ok(ToolCallDecision::Allow) => HookDecision::AllowCall,
                Ok(ToolCallDecision::Control) => HookDecision::PassControl,
                Ok(ToolCallDecision::Deny { feedback }) => HookDecision::DenyCall { feedback },
                Err(error) => fold(error, deny),
            }
        }
        HookEvent::ToolResult { actor, call, outcome } => {
            if is_control_tool(&call.tool) {
                tracing::debug!(trajectory = %actor.root.0, "control tool outcome absorbed");
                return HookDecision::Ack;
            }
            let mut session = match event_session(runtime, &actor) {
                Ok(session) => session,
                Err(error) => return fold(error, block),
            };
            match session.on_tool_result(call, outcome).await {
                Ok(ToolResultDecision::Keep) => HookDecision::Ack,
                Ok(ToolResultDecision::Replace { placeholder }) => HookDecision::ReplaceOutput { output: placeholder },
                Err(error) => fold(error, block),
            }
        }
        HookEvent::ChildStart { parent, child } => {
            let mut parent = match open_or_reopen(runtime, &parent) {
                Ok(session) => session,
                Err(error) => return refuse(error.to_string()),
            };
            match parent.on_child_start(child.clone()) {
                Ok(_) => HookDecision::Ack,
                Err(EventError::TrajectoryExists) => match runtime.session(&child) {
                    Ok(_) => HookDecision::Ack,
                    Err(error) => refuse(error.to_string()),
                },
                Err(error) => refuse(error.to_string()),
            }
        }
        HookEvent::ChildEnd { parent, child, value } => {
            let mut child = match child_session(runtime, &parent, &child) {
                Ok(session) => session,
                Err(error) => return fold(error, block),
            };
            let said = value.clone();
            match child.on_child_end(value).await {
                Ok(ChildReturnDecision::Returned { value }) if said.as_deref() != Some(value.as_str()) => {
                    HookDecision::ChildReturn { value }
                }
                Ok(ChildReturnDecision::Returned { .. }) | Ok(ChildReturnDecision::NoValue) => HookDecision::Ack,
                Ok(ChildReturnDecision::Blocked { feedback }) => block(feedback),
                Err(error) => fold(error, block),
            }
        }
    }
}

fn open_or_reopen(runtime: &Runtime, id: &appa_runtime_api::TrajectoryId) -> Result<Session, SessionError> {
    match runtime.session(id) {
        Ok(session) => Ok(session),
        Err(SessionError::Unknown) => match runtime.create_session(id.clone()) {
            Ok(session) => Ok(session),
            Err(SessionError::AlreadyExists) => runtime.session(id),
            Err(error) => Err(error),
        },
        Err(error) => Err(error),
    }
}

fn child_session(
    runtime: &Runtime,
    parent: &appa_runtime_api::TrajectoryId,
    child: &appa_runtime_api::TrajectoryId,
) -> Result<Session, EventError> {
    match runtime.session(child) {
        Ok(session) => Ok(session),
        Err(SessionError::Unknown) => {
            let mut parent = open_or_reopen(runtime, parent).map_err(EventError::from)?;
            match parent.on_child_start(child.clone()) {
                Ok(session) => Ok(session),
                // A concurrent event won the opening race.
                Err(EventError::TrajectoryExists) => runtime.session(child).map_err(EventError::from),
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(EventError::from(error)),
    }
}

fn event_session(runtime: &Runtime, actor: &Actor) -> Result<Session, EventError> {
    match &actor.child {
        Some(child) => child_session(runtime, &actor.root, child),
        None => open_or_reopen(runtime, &actor.root).map_err(EventError::from),
    }
}

fn fold(error: EventError, family: fn(String) -> HookDecision) -> HookDecision {
    if error.is_operational() {
        refuse(error.to_string())
    } else {
        family(error.to_string())
    }
}

fn deny(feedback: String) -> HookDecision {
    HookDecision::DenyCall { feedback }
}

fn block(reason: String) -> HookDecision {
    HookDecision::Block { reason }
}

fn refuse(detail: String) -> HookDecision {
    HookDecision::Refuse { detail }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Runtime, testing};
    use crate::config::Config;

    fn codec() -> Codec {
        appa_adapter_claude_code::codec()
    }

    fn config() -> Config {
        let text = r#"
            [policy]
            version = 1
            [externals]
            timeout_ms = 1000
            max_body_bytes = 4096
        "#;
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.toml");
        std::fs::write(&path, text).expect("the fixture writes");
        Config::load(&path).expect("the minimal fixture validates")
    }

    fn open_test_runtime(dir: &tempfile::TempDir) -> Runtime {
        testing::runtime(config(), dir.path().join("appa.db"))
    }

    async fn call_hook(runtime: &Runtime, body: &[u8]) -> (u16, serde_json::Value) {
        answer(runtime, &codec(), body).await
    }

    const CONTROL_TOOL_FIXTURE_NAME: &str = "mcp__plugin_appa-runtime_appa__execute_remedy_plan";

    fn fixtures() -> Vec<serde_json::Value> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hooks.jsonl");
        std::fs::read_to_string(path)
            .expect("the recorded hook fixtures are readable")
            .lines()
            .map(|line| serde_json::from_str(line).expect("each fixture line is JSON"))
            .collect()
    }

    #[tokio::test]
    async fn the_recorded_session_replays_end_to_end() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        for event in fixtures() {
            match event["hook_event_name"].as_str().expect("each fixture names its hook") {
                "UserPromptSubmit" => {}
                "PreToolUse" if event["tool_name"] != CONTROL_TOOL_FIXTURE_NAME => testing::enqueue_release(
                    &runtime,
                    &format!("d-{}", event["tool_use_id"].as_str().unwrap_or("fixture")),
                    event["tool_name"].as_str().expect("the fixture names a tool"),
                    &event["tool_input"],
                ),
                "PostToolUse" if event["tool_name"] != CONTROL_TOOL_FIXTURE_NAME => {
                    testing::enqueue_done(&runtime);
                    testing::enqueue_keep_output(&runtime);
                }
                "SubagentStart" => testing::enqueue_done(&runtime),
                "SubagentStop" => {
                    testing::enqueue_value(&runtime, event["last_assistant_message"].as_str().unwrap_or_default())
                }
                _ => {}
            }
            let body = serde_json::to_vec(&event).expect("the fixture re-serializes");
            let (status, answer) = call_hook(&runtime, &body).await;
            assert_eq!(status, 200, "hook {} refused: {}", event["hook_event_name"], answer,);
            match event["hook_event_name"].as_str().expect("named hook") {
                "PreToolUse" => {
                    let reason = if event["tool_name"] == CONTROL_TOOL_FIXTURE_NAME {
                        "appa: the runtime's own control tool"
                    } else {
                        "appa: the call is released"
                    };
                    assert_eq!(
                        answer,
                        serde_json::json!({
                            "hookSpecificOutput": {
                                "hookEventName": "PreToolUse",
                                "permissionDecision": "allow",
                                "permissionDecisionReason": reason,
                            }
                        }),
                        "the call must render as exactly its allow answer",
                    );
                }
                other => assert_eq!(
                    answer,
                    serde_json::json!({}),
                    "the {other} hook answers with no opinion",
                ),
            }
        }
    }

    #[tokio::test]
    async fn a_deny_renders_in_the_pre_tool_use_wire() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        testing::enqueue_deny(&runtime, "blocked: the recipient cannot read this", &["offer-1"]);
        let event = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "tool_name": "Bash",
            "tool_input": {"command": "ls"},
            "tool_use_id": "toolu_1",
        });
        let (status, answer) = call_hook(&runtime, &serde_json::to_vec(&event).expect("serializes")).await;
        assert_eq!(status, 200);
        assert_eq!(
            answer,
            serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": "blocked: the recipient cannot read this",
                }
            }),
        );
    }

    #[tokio::test]
    async fn an_event_error_renders_as_a_deny() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        testing::enqueue_release(&runtime, "d1", "Bash", &serde_json::json!({"command": "ls"}));
        let event = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "tool_name": "Bash",
            "tool_input": {"command": "ls"},
        });
        let body = serde_json::to_vec(&event).expect("serializes");
        assert_eq!(call_hook(&runtime, &body).await.0, 200);

        let second = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "tool_name": "Write",
            "tool_input": {"file_path": "/tmp/x", "content": "y"},
        });
        let (status, answer) = call_hook(&runtime, &serde_json::to_vec(&second).expect("serializes")).await;
        assert_eq!(status, 200);
        assert_eq!(
            answer["hookSpecificOutput"]["permissionDecision"], "deny",
            "lifecycle misuse renders as a deny",
        );
    }

    #[tokio::test]
    async fn a_replaced_output_renders_as_a_block_with_the_placeholder() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        testing::enqueue_release(&runtime, "d1", "Read", &serde_json::json!({"file_path": "secret.txt"}));
        let pre = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "tool_name": "Read",
            "tool_input": {"file_path": "secret.txt"},
        });
        call_hook(&runtime, &serde_json::to_vec(&pre).expect("serializes")).await;

        testing::enqueue_done(&runtime);
        testing::enqueue_replace_output(&runtime, "the output is confined");
        let post = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": "s1",
            "tool_name": "Read",
            "tool_input": {"file_path": "secret.txt"},
            "tool_response": {"content": "the secret"},
        });
        let (_, answer) = call_hook(&runtime, &serde_json::to_vec(&post).expect("serializes")).await;
        assert_eq!(
            answer,
            serde_json::json!({"decision": "block", "reason": "the output is confined"}),
        );
    }

    #[tokio::test]
    async fn the_control_tool_opens_no_dispatch() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let event = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "tool_name": "mcp__appa__execute_remedy_plan",
            "tool_input": {"offer_id": "offer-1"},
        });
        let (status, answer) = call_hook(&runtime, &serde_json::to_vec(&event).expect("serializes")).await;
        assert_eq!(status, 200);
        assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "allow");
        assert_eq!(
            answer["hookSpecificOutput"]["permissionDecisionReason"],
            "appa: the runtime's own control tool",
        );
        testing::enqueue_release(&runtime, "d1", "Bash", &serde_json::json!({"command": "ls"}));
        let call = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "tool_name": "Bash",
            "tool_input": {"command": "ls"},
        });
        let (_, answer) = call_hook(&runtime, &serde_json::to_vec(&call).expect("serializes")).await;
        assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "allow");
    }

    #[tokio::test]
    async fn a_lookalike_control_tool_is_checked() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        testing::enqueue_deny(&runtime, "blocked: not the runtime's tool", &[]);
        let event = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "tool_name": "mcp__evil__execute_remedy_plan",
            "tool_input": {"offer_id": "whatever"},
        });
        let (_, answer) = call_hook(&runtime, &serde_json::to_vec(&event).expect("serializes")).await;
        assert_eq!(
            answer["hookSpecificOutput"]["permissionDecision"], "deny",
            "a colliding name must reach the engine, not the exemption",
        );
    }

    #[tokio::test]
    async fn a_control_tools_post_hook_answers_with_no_opinion() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let event = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": "s1",
            "tool_name": "mcp__plugin_appa-runtime_appa__execute_remedy_plan",
            "tool_input": {"offer_id": "offer-1"},
            "tool_response": {"content": "Authorized."},
        });
        let (status, answer) = call_hook(&runtime, &serde_json::to_vec(&event).expect("serializes")).await;
        assert_eq!(status, 200);
        assert_eq!(answer, serde_json::json!({}));
    }

    #[tokio::test]
    async fn a_control_call_passes_even_on_an_ended_trajectory() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        testing::enqueue_done(&runtime);
        let start = serde_json::json!({
            "hook_event_name": "SubagentStart",
            "session_id": "s1",
            "agent_id": "a1",
        });
        assert_eq!(
            call_hook(&runtime, &serde_json::to_vec(&start).expect("serializes"))
                .await
                .0,
            200
        );
        testing::enqueue_value(&runtime, "done");
        let stop = serde_json::json!({
            "hook_event_name": "SubagentStop",
            "session_id": "s1",
            "agent_id": "a1",
            "last_assistant_message": "done",
        });
        assert_eq!(
            call_hook(&runtime, &serde_json::to_vec(&stop).expect("serializes"))
                .await
                .0,
            200
        );

        let control = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "agent_id": "a1",
            "tool_name": "execute_remedy_plan",
            "tool_input": {"offer_id": "offer-1"},
        });
        let (status, answer) = call_hook(&runtime, &serde_json::to_vec(&control).expect("serializes")).await;
        assert_eq!(status, 200);
        assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "allow");

        let outcome = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": "s1",
            "agent_id": "a1",
            "tool_name": "execute_remedy_plan",
            "tool_input": {"offer_id": "offer-1"},
            "tool_response": {"ok": true},
        });
        let (status, answer) = call_hook(&runtime, &serde_json::to_vec(&outcome).expect("serializes")).await;
        assert_eq!(status, 200);
        assert_eq!(
            answer,
            serde_json::json!({}),
            "the control outcome is absorbed on an ended trajectory"
        );
    }

    #[tokio::test]
    async fn only_a_substituted_child_return_answers_with_the_admitted_value() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let root = appa_runtime_api::TrajectoryId("cc:s1".to_string());

        let end = |child: &str, said: &str| HookEvent::ChildEnd {
            parent: root.clone(),
            child: appa_runtime_api::TrajectoryId(format!("cc:s1:{child}")),
            value: Some(said.to_string()),
        };

        testing::enqueue_done(&runtime);
        testing::enqueue_value(&runtime, "the report, with the account numbers removed");
        assert_eq!(
            handle(&runtime, end("a1", "the report, account 4821-9930")).await,
            HookDecision::ChildReturn {
                value: "the report, with the account numbers removed".to_string(),
            },
        );

        testing::enqueue_done(&runtime);
        testing::enqueue_value(&runtime, "the report, nothing sensitive");
        assert_eq!(
            handle(&runtime, end("a2", "the report, nothing sensitive")).await,
            HookDecision::Ack,
        );
    }

    #[tokio::test]
    async fn an_ended_parent_blocks_its_unknown_childs_return() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let root = appa_runtime_api::TrajectoryId("cc:s1".to_string());
        let ended = appa_runtime_api::TrajectoryId("cc:s1:a1".to_string());

        testing::enqueue_done(&runtime);
        assert_eq!(
            handle(
                &runtime,
                HookEvent::ChildStart {
                    parent: root.clone(),
                    child: ended.clone(),
                },
            )
            .await,
            HookDecision::Ack,
        );
        testing::enqueue_value(&runtime, "done");
        assert_eq!(
            handle(
                &runtime,
                HookEvent::ChildEnd {
                    parent: root.clone(),
                    child: ended.clone(),
                    value: Some("done".to_string()),
                },
            )
            .await,
            HookDecision::Ack,
        );

        let decision = handle(
            &runtime,
            HookEvent::ChildEnd {
                parent: ended,
                child: appa_runtime_api::TrajectoryId("cc:s1:a1:b1".to_string()),
                value: Some("late".to_string()),
            },
        )
        .await;
        assert!(
            matches!(decision, HookDecision::Block { .. }),
            "an ended trajectory blocks; only an operational failure refuses. Got {decision:?}",
        );
    }

    #[tokio::test]
    async fn an_operational_failure_refuses_instead_of_answering_the_model() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        testing::fail_next_commit(&runtime);
        let prompt = serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "s1",
            "prompt": "read the report",
        });
        let (status, answer) = call_hook(&runtime, &serde_json::to_vec(&prompt).expect("serializes")).await;
        assert_eq!(status, 409, "the harness must fail closed on a storage failure");
        assert!(
            answer.get("error").is_some(),
            "an operational failure renders as a refusal, never as model-facing feedback: {answer}",
        );

        testing::enqueue_done(&runtime);
        let call = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "tool_name": "Bash",
            "tool_input": {"command": "ls"},
        });
        let (status, answer) = call_hook(&runtime, &serde_json::to_vec(&call).expect("serializes")).await;
        assert_eq!(status, 409, "an undeliverable decision refuses rather than denying");
        assert!(answer.get("error").is_some(), "got {answer}");
    }

    #[tokio::test]
    async fn an_unreadable_hook_event_is_a_400() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let (status, answer) = call_hook(&runtime, b"not json").await;
        assert_eq!(status, 400);
        assert!(
            answer["error"]
                .as_str()
                .expect("the refusal names its cause")
                .starts_with("unreadable hook event: "),
        );
    }

    #[tokio::test]
    async fn a_malformed_hook_event_is_a_409() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let event = serde_json::json!({"hook_event_name": "PreToolUse", "session_id": "s1"});
        let (status, answer) = call_hook(&runtime, &serde_json::to_vec(&event).expect("serializes")).await;
        assert_eq!(status, 409);
        assert_eq!(answer, serde_json::json!({"error": "PreToolUse without a tool call"}));
    }
}
