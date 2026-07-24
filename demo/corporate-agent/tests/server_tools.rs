//! Drives the real `corp-systems-mcp` binary over MCP against a temp data root
//! and exercises the tool surface. No LLM or API key involved — this is the
//! CI-safe correctness gate for the server half of the demo.

use std::path::PathBuf;

use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use tokio::process::Command;

/// A throwaway data root seeded with two systems, cleaned up on drop.
struct TempData(PathBuf);

impl TempData {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("corp-systems-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("hr")).unwrap();
        std::fs::create_dir_all(dir.join("task_tracker")).unwrap();
        std::fs::write(
            dir.join("hr/alice-chen.md"),
            "# Alice Chen\nCompensation: $185,000\nSSN (last4): 4821\n",
        )
        .unwrap();
        Self(dir)
    }

    fn path(&self) -> &PathBuf {
        &self.0
    }
}

impl Drop for TempData {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Spawn the built `corp-systems-mcp` binary pointed at `root` (corpus and sink alike).
async fn spawn_server(root: &PathBuf) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    spawn_server_split(root, root).await
}

/// Spawn the server with a corpus root and a separate `send_email` sink root.
async fn spawn_server_split(corpus: &PathBuf, sink: &PathBuf) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    let bin = env!("CARGO_BIN_EXE_corp-systems-mcp");
    let transport = TokioChildProcess::new(Command::new(bin).configure(|cmd| {
        cmd.arg("--data-root").arg(corpus);
        cmd.arg("--sink-root").arg(sink);
    }))
    .expect("spawn corp-systems-mcp");
    ().serve(transport).await.expect("mcp handshake")
}

fn text_of(result: &rmcp::model::CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("")
}

async fn call(
    server: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    name: &'static str,
    args: serde_json::Value,
) -> rmcp::model::CallToolResult {
    let mut params = CallToolRequestParams::new(name);
    params.arguments = args.as_object().cloned();
    server.peer().call_tool(params).await.expect("tool call")
}

#[tokio::test]
async fn advertises_thirteen_tools() {
    let data = TempData::new("list");
    let server = spawn_server(data.path()).await;
    let tools = server.peer().list_all_tools().await.expect("list tools");
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    assert_eq!(names.len(), 13, "expected 13 tools, got: {names:?}");
    for expected in [
        "search_hr",
        "read_hr",
        "create_hr",
        "search_finance",
        "read_finance",
        "create_finance",
        "search_task_tracker",
        "read_task_tracker",
        "create_task_tracker",
        "search_public_forum",
        "read_public_forum",
        "create_public_forum",
        "send_email",
    ] {
        assert!(names.contains(&expected), "missing tool {expected}; have {names:?}");
    }
    server.cancel().await.ok();
}

#[tokio::test]
async fn search_read_create_and_email() {
    let data = TempData::new("crud");
    let server = spawn_server(data.path()).await;

    // search_hr finds the seeded record.
    let hit = text_of(&call(&server, "search_hr", serde_json::json!({ "query": "Alice" })).await);
    assert!(hit.contains("alice-chen.md"), "search_hr result: {hit}");

    // read_hr returns the secret content.
    let record = text_of(&call(&server, "read_hr", serde_json::json!({ "file": "alice-chen.md" })).await);
    assert!(record.contains("185,000"), "read_hr result: {record}");

    // create_task_tracker writes a new file.
    call(
        &server,
        "create_task_tracker",
        serde_json::json!({ "file": "TASK-103.md", "content": "# TASK-103\nDo the thing.\n" }),
    )
    .await;
    assert!(
        data.path().join("task_tracker/TASK-103.md").exists(),
        "task file not written"
    );

    // send_email drops a file into the email folder.
    let sent = text_of(
        &call(
            &server,
            "send_email",
            serde_json::json!({ "to": "auditor@example.com", "subject": "Q2 sync", "body": "hello" }),
        )
        .await,
    );
    assert!(sent.contains("email sent"), "send_email result: {sent}");
    let emails: Vec<_> = std::fs::read_dir(data.path().join("email"))
        .expect("email dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
        .collect();
    assert_eq!(emails.len(), 1, "expected exactly one email file");

    server.cancel().await.ok();
}

#[tokio::test]
async fn send_email_writes_to_the_sink_root_not_the_corpus() {
    let data = TempData::new("split-corpus");
    let sink = TempData::new("split-sink");
    let server = spawn_server_split(data.path(), sink.path()).await;

    call(
        &server,
        "send_email",
        serde_json::json!({ "to": "a@b.example", "subject": "split", "body": "x" }),
    )
    .await;
    assert!(
        !data.path().join("email").exists(),
        "corpus root must stay untouched by send_email"
    );
    let emails: Vec<_> = std::fs::read_dir(sink.path().join("email"))
        .expect("sink email dir")
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(emails.len(), 1, "expected the email under the sink root");

    server.cancel().await.ok();
}

#[tokio::test]
async fn read_missing_file_lists_available() {
    let data = TempData::new("missing");
    let server = spawn_server(data.path()).await;
    let out = text_of(&call(&server, "read_hr", serde_json::json!({ "file": "nope.md" })).await);
    assert!(out.contains("no file named"), "expected not-found message, got: {out}");
    assert!(
        out.contains("alice-chen.md"),
        "not-found should list available files, got: {out}"
    );
    server.cancel().await.ok();
}

#[tokio::test]
async fn rejects_path_traversal() {
    let data = TempData::new("traversal");
    let server = spawn_server(data.path()).await;
    let out = text_of(
        &call(
            &server,
            "read_hr",
            serde_json::json!({ "file": "../finance/q2-budget.md" }),
        )
        .await,
    );
    assert!(
        out.contains("invalid file name"),
        "traversal should be rejected, got: {out}"
    );
    server.cancel().await.ok();
}
