//! Drives the real `corp-systems-mcp` binary over MCP against a temp data root
//! and exercises the tool surface. No LLM or API key involved — this is the
//! CI-safe correctness gate for the server half of the demo.

use std::path::PathBuf;

use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use tokio::process::Command;

/// A throwaway data root seeded with representative systems, cleaned up on drop.
struct TempData(PathBuf);

impl TempData {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("corp-systems-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("hr")).unwrap();
        std::fs::create_dir_all(dir.join("finance")).unwrap();
        std::fs::create_dir_all(dir.join("task_tracker")).unwrap();
        std::fs::create_dir_all(dir.join("vendor")).unwrap();
        std::fs::write(
            dir.join("hr/alice-chen.md"),
            "# Alice Chen\nCompensation: $185,000\nSSN (last4): 4821\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("finance/acme-legal.md"),
            "# Acme legal packet\n\nCounterparty: Acme Cloud Services\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("vendor/acme-cloud.md"),
            "# Acme Cloud Services\n\nStatus: approved\n",
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
    spawn_server_with(root, root, None).await
}

/// Spawn the server with a corpus root and a separate `send_email` sink root.
async fn spawn_server_split(corpus: &PathBuf, sink: &PathBuf) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    spawn_server_with(corpus, sink, None).await
}

/// Spawn the server with a `--systems` enable list.
async fn spawn_server_systems(
    root: &PathBuf,
    systems: &'static str,
) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    spawn_server_with(root, root, Some(systems)).await
}

async fn spawn_server_with(
    corpus: &PathBuf,
    sink: &PathBuf,
    systems: Option<&'static str>,
) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    let bin = env!("CARGO_BIN_EXE_corp-systems-mcp");
    let transport = TokioChildProcess::new(Command::new(bin).configure(|cmd| {
        cmd.arg("--data-root").arg(corpus);
        cmd.arg("--sink-root").arg(sink);
        if let Some(list) = systems {
            cmd.arg("--systems").arg(list);
        }
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
async fn advertises_seventeen_tools() {
    let data = TempData::new("list");
    let server = spawn_server(data.path()).await;
    let tools = server.peer().list_all_tools().await.expect("list tools");
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    assert_eq!(names.len(), 17, "expected 17 tools, got: {names:?}");
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
        "search_vendor",
        "read_vendor",
        "create_vendor",
        "send_email",
        "share_legal_packet",
    ] {
        assert!(names.contains(&expected), "missing tool {expected}; have {names:?}");
    }
    server.cancel().await.ok();
}

#[tokio::test]
async fn vendor_search_read_and_create_use_the_vendor_folder() {
    let data = TempData::new("vendor");
    let server = spawn_server(data.path()).await;

    let search = call(&server, "search_vendor", serde_json::json!({ "query": "Acme" })).await;
    assert_ne!(search.is_error, Some(true));

    let record = call(&server, "read_vendor", serde_json::json!({ "file": "acme-cloud.md" })).await;
    assert_eq!(text_of(&record), "# Acme Cloud Services\n\nStatus: approved\n");

    let created = call(
        &server,
        "create_vendor",
        serde_json::json!({ "file": "northstar.md", "content": "# Northstar Legal\n" }),
    )
    .await;
    assert_ne!(created.is_error, Some(true));
    assert_eq!(
        std::fs::read_to_string(data.path().join("vendor/northstar.md")).unwrap(),
        "# Northstar Legal\n"
    );

    server.cancel().await.ok();
}

#[tokio::test]
async fn share_legal_packet_returns_content_and_sends_exact_body() {
    let data = TempData::new("legal-packet");
    let sink = TempData::new("legal-packet-sink");
    let server = spawn_server_split(data.path(), sink.path()).await;
    let packet = std::fs::read_to_string(data.path().join("finance/acme-legal.md")).unwrap();

    let result = call(
        &server,
        "share_legal_packet",
        serde_json::json!({ "file": "acme-legal.md", "to": "legal@example.com" }),
    )
    .await;

    assert_ne!(result.is_error, Some(true));
    let mut emails = std::fs::read_dir(sink.path().join("email")).unwrap();
    let email = emails.next().unwrap().unwrap().path();
    assert!(emails.next().is_none());
    assert_eq!(
        std::fs::read_to_string(email).unwrap(),
        format!("To: legal@example.com\nSubject: Legal packet: acme-legal.md\n\n{packet}")
    );
    assert_eq!(
        result
            .content
            .get(1)
            .and_then(|content| content.as_text())
            .map(|text| text.text.as_str()),
        Some(packet.as_str())
    );

    server.cancel().await.ok();
}

#[tokio::test]
async fn share_legal_packet_read_failure_sends_no_email() {
    let data = TempData::new("legal-packet-missing");
    let sink = TempData::new("legal-packet-missing-sink");
    let server = spawn_server_split(data.path(), sink.path()).await;

    let result = call(
        &server,
        "share_legal_packet",
        serde_json::json!({ "file": "missing.md", "to": "legal@example.com" }),
    )
    .await;

    assert_eq!(result.is_error, Some(true));
    assert!(!sink.path().join("email").exists());

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

#[tokio::test]
async fn systems_flag_narrows_the_tool_surface() {
    let data = TempData::new("narrow");
    let server = spawn_server_systems(data.path(), "hr, email, vendor").await;
    let tools = server.peer().list_all_tools().await.expect("list tools");
    let mut names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec![
            "create_hr",
            "create_vendor",
            "read_hr",
            "read_vendor",
            "search_hr",
            "search_vendor",
            "send_email"
        ],
        "only hr + email + vendor tools should be listed"
    );
    server.cancel().await.ok();
}

#[tokio::test]
async fn share_legal_packet_filter_requires_finance_and_email() {
    for (tag, systems) in [("finance-only", "finance"), ("email-only", "email")] {
        let data = TempData::new(tag);
        let server = spawn_server_systems(data.path(), systems).await;
        let names: Vec<_> = server
            .peer()
            .list_all_tools()
            .await
            .expect("list tools")
            .into_iter()
            .map(|tool| tool.name.into_owned())
            .collect();
        assert!(!names.iter().any(|name| name == "share_legal_packet"));
        server.cancel().await.ok();
    }

    let data = TempData::new("finance-and-email");
    let server = spawn_server_systems(data.path(), "finance,email").await;
    let mut names: Vec<_> = server
        .peer()
        .list_all_tools()
        .await
        .expect("list tools")
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec![
            "create_finance",
            "read_finance",
            "search_finance",
            "send_email",
            "share_legal_packet"
        ]
    );
    server.cancel().await.ok();
}

#[tokio::test]
async fn disabled_tool_is_refused_and_touches_nothing() {
    let data = TempData::new("disabled");
    let server = spawn_server_systems(data.path(), "hr").await;
    let mut params = CallToolRequestParams::new("create_task_tracker");
    params.arguments = serde_json::json!({ "file": "TASK-999.md", "content": "x" })
        .as_object()
        .cloned();
    let result = server.peer().call_tool(params).await;
    assert!(result.is_err(), "calling a disabled tool must fail, got: {result:?}");
    assert!(
        !data.path().join("task_tracker/TASK-999.md").exists(),
        "disabled create_task_tracker must not write"
    );

    let mut params = CallToolRequestParams::new("share_legal_packet");
    params.arguments = serde_json::json!({ "file": "acme-legal.md", "to": "legal@example.com" })
        .as_object()
        .cloned();
    let result = server.peer().call_tool(params).await;
    assert!(
        result.is_err(),
        "calling a disabled composite must fail, got: {result:?}"
    );
    assert!(!data.path().join("email").exists());
    server.cancel().await.ok();
}

#[tokio::test]
async fn bad_systems_value_fails_startup() {
    let bin = env!("CARGO_BIN_EXE_corp-systems-mcp");
    let out = std::process::Command::new(bin)
        .arg("--systems")
        .arg("hr,internet")
        .output()
        .expect("run corp-systems-mcp");
    assert!(!out.status.success(), "unknown system must exit nonzero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("internet"), "error should name the bad token: {stderr}");
}

#[tokio::test]
async fn rapid_same_subject_emails_land_as_separate_files() {
    let data = TempData::new("email-seq");
    let server = spawn_server(data.path()).await;
    for _ in 0..2 {
        call(
            &server,
            "send_email",
            serde_json::json!({ "to": "a@b.example", "subject": "same subject", "body": "x" }),
        )
        .await;
    }
    let emails: Vec<_> = std::fs::read_dir(data.path().join("email"))
        .expect("email dir")
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(emails.len(), 2, "same-second same-subject sends must not overwrite");
    server.cancel().await.ok();
}
