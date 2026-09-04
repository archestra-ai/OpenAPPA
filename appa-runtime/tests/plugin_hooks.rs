mod common;
use common::serve;

use std::io::Write;
use std::process::{Command, Stdio};

use axum::Router;
use axum::routing::post;

/// The hooks that report the root actor's finished turn. Every blocking
/// outcome on one of these means "do not stop", so none of them may carry
/// one. SubagentStop is not one: it checks the subagent's final message and
/// blocks like a tool hook.
const TURN_ENDS: [&str; 2] = ["Stop", "StopFailure"];

fn turn_end_command() -> &'static str {
    "[ \"${APPA_GATE:-}\" = 1 ] || exit 0; sh \"${CLAUDE_PLUGIN_ROOT}/hooks/hook.sh\" --turn-end || exit 0"
}

/// The binary the shipped commands must reach. A deployed tree has this
/// rendered into appa-paths.sh as an absolute path; the checkout's development
/// copy takes it from the environment, which is what lets a checkout run
/// against the binary built here rather than whatever appa this machine has.
fn built_binary() -> &'static std::path::Path {
    std::path::Path::new(env!("CARGO_BIN_EXE_appa"))
}

fn plugin_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../integrations/claude-code/plugin")
}

fn shipped(file: &str) -> serde_json::Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../integrations/claude-code/plugin/hooks")
        .join(file);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap_or_else(|_| panic!("the shipped {file} is readable")))
        .unwrap_or_else(|_| panic!("the shipped {file} parses"))
}

fn plugin_file(file: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../integrations/claude-code/plugin")
        .join(file)
}

#[test]
fn an_ungated_session_has_no_appa_statusline() {
    let output = Command::new("sh")
        .arg(plugin_file("statusline.sh"))
        .env_remove("APPA_GATE")
        .stdin(Stdio::piped())
        .output()
        .expect("the POSIX statusline runs");
    assert!(output.status.success());
    assert_eq!(output.stdout, b"", "plain Claude must have no APPA statusline");
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

/// The events each shipped map marks as turn ends, from the flag its hook
/// command carries: `--turn-end` to hook.sh, `-TurnEnd` to hook.ps1. Nothing in
/// the shipped scripts reads the event name for this, so the map is the only
/// place the marking lives.
fn turn_end_events(map: &serde_json::Value) -> Vec<String> {
    let mut events: Vec<String> = map["hooks"]
        .as_object()
        .expect("the hook map is an object")
        .iter()
        .filter(|(_, groups)| {
            groups
                .as_array()
                .expect("each event carries groups")
                .iter()
                .flat_map(|group| group["hooks"].as_array().expect("each group carries hooks"))
                .any(|hook| {
                    let command_words = hook["command"]
                        .as_str()
                        .map(str::split_whitespace)
                        .into_iter()
                        .flatten();
                    let args = hook["args"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|arg| arg.as_str());
                    command_words
                        .chain(args)
                        .any(|word| word == "--turn-end" || word == "-TurnEnd")
                })
        })
        .map(|(event, _)| event.clone())
        .collect();
    events.sort();
    events
}

/// Both shipped maps mark the same events as turn ends. A turn end marked on one
/// platform and not the other would block, or fail to, on that platform alone.
#[test]
fn both_shipped_hook_maps_mark_the_same_turn_ends() {
    let mut expected: Vec<String> = TURN_ENDS.iter().map(|event| (*event).to_owned()).collect();
    expected.sort();
    assert_eq!(turn_end_events(&shipped("hooks.json")), expected);
    assert_eq!(turn_end_events(&shipped("hooks.windows.json")), expected);
}

/// Both shipped maps inject the same session context, and nothing else reaches
/// the second SessionStart entry: a rename or a drift on one platform would
/// leave that platform's sessions without the context the other one gets. The
/// assertions read paths and wiring, never the file's wording.
#[test]
fn both_shipped_hook_maps_inject_the_same_session_context() {
    const CONTEXT: &str = "session-context.md";
    let hooks_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../integrations/claude-code/plugin/hooks");
    assert!(hooks_dir.join(CONTEXT).is_file(), "the shipped {CONTEXT} is missing");

    let posix = shipped("hooks.json");
    let printed = posix["hooks"]["SessionStart"][0]["hooks"][1]["command"]
        .as_str()
        .expect("the POSIX SessionStart group carries a second, context-printing hook");
    assert!(
        printed.contains(CONTEXT),
        "the POSIX SessionStart hook no longer prints {CONTEXT}",
    );

    let windows = shipped("hooks.windows.json");
    let args: Vec<&str> = windows["hooks"]["SessionStart"][0]["hooks"][1]["args"]
        .as_array()
        .expect("the Windows SessionStart group carries a second, context-printing hook")
        .iter()
        .map(|arg| arg.as_str().expect("every argument is a string"))
        .collect();
    assert!(
        args.contains(&"-SessionContext"),
        "the Windows SessionStart hook no longer asks hook.ps1 for the session context",
    );
}

/// The one command every posting hook in the shipped POSIX map registers,
/// read from the map itself. SessionStart is that command with the runtime
/// start chained in between its gate and its hook: `<gate>; <starter> && <hook>`.
fn shipped_command() -> String {
    let hooks = shipped("hooks.json");
    let command = hooks["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .expect("the PreToolUse hook carries a command")
        .to_string();
    for event in ["PostToolUse", "SubagentStart", "SubagentStop", "PostToolUseFailure"] {
        let entry = &hooks["hooks"][event][0]["hooks"][0]["command"];
        assert_eq!(
            entry.as_str(),
            Some(command.as_str()),
            "the {event} hook command drifted from the PreToolUse one",
        );
    }
    // A turn end decides nothing, and blocking this hook holds the actor
    // in a turn it has finished, so these never carry the blocking exit
    // and never print an answer.
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
    let (gate, hook) = command
        .split_once("; ")
        .expect("the shared command is a gate followed by the hook");
    let starter = "sh \"${CLAUDE_PLUGIN_ROOT}/hooks/ensure-runtime.sh\" </dev/null";
    assert_eq!(
        hooks["hooks"]["SessionStart"][0]["hooks"][0]["command"].as_str(),
        Some(format!("{gate}; {starter} && {hook}").as_str()),
        "the SessionStart hook is no longer the shared command plus its runtime start",
    );
    // A prompt is refused while a subagent definition declares maxTurns: the
    // scan runs before the post, under the same guard and blocking exit.
    let scan = "sh \"${CLAUDE_PLUGIN_ROOT}/hooks/scan-agents.sh\"";
    assert_eq!(
        hooks["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"].as_str(),
        Some(format!("{gate}; {scan} && {hook}").as_str()),
        "the UserPromptSubmit hook is no longer the shared command plus the agent scan",
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
        .env("CLAUDE_PLUGIN_ROOT", plugin_root())
        .env("APPA_BIN", built_binary())
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

#[tokio::test(flavor = "multi_thread")]
async fn a_2xx_answer_passes_its_body_through_with_exit_0() {
    let url = serve(Router::new().route("/hook", post(|| async { r#"{"decision":"block","reason":"denied"}"# }))).await;
    let command = shipped_command();
    let (code, stdout) = tokio::task::spawn_blocking(move || run_hook(&command, &url))
        .await
        .expect("the blocking task joins");
    assert_eq!(code, 0);
    assert_eq!(stdout, r#"{"decision":"block","reason":"denied"}"#);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_error_exits_2_instead_of_failing_open() {
    let url = serve(Router::new().route(
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

/// The same guarantee where the runtime answers and refuses: the answer decides
/// nothing, so it never reaches the harness and never becomes a blocking outcome.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_turn_end_still_prints_nothing_and_exits_0() {
    let url = serve(Router::new().route(
        "/hook",
        post(|| async { (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom") }),
    ))
    .await;
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
