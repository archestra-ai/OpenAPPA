use std::io::Write;
use std::process::{Command, Stdio};

use axum::Router;
use axum::routing::post;

/// The hooks that report a finished turn. Every blocking outcome on one
/// of these means "do not stop", so none of them may carry one.
const TURN_ENDS: [&str; 3] = ["Stop", "StopFailure", "SubagentStop"];

fn turn_end_command() -> &'static str {
    "[ \"${APPA_GATE:-}\" = 1 ] || exit 0; curl -s -m 30 -X POST \"${APPA_RUNTIME_URL:-http://127.0.0.1:8787}/hook\" \
     -H 'content-type: application/json' --data-binary @- -o /dev/null; exit 0"
}

fn shipped(file: &str) -> serde_json::Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../integrations/claude-code/plugin/hooks")
        .join(file);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap_or_else(|_| panic!("the shipped {file} is readable")))
        .unwrap_or_else(|_| panic!("the shipped {file} parses"))
}

/// The two shipped hook maps gate the same events. Nothing else compares
/// them, so a hook added to one and not the other leaves that platform
/// on the behaviour the other one fixed.
#[test]
fn both_shipped_hook_maps_gate_the_same_events() {
    let posix = shipped("hooks.json");
    let windows = shipped("hooks.windows.json");
    let names = |map: &serde_json::Value| {
        let mut names: Vec<String> = map["hooks"]
            .as_object()
            .expect("the hook map is an object")
            .keys()
            .cloned()
            .collect();
        names.sort();
        names
    };
    assert_eq!(
        names(&posix),
        names(&windows),
        "the shipped hook maps gate different events"
    );
    for event in TURN_ENDS {
        assert!(
            windows["hooks"][event][0]["hooks"][0]["command"].is_string(),
            "the Windows map does not gate {event}",
        );
    }
}

fn shipped_command() -> String {
    let hooks = shipped("hooks.json");
    let command = hooks["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .expect("the PreToolUse hook carries a command")
        .to_string();
    for event in ["UserPromptSubmit", "PostToolUse", "SubagentStart", "PostToolUseFailure"] {
        let entry = &hooks["hooks"][event][0]["hooks"][0]["command"];
        assert_eq!(
            entry.as_str(),
            Some(command.as_str()),
            "the {event} hook command drifted from the PreToolUse one",
        );
    }
    // A turn end decides nothing, and blocking this hook holds the actor
    // in a turn it has finished, so these three never carry the blocking
    // exit and never print an answer.
    for event in TURN_ENDS {
        let entry = hooks["hooks"][event][0]["hooks"][0]["command"]
            .as_str()
            .unwrap_or_else(|| panic!("the {event} hook carries a command"));
        assert_eq!(
            entry,
            turn_end_command(),
            "the {event} hook is no longer the non-blocking turn-end command",
        );
    }
    assert_eq!(
        hooks["hooks"]["SessionStart"][0]["hooks"][0]["command"].as_str(),
        Some(
            command
                .replace(
                    "; curl",
                    "; sh \"${CLAUDE_PLUGIN_ROOT}/hooks/ensure-runtime.sh\" </dev/null && curl",
                )
                .as_str()
        ),
        "the SessionStart hook is no longer the shared command plus its runtime start",
    );
    command
}

fn run_hook(command: &str, url: &str) -> (i32, String) {
    run_gated(command, url, true)
}

fn run_gated(command: &str, url: &str, gated: bool) -> (i32, String) {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .env("APPA_RUNTIME_URL", url)
        .env("APPA_GATE", if gated { "1" } else { "0" })
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the hook command spawns");
    // An ungated hook may exit before reading its stdin, closing the pipe
    // mid-write; that is a pass condition, so only a non-EPIPE error fails.
    if let Err(error) = child
        .stdin
        .as_mut()
        .expect("the child has a stdin pipe")
        .write_all(br#"{"hook_event_name":"PreToolUse","session_id":"plugin-test"}"#)
    {
        assert_eq!(
            error.kind(),
            std::io::ErrorKind::BrokenPipe,
            "the event writes to the hook's stdin",
        );
    }
    let output = child.wait_with_output().expect("the hook command finishes");
    (
        output.status.code().expect("the hook command exits with a code"),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

async fn refused_url() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral loopback port binds");
    let url = format!(
        "http://{}",
        listener.local_addr().expect("the bound address is readable")
    );
    drop(listener);
    url
}

async fn stub(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral loopback port binds");
    let addr = listener.local_addr().expect("the bound address is readable");
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("the stub serves");
    });
    format!("http://{addr}")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_2xx_answer_passes_its_body_through_with_exit_0() {
    let url = stub(Router::new().route("/hook", post(|| async { r#"{"decision":"block","reason":"denied"}"# }))).await;
    let command = shipped_command();
    let (code, stdout) = tokio::task::spawn_blocking(move || run_hook(&command, &url))
        .await
        .expect("the blocking task joins");
    assert_eq!(code, 0);
    assert_eq!(stdout, r#"{"decision":"block","reason":"denied"}"#);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_error_exits_2_instead_of_failing_open() {
    let url = stub(Router::new().route(
        "/hook",
        post(|| async { (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom") }),
    ))
    .await;
    let command = shipped_command();
    let (code, _) = tokio::task::spawn_blocking(move || run_hook(&command, &url))
        .await
        .expect("the blocking task joins");
    assert_eq!(code, 2, "a non-2xx answer must block the action");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_ungated_session_posts_nothing_and_never_blocks() {
    let url = refused_url().await;
    let command = shipped_command();
    let (code, stdout) = tokio::task::spawn_blocking(move || run_gated(&command, &url, false))
        .await
        .expect("the blocking task joins");
    assert_eq!(code, 0, "an ungated session must not be blocked");
    assert_eq!(stdout, "", "an ungated session posts nothing");
}

/// Blocking a turn end holds the actor in a turn it has finished, so a
/// runtime that answers nothing costs a call left open, never a turn
/// that cannot end.
#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_runtime_never_blocks_a_turn_end() {
    let url = refused_url().await;
    let (code, stdout) = tokio::task::spawn_blocking(move || run_hook(turn_end_command(), &url))
        .await
        .expect("the blocking task joins");
    assert_eq!(code, 0, "a turn end must never block the harness");
    assert_eq!(stdout, "", "a turn end prints no decision");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_runtime_exits_2() {
    let url = refused_url().await;
    let command = shipped_command();
    let (code, _) = tokio::task::spawn_blocking(move || run_hook(&command, &url))
        .await
        .expect("the blocking task joins");
    assert_eq!(code, 2, "no answer from the runtime must block the action");
}
