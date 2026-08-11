//! The Claude Code adapter: hook JSON to `Session` calls and back.

use serde::Deserialize;

use crate::api::{
    ChildTask, EventError, OutcomeBody, ProposedCall, Runtime, Session, SessionError, ToolCallDecision, ToolOutcome,
    ToolResultDecision, TrajectoryId,
};

/// What the HTTP layer sends back for one hook call: a status and the
/// JSON body the hook command prints on stdout. A non-2xx status makes
/// the hook command exit 2, which blocks the action — hooks fail
/// closed.
#[derive(Debug, Clone, PartialEq)]
pub struct HookAnswer {
    pub status: u16,
    pub body: serde_json::Value,
}

impl HookAnswer {
    fn ok(body: serde_json::Value) -> HookAnswer {
        HookAnswer { status: 200, body }
    }

    fn refused(detail: String) -> HookAnswer {
        HookAnswer {
            status: 409,
            body: serde_json::json!({ "error": detail }),
        }
    }
}

#[derive(Debug, Deserialize)]
struct HookEvent {
    hook_event_name: String,
    session_id: String,
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    agent_type: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    tool_input: Option<serde_json::Value>,
    #[serde(default)]
    tool_response: Option<serde_json::Value>,
    #[serde(default)]
    last_assistant_message: Option<String>,
}

impl HookEvent {
    fn root(&self) -> TrajectoryId {
        TrajectoryId(format!("cc:{}", self.session_id))
    }

    fn trajectory(&self) -> TrajectoryId {
        match &self.agent_id {
            Some(agent) => TrajectoryId(format!("cc:{}:{agent}", self.session_id)),
            None => self.root(),
        }
    }
}

/// Handle one hook call: parse, route to the Session event, render the
/// decision. The adapter holds nothing between calls; every id it
/// needs is in the event or in the runtime's persistence.
pub async fn handle_hook(runtime: &Runtime, body: &[u8]) -> HookAnswer {
    let event: HookEvent = match serde_json::from_slice(body) {
        Ok(event) => event,
        Err(error) => {
            return HookAnswer {
                status: 400,
                body: serde_json::json!({ "error": format!("unreadable hook event: {error}") }),
            };
        }
    };
    tracing::debug!(hook = %event.hook_event_name, session = %event.session_id, "hook event");
    match event.hook_event_name.as_str() {
        "SessionStart" => on_session_start(runtime, &event),
        "UserPromptSubmit" => on_user_prompt(runtime, &event),
        "PreToolUse" => on_pre_tool_use(runtime, &event).await,
        "PostToolUse" | "PostToolUseFailure" => on_post_tool_use(runtime, &event).await,
        "SubagentStart" => on_subagent_start(runtime, &event),
        "SubagentStop" => on_subagent_stop(runtime, &event).await,
        "Stop" => HookAnswer::ok(serde_json::json!({})),
        other => {
            tracing::debug!(hook = other, "hook event outside the adapter's mapping");
            HookAnswer::ok(serde_json::json!({}))
        }
    }
}

fn open_or_reopen(runtime: &Runtime, id: &TrajectoryId) -> Result<Session, SessionError> {
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

fn child_session(runtime: &Runtime, event: &HookEvent) -> Result<Session, EventError> {
    let id = event.trajectory();
    match runtime.session(&id) {
        Ok(session) => Ok(session),
        Err(SessionError::Unknown) => {
            let mut parent =
                open_or_reopen(runtime, &event.root()).map_err(|error| EventError::Storage(error.to_string()))?;
            let task = child_task(&parent, event);
            match parent.on_child_start(id.clone(), task) {
                Ok(child) => Ok(child),
                // A concurrent event won the opening race.
                Err(EventError::TrajectoryExists) => runtime.session(&id).map_err(|error| match error {
                    SessionError::Ended => EventError::TrajectoryEnded,
                    error => EventError::Storage(error.to_string()),
                }),
                Err(error) => Err(error),
            }
        }
        Err(SessionError::Ended) => Err(EventError::TrajectoryEnded),
        Err(error) => Err(EventError::Storage(error.to_string())),
    }
}

fn child_task(parent: &Session, event: &HookEvent) -> ChildTask {
    let fallback = || event.agent_type.clone().unwrap_or_else(|| "agent".to_string());
    let Ok(Some(open)) = parent.open_dispatch() else {
        return ChildTask(fallback());
    };
    if open.tool != "Agent" && open.tool != "Task" {
        return ChildTask(fallback());
    }
    let Ok(call) = serde_json::from_slice::<serde_json::Value>(&open.bytes) else {
        return ChildTask(fallback());
    };
    let text = call["arguments"]["prompt"]
        .as_str()
        .or_else(|| call["arguments"]["description"].as_str());
    ChildTask(text.map(str::to_string).unwrap_or_else(fallback))
}

fn event_session(runtime: &Runtime, event: &HookEvent) -> Result<Session, EventError> {
    if event.agent_id.is_some() {
        child_session(runtime, event)
    } else {
        match open_or_reopen(runtime, &event.root()) {
            Ok(session) => Ok(session),
            Err(SessionError::Ended) => Err(EventError::TrajectoryEnded),
            Err(error) => Err(EventError::Storage(error.to_string())),
        }
    }
}

fn on_session_start(runtime: &Runtime, event: &HookEvent) -> HookAnswer {
    match open_or_reopen(runtime, &event.root()) {
        Ok(_) => HookAnswer::ok(serde_json::json!({})),
        Err(error) => HookAnswer::refused(error.to_string()),
    }
}

fn on_user_prompt(runtime: &Runtime, event: &HookEvent) -> HookAnswer {
    let Some(prompt) = event.prompt.clone() else {
        return HookAnswer::refused("UserPromptSubmit without a prompt".to_string());
    };
    let mut session = match event_session(runtime, event) {
        Ok(session) => session,
        Err(error) => return block(&error.to_string()),
    };
    match session.on_prompt(prompt) {
        Ok(()) => HookAnswer::ok(serde_json::json!({})),
        Err(error) => block(&error.to_string()),
    }
}

/// The runtime-owned control tool. Selecting an offer is
/// not a checked flow — the `ExecuteOffer` event it triggers is
/// — and the live harness delivers no PostToolUse for it,
/// so a dispatch opened here would never close, and every later call
/// in the session would be refused with `CallOutstanding`.
///
/// Only the exact names of the runtime's own endpoint qualify: the
/// plugin ships the MCP server under the key `appa`, so its tool is
/// `mcp__appa__execute_remedy_plan`. A lookalike on another server —
/// `mcp__evil__execute_remedy_plan` — is an ordinary tool call and is
/// checked like one.
fn is_control_tool(tool: &str) -> bool {
    tool == "execute_remedy_plan" || tool == "mcp__appa__execute_remedy_plan"
}

async fn on_pre_tool_use(runtime: &Runtime, event: &HookEvent) -> HookAnswer {
    let (Some(tool), Some(arguments)) = (event.tool_name.clone(), event.tool_input.clone()) else {
        return HookAnswer::refused("PreToolUse without a tool call".to_string());
    };
    if is_control_tool(&tool) {
        return HookAnswer::ok(serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "allow",
                "permissionDecisionReason": "appa: the runtime's own control tool",
            }
        }));
    }
    let mut session = match event_session(runtime, event) {
        Ok(session) => session,
        Err(error) => return deny(&error.to_string()),
    };
    match session.on_tool_call(ProposedCall { tool, arguments }).await {
        Ok(ToolCallDecision::Allow { .. }) => HookAnswer::ok(serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "allow",
                "permissionDecisionReason": "appa: the call is released",
            }
        })),
        Ok(ToolCallDecision::Deny { feedback }) => deny(&feedback),
        Err(EventError::Storage(detail)) => HookAnswer::refused(detail),
        Err(error) => deny(&error.to_string()),
    }
}

async fn on_post_tool_use(runtime: &Runtime, event: &HookEvent) -> HookAnswer {
    let mut session = match event_session(runtime, event) {
        Ok(session) => session,
        Err(error) => return block(&error.to_string()),
    };
    let dispatch = match matched_dispatch(&session, event) {
        Ok(dispatch) => dispatch,
        Err(answer) => return *answer,
    };
    let outcome = match event.hook_event_name.as_str() {
        "PostToolUseFailure" => ToolOutcome::Failure {
            message: "the tool run failed".to_string(),
        },
        _ => map_outcome(event.tool_response.as_ref(), runtime.max_body_bytes()),
    };
    match session.on_tool_result(dispatch, outcome).await {
        Ok(ToolResultDecision::Keep) => HookAnswer::ok(serde_json::json!({})),
        Ok(ToolResultDecision::Replace { placeholder }) => block(&placeholder),
        Err(EventError::Storage(detail)) => HookAnswer::refused(detail),
        Err(error) => block(&error.to_string()),
    }
}

fn matched_dispatch(session: &Session, event: &HookEvent) -> Result<crate::api::DispatchId, Box<HookAnswer>> {
    let open = match session.open_dispatch() {
        Ok(Some(open)) => open,
        Ok(None) => return Err(Box::new(block(&EventError::UnknownDispatch.to_string()))),
        Err(error) => return Err(Box::new(HookAnswer::refused(error.to_string()))),
    };
    let (Some(tool), Some(arguments)) = (event.tool_name.clone(), event.tool_input.clone()) else {
        return Err(Box::new(HookAnswer::refused(
            "a tool outcome without its tool call".to_string(),
        )));
    };
    let reported = ProposedCall { tool, arguments };
    let Ok(bytes) = serde_json::to_vec(&reported) else {
        return Err(Box::new(HookAnswer::refused(
            "the reported call does not serialize".to_string(),
        )));
    };
    if reported.tool != open.tool || bytes != open.bytes {
        return Err(Box::new(block(
            "this outcome does not match the open dispatch; it is not reported",
        )));
    }
    Ok(open.id)
}

fn on_subagent_start(runtime: &Runtime, event: &HookEvent) -> HookAnswer {
    let Some(agent) = event.agent_id.clone() else {
        return HookAnswer::refused("SubagentStart without an agent id".to_string());
    };
    let mut parent = match open_or_reopen(runtime, &event.root()) {
        Ok(session) => session,
        Err(error) => return HookAnswer::refused(error.to_string()),
    };
    let child = TrajectoryId(format!("cc:{}:{agent}", event.session_id));
    let task = child_task(&parent, event);
    match parent.on_child_start(child.clone(), task) {
        Ok(_) => HookAnswer::ok(serde_json::json!({})),
        Err(EventError::TrajectoryExists) => match runtime.session(&child) {
            Ok(_) => HookAnswer::ok(serde_json::json!({})),
            Err(error) => HookAnswer::refused(error.to_string()),
        },
        Err(error) => HookAnswer::refused(error.to_string()),
    }
}

async fn on_subagent_stop(runtime: &Runtime, event: &HookEvent) -> HookAnswer {
    let mut child = match child_session(runtime, event) {
        Ok(session) => session,
        Err(error) => return block(&error.to_string()),
    };
    match child.on_child_end(event.last_assistant_message.clone()).await {
        Ok(crate::api::ChildReturnDecision::Returned { .. }) | Ok(crate::api::ChildReturnDecision::NoValue) => {
            HookAnswer::ok(serde_json::json!({}))
        }
        Ok(crate::api::ChildReturnDecision::Blocked { feedback }) => block(&feedback),
        Err(EventError::Storage(detail)) => HookAnswer::refused(detail),
        Err(error) => block(&error.to_string()),
    }
}

fn map_outcome(response: Option<&serde_json::Value>, cap: usize) -> ToolOutcome {
    let Some(response) = response else {
        return ToolOutcome::Indeterminate;
    };
    let Ok(body) = serde_json::to_string(response) else {
        return ToolOutcome::Indeterminate;
    };
    if body.len() > cap {
        return ToolOutcome::Success {
            body: OutcomeBody::Unavailable,
        };
    }
    ToolOutcome::Success {
        body: OutcomeBody::Available(body),
    }
}

fn deny(reason: &str) -> HookAnswer {
    HookAnswer::ok(serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    }))
}

fn block(reason: &str) -> HookAnswer {
    HookAnswer::ok(serde_json::json!({
        "decision": "block",
        "reason": reason,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::testing;
    use crate::config::Config;

    fn config() -> Config {
        let text = r#"
            [policy]
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
                "UserPromptSubmit" => testing::enqueue_done(&runtime),
                "PreToolUse" => testing::enqueue_release(
                    &runtime,
                    &format!("d-{}", event["tool_use_id"].as_str().unwrap_or("fixture")),
                    event["tool_name"].as_str().expect("the fixture names a tool"),
                    &event["tool_input"],
                ),
                "PostToolUse" => testing::enqueue_keep_output(&runtime),
                "SubagentStop" => {
                    testing::enqueue_value(&runtime, event["last_assistant_message"].as_str().unwrap_or_default())
                }
                _ => {}
            }
            let body = serde_json::to_vec(&event).expect("the fixture re-serializes");
            let answer = handle_hook(&runtime, &body).await;
            assert_eq!(
                answer.status, 200,
                "hook {} refused: {}",
                event["hook_event_name"], answer.body,
            );
            match event["hook_event_name"].as_str().expect("named hook") {
                "PreToolUse" => assert_eq!(
                    answer.body,
                    serde_json::json!({
                        "hookSpecificOutput": {
                            "hookEventName": "PreToolUse",
                            "permissionDecision": "allow",
                            "permissionDecisionReason": "appa: the call is released",
                        }
                    }),
                    "the released call must render as exactly the allow answer",
                ),
                other => assert_eq!(
                    answer.body,
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
        let answer = handle_hook(&runtime, &serde_json::to_vec(&event).expect("serializes")).await;
        assert_eq!(answer.status, 200);
        assert_eq!(
            answer.body,
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
        assert_eq!(handle_hook(&runtime, &body).await.status, 200);

        let second = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "tool_name": "Write",
            "tool_input": {"file_path": "/tmp/x", "content": "y"},
        });
        let answer = handle_hook(&runtime, &serde_json::to_vec(&second).expect("serializes")).await;
        assert_eq!(answer.status, 200);
        assert_eq!(
            answer.body["hookSpecificOutput"]["permissionDecision"], "deny",
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
        handle_hook(&runtime, &serde_json::to_vec(&pre).expect("serializes")).await;

        testing::enqueue_replace_output(&runtime, "the output is confined");
        let post = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": "s1",
            "tool_name": "Read",
            "tool_input": {"file_path": "secret.txt"},
            "tool_response": {"content": "the secret"},
        });
        let answer = handle_hook(&runtime, &serde_json::to_vec(&post).expect("serializes")).await;
        assert_eq!(
            answer.body,
            serde_json::json!({"decision": "block", "reason": "the output is confined"}),
        );
    }

    #[test]
    fn the_q14_outcome_mapping_holds() {
        let available = map_outcome(Some(&serde_json::json!({"ok": true})), 4096);
        assert_eq!(
            available,
            ToolOutcome::Success {
                body: OutcomeBody::Available("{\"ok\":true}".to_string()),
            },
        );
        let oversized = map_outcome(Some(&serde_json::json!("x".repeat(5000))), 4096);
        assert_eq!(
            oversized,
            ToolOutcome::Success {
                body: OutcomeBody::Unavailable
            }
        );
        assert_eq!(map_outcome(None, 4096), ToolOutcome::Indeterminate);
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
        let answer = handle_hook(&runtime, &serde_json::to_vec(&event).expect("serializes")).await;
        assert_eq!(answer.status, 200);
        assert_eq!(answer.body["hookSpecificOutput"]["permissionDecision"], "allow",);
        testing::enqueue_release(&runtime, "d1", "Bash", &serde_json::json!({"command": "ls"}));
        let call = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "tool_name": "Bash",
            "tool_input": {"command": "ls"},
        });
        let answer = handle_hook(&runtime, &serde_json::to_vec(&call).expect("serializes")).await;
        assert_eq!(answer.body["hookSpecificOutput"]["permissionDecision"], "allow",);
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
        let answer = handle_hook(&runtime, &serde_json::to_vec(&event).expect("serializes")).await;
        assert_eq!(
            answer.body["hookSpecificOutput"]["permissionDecision"], "deny",
            "a colliding name must reach the engine, not the exemption",
        );
    }

    #[tokio::test]
    async fn an_unreadable_hook_event_is_a_400() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let answer = handle_hook(&runtime, b"not json").await;
        assert_eq!(answer.status, 400);
    }
}
