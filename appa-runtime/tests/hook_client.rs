//! `appa hook` as the harness runs it: the client contract of the hook entries
//! a deployment registers. Every test spawns the built binary with a hook event
//! on stdin and reads the exit code and stdout the harness would.

mod common;
use common::{serve, serve_runtime};

use std::io::Write;
use std::process::{Command, Stdio};

use axum::Router;
use axum::routing::post;

/// The gated hook every test posts: a call the runtime must decide.
const PRE_TOOL_USE: &str =
    r#"{"hook_event_name":"PreToolUse","session_id":"client-test","tool_name":"Bash","tool_input":{"command":"ls"}}"#;

const POST_TOOL_USE: &str = r#"{"hook_event_name":"PostToolUse","session_id":"client-test","tool_name":"Bash","tool_input":{"command":"ls"},"tool_response":{"stdout":"readme.txt"}}"#;

fn built_binary() -> &'static std::path::Path {
    std::path::Path::new(env!("CARGO_BIN_EXE_appa"))
}

/// The client in a protected session, pointed at `url` as the deployment's endpoint.
fn client(url: &str) -> Command {
    let mut command = Command::new(built_binary());
    command
        .arg("hook")
        .arg("--deployment-url")
        .arg(url)
        .env("APPA_GATE", "1")
        .env_remove("APPA_RUNTIME_URL")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command
}

/// Feed `stdin` to a spawned client and collect its exit code and stdout.
fn finish(mut child: std::process::Child, stdin: &str) -> (i32, String) {
    // An ungated client may exit before reading its stdin, closing the pipe
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
    let output = child.wait_with_output().expect("the hook client finishes");
    (
        output.status.code().expect("the hook client exits with a code"),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

fn run_client(url: &str, stdin: &str) -> (i32, String) {
    finish(client(url).spawn().expect("the hook client spawns"), stdin)
}

fn run_turn_end(url: &str, stdin: &str) -> (i32, String) {
    finish(
        client(url).arg("--turn-end").spawn().expect("the hook client spawns"),
        stdin,
    )
}

/// The client with the read end of its stdout closed before it answers: whatever it renders
/// cannot reach the harness, and only its exit code is left to report that.
fn run_unheard_client(url: &str, stdin: &str) -> i32 {
    let mut child = client(url).spawn().expect("the hook client spawns");
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
    let (code, stdout) = tokio::task::spawn_blocking(move || run_client(&url, PRE_TOOL_USE))
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
    let (code, stdout) = tokio::task::spawn_blocking(move || run_client(&url, PRE_TOOL_USE))
        .await
        .expect("the blocking task joins");
    assert_eq!(code, 2, "an answer off the wire must block the action");
    assert_eq!(stdout, "");
}

/// `appa hook` end to end against a served runtime: the Claude Code hook is
/// translated onto the wire, the runtime decides under a policy naming the
/// canonical tool, and the decision comes back in Claude Code's shape with the
/// exit code its outcome takes.
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

    let (code, stdout) = run_client(&runtime.url, POST_TOOL_USE);
    assert_eq!(code, 0, "{stdout}");
    assert_eq!(stdout, "{}", "a kept result answers with no opinion");

    // Nothing covers Write: the runtime refuses the call typed, and the client blocks.
    let uncovered = r#"{"hook_event_name":"PreToolUse","session_id":"client-test","tool_name":"Write","tool_input":{"file_path":"x","content":"y"}}"#;
    let (code, stdout) = run_client(&runtime.url, uncovered);
    assert_eq!(code, 2, "a runtime refusal must block the action: {stdout}");
    let answer: serde_json::Value = serde_json::from_str(&stdout).expect("the refusal renders");
    assert!(answer["error"].is_string(), "{answer}");

    let ungated = r#"{"hook_event_name":"Notification","session_id":"client-test"}"#;
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
    let (code, _) = tokio::task::spawn_blocking(move || run_client(&url, PRE_TOOL_USE))
        .await
        .expect("the blocking task joins");
    assert_eq!(code, 2, "a non-2xx answer must block the action");
}

/// Only a session launched with `APPA_GATE=1` is protected. Any other value, or
/// none, makes the client say nothing and exit 0 without a round trip: the
/// endpoint here refuses every connection, so a post would have exited 2.
#[tokio::test(flavor = "multi_thread")]
async fn an_ungated_session_posts_nothing_and_never_blocks() {
    let url = refused_url().await;
    let outcomes = tokio::task::spawn_blocking(move || {
        [Some("0"), Some(""), Some("true"), None]
            .into_iter()
            .map(|gate| {
                let mut command = client(&url);
                match gate {
                    Some(gate) => command.env("APPA_GATE", gate),
                    None => command.env_remove("APPA_GATE"),
                };
                (
                    gate,
                    finish(command.spawn().expect("the hook client spawns"), PRE_TOOL_USE),
                )
            })
            .collect::<Vec<_>>()
    })
    .await
    .expect("the blocking task joins");
    for (gate, (code, stdout)) in outcomes {
        assert_eq!(code, 0, "APPA_GATE={gate:?}: an ungated session must not be blocked");
        assert_eq!(stdout, "", "APPA_GATE={gate:?}: an ungated session posts nothing");
    }
}

/// Blocking a turn end holds the actor in a turn it has finished, so a
/// runtime that answers nothing costs a call left open, never a turn
/// that cannot end.
#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_runtime_never_blocks_a_turn_end() {
    let url = refused_url().await;
    let stop = r#"{"hook_event_name":"Stop","session_id":"client-test","stop_hook_active":false}"#;
    let (code, stdout) = tokio::task::spawn_blocking(move || run_turn_end(&url, stop))
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
    let stop = r#"{"hook_event_name":"Stop","session_id":"client-test","stop_hook_active":false}"#;
    let (code, stdout) = tokio::task::spawn_blocking(move || run_turn_end(&url, stop))
        .await
        .expect("the blocking task joins");
    assert_eq!(code, 0, "a turn end must never block the harness");
    assert_eq!(stdout, "", "a turn end prints no decision");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_runtime_exits_2() {
    let url = refused_url().await;
    let (code, _) = tokio::task::spawn_blocking(move || run_client(&url, PRE_TOOL_USE))
        .await
        .expect("the blocking task joins");
    assert_eq!(code, 2, "no answer from the runtime must block the action");
}

/// A session that names its own runtime posts there, not to the deployment's
/// endpoint the hook entry carries: the entry's URL refuses every connection,
/// and the session's answers.
#[tokio::test(flavor = "multi_thread")]
async fn the_sessions_runtime_url_beats_the_deployments_endpoint() {
    let session = serve(Router::new().route(
        "/hook",
        post(|| async { r#"{"protocol":1,"decision":"block","reason":"from the session's runtime"}"# }),
    ))
    .await;
    let deployment = refused_url().await;
    let (code, stdout) = tokio::task::spawn_blocking(move || {
        finish(
            client(&deployment)
                .env("APPA_RUNTIME_URL", &session)
                .spawn()
                .expect("the hook client spawns"),
            PRE_TOOL_USE,
        )
    })
    .await
    .expect("the blocking task joins");
    assert_eq!(code, 0, "{stdout}");
    let answer: serde_json::Value = serde_json::from_str(&stdout).expect("the answer is JSON");
    assert_eq!(answer["reason"], "from the session's runtime");
}

/// A tool whose result already ran needs more than an exit code: the harness
/// keeps output it was not told to replace, so an unanswered post-use hook
/// renders the withholding for the result it reports.
#[tokio::test(flavor = "multi_thread")]
async fn an_unanswered_post_use_hook_withholds_the_result_it_reports() {
    let url = refused_url().await;
    let (code, stdout) = tokio::task::spawn_blocking(move || run_client(&url, POST_TOOL_USE))
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
    for answer in [
        (
            axum::http::StatusCode::CONFLICT,
            r#"{"protocol":1,"decision":"refuse","detail":"storage failure: disk full"}"#,
        ),
        (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom"),
    ] {
        let url = serve(Router::new().route("/hook", post(move || async move { answer }))).await;
        let (code, stdout) = tokio::task::spawn_blocking(move || run_client(&url, POST_TOOL_USE))
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
    let (code, stdout) = tokio::task::spawn_blocking(move || run_client(&url, PRE_TOOL_USE))
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
    let ran = r#"{"hook_event_name":"PostToolUse","session_id":"client-test","tool_name":"Bash","tool_response":{"stdout":"readme.txt"}}"#;
    let proposed = r#"{"hook_event_name":"PreToolUse","session_id":"client-test","tool_name":"Bash"}"#;
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

/// A withholding carries its whole effect through what the client prints, so one the
/// harness never received withheld nothing. Here the read end of the client's stdout is
/// closed before it answers, so the write fails: the client must not exit zero and report a
/// replacement that never arrived.
#[tokio::test(flavor = "multi_thread")]
async fn a_withholding_that_cannot_be_written_does_not_exit_zero() {
    let url = refused_url().await;
    let code = tokio::task::spawn_blocking(move || run_unheard_client(&url, POST_TOOL_USE))
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

/// The SessionStart entry brings the runtime up before it posts: pointed at a
/// port nothing answers, `--ensure-runtime` starts this same binary there over
/// the given config and data directory, then the session's first event is
/// decided by the runtime it started. The runtime outlives the hook.
#[test]
fn the_session_start_entry_starts_the_deployed_runtime_then_posts_to_it() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let config = dir.path().join("config").join("appa.toml");
    let data = dir.path().join("data");
    let url = format!("http://127.0.0.1:{}", common::free_port());
    let session_start = r#"{"hook_event_name":"SessionStart","session_id":"client-test","source":"startup"}"#;
    let (code, stdout) = finish(
        client(&url)
            .arg("--ensure-runtime")
            .arg("--config")
            .arg(&config)
            .arg("--data-dir")
            .arg(&data)
            .spawn()
            .expect("the hook client spawns"),
        session_start,
    );
    let health = common::http(&format!("{url}/health"), "GET", None);
    let fingerprint = common::http(&format!("{url}/binary-fingerprint"), "GET", None);
    // Whatever the assertions below say, the runtime this test started is stopped.
    if let Some(pid) = fingerprint
        .as_deref()
        .and_then(|answer| answer.lines().next())
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|pid| pid.parse::<i32>().ok())
    {
        // SAFETY: the pid was read from the runtime this test started on its own port.
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
    assert_eq!(code, 0, "{stdout}");
    assert_eq!(
        health.as_deref().map(str::trim),
        Some("ok"),
        "the started runtime answers healthy"
    );
    assert!(
        config.is_file(),
        "the runtime wrote the default policy on its first start"
    );
    assert!(
        data.join("appa.db").exists(),
        "the runtime keeps its log under the data directory"
    );
    assert!(
        fingerprint.is_some_and(|answer| answer
            .lines()
            .nth(1)
            .is_some_and(|served| served == config.to_string_lossy())),
        "the started runtime serves the config the entry named"
    );

    // A second ensure finds the healthy runtime and starts nothing.
    let (code, _) = finish(
        client(&url)
            .arg("--ensure-runtime")
            .arg("--config")
            .arg(&config)
            .arg("--data-dir")
            .arg(&data)
            .spawn()
            .expect("the hook client spawns"),
        session_start,
    );
    assert_eq!(code, 0);
}

/// A start that fails blocks the hook like an unanswered one, without posting:
/// the executable is this binary, but the port belongs to something that
/// answers `/health` with neither `ok` nor a stale pid.
#[tokio::test(flavor = "multi_thread")]
async fn a_runtime_that_cannot_be_started_blocks_the_session_start_hook() {
    let url = serve(Router::new().route("/health", axum::routing::get(|| async { "someone else" }))).await;
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let config = dir.path().join("appa.toml");
    let data = dir.path().join("data");
    let session_start = r#"{"hook_event_name":"SessionStart","session_id":"client-test","source":"startup"}"#;
    let (code, stdout) = tokio::task::spawn_blocking(move || {
        finish(
            client(&url)
                .arg("--ensure-runtime")
                .arg("--config")
                .arg(&config)
                .arg("--data-dir")
                .arg(&data)
                .spawn()
                .expect("the hook client spawns"),
            session_start,
        )
    })
    .await
    .expect("the blocking task joins");
    assert_eq!(code, 2, "a runtime that cannot be started blocks: {stdout}");
    assert_eq!(stdout, "", "nothing is posted into a runtime that is not there");
}

/// The advice entry prints the session context in a protected session and
/// nothing outside one; it never blocks.
#[test]
fn the_session_context_entry_speaks_only_in_a_protected_session() {
    let gated = Command::new(built_binary())
        .arg("session-context")
        .env("APPA_GATE", "1")
        .output()
        .expect("the binary runs");
    assert!(gated.status.success());
    assert!(!gated.stdout.is_empty(), "a protected session gets the advice");

    let ungated = Command::new(built_binary())
        .arg("session-context")
        .env_remove("APPA_GATE")
        .output()
        .expect("the binary runs");
    assert!(ungated.status.success());
    assert!(ungated.stdout.is_empty(), "an unprotected session hears nothing");
}
