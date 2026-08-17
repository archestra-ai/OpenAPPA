
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const CONFIG: &str = r#"
[policy]
version = 1

[[policy.tool]]
name = "Bash"

[externals]
timeout_ms = 5000
max_body_bytes = 65536
"#;

struct Server {
    child: Child,
    url: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("an ephemeral port binds");
    let port = listener.local_addr().expect("the bound address is readable").port();
    drop(listener);
    port
}

fn start(config: &Path, db: &Path, port: u16) -> Server {
    let child = Command::new(env!("CARGO_BIN_EXE_appa-runtime-v2"))
        .arg("--config")
        .arg(config)
        .arg("--db")
        .arg(db)
        .arg("--listen")
        .arg(format!("127.0.0.1:{port}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the binary spawns");
    Server {
        child,
        url: format!("http://127.0.0.1:{port}"),
    }
}

fn wait_for_health(server: &mut Server) {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while std::time::Instant::now() < deadline {
        if let Some(status) = server.child.try_wait().expect("the child polls") {
            panic!("the server exited before becoming healthy: {status}");
        }
        if ureq_get(&format!("{}/health", server.url)).is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("the server never became healthy within the deadline");
}

fn ureq_get(url: &str) -> Option<String> {
    http(url, "GET", None)
}

fn post_hook(server: &Server, body: &str) -> Option<String> {
    http(&format!("{}/hook", server.url), "POST", Some(body))
}

fn http(url: &str, method: &str, body: Option<&str>) -> Option<String> {
    use std::io::{Read, Write};
    let rest = url.strip_prefix("http://")?;
    let (host, path) = rest.split_once('/').map(|(h, p)| (h, format!("/{p}")))?;
    let mut stream = std::net::TcpStream::connect(host).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("the read timeout sets");
    let body = body.unwrap_or("");
    let request = format!(
        "{method} {path} HTTP/1.1\r\nhost: {host}\r\ncontent-type: application/json\r\n\
         content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len(),
    );
    stream.write_all(request.as_bytes()).ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    let (head, payload) = response.split_once("\r\n\r\n")?;
    head.starts_with("HTTP/1.1 2").then(|| payload.to_string())
}

fn expect_startup_refusal(config: &Path, db: &Path, needle: &str) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_appa-runtime-v2"))
        .arg("--config")
        .arg(config)
        .arg("--db")
        .arg(db)
        .arg("--listen")
        .arg(format!("127.0.0.1:{}", free_port()))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the binary spawns");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().expect("the child polls") {
            break status;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the binary kept running instead of refusing");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(!status.success(), "the binary must refuse to serve");
    let mut stderr = String::new();
    use std::io::Read;
    child
        .stderr
        .take()
        .expect("stderr is piped")
        .read_to_string(&mut stderr)
        .expect("stderr reads");
    assert!(
        stderr.contains(needle),
        "the refusal must name its cause ({needle}); stderr was: {stderr}",
    );
}

fn write_config(dir: &Path, text: &str) -> PathBuf {
    let path = dir.join("appa.toml");
    std::fs::write(&path, text).expect("the config writes");
    path
}

#[test]
fn committed_state_survives_a_hard_kill_and_the_dispatch_stays_open() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let config = write_config(dir.path(), CONFIG);
    let db = dir.path().join("appa.db");
    let port = free_port();

    let mut server = start(&config, &db, port);
    wait_for_health(&mut server);
    post_hook(
        &server,
        r#"{"hook_event_name":"SessionStart","session_id":"crash-1","source":"startup"}"#,
    )
    .expect("SessionStart answers");
    post_hook(
        &server,
        r#"{"hook_event_name":"UserPromptSubmit","session_id":"crash-1","prompt":"read the report"}"#,
    )
    .expect("UserPromptSubmit answers");
    let allow = post_hook(
        &server,
        r#"{"hook_event_name":"PreToolUse","session_id":"crash-1","tool_name":"Bash","tool_input":{"command":"ls"},"tool_use_id":"t1"}"#,
    )
    .expect("PreToolUse answers");
    assert!(allow.contains("\"permissionDecision\":\"allow\""));

    let pid = server.child.id();
    drop(server); // SIGKILL via Drop
    let _ = pid;

    let port = free_port();
    let mut server = start(&config, &db, port);
    wait_for_health(&mut server);
    let kept = post_hook(
        &server,
        r#"{"hook_event_name":"PostToolUse","session_id":"crash-1","tool_name":"Bash","tool_input":{"command":"ls"},"tool_use_id":"t1","tool_response":{"stdout":"readme.txt"}}"#,
    )
    .expect("PostToolUse answers after the reopen");
    assert_eq!(kept, "{}", "the kept output answers with no opinion");

    let refused = post_hook(
        &server,
        r#"{"hook_event_name":"PostToolUse","session_id":"crash-1","tool_name":"Bash","tool_input":{"command":"ls"},"tool_use_id":"t1","tool_response":{"stdout":"again"}}"#,
    )
    .expect("the second PostToolUse still answers 200 with a block");
    assert!(refused.contains("\"decision\":\"block\""));
}

#[test]
fn a_changed_policy_keeps_old_roots_on_their_opening_policy() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let config = write_config(dir.path(), CONFIG);
    let db = dir.path().join("appa.db");
    let port = free_port();
    let mut server = start(&config, &db, port);
    wait_for_health(&mut server);
    post_hook(
        &server,
        r#"{"hook_event_name":"SessionStart","session_id":"old-1","source":"startup"}"#,
    )
    .expect("SessionStart answers");
    let allow = post_hook(
        &server,
        r#"{"hook_event_name":"PreToolUse","session_id":"old-1","tool_name":"Bash","tool_input":{"command":"ls"},"tool_use_id":"t1"}"#,
    )
    .expect("PreToolUse answers");
    assert!(allow.contains("\"permissionDecision\":\"allow\""));
    post_hook(
        &server,
        r#"{"hook_event_name":"PostToolUse","session_id":"old-1","tool_name":"Bash","tool_input":{"command":"ls"},"tool_use_id":"t1","tool_response":{"stdout":"readme.txt"}}"#,
    )
    .expect("PostToolUse answers");
    drop(server);

    let changed = write_config(dir.path(), &CONFIG.replace("name = \"Bash\"", "name = \"Read\""));
    let port = free_port();
    let mut server = start(&changed, &db, port);
    wait_for_health(&mut server);

    let old_allows = post_hook(
        &server,
        r#"{"hook_event_name":"PreToolUse","session_id":"old-1","tool_name":"Bash","tool_input":{"command":"pwd"},"tool_use_id":"t2"}"#,
    )
    .expect("the old root answers");
    assert!(
        old_allows.contains("\"permissionDecision\":\"allow\""),
        "the old root keeps its opening policy: {old_allows}",
    );

    post_hook(
        &server,
        r#"{"hook_event_name":"SessionStart","session_id":"new-1","source":"startup"}"#,
    )
    .expect("the new SessionStart answers");
    let new_denies = post_hook(
        &server,
        r#"{"hook_event_name":"PreToolUse","session_id":"new-1","tool_name":"Bash","tool_input":{"command":"ls"},"tool_use_id":"t3"}"#,
    )
    .expect("the new root answers");
    assert!(
        new_denies.contains("\"permissionDecision\":\"deny\""),
        "the new root follows the edited policy: {new_denies}",
    );
    let new_allows = post_hook(
        &server,
        r#"{"hook_event_name":"PreToolUse","session_id":"new-1","tool_name":"Read","tool_input":{"command":"x"},"tool_use_id":"t4"}"#,
    )
    .expect("the new root answers");
    assert!(
        new_allows.contains("\"permissionDecision\":\"allow\""),
        "the edited policy's tool releases on the new root: {new_allows}",
    );
}

#[test]
fn the_reload_route_installs_an_edited_policy_without_a_restart() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let config = write_config(dir.path(), CONFIG);
    let db = dir.path().join("appa.db");
    let port = free_port();
    let mut server = start(&config, &db, port);
    wait_for_health(&mut server);
    let reload = format!("{}/reload", server.url);

    post_hook(
        &server,
        r#"{"hook_event_name":"SessionStart","session_id":"old-1","source":"startup"}"#,
    )
    .expect("SessionStart answers");
    let allow = post_hook(
        &server,
        r#"{"hook_event_name":"PreToolUse","session_id":"old-1","tool_name":"Bash","tool_input":{"command":"ls"},"tool_use_id":"t1"}"#,
    )
    .expect("PreToolUse answers");
    assert!(allow.contains("\"permissionDecision\":\"allow\""));
    post_hook(
        &server,
        r#"{"hook_event_name":"PostToolUse","session_id":"old-1","tool_name":"Bash","tool_input":{"command":"ls"},"tool_use_id":"t1","tool_response":{"stdout":"readme.txt"}}"#,
    )
    .expect("PostToolUse answers");

    write_config(dir.path(), &CONFIG.replace("version = 1", "version = 1\nbogus_key = 1"));
    assert!(
        http(&reload, "POST", None).is_none(),
        "a file the dialect refuses must not install",
    );
    let still_allows = post_hook(
        &server,
        r#"{"hook_event_name":"PreToolUse","session_id":"old-1","tool_name":"Bash","tool_input":{"command":"pwd"},"tool_use_id":"t2"}"#,
    )
    .expect("the gate still answers after a refused reload");
    assert!(
        still_allows.contains("\"permissionDecision\":\"allow\""),
        "a refused reload changes nothing: {still_allows}",
    );
    post_hook(
        &server,
        r#"{"hook_event_name":"PostToolUse","session_id":"old-1","tool_name":"Bash","tool_input":{"command":"pwd"},"tool_use_id":"t2","tool_response":{"stdout":"/"}}"#,
    )
    .expect("PostToolUse answers");

    write_config(dir.path(), &CONFIG.replace("name = \"Bash\"", "name = \"Read\""));
    let installed = http(&reload, "POST", None).expect("the edited file installs");
    assert!(
        installed.contains("\"changed\":true"),
        "the answer names what is serving now: {installed}",
    );

    let old_allows = post_hook(
        &server,
        r#"{"hook_event_name":"PreToolUse","session_id":"old-1","tool_name":"Bash","tool_input":{"command":"id"},"tool_use_id":"t3"}"#,
    )
    .expect("the old root answers");
    assert!(
        old_allows.contains("\"permissionDecision\":\"allow\""),
        "the old root keeps the policy it opened with: {old_allows}",
    );

    post_hook(
        &server,
        r#"{"hook_event_name":"SessionStart","session_id":"new-1","source":"startup"}"#,
    )
    .expect("the new SessionStart answers");
    let new_denies = post_hook(
        &server,
        r#"{"hook_event_name":"PreToolUse","session_id":"new-1","tool_name":"Bash","tool_input":{"command":"ls"},"tool_use_id":"t4"}"#,
    )
    .expect("the new root answers");
    assert!(
        new_denies.contains("\"permissionDecision\":\"deny\""),
        "the new root follows the installed policy: {new_denies}",
    );

    let unchanged = http(&reload, "POST", None).expect("the unchanged file reloads");
    assert!(
        unchanged.contains("\"changed\":false"),
        "an unchanged file reports no change: {unchanged}",
    );
}

#[test]
fn a_damaged_database_refuses_to_serve() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let config = write_config(dir.path(), CONFIG);
    let db = dir.path().join("appa.db");
    std::fs::write(&db, b"not a sqlite database at all").expect("the file writes");
    expect_startup_refusal(&config, &db, "database");
}
