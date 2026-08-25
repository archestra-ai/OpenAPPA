mod common;
use common::repo_root;

use std::path::Path;

use appa_runtime::api::{AuditEvent, Runtime, TrajectoryId};
use appa_runtime::config::Config;
use appa_runtime::hooks;

const SESSION: &str = "18ebc556-b78f-452b-99ec-487a4f40e824";

fn recorded() -> Vec<serde_json::Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hooks.jsonl");
    std::fs::read_to_string(path)
        .expect("the recorded hook fixtures are readable")
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("each fixture line is JSON"))
        .filter(|event| event["session_id"] == SESSION)
        .collect()
}

fn hook(events: &[serde_json::Value], name: &str, tool: Option<&str>, subagent: bool) -> serde_json::Value {
    events
        .iter()
        .find(|event| {
            event["hook_event_name"] == name
                && tool.is_none_or(|tool| event["tool_name"] == tool)
                && event.get("agent_id").is_some() == subagent
        })
        .unwrap_or_else(|| panic!("the fixture carries a {name} {tool:?} (subagent: {subagent})"))
        .clone()
}

fn as_root(mut event: serde_json::Value) -> serde_json::Value {
    let fields = event.as_object_mut().expect("a hook event is an object");
    fields.remove("agent_id");
    fields.remove("agent_type");
    event
}

fn deployment(policy_extra: &str, externals_extra: &str) -> Runtime {
    let example = std::fs::read_to_string(repo_root().join("integrations/claude-code/examples/claude-code.appa.toml"))
        .expect("the shipped example is readable");
    let (policy, externals) = example
        .split_once("[externals]")
        .expect("the example carries an [externals] table");
    let text = format!("{policy}{policy_extra}\n[externals]{externals}\n{externals_extra}");
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, text).expect("the deployment writes");
    let config = Config::load(&path).expect("the deployment loads");
    Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens")
}

async fn call(runtime: &Runtime, event: &serde_json::Value) -> (u16, serde_json::Value) {
    let body = serde_json::to_vec(event).expect("the event serializes");
    hooks::answer(runtime, &appa_adapter_claude_code::codec(), &body).await
}

fn root() -> TrajectoryId {
    TrajectoryId(format!("cc:{SESSION}"))
}

fn forks(runtime: &Runtime) -> usize {
    runtime
        .audit(&root())
        .expect("the audit reads")
        .iter()
        .filter(|entry| matches!(entry.event, AuditEvent::Forked { .. }))
        .count()
}

fn returns(runtime: &Runtime) -> Vec<Option<String>> {
    runtime
        .audit(&root())
        .expect("the audit reads")
        .into_iter()
        .filter_map(|entry| match entry.event {
            AuditEvent::ChildReturn { sanitizer, .. } => Some(sanitizer),
            _ => None,
        })
        .collect()
}

async fn run_until_stop(runtime: &Runtime, events: &[serde_json::Value]) -> Vec<serde_json::Value> {
    let stop = events
        .iter()
        .position(|event| event["hook_event_name"] == "SubagentStop")
        .expect("the fixture carries the subagent's stop");
    for event in &events[..=stop] {
        let name = event["hook_event_name"].as_str().expect("each event names its hook");
        let (status, answer) = call(runtime, event).await;
        assert_eq!(status, 200, "{name} answered {answer}");
        match name {
            "PreToolUse" => assert_eq!(
                answer["hookSpecificOutput"]["permissionDecision"], "allow",
                "{name} {} is released: {answer}",
                event["tool_name"]
            ),
            _ => assert_eq!(answer, serde_json::json!({}), "{name} carries no opinion"),
        }
    }
    assert_eq!(forks(runtime), 1, "the start bound the one spawn in flight");
    events[stop + 1..].to_vec()
}

#[tokio::test]
async fn the_recorded_subagent_session_binds_and_crosses_its_return() {
    let runtime = deployment("", "");
    let events = recorded();
    let rest = run_until_stop(&runtime, &events).await;

    let (status, answer) = call(&runtime, &hook(&events, "SubagentStart", None, true)).await;
    assert_eq!((status, answer), (200, serde_json::json!({})));
    assert_eq!(forks(&runtime), 1, "a repeated start binds nothing new");

    let (status, answer) = call(&runtime, &rest[0]).await;
    assert_eq!(rest[0]["tool_name"], "Agent");
    assert_eq!(
        (status, answer),
        (200, serde_json::json!({})),
        "the unchanged return needs no answer"
    );
    assert_eq!(
        returns(&runtime),
        vec![None],
        "the return crossed as the subagent spelled it"
    );

    let (status, answer) = call(&runtime, &hook(&events, "PreToolUse", Some("Bash"), true)).await;
    assert_eq!(status, 200);
    assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "deny", "{answer}");
    let (status, answer) = call(&runtime, &as_root(hook(&events, "PreToolUse", Some("Bash"), true))).await;
    assert_eq!(status, 200);
    assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "allow", "{answer}");
}

#[tokio::test]
async fn a_sanitized_return_replaces_the_subagents_message_in_the_agent_result() {
    let runtime = deployment(
        r#"confined_child_return = true

[[policy.sanitizer]]
name = "redactor"
on = ["tool_output"]
permits = { audience = { from = ["internal"], to = ["public"] } }

[policy.child]
return_sanitizer = "redactor"
"#,
        "[externals.sanitizers.redactor]\nbuiltin = \"redact-email\"\n",
    );
    let events = recorded();
    let rest = run_until_stop(&runtime, &events).await;

    let mut result = rest[0].clone();
    result["tool_response"]["content"][0]["text"] = serde_json::json!("one file; ask bob@example.com for more");
    let (status, answer) = call(&runtime, &result).await;
    assert_eq!(status, 200, "{answer}");
    let mut expected = result["tool_response"].clone();
    expected["content"][0]["text"] = serde_json::json!("one file; ask [redacted-email] for more");
    assert_eq!(
        answer,
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PostToolUse",
                "updatedToolOutput": expected,
            }
        }),
    );
    assert_eq!(returns(&runtime), vec![Some("redactor".to_string())]);
}

#[tokio::test]
async fn an_agent_result_naming_another_subagent_is_withheld() {
    let runtime = deployment("", "");
    let events = recorded();
    let rest = run_until_stop(&runtime, &events).await;

    let mut result = rest[0].clone();
    result["tool_response"]["agentId"] = serde_json::json!("someone-else");
    let (status, answer) = call(&runtime, &result).await;
    assert_eq!(
        status, 200,
        "a mismatch is a decision the model hears, not a fault: {answer}"
    );
    assert_eq!(answer["decision"], "block");
    let text = answer["hookSpecificOutput"]["updatedToolOutput"]["content"][0]["text"]
        .as_str()
        .expect("the withheld result restates the delivered shape");
    assert!(text.starts_with("[appa] the tool result was withheld: "), "{answer}");
    assert_eq!(
        answer["hookSpecificOutput"]["updatedToolOutput"]["agentId"], "someone-else",
        "the rest of the response is restated as delivered",
    );
    assert!(returns(&runtime).is_empty(), "nothing crossed");

    let (status, answer) = call(&runtime, &as_root(hook(&events, "PreToolUse", Some("Bash"), true))).await;
    assert_eq!(status, 200);
    assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "allow", "{answer}");
}

#[tokio::test]
async fn a_background_subagent_is_denied_before_it_starts() {
    let runtime = deployment("", "");
    let events = recorded();
    for event in &events[..2] {
        call(&runtime, event).await;
    }
    let mut spawn = hook(&events, "PreToolUse", Some("Agent"), false);
    spawn["tool_input"]["run_in_background"] = serde_json::json!(true);
    let (status, answer) = call(&runtime, &spawn).await;
    assert_eq!(status, 200);
    assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "deny", "{answer}");
    assert_eq!(forks(&runtime), 0, "no fork was prepared");
}

#[tokio::test]
async fn a_start_with_no_spawn_in_flight_refuses_and_its_calls_are_denied() {
    let runtime = deployment("", "");
    let events = recorded();
    for event in &events[..2] {
        call(&runtime, event).await;
    }
    let (status, _) = call(&runtime, &hook(&events, "SubagentStart", None, true)).await;
    assert_eq!(status, 409, "no spawn in flight: the start refuses");
    let (status, answer) = call(&runtime, &hook(&events, "PreToolUse", Some("Bash"), true)).await;
    assert_eq!(status, 200);
    assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "deny", "{answer}");
    assert_eq!(forks(&runtime), 0);
}
