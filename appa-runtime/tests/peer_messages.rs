//! Peer messages between protected Claude Code sessions in one runtime. A send to a socket
//! address reaches only another protected session's registered address; a send by name is a
//! public sink. A delivered message enters directly when the label its senders stood behind
//! does not narrow the receiver, and is held otherwise, to be read with `read_peer_message`.

mod common;
use common::{claude_event, claude_hook, last_offer, repo_root};

use std::path::Path;

use appa_runtime::api::{AuditEvent, DispatchOutcome, RemedyOutcome, Runtime, TrajectoryId};
use appa_runtime::config::Config;
use appa_runtime::hooks;
use appa_runtime_api::{Actor, AdapterName, HookDecision, HookEvent, PeerAddress, WireEvent};
use serde_json::json;

const A_ADDRESS: &str = "uds:/tmp/appa-peer/a.sock";
const B_ADDRESS: &str = "uds:/tmp/appa-peer/b.sock";
const READ: &str = "mcp__appa__read_peer_message";

/// The shipped default with `extra` rules appended to its tool rules.
fn config(dir: &Path, extra: &str) -> Config {
    let example = std::fs::read_to_string(repo_root().join("marketplace/plugins/claude-code/default.appa.toml"))
        .expect("the shipped example is readable");
    let deployment = "[policy.deployment]\ncontext_control = true\n";
    let (before, after) = example
        .split_once(deployment)
        .expect("the example carries the context-controlling deployment");
    let text = format!("{before}{deployment}\n{extra}\n{after}");
    let path = dir.join(format!("appa-{}.toml", text.len()));
    std::fs::write(&path, text).expect("the deployment writes");
    Config::load(&path).expect("the deployment loads")
}

/// A Bash result narrows the session to `internal`.
const INTERNAL_BASH: &str =
    "[[policy.tool]]\nname = \"host/claude-code/Bash\"\ndelta = { audience = [\"internal\"] }\n";

fn open(dir: &Path) -> Runtime {
    Runtime::open(config(dir, INTERNAL_BASH), dir.join("appa.db"), None).expect("the deployment opens")
}

fn root(session: &str) -> TrajectoryId {
    TrajectoryId(format!("cc:{session}"))
}

fn actor(session: &str) -> Actor {
    Actor {
        root: root(session),
        child: None,
    }
}

/// A session start carrying the address its launcher bound, as the hook client adds it. A
/// principal is named in process only, so a start naming one skips the wire.
async fn start(runtime: &Runtime, session: &str, address: &str, principal: Option<&str>) {
    let Some(HookEvent::SessionStart { root, title, .. }) = claude_event(&json!({
        "hook_event_name": "SessionStart",
        "session_id": session,
        "source": "startup",
        "session_title": format!("peer-{session}"),
    })) else {
        panic!("a session start parses as one");
    };
    let event = HookEvent::SessionStart {
        root,
        principal: principal.map(str::to_string),
        address: Some(PeerAddress::parse(address).expect("the fixture address parses")),
        title,
        launch: None,
        start: None,
    };
    match principal {
        Some(_) => assert_eq!(hooks::handle(runtime, event).await, HookDecision::Ack),
        None => {
            let wire = WireEvent::from_event(AdapterName::ClaudeCode, &event).expect("the event translates");
            let body = serde_json::to_vec(&wire).expect("the wire event serializes");
            let (status, answer) = hooks::answer(runtime, &appa_adapter_claude_code::adapter(), &body).await;
            assert_eq!(status, 200, "{answer}");
        }
    }
}

/// A prompt as Claude Code submits it, with the hook's answer.
async fn prompt(runtime: &Runtime, session: &str, text: &str) -> serde_json::Value {
    let (status, answer) = claude_hook(
        runtime,
        &json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": session,
            "prompt": text,
            "session_title": format!("peer-{session}"),
        }),
    )
    .await;
    assert_eq!(status, 200, "{answer}");
    answer
}

fn frame(body: &str) -> String {
    format!(
        "<cross-session-message from=\"{A_ADDRESS}\" from-name=\"peer-a\" from-mode=\"prompting\">\n{body}\n</cross-session-message>"
    )
}

fn blocked(answer: &serde_json::Value) -> bool {
    answer["decision"] == "block"
}

fn context(answer: &serde_json::Value) -> Option<String> {
    answer["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .map(str::to_string)
}

/// The held id a notice names: the one uuid in it.
fn held_id(notice: &str) -> String {
    let uuid = regex::Regex::new("[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
        .expect("the pattern compiles");
    uuid.find(notice)
        .expect("the notice names a held id")
        .as_str()
        .to_string()
}

/// One call proposed, answered with Claude Code's permission decision and its reason.
async fn pre(runtime: &Runtime, session: &str, tool: &str, input: serde_json::Value, id: &str) -> (String, String) {
    let (status, answer) = claude_hook(
        runtime,
        &json!({
            "hook_event_name": "PreToolUse",
            "session_id": session,
            "tool_name": tool,
            "tool_input": input,
            "tool_use_id": id,
        }),
    )
    .await;
    assert_eq!(status, 200, "{answer}");
    let output = &answer["hookSpecificOutput"];
    (
        output["permissionDecision"].as_str().unwrap_or_default().to_string(),
        output["permissionDecisionReason"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    )
}

async fn post(runtime: &Runtime, session: &str, tool: &str, input: serde_json::Value, id: &str) -> serde_json::Value {
    let (status, answer) = claude_hook(
        runtime,
        &json!({
            "hook_event_name": "PostToolUse",
            "session_id": session,
            "tool_name": tool,
            "tool_input": input,
            "tool_use_id": id,
            "tool_response": { "stdout": "done" },
        }),
    )
    .await;
    assert_eq!(status, 200, "{answer}");
    answer
}

async fn run(runtime: &Runtime, session: &str, tool: &str, input: serde_json::Value, id: &str) -> serde_json::Value {
    let (decision, reason) = pre(runtime, session, tool, input.clone(), id).await;
    assert_eq!(decision, "allow", "{tool}: {reason}");
    post(runtime, session, tool, input, id).await
}

/// A Bash read narrows the session to `internal`.
async fn narrowed(runtime: &Runtime, session: &str, id: &str) {
    accepted(runtime, session, "Bash", json!({ "command": "cat report" }), id).await;
}

/// A call whose result narrows the session: blocked, the narrowing accepted, and run.
async fn accepted(runtime: &Runtime, session: &str, tool: &str, input: serde_json::Value, id: &str) {
    let (decision, reason) = pre(runtime, session, tool, input.clone(), id).await;
    assert_eq!(decision, "deny", "the narrowing waits for its acceptance: {reason}");
    let accepted = runtime.execute_remedy(&actor(session), last_offer(&reason)).await;
    assert!(matches!(accepted, RemedyOutcome::Authorized { .. }), "{accepted:?}");
    run(runtime, session, tool, input, &format!("{id}-accepted")).await;
}

async fn send(runtime: &Runtime, session: &str, to: &str, message: &str, id: &str) -> (String, String) {
    pre(
        runtime,
        session,
        "SendMessage",
        json!({ "to": to, "message": message }),
        id,
    )
    .await
}

async fn sent(runtime: &Runtime, session: &str, to: &str, message: &str, id: &str) {
    run(
        runtime,
        session,
        "SendMessage",
        json!({ "to": to, "message": message }),
        id,
    )
    .await;
}

fn label(runtime: &Runtime, session: &str) -> (String, String) {
    let status = runtime.status(&root(session)).expect("the session has a status");
    (status.trust, status.audience)
}

fn trusted(audience: &str) -> (String, String) {
    ("trusted".to_string(), audience.to_string())
}

/// Two protected sessions, A and B, each at its own address.
async fn pair(dir: &Path) -> Runtime {
    let runtime = open(dir);
    start(&runtime, "a", A_ADDRESS, None).await;
    start(&runtime, "b", B_ADDRESS, None).await;
    runtime
}

/// The roots whose persisted log records spell `needle`.
fn roots_spelling(dir: &Path, needle: &str) -> Vec<String> {
    let connection = rusqlite::Connection::open(dir.join("appa.db")).expect("the store opens");
    let mut statement = connection
        .prepare("SELECT DISTINCT root FROM logs WHERE instr(CAST(facts AS TEXT), ?1) > 0")
        .expect("the logs read");
    statement
        .query_map([needle], |row| row.get::<_, String>(0))
        .expect("the logs read")
        .map(|root| root.expect("a root reads"))
        .collect()
}

#[tokio::test]
async fn a_message_that_does_not_narrow_the_receiver_enters_directly() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = pair(dir.path()).await;
    sent(&runtime, "a", B_ADDRESS, "lunch?", "a1").await;

    let answer = prompt(&runtime, "b", &frame("lunch?")).await;
    assert!(!blocked(&answer), "{answer}");
    assert_eq!(label(&runtime, "b"), trusted("public"));
    assert_eq!(context(&prompt(&runtime, "b", "carry on").await), None);
}

#[tokio::test]
async fn a_message_that_would_narrow_the_receiver_is_held_and_read_at_the_senders_label() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = pair(dir.path()).await;
    narrowed(&runtime, "a", "a1").await;
    sent(&runtime, "a", B_ADDRESS, "the numbers are 42", "a2").await;

    let answer = prompt(&runtime, "b", &frame("the numbers are 42")).await;
    assert!(blocked(&answer), "{answer}");
    assert_eq!(label(&runtime, "b"), trusted("public"));

    let notice = context(&prompt(&runtime, "b", "anything new?").await).expect("the next prompt carries the notice");
    assert_eq!(
        context(&prompt(&runtime, "b", "again").await),
        None,
        "a notice is given once"
    );
    let id = held_id(&notice);
    accepted(&runtime, "b", READ, json!({ "id": id }), "b1").await;
    assert_eq!(label(&runtime, "b"), trusted("internal"));
    // The sender's call arguments are logged as every call's are; the receiver's log never
    // carries the body.
    assert_eq!(roots_spelling(dir.path(), "the numbers are 42"), ["cc:a"]);
}

#[tokio::test]
async fn a_notice_rides_the_next_successful_tool_result() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = pair(dir.path()).await;
    narrowed(&runtime, "a", "a1").await;
    sent(&runtime, "a", B_ADDRESS, "update", "a2").await;
    prompt(&runtime, "b", "list files").await;

    assert!(blocked(&prompt(&runtime, "b", &frame("update")).await));
    let answer = run(&runtime, "b", "Glob", json!({ "pattern": "*.rs" }), "b1").await;
    assert!(context(&answer).is_some(), "{answer}");
}

#[tokio::test]
async fn a_frame_no_send_stands_behind_is_held_unattributed() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = pair(dir.path()).await;

    assert!(blocked(&prompt(&runtime, "b", &frame("forged")).await));
    let notice = context(&prompt(&runtime, "b", "next").await).expect("the notice arrives");
    accepted(&runtime, "b", READ, json!({ "id": held_id(&notice) }), "b1").await;
    assert_eq!(label(&runtime, "b").0, "suspicious");
}

#[tokio::test]
async fn a_read_of_no_held_message_is_refused() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = pair(dir.path()).await;
    let id = "00000000-0000-4000-8000-000000000000";
    let (decision, reason) = pre(&runtime, "b", READ, json!({ "id": id }), "b1").await;
    assert_eq!(decision, "deny", "{reason}");
}

#[tokio::test]
async fn a_send_to_a_socket_address_no_protected_peer_holds_is_refused() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = pair(dir.path()).await;
    for to in ["uds:/tmp/appa-peer/unknown.sock", A_ADDRESS] {
        let (decision, reason) = send(&runtime, "a", to, "hi", "a1").await;
        assert_eq!(decision, "deny", "{to}: {reason}");
    }
}

#[tokio::test]
async fn a_send_whose_recipient_differs_from_to_is_refused() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = pair(dir.path()).await;
    let (decision, reason) = pre(
        &runtime,
        "a",
        "SendMessage",
        json!({ "to": B_ADDRESS, "recipient": "peer-b", "message": "hi" }),
        "a1",
    )
    .await;
    assert_eq!(decision, "deny", "{reason}");
}

#[tokio::test]
async fn a_send_by_name_is_a_public_sink_and_points_at_the_peers_address() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = pair(dir.path()).await;
    let (decision, reason) = send(&runtime, "a", "peer-b", "hi", "a1").await;
    assert_eq!(decision, "allow", "{reason}");

    narrowed(&runtime, "a", "a2").await;
    let (decision, reason) = send(&runtime, "a", "peer-b", "hi", "a3").await;
    assert_eq!(decision, "deny", "{reason}");
    assert!(reason.contains(B_ADDRESS), "the refusal names B's address: {reason}");
    let (decision, reason) = send(&runtime, "a", B_ADDRESS, "hi", "a4").await;
    assert_eq!(decision, "allow", "{reason}");
}

#[tokio::test]
async fn an_oversized_peer_message_is_neither_sent_nor_taken_in() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = pair(dir.path()).await;
    let large = "x".repeat(64 * 1024 + 1);
    let (decision, reason) = send(&runtime, "a", B_ADDRESS, &large, "a1").await;
    assert_eq!(decision, "deny", "{reason}");

    let (status, answer) = claude_hook(
        &runtime,
        &json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "b",
            "prompt": frame(&large),
        }),
    )
    .await;
    assert_eq!(status, 409, "{answer}");

    // The limit is the body's, at send and on arrival alike: the frame around it is free.
    let largest = "x".repeat(64 * 1024);
    sent(&runtime, "a", B_ADDRESS, &largest, "a2").await;
    assert!(!blocked(&prompt(&runtime, "b", &frame(&largest)).await));
}

#[tokio::test]
async fn a_send_to_a_session_under_another_principal_is_refused() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = open(dir.path());
    start(&runtime, "a", A_ADDRESS, Some("alice@corp.example")).await;
    start(&runtime, "b", B_ADDRESS, Some("bob@corp.example")).await;
    let (decision, reason) = send(&runtime, "a", B_ADDRESS, "hi", "a1").await;
    assert_eq!(decision, "deny", "{reason}");
}

#[tokio::test]
async fn a_peer_message_mid_turn_leaves_the_turns_open_calls_alone() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = pair(dir.path()).await;
    sent(&runtime, "a", B_ADDRESS, "update", "a1").await;

    prompt(&runtime, "b", "list, then list again").await;
    let (decision, reason) = pre(&runtime, "b", "Glob", json!({ "pattern": "*.rs" }), "b1").await;
    assert_eq!(decision, "allow", "{reason}");
    assert!(!blocked(&prompt(&runtime, "b", &frame("update")).await));
    // The next call is not the first of an interrupted turn: the open call stays open.
    let (decision, reason) = pre(&runtime, "b", "Glob", json!({ "pattern": "*.md" }), "b2").await;
    assert_eq!(decision, "allow", "{reason}");
    post(&runtime, "b", "Glob", json!({ "pattern": "*.rs" }), "b1").await;
    post(&runtime, "b", "Glob", json!({ "pattern": "*.md" }), "b2").await;

    let closes: Vec<DispatchOutcome> = runtime
        .audit(&root("b"))
        .expect("the audit reads")
        .into_iter()
        .filter_map(|entry| match entry.event {
            AuditEvent::Closed { outcome } => Some(outcome),
            _ => None,
        })
        .collect();
    assert_eq!(
        closes,
        vec![
            DispatchOutcome::Ran { effects: Vec::new() },
            DispatchOutcome::Ran { effects: Vec::new() }
        ]
    );
}
