//! The served process across a hard kill, a policy edit, and a reload. Every
//! event is posted to `/hook` as the canonical wire event `appa hook` would
//! post for the Claude Code hook it is written as, and every answer is read as
//! the wire decision the server sends.

mod common;
use common::{free_port, http};

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use appa_runtime_api::{AdapterName, WireEvent};

const CONFIG: &str = r#"
[policy]
version = 2

[[policy.tool]]
name = "host/claude-code/Bash"

[externals]
timeout_ms = 5000
max_body_bytes = 65536
"#;

struct Server {
    child: Child,
    url: String,
}

static SERVER_SCENARIO: Mutex<()> = Mutex::new(());

fn serialize_server_scenarios() -> MutexGuard<'static, ()> {
    SERVER_SCENARIO
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start(config: &Path, db: &Path, port: u16) -> Server {
    let child = Command::new(env!("CARGO_BIN_EXE_appa"))
        .arg("runtime")
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
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        if let Some(status) = server.child.try_wait().expect("the child polls") {
            panic!("the server exited before becoming healthy: {status}");
        }
        if http(&format!("{}/health", server.url), "GET", None).is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("the server never became healthy within the deadline");
}

/// The wire event `appa hook` posts for one Claude Code hook.
fn wire(claude_hook_json: &str) -> String {
    let event = (appa_adapter_claude_code::codec().parse)(claude_hook_json.as_bytes())
        .expect("the hook parses")
        .expect("the hook is gated");
    let wire = WireEvent::from_event(AdapterName::ClaudeCode, &event).expect("the event translates");
    serde_json::to_string(&wire).expect("the wire event serializes")
}

/// The wire decision a 2xx answer carries, `None` on a non-2xx answer.
fn post_hook(server: &Server, claude_hook_json: &str) -> Option<serde_json::Value> {
    let body = http(&format!("{}/hook", server.url), "POST", Some(&wire(claude_hook_json)))?;
    Some(serde_json::from_str(&body).expect("a 2xx answer is a wire decision"))
}

fn expect_startup_refusal(config: &Path, db: &Path, needle: &str) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_appa"))
        .arg("runtime")
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

fn allowed(answer: &serde_json::Value) -> bool {
    answer["decision"] == "allow_call"
}

fn acked(answer: &serde_json::Value) -> bool {
    answer["decision"] == "ack"
}

#[test]
fn committed_state_survives_a_hard_kill_and_the_dispatch_stays_open() {
    let _scenario = serialize_server_scenarios();
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
    assert!(allowed(&allow), "{allow}");

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
    assert!(acked(&kept), "the kept output answers with no opinion: {kept}");

    let refused = post_hook(
        &server,
        r#"{"hook_event_name":"PostToolUse","session_id":"crash-1","tool_name":"Bash","tool_input":{"command":"ls"},"tool_use_id":"t1","tool_response":{"stdout":"again"}}"#,
    )
    .expect("the second PostToolUse still answers 200 with a block");
    assert_eq!(refused["decision"], "block", "{refused}");
}

#[test]
fn a_changed_policy_keeps_old_roots_on_their_opening_policy() {
    let _scenario = serialize_server_scenarios();
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
    assert!(allowed(&allow), "{allow}");
    post_hook(
        &server,
        r#"{"hook_event_name":"PostToolUse","session_id":"old-1","tool_name":"Bash","tool_input":{"command":"ls"},"tool_use_id":"t1","tool_response":{"stdout":"readme.txt"}}"#,
    )
    .expect("PostToolUse answers");
    drop(server);

    let changed = write_config(
        dir.path(),
        &CONFIG.replace("host/claude-code/Bash", "host/claude-code/Read"),
    );
    let port = free_port();
    let mut server = start(&changed, &db, port);
    wait_for_health(&mut server);

    let old_allows = post_hook(
        &server,
        r#"{"hook_event_name":"PreToolUse","session_id":"old-1","tool_name":"Bash","tool_input":{"command":"pwd"},"tool_use_id":"t2"}"#,
    )
    .expect("the old root answers");
    assert!(
        allowed(&old_allows),
        "the old root keeps its opening policy: {old_allows}",
    );

    post_hook(
        &server,
        r#"{"hook_event_name":"SessionStart","session_id":"new-1","source":"startup"}"#,
    )
    .expect("the new SessionStart answers");
    // The edited policy no longer covers Bash: the hook refuses the call typed, a
    // non-2xx answer.
    assert!(
        post_hook(
            &server,
            r#"{"hook_event_name":"PreToolUse","session_id":"new-1","tool_name":"Bash","tool_input":{"command":"ls"},"tool_use_id":"t3"}"#,
        )
        .is_none(),
        "the new root follows the edited policy: nothing covers Bash",
    );
    let new_allows = post_hook(
        &server,
        r#"{"hook_event_name":"PreToolUse","session_id":"new-1","tool_name":"Read","tool_input":{"command":"x"},"tool_use_id":"t4"}"#,
    )
    .expect("the new root answers");
    assert!(
        allowed(&new_allows),
        "the edited policy's tool releases on the new root: {new_allows}",
    );
}

#[test]
fn the_reload_route_installs_an_edited_policy_without_a_restart() {
    let _scenario = serialize_server_scenarios();
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
    assert!(allowed(&allow), "{allow}");
    post_hook(
        &server,
        r#"{"hook_event_name":"PostToolUse","session_id":"old-1","tool_name":"Bash","tool_input":{"command":"ls"},"tool_use_id":"t1","tool_response":{"stdout":"readme.txt"}}"#,
    )
    .expect("PostToolUse answers");

    write_config(dir.path(), &CONFIG.replace("version = 2", "version = 2\nbogus_key = 1"));
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
        allowed(&still_allows),
        "a refused reload changes nothing: {still_allows}",
    );
    post_hook(
        &server,
        r#"{"hook_event_name":"PostToolUse","session_id":"old-1","tool_name":"Bash","tool_input":{"command":"pwd"},"tool_use_id":"t2","tool_response":{"stdout":"/"}}"#,
    )
    .expect("PostToolUse answers");

    write_config(
        dir.path(),
        &CONFIG.replace("host/claude-code/Bash", "host/claude-code/Read"),
    );
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
        allowed(&old_allows),
        "the old root keeps the policy it opened with: {old_allows}",
    );

    post_hook(
        &server,
        r#"{"hook_event_name":"SessionStart","session_id":"new-1","source":"startup"}"#,
    )
    .expect("the new SessionStart answers");
    // The installed policy no longer covers Bash: the hook refuses the call typed, a
    // non-2xx answer.
    assert!(
        post_hook(
            &server,
            r#"{"hook_event_name":"PreToolUse","session_id":"new-1","tool_name":"Bash","tool_input":{"command":"ls"},"tool_use_id":"t4"}"#,
        )
        .is_none(),
        "the new root follows the installed policy: nothing covers Bash",
    );

    let unchanged = http(&reload, "POST", None).expect("the unchanged file reloads");
    assert!(
        unchanged.contains("\"changed\":false"),
        "an unchanged file reports no change: {unchanged}",
    );
}

/// A served deployment names tools canonically, because the wire carries the
/// host's raw spelling and the adapter derives the identity a contract must
/// match. A policy naming a tool the host's way refuses to serve at startup and
/// refuses to install on reload, leaving the running deployment serving.
#[test]
fn a_policy_naming_a_non_canonical_tool_refuses_to_serve_and_to_install() {
    let _scenario = serialize_server_scenarios();
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let db = dir.path().join("appa.db");
    let raw = CONFIG.replace("host/claude-code/Bash", "Bash");

    let config = write_config(dir.path(), &raw);
    expect_startup_refusal(&config, &db, "Bash");

    let config = write_config(dir.path(), CONFIG);
    let port = free_port();
    let mut server = start(&config, &db, port);
    wait_for_health(&mut server);
    let reload = format!("{}/reload", server.url);
    post_hook(
        &server,
        r#"{"hook_event_name":"SessionStart","session_id":"canon-1","source":"startup"}"#,
    )
    .expect("SessionStart answers");

    write_config(dir.path(), &raw);
    assert!(
        http(&reload, "POST", None).is_none(),
        "a policy naming a raw tool must not install",
    );
    let still_allows = post_hook(
        &server,
        r#"{"hook_event_name":"PreToolUse","session_id":"canon-1","tool_name":"Bash","tool_input":{"command":"ls"},"tool_use_id":"t1"}"#,
    )
    .expect("the gate still answers after the refused reload");
    assert!(allowed(&still_allows), "{still_allows}");

    // A selector on a canonical name is canonical: the rule reads the tool it names.
    write_config(
        dir.path(),
        &CONFIG.replace(
            "name = \"host/claude-code/Bash\"",
            "name = \"host/claude-code/Bash(command:ls*)\"",
        ),
    );
    let installed = http(&reload, "POST", None).expect("a selector on a canonical name installs");
    assert!(installed.contains("\"changed\":true"), "{installed}");
}

/// A wildcard contract covers every name, so a `[deployment]` field naming a tool the
/// host's way passes coverage: only the served rule refuses it. Every field here names a
/// tool the served adapter derives.
const DEPLOYMENT_CONFIG: &str = r#"
[policy]
version = 2

[[policy.annotator]]
name = "any"
builtin = "claude-code"

[[policy.tool]]
name = "*"
annotator = "any"

[policy.deployment]
assumed_tools = ["host/claude-code/Read"]
provider_run_tools = ["host/claude-code/WebSearch"]
confined_results = ["host/claude-code/Bash"]

[externals]
timeout_ms = 5000
max_body_bytes = 65536
"#;

/// A `[deployment]` field names a tool the way a contract does: the identity the served
/// adapter derives, matched exactly. A raw spelling there confines or excepts nothing, so
/// each field refuses to serve at startup and to install on reload, naming the field it
/// refused.
#[test]
fn a_deployment_field_naming_a_non_canonical_tool_refuses_to_serve_and_to_install() {
    let _scenario = serialize_server_scenarios();
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let db = dir.path().join("appa.db");
    let config = write_config(dir.path(), DEPLOYMENT_CONFIG);
    let port = free_port();
    let mut server = start(&config, &db, port);
    wait_for_health(&mut server);
    let reload = format!("{}/reload", server.url);

    for (field, canonical, raw_name) in [
        ("assumed_tools", "host/claude-code/Read", "Read"),
        ("provider_run_tools", "host/claude-code/WebSearch", "WebSearch"),
        ("confined_results", "host/claude-code/Bash", "Bash"),
    ] {
        let raw = DEPLOYMENT_CONFIG.replace(canonical, raw_name);
        write_config(dir.path(), &raw);
        assert!(
            http(&reload, "POST", None).is_none(),
            "a raw name in {field} must not install",
        );
        expect_startup_refusal(&config, &dir.path().join("refused.db"), field);
    }

    write_config(dir.path(), DEPLOYMENT_CONFIG);
    assert!(http(&reload, "POST", None).is_some(), "the canonical policy installs",);
}

#[test]
fn a_damaged_database_refuses_to_serve() {
    let _scenario = serialize_server_scenarios();
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let config = write_config(dir.path(), CONFIG);
    let db = dir.path().join("appa.db");
    std::fs::write(&db, b"not a sqlite database at all").expect("the file writes");
    expect_startup_refusal(&config, &db, "database");
}
