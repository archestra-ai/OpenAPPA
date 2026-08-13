
use std::io::Write;
use std::process::{Command, Stdio};

use axum::Router;
use axum::routing::post;

fn shipped_command() -> String {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../integrations/claude-code/plugin/hooks/hooks.json");
    let hooks: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("the shipped hooks.json is readable"))
            .expect("the shipped hooks.json parses");
    let command = hooks["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .expect("the PreToolUse hook carries a command")
        .to_string();
    for event in [
        "SessionStart",
        "UserPromptSubmit",
        "PostToolUse",
        "SubagentStart",
        "SubagentStop",
        "PostToolUseFailure",
    ] {
        let entry = &hooks["hooks"][event][0]["hooks"][0]["command"];
        assert_eq!(
            entry.as_str(),
            Some(command.as_str()),
            "the {event} hook command drifted from the PreToolUse one",
        );
    }
    command
}

fn run_hook(command: &str, url: &str) -> (i32, String) {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .env("APPA_RUNTIME_URL", url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the hook command spawns");
    child
        .stdin
        .as_mut()
        .expect("the child has a stdin pipe")
        .write_all(br#"{"hook_event_name":"PreToolUse","session_id":"plugin-test"}"#)
        .expect("the event writes to the hook's stdin");
    let output = child.wait_with_output().expect("the hook command finishes");
    (
        output.status.code().expect("the hook command exits with a code"),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
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
async fn an_unreachable_runtime_exits_2() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral loopback port binds");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    drop(listener);
    let command = shipped_command();
    let (code, _) = tokio::task::spawn_blocking(move || run_hook(&command, &url))
        .await
        .expect("the blocking task joins");
    assert_eq!(code, 2, "no answer from the runtime must block the action");
}
