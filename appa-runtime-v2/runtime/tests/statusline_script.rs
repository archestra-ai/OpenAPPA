
use std::io::Write;
use std::process::{Command, Stdio};

use axum::Router;
use axum::routing::get;

const MASCOT: &str = "▄█▄▄▄█▄\n██▄█▄██\n";

fn script_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../plugin/statusline.sh")
}

fn run_statusline(url: &str, stdin: &[u8]) -> (i32, String) {
    let mut child = Command::new("sh")
        .arg(script_path())
        .env("APPA_RUNTIME_URL", url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the statusline script spawns");
    child
        .stdin
        .as_mut()
        .expect("the child has a stdin pipe")
        .write_all(stdin)
        .expect("the session JSON writes to the script's stdin");
    let output = child.wait_with_output().expect("the script finishes");
    (
        output.status.code().expect("the script exits with a code"),
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

const SESSION: &[u8] = br#"{"session_id":"s1","transcript_path":"/tmp/t.jsonl"}"#;

#[tokio::test(flavor = "multi_thread")]
async fn a_status_answer_renders_the_chips() {
    let url = stub(Router::new().route(
        "/status",
        get(|| async { r#"{"trajectory":"cc:s1","trust":"suspicious","audience":"public"}"# }),
    ))
    .await;
    let (code, stdout) = tokio::task::spawn_blocking(move || run_statusline(&url, SESSION))
        .await
        .expect("the blocking task joins");
    assert_eq!(code, 0);
    assert_eq!(stdout, "▄█▄▄▄█▄  trust:suspicious  audience:public\n██▄█▄██\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_404_prints_the_mascot_alone() {
    let url = stub(Router::new().route("/status", get(|| async { (axum::http::StatusCode::NOT_FOUND, "") }))).await;
    let (code, stdout) = tokio::task::spawn_blocking(move || run_statusline(&url, SESSION))
        .await
        .expect("the blocking task joins");
    assert_eq!(code, 0, "a statusline never fails closed");
    assert_eq!(stdout, MASCOT);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_shape_200_prints_the_mascot_alone() {
    let url = stub(Router::new().route("/status", get(|| async { "{}" }))).await;
    let (code, stdout) = tokio::task::spawn_blocking(move || run_statusline(&url, SESSION))
        .await
        .expect("the blocking task joins");
    assert_eq!(code, 0);
    assert_eq!(stdout, MASCOT);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_malformed_body_prints_the_mascot_alone() {
    let url = stub(Router::new().route("/status", get(|| async { "not json at all" }))).await;
    let (code, stdout) = tokio::task::spawn_blocking(move || run_statusline(&url, SESSION))
        .await
        .expect("the blocking task joins");
    assert_eq!(code, 0);
    assert_eq!(stdout, MASCOT);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_runtime_prints_the_mascot_alone() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral loopback port binds");
    let url = format!("http://{}", listener.local_addr().expect("the address is readable"));
    drop(listener);
    let (code, stdout) = tokio::task::spawn_blocking(move || run_statusline(&url, SESSION))
        .await
        .expect("the blocking task joins");
    assert_eq!(code, 0);
    assert_eq!(stdout, MASCOT);
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_stdin_prints_the_mascot_alone() {
    let url = stub(Router::new()).await;
    let (code, stdout) = tokio::task::spawn_blocking(move || run_statusline(&url, b"not the session JSON"))
        .await
        .expect("the blocking task joins");
    assert_eq!(code, 0);
    assert_eq!(stdout, MASCOT);
}

#[test]
fn missing_tools_print_the_mascot_alone() {
    let mut child = Command::new("/bin/sh")
        .arg(script_path())
        .env("PATH", "/nonexistent")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the statusline script spawns");
    child
        .stdin
        .as_mut()
        .expect("the child has a stdin pipe")
        .write_all(SESSION)
        .expect("the session JSON writes");
    let output = child.wait_with_output().expect("the script finishes");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&output.stdout), MASCOT);
}
