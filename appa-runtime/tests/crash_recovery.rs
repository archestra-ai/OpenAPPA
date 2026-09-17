//! The served process across a hard kill, a policy edit, and a reload. Every
//! event is posted to `/hook` as the canonical wire event `appa hook` would
//! post for the Claude Code hook it is written as, and every answer is read as
//! the wire decision the server sends.

mod common;
use common::{ServedRuntime, free_port, http, serve_runtime};

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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

static SERVER_SCENARIO: Mutex<()> = Mutex::new(());

fn serialize_server_scenarios() -> MutexGuard<'static, ()> {
    SERVER_SCENARIO
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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
fn post_hook(server: &ServedRuntime, claude_hook_json: &str) -> Option<serde_json::Value> {
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

    let server = serve_runtime(&config, &db);
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

    // `ServedRuntime` kills the process on drop, which is the hard kill this
    // scenario needs: the server never runs a shutdown path.
    drop(server);

    let server = serve_runtime(&config, &db);
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
    let server = serve_runtime(&config, &db);
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
    let server = serve_runtime(&changed, &db);

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
    let server = serve_runtime(&config, &db);
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

/// Native policy names resolve internally at startup and reload. The authored
/// file stays unchanged, while the canonical wire call selects the same contract.
#[test]
fn a_policy_naming_native_tools_serves_and_reloads() {
    let _scenario = serialize_server_scenarios();
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let db = dir.path().join("appa.db");
    let raw = CONFIG.replace("host/claude-code/Bash", "Bash");

    let config = write_config(dir.path(), &raw);
    let server = serve_runtime(&config, &db);
    let reload = format!("{}/reload", server.url);
    post_hook(
        &server,
        r#"{"hook_event_name":"SessionStart","session_id":"canon-1","source":"startup"}"#,
    )
    .expect("SessionStart answers");

    write_config(dir.path(), CONFIG);
    assert!(
        http(&reload, "POST", None).is_some(),
        "canonical policy names also install",
    );
    let still_allows = post_hook(
        &server,
        r#"{"hook_event_name":"PreToolUse","session_id":"canon-1","tool_name":"Bash","tool_input":{"command":"ls"},"tool_use_id":"t1"}"#,
    )
    .expect("the gate answers under the opening native policy after reload");
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

/// Deployment tool references normalize through the same resolver as tool rules.
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

#[test]
fn deployment_fields_accept_native_names_at_startup_and_reload() {
    let _scenario = serialize_server_scenarios();
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let db = dir.path().join("appa.db");
    let config = write_config(dir.path(), DEPLOYMENT_CONFIG);
    let server = serve_runtime(&config, &db);
    let reload = format!("{}/reload", server.url);

    for (field, canonical, raw_name) in [
        ("assumed_tools", "host/claude-code/Read", "Read"),
        ("provider_run_tools", "host/claude-code/WebSearch", "WebSearch"),
        ("confined_results", "host/claude-code/Bash", "Bash"),
    ] {
        let raw = DEPLOYMENT_CONFIG.replace(canonical, raw_name);
        write_config(dir.path(), &raw);
        assert!(
            http(&reload, "POST", None).is_some(),
            "a native name in {field} installs",
        );
        let fresh = serve_runtime(&config, &dir.path().join(format!("{field}.db")));
        drop(fresh);
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
