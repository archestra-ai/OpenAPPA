mod common;
use common::{serve, serve_runtime};

use std::io::Write;
use std::process::{Command, Stdio};

use axum::Router;
use axum::routing::post;

/// The gated hook every command here posts: a call the runtime must decide.
const PRE_TOOL_USE: &str =
    r#"{"hook_event_name":"PreToolUse","session_id":"plugin-test","tool_name":"Bash","tool_input":{"command":"ls"}}"#;

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
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../marketplace/adapters/claude-code/plugin")
}

fn shipped(file: &str) -> serde_json::Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../marketplace/adapters/claude-code/plugin/hooks")
        .join(file);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap_or_else(|_| panic!("the shipped {file} is readable")))
        .unwrap_or_else(|_| panic!("the shipped {file} parses"))
}

fn plugin_file(file: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../marketplace/adapters/claude-code/plugin")
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
    let hooks_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../marketplace/adapters/claude-code/plugin/hooks");
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

/// The shipped POSIX command over one host event, with `APPA_GATE` set to
/// whatever the caller's session carries.
fn spawn_hook(command: &str, url: &str, gate: &str, host_event: &str) -> (i32, String) {
    let child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .env("APPA_RUNTIME_URL", url)
        .env("CLAUDE_PLUGIN_ROOT", plugin_root())
        .env("APPA_BIN", built_binary())
        .env("APPA_GATE", gate)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the hook command spawns");
    finish(child, host_event)
}

/// The command in a gated session, over one host event of the caller's choosing.
fn run_hook_on(command: &str, url: &str, host_event: &str) -> (i32, String) {
    spawn_hook(command, url, "1", host_event)
}

fn run_hook(command: &str, url: &str) -> (i32, String) {
    run_hook_on(command, url, PRE_TOOL_USE)
}

/// The same command in a session APPA does not gate.
fn run_ungated(command: &str, url: &str) -> (i32, String) {
    spawn_hook(command, url, "0", PRE_TOOL_USE)
}

/// Feed `stdin` to a spawned hook and collect its exit code and stdout.
fn finish(mut child: std::process::Child, stdin: &str) -> (i32, String) {
    // An ungated hook may exit before reading its stdin, closing the pipe
    // mid-write; that is a pass condition, so only a non-EPIPE error fails.
    if let Err(error) = child
        .stdin
        .as_mut()
        .expect("the child has a stdin pipe")
        .write_all(stdin.as_bytes())
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

/// A 2xx wire decision is rendered into Claude Code's hook answer and exits 0.
#[tokio::test(flavor = "multi_thread")]
async fn a_2xx_answer_is_rendered_for_the_host_with_exit_0() {
    let url = serve(Router::new().route(
        "/hook",
        post(|| async { r#"{"protocol":1,"decision":"block","reason":"denied"}"# }),
    ))
    .await;
    let command = shipped_command();
    let (code, stdout) = tokio::task::spawn_blocking(move || run_hook(&command, &url))
        .await
        .expect("the blocking task joins");
    assert_eq!(code, 0);
    let answer: serde_json::Value = serde_json::from_str(&stdout).expect("the answer is JSON");
    assert_eq!(answer, serde_json::json!({"decision": "block", "reason": "denied"}));
}

/// A 2xx body that is no wire decision is not passed through: the hook fails closed.
#[tokio::test(flavor = "multi_thread")]
async fn a_2xx_answer_that_is_no_wire_decision_exits_2() {
    let url = serve(Router::new().route("/hook", post(|| async { "{}" }))).await;
    let command = shipped_command();
    let (code, stdout) = tokio::task::spawn_blocking(move || run_hook(&command, &url))
        .await
        .expect("the blocking task joins");
    assert_eq!(code, 2, "an answer off the wire must block the action");
    assert_eq!(stdout, "");
}

fn run_client(url: &str, stdin: &str) -> (i32, String) {
    let child = Command::new(built_binary())
        .arg("hook")
        .arg("--url")
        .arg(url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the hook client spawns");
    finish(child, stdin)
}

/// `appa hook` end to end against a served runtime: the
/// Claude Code hook is translated onto the wire, the runtime decides under a
/// policy naming the canonical tool, and the decision comes back in Claude Code's
/// shape with the exit code its outcome takes.
#[test]
fn the_hook_client_translates_both_ways_against_a_served_runtime() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let config = dir.path().join("appa.toml");
    std::fs::write(
        &config,
        "[policy]\nversion = 2\n\n[[policy.tool]]\nname = \"host/claude-code/Bash\"\n\n\
         [externals]\ntimeout_ms = 5000\nmax_body_bytes = 65536\n",
    )
    .expect("the config writes");
    let runtime = serve_runtime(&config, &dir.path().join("appa.db"));

    let (code, stdout) = run_client(&runtime.url, PRE_TOOL_USE);
    assert_eq!(code, 0, "{stdout}");
    let answer: serde_json::Value = serde_json::from_str(&stdout).expect("the answer is JSON");
    assert_eq!(answer["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "allow", "{answer}");

    let ran = r#"{"hook_event_name":"PostToolUse","session_id":"plugin-test","tool_name":"Bash","tool_input":{"command":"ls"},"tool_response":{"stdout":"readme.txt"}}"#;
    let (code, stdout) = run_client(&runtime.url, ran);
    assert_eq!(code, 0, "{stdout}");
    assert_eq!(stdout, "{}", "a kept result answers with no opinion");

    // Nothing covers Write: the runtime refuses the call typed, and the client blocks.
    let uncovered = r#"{"hook_event_name":"PreToolUse","session_id":"plugin-test","tool_name":"Write","tool_input":{"file_path":"x","content":"y"}}"#;
    let (code, stdout) = run_client(&runtime.url, uncovered);
    assert_eq!(code, 2, "a runtime refusal must block the action: {stdout}");
    let answer: serde_json::Value = serde_json::from_str(&stdout).expect("the refusal renders");
    assert!(answer["error"].is_string(), "{answer}");

    let ungated = r#"{"hook_event_name":"Notification","session_id":"plugin-test"}"#;
    let (code, stdout) = run_client(&runtime.url, ungated);
    assert_eq!(code, 0);
    assert_eq!(stdout, "{}", "an ungated hook answers without a round trip");
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
    let (code, stdout) = tokio::task::spawn_blocking(move || run_ungated(&command, &url))
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

/// The shipped POSIX wrapper carries the withholding out the way the harness reads
/// it. The wrapper adds its own exit handling around the client, so the platform a
/// deployment actually runs is checked here rather than only the client under it.
#[tokio::test(flavor = "multi_thread")]
async fn the_posix_hook_exits_zero_when_it_withholds_a_result() {
    let url = refused_url().await;
    let command = shipped_command();
    let ran = r#"{"hook_event_name":"PostToolUse","session_id":"plugin-test","tool_name":"Bash","tool_input":{"command":"ls"},"tool_response":{"stdout":"readme.txt"}}"#;
    let (code, stdout) = tokio::task::spawn_blocking(move || run_hook_on(&command, &url, ran))
        .await
        .expect("the blocking task joins");
    assert_eq!(code, 0, "a discarded replacement leaves the output in place: {stdout}");
    let answer: serde_json::Value = serde_json::from_str(&stdout).expect("the wrapper prints the withholding");
    assert!(
        !answer["hookSpecificOutput"]["updatedToolOutput"].is_null(),
        "the wrapper carries the replacement through: {answer}"
    );
    assert!(!answer.to_string().contains("readme.txt"), "{answer}");
}

/// A tool whose result already ran needs more than an exit code: the harness
/// keeps output it was not told to replace, so an unanswered post-use hook
/// renders the withholding for the result it reports.
#[tokio::test(flavor = "multi_thread")]
async fn an_unanswered_post_use_hook_withholds_the_result_it_reports() {
    let url = refused_url().await;
    let ran = r#"{"hook_event_name":"PostToolUse","session_id":"plugin-test","tool_name":"Bash","tool_input":{"command":"ls"},"tool_response":{"stdout":"readme.txt"}}"#;
    let (code, stdout) = tokio::task::spawn_blocking(move || run_client(&url, ran))
        .await
        .expect("the blocking task joins");
    assert_eq!(
        code, 0,
        "the harness applies the replacement only from a hook that exits zero: {stdout}"
    );
    let answer: serde_json::Value = serde_json::from_str(&stdout).expect("the withholding renders as JSON: {stdout}");
    assert_eq!(answer["hookSpecificOutput"]["hookEventName"], "PostToolUse", "{answer}");
    assert!(
        !answer["hookSpecificOutput"]["updatedToolOutput"].is_null(),
        "the produced output is replaced, not left in front of the model: {answer}"
    );
    assert!(
        !answer.to_string().contains("readme.txt"),
        "the withheld body never reaches the model: {answer}"
    );
}

/// The same guarantee where the runtime answers and refuses. Neither answer carries a
/// replacement — a `refuse` decides nothing the harness can put in a result's place, and an
/// error body is no wire decision at all — so the client synthesizes the withholding: a
/// replacement carried out on a blocking exit is discarded, and the output the tool already
/// produced would stay in front of the model.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_post_use_hook_withholds_the_result_it_reports() {
    let ran = r#"{"hook_event_name":"PostToolUse","session_id":"plugin-test","tool_name":"Bash","tool_input":{"command":"ls"},"tool_response":{"stdout":"readme.txt"}}"#;
    for answer in [
        (
            axum::http::StatusCode::CONFLICT,
            r#"{"protocol":1,"decision":"refuse","detail":"storage failure: disk full"}"#,
        ),
        (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom"),
    ] {
        let url = serve(Router::new().route("/hook", post(move || async move { answer }))).await;
        let (code, stdout) = tokio::task::spawn_blocking(move || run_client(&url, ran))
            .await
            .expect("the blocking task joins");
        assert_eq!(
            code, 0,
            "the harness applies the replacement only from a hook that exits zero: {answer:?} {stdout}"
        );
        let rendered: serde_json::Value = serde_json::from_str(&stdout).expect("the withholding renders as JSON");
        assert!(
            !rendered["hookSpecificOutput"]["updatedToolOutput"].is_null(),
            "the produced output is replaced, not left in front of the model: {rendered}"
        );
        assert!(
            !rendered.to_string().contains("readme.txt"),
            "the withheld body never reaches the model: {rendered}"
        );
    }
}

/// A call the harness has not run yet needs no replacement: exiting non-zero is
/// what stops it, and nothing was produced to withhold.
#[tokio::test(flavor = "multi_thread")]
async fn an_unanswered_pre_use_hook_prints_no_replacement() {
    let url = refused_url().await;
    let proposed = r#"{"hook_event_name":"PreToolUse","session_id":"plugin-test","tool_name":"Bash","tool_input":{"command":"ls"}}"#;
    let (code, stdout) = tokio::task::spawn_blocking(move || run_client(&url, proposed))
        .await
        .expect("the blocking task joins");
    assert_eq!(code, 2, "no answer from the runtime must block the call");
    assert_eq!(stdout, "", "a call that never ran has no output to replace");
}

/// A host event the adapter reads but that cannot cross the wire: an empty session id
/// names no trajectory, so the translation fails after the event is already understood.
/// The tool has run by then, so the event is answered rather than dropped — the result is
/// withheld and the exit is the zero the harness applies a replacement from. The same
/// failure on a call that has not run is stopped by the exit code alone.
#[tokio::test(flavor = "multi_thread")]
async fn an_event_that_cannot_cross_the_wire_still_withholds_the_result_it_reports() {
    // A runtime that allows whatever reaches it: nothing here may, so an allowed call is
    // what a client that posted this event would print.
    let url = serve(Router::new().route("/hook", post(|| async { r#"{"protocol":1,"decision":"allow_call"}"# }))).await;
    let ran = r#"{"hook_event_name":"PostToolUse","session_id":"","tool_name":"Bash","tool_input":{"command":"ls"},"tool_response":{"stdout":"readme.txt"}}"#;
    let proposed =
        r#"{"hook_event_name":"PreToolUse","session_id":"","tool_name":"Bash","tool_input":{"command":"ls"}}"#;
    let (ran, proposed) = tokio::task::spawn_blocking(move || (run_client(&url, ran), run_client(&url, proposed)))
        .await
        .expect("the blocking task joins");

    let (code, stdout) = ran;
    assert_eq!(
        code, 0,
        "the harness applies the replacement only from a hook that exits zero: {stdout}"
    );
    let answer: serde_json::Value = serde_json::from_str(&stdout).expect("the withholding renders as JSON");
    assert_eq!(answer["hookSpecificOutput"]["hookEventName"], "PostToolUse", "{answer}");
    assert!(
        !answer["hookSpecificOutput"]["updatedToolOutput"].is_null(),
        "the produced output is replaced, not left in front of the model: {answer}"
    );
    assert!(
        !answer.to_string().contains("readme.txt"),
        "the withheld body never reaches the model: {answer}"
    );

    let (code, stdout) = proposed;
    assert_eq!(code, 2, "a call that never ran is stopped by the exit code");
    assert_eq!(stdout, "", "a call that never ran has no output to replace");
}

/// A host event the codec cannot read at all: this `PostToolUse` misses `tool_input`, which
/// every parse of one requires. The tool has run all the same, so the hook is answered by
/// the withholding the codec reads out of the bytes — a hook that only exited non-zero here
/// would leave the output it reports in front of the model. A call that has not run is
/// stopped by the exit code alone, with nothing printed.
#[tokio::test(flavor = "multi_thread")]
async fn a_host_event_the_codec_cannot_read_still_withholds_the_result_it_reports() {
    let url = refused_url().await;
    let ran = r#"{"hook_event_name":"PostToolUse","session_id":"plugin-test","tool_name":"Bash","tool_response":{"stdout":"readme.txt"}}"#;
    let proposed = r#"{"hook_event_name":"PreToolUse","session_id":"plugin-test","tool_name":"Bash"}"#;
    let (ran, proposed) = tokio::task::spawn_blocking(move || (run_client(&url, ran), run_client(&url, proposed)))
        .await
        .expect("the blocking task joins");

    let (code, stdout) = ran;
    assert_eq!(
        code, 0,
        "the harness applies the replacement only from a hook that exits zero: {stdout}"
    );
    let answer: serde_json::Value = serde_json::from_str(&stdout).expect("the withholding renders as JSON");
    assert_eq!(answer["hookSpecificOutput"]["hookEventName"], "PostToolUse", "{answer}");
    assert!(
        !answer["hookSpecificOutput"]["updatedToolOutput"].is_null(),
        "the produced output is replaced, not left in front of the model: {answer}"
    );
    assert!(
        !answer.to_string().contains("readme.txt"),
        "the withheld body never reaches the model: {answer}"
    );

    let (code, stdout) = proposed;
    assert_eq!(code, 2, "a call that never ran is stopped by the exit code");
    assert_eq!(stdout, "", "a call that never ran has no output to replace");
}

/// The client with the read end of its stdout closed before it answers: whatever it renders
/// cannot reach the harness, and only its exit code is left to report that.
fn run_unheard_client(url: &str, stdin: &str) -> i32 {
    let mut child = Command::new(built_binary())
        .arg("hook")
        .arg("--url")
        .arg(url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the hook client spawns");
    drop(child.stdout.take().expect("the child has a stdout pipe"));
    child
        .stdin
        .as_mut()
        .expect("the child has a stdin pipe")
        .write_all(stdin.as_bytes())
        .expect("the event writes to the hook's stdin");
    child
        .wait()
        .expect("the hook client finishes")
        .code()
        .expect("the hook client exits with a code")
}

/// A withholding carries its whole effect through what the client prints, so one the
/// harness never received withheld nothing. Here the read end of the client's stdout is
/// closed before it answers, so the write fails: the client must not exit zero and report a
/// replacement that never arrived.
#[tokio::test(flavor = "multi_thread")]
async fn a_withholding_that_cannot_be_written_does_not_exit_zero() {
    let url = refused_url().await;
    let ran = r#"{"hook_event_name":"PostToolUse","session_id":"plugin-test","tool_name":"Bash","tool_input":{"command":"ls"},"tool_response":{"stdout":"readme.txt"}}"#;
    let code = tokio::task::spawn_blocking(move || run_unheard_client(&url, ran))
        .await
        .expect("the blocking task joins");
    assert_eq!(code, 2, "a replacement the harness never received must not exit zero");
}

/// The same holds for an answer the runtime did give: a decision the harness never received
/// decided nothing, and exiting zero on it would release the call the runtime denied. The
/// pair pins the write as the only difference — heard, the same answer renders and exits 0.
#[tokio::test(flavor = "multi_thread")]
async fn a_decision_that_cannot_be_written_does_not_exit_zero() {
    for answer in [
        r#"{"protocol":1,"decision":"deny_call","feedback":"[appa] Blocked: this call cannot run yet."}"#,
        r#"{"protocol":1,"decision":"block","reason":"denied"}"#,
        r#"{"protocol":1,"decision":"replace_output","output":"[appa] the output is confined"}"#,
        r#"{"protocol":1,"decision":"deliver_value","value":"{\"ticket\":\"scrubbed\"}"}"#,
    ] {
        let url = serve(Router::new().route("/hook", post(move || async move { answer }))).await;
        let heard = url.clone();
        let (heard, unheard) = tokio::task::spawn_blocking(move || {
            (run_client(&heard, PRE_TOOL_USE), run_unheard_client(&url, PRE_TOOL_USE))
        })
        .await
        .expect("the blocking task joins");

        let (code, stdout) = heard;
        assert_eq!(code, 0, "the answer is one the client renders and exits 0 on: {stdout}");
        assert!(!stdout.is_empty(), "{answer} renders an answer for the harness");
        assert_eq!(
            unheard, 2,
            "a decision the harness never received must not exit zero: {answer}"
        );
    }
}

/// Run the shipped Windows hook under whatever PowerShell this machine has, and
/// answer `None` where it has none: the script is shipped for Windows, and the
/// developer machines that build this crate mostly cannot run it. The paths a
/// deployment renders into appa-paths.ps1 are passed as the environment the
/// checkout's development copy reads instead.
fn run_windows_hook(url: &str, event: &str) -> Option<(i32, String)> {
    let data = tempfile::tempdir().expect("a temp dir is creatable");
    let spawned = Command::new("pwsh")
        .arg("-NoProfile")
        .arg("-File")
        .arg(plugin_file("hooks/hook.ps1"))
        .env("APPA_RUNTIME_URL", url)
        .env("APPA_BIN", built_binary())
        .env("APPA_GATE", "1")
        .env("APPA_DATA_DIR", data.path())
        .env("APPA_CONFIG", data.path().join("appa.toml"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    match spawned {
        Ok(child) => Some(finish(child, event)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => panic!("the Windows hook script spawns: {error}"),
    }
}

/// A native command that exits non-zero raises nothing in PowerShell, so the
/// Windows hook reads the client's exit code itself. An unanswered PostToolUse
/// replaces the result the harness has already produced -- the exit code alone
/// leaves the tool's own output in front of the model -- and an unanswered
/// PreToolUse blocks the call.
#[tokio::test(flavor = "multi_thread")]
async fn the_windows_hook_fails_closed_when_its_client_cannot_answer() {
    let url = refused_url().await;
    let ran = tokio::task::spawn_blocking(move || {
        let post = r#"{"hook_event_name":"PostToolUse","session_id":"plugin-test","tool_name":"Bash","tool_input":{"command":"ls"},"tool_response":{"stdout":"readme.txt"}}"#;
        (run_windows_hook(&url, post), run_windows_hook(&url, PRE_TOOL_USE))
    })
    .await
    .expect("the blocking task joins");
    let (Some((post_code, post_stdout)), Some((pre_code, pre_stdout))) = ran else {
        return;
    };

    assert_eq!(
        post_code, 0,
        "a replaced result reaches the model only on exit 0: {post_stdout}"
    );
    let answer: serde_json::Value = serde_json::from_str(post_stdout.trim()).expect("the fail-closed answer is JSON");
    assert_eq!(answer["decision"], "block", "{answer}");
    assert_eq!(answer["hookSpecificOutput"]["hookEventName"], "PostToolUse", "{answer}");
    assert!(
        answer["hookSpecificOutput"]["updatedToolOutput"].is_string(),
        "the tool's own output must be replaced, not left in front of the model: {answer}"
    );

    assert_eq!(pre_code, 2, "an unanswered pre-use hook must block the call");
    assert_eq!(pre_stdout, "", "a blocked call carries no answer");
}
