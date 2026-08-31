//! Tool annotators over real boundaries: a loopback HTTP annotator, a fake `claude`
//! executable behind the command override, a real store, the real hook path.

mod common;
use common::{raw, serve};

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use appa_runtime::api::{AuditEvent, Runtime};
use appa_runtime::{config::Config, hooks};
use appa_runtime_api::{Actor, HookDecision, HookEvent, OutcomeBody, ProposedCall, ToolOutcome, TrajectoryId};
use axum::Router;
use axum::extract::State;
use axum::routing::post;

#[derive(Clone)]
enum Answer {
    Wire(serde_json::Value),
    Down,
    Malformed,
}

#[derive(Clone)]
struct Annotator {
    answers: Arc<Mutex<std::collections::BTreeMap<String, Answer>>>,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl Annotator {
    fn set(&self, annotator: &str, answer: Answer) {
        self.answers.lock().unwrap().insert(annotator.to_string(), answer);
    }

    fn requests(&self) -> Vec<serde_json::Value> {
        self.requests.lock().unwrap().clone()
    }
}

async fn serve_annotator() -> (String, Annotator) {
    let annotator = Annotator {
        answers: Arc::new(Mutex::new(Default::default())),
        requests: Arc::new(Mutex::new(Vec::new())),
    };
    let router = Router::new()
        .route(
            "/annotate",
            post(|State(annotator): State<Annotator>, body: String| async move {
                let request: serde_json::Value = serde_json::from_str(&body).expect("the request is JSON");
                let name = request["name"].as_str().unwrap_or_default().to_string();
                annotator.requests.lock().unwrap().push(request);
                let answer = annotator.answers.lock().unwrap().get(&name).cloned();
                match answer {
                    Some(Answer::Wire(value)) => (axum::http::StatusCode::OK, value.to_string()),
                    Some(Answer::Malformed) => (axum::http::StatusCode::OK, "not json".to_string()),
                    Some(Answer::Down) | None => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom".to_string()),
                }
            }),
        )
        .with_state(annotator.clone());
    (format!("{}/annotate", serve(router).await), annotator)
}

/// A produced annotation whose only semantics is the given output-trust delta.
fn produced(delta_trust: &str) -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "answer": {
            "delta": { "trust": delta_trust },
            "requires": { "history": [], "attention": [] },
            "emits": [],
        }
    })
}

fn root() -> TrajectoryId {
    TrajectoryId("annotators-test".to_string())
}

fn actor() -> Actor {
    Actor {
        root: root(),
        child: None,
    }
}

fn fetch(url: &str) -> ProposedCall {
    ProposedCall {
        tool: "fetch".to_string(),
        arguments: raw(serde_json::json!({ "url": url })),
    }
}

async fn propose(runtime: &Arc<Runtime>, call: ProposedCall) -> HookDecision {
    hooks::handle(
        runtime,
        HookEvent::ToolCall {
            actor: actor(),
            call,
            spawn: false,
        },
    )
    .await
}

async fn ran(runtime: &Arc<Runtime>, call: ProposedCall) {
    assert_eq!(
        hooks::handle(
            runtime,
            HookEvent::ToolResult {
                actor: actor(),
                call,
                outcome: ToolOutcome::Success {
                    body: OutcomeBody::Available("done".to_string()),
                },
            },
        )
        .await,
        HookDecision::Ack
    );
}

async fn open_runtime(dir: &tempfile::TempDir, config_toml: &str) -> Arc<Runtime> {
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, config_toml).expect("the fixture writes");
    let config = Config::load(&path).expect("the fixture validates");
    let runtime = Arc::new(Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens"));
    assert_eq!(
        hooks::handle(&runtime, HookEvent::SessionStart { root: root() }).await,
        HookDecision::Ack
    );
    runtime
}

fn audit_len(runtime: &Runtime) -> usize {
    runtime.audit(&root()).expect("the audit reads").len()
}

fn http_policy(url: &str) -> String {
    format!(
        r#"
[policy]
version = 1

[[policy.annotator]]
name = "classifier"
audiences = ["internal"]

[[policy.tool]]
name = "fetch"
description = "Fetches one URL and returns its body."
parameters = {{ type = "object", properties = {{ url = {{ type = "string" }} }}, required = ["url"] }}
annotator = "classifier"

[externals]
timeout_ms = 2000
max_body_bytes = 65536

[externals.annotators.classifier]
url = "{url}"
"#
    )
}

#[cfg(unix)]
fn command_policy(script: &str, matched_without_annotator: bool) -> String {
    let direct = if matched_without_annotator {
        r#"
[[policy.tool]]
name = "fetch(url:https://public*)"
delta = {}
"#
    } else {
        ""
    };
    format!(
        r#"
[policy]
version = 1

[[policy.annotator]]
name = "classifier"

{direct}
[[policy.tool]]
name = "fetch"
description = "Fetches one URL and returns its body."
parameters = {{ type = "object", properties = {{ url = {{ type = "string" }} }}, required = ["url"] }}
annotator = "classifier"

[externals]
timeout_ms = 5000
max_body_bytes = 65536

[externals.annotators.classifier]
command = ["/bin/sh", "{script}"]
"#
    )
}

#[tokio::test]
async fn an_http_annotator_annotates_the_complete_call_and_a_fresh_proposal_consults_again() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, annotator) = serve_annotator().await;
    annotator.set("classifier", Answer::Wire(produced("trusted")));
    let runtime = open_runtime(&dir, &http_policy(&url)).await;

    assert_eq!(
        propose(&runtime, fetch("https://a.example")).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, fetch("https://a.example")).await;

    let requests = annotator.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request["version"], 1);
    assert_eq!(request["kind"], "annotation");
    assert_eq!(request["name"], "classifier");
    // The annotator declares no inputs, so `args` is the complete call.
    assert_eq!(
        request["artifact"],
        serde_json::json!({
            "args": {
                "name": "fetch",
                "description": "Fetches one URL and returns its body.",
                "arguments": { "url": "https://a.example" },
            }
        })
    );
    // The declaration restates the resolved mandate: the closed vocabulary a produced
    // annotation may use.
    assert_eq!(
        request["declaration"],
        serde_json::json!({
            "inputs": [],
            "trust_ranks": ["suspicious", "trusted"],
            "audiences": ["internal"],
            "attention_marks": [],
            "effects": [],
        })
    );
    let keys: Vec<&str> = request
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        ["artifact", "declaration", "kind", "name", "version"],
        "nothing about the trajectory rides along"
    );

    // Different canonical arguments are a different annotation subject.
    assert_eq!(
        propose(&runtime, fetch("https://b.example")).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, fetch("https://b.example")).await;
    assert_eq!(annotator.requests().len(), 2);

    // And a fresh proposal of the first call is a new act: nothing durable answers for
    // it, so the annotator is consulted again.
    assert_eq!(
        propose(&runtime, fetch("https://a.example")).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert_eq!(annotator.requests().len(), 3);
}

#[tokio::test]
async fn only_the_selected_declaration_consults_its_annotator() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, annotator) = serve_annotator().await;
    annotator.set("classifier", Answer::Wire(produced("trusted")));
    let config = format!(
        r#"
[policy]
version = 1

[[policy.annotator]]
name = "classifier"

[[policy.tool]]
name = "fetch(url:https://private*)"
annotator = "classifier"

[[policy.tool]]
name = "fetch"
delta = {{}}

[externals]
timeout_ms = 2000
max_body_bytes = 65536

[externals.annotators.classifier]
url = "{url}"
"#
    );
    let runtime = open_runtime(&dir, &config).await;

    let public = fetch("https://public.example");
    assert_eq!(
        propose(&runtime, public.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert!(
        annotator.requests().is_empty(),
        "the fallback contract is static and owes no annotation"
    );
    ran(&runtime, public).await;

    assert_eq!(
        propose(&runtime, fetch("https://private.example")).await,
        HookDecision::AllowCall { spawn: None }
    );
    let requests = annotator.requests();
    assert_eq!(requests.len(), 1);
    // The matched declaration writes no description, so the complete call carries none.
    assert_eq!(
        requests[0]["artifact"]["args"],
        serde_json::json!({ "name": "fetch", "arguments": { "url": "https://private.example" } })
    );
}

#[cfg(unix)]
#[tokio::test]
async fn command_annotators_run_only_for_the_selected_declaration_and_pick_up_script_edits() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let script = dir.path().join("annotator.sh");
    let calls = dir.path().join("calls.txt");
    std::fs::write(
        &script,
        "cat >> calls.txt\nprintf '%s' '{\"version\":1,\"answer\":{\"delta\":{\"trust\":\"trusted\"},\"requires\":{\"history\":[],\"attention\":[]},\"emits\":[]}}'",
    )
    .expect("the annotator script writes");
    let runtime = open_runtime(&dir, &command_policy("annotator.sh", true)).await;

    let public = fetch("https://public.example");
    assert_eq!(
        propose(&runtime, public.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert!(!calls.exists(), "the earlier direct contract starts no annotator");
    ran(&runtime, public).await;

    let private = fetch("https://private.example");
    assert_eq!(
        propose(&runtime, private.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert!(calls.exists(), "the fallback declaration runs its command annotator");
    ran(&runtime, private).await;

    std::fs::write(&script, "cat > /dev/null\nprintf 'not-json'").expect("the annotator edit writes");
    let baseline = audit_len(&runtime);
    assert!(matches!(
        propose(&runtime, fetch("https://another.example")).await,
        HookDecision::Refuse { .. }
    ));
    assert_eq!(
        audit_len(&runtime),
        baseline,
        "a script edit applies on the next call and malformed output appends nothing"
    );
}

#[cfg(unix)]
async fn wait_for_file(path: &std::path::Path) {
    for _ in 0..200 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {}", path.display());
}

#[cfg(unix)]
#[tokio::test]
async fn a_command_consult_keeps_its_deployment_during_reload() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let config_path = dir.path().join("appa.toml");
    let started = dir.path().join("started");
    let gate = dir.path().join("gate");
    std::fs::write(
        dir.path().join("old.sh"),
        "cat > /dev/null\ntouch started\nwhile [ ! -f gate ]; do sleep 0.01; done\nprintf '%s' '{\"version\":1,\"answer\":{\"delta\":{\"trust\":\"trusted\"},\"requires\":{\"history\":[],\"attention\":[]},\"emits\":[]}}'",
    )
    .expect("the old annotator writes");
    std::fs::write(dir.path().join("new.sh"), "cat > /dev/null\nexit 7").expect("the new annotator writes");
    let runtime = open_runtime(&dir, &command_policy("old.sh", false)).await;

    let in_flight = {
        let runtime = Arc::clone(&runtime);
        tokio::spawn(async move { propose(&runtime, fetch("https://a.example")).await })
    };
    wait_for_file(&started).await;

    std::fs::write(&config_path, command_policy("new.sh", false)).expect("the replacement config writes");
    let reloaded = runtime
        .reload(Config::load(&config_path).expect("the replacement config loads"))
        .expect("the replacement deployment opens");
    assert!(reloaded.changed);
    std::fs::write(&gate, "go").expect("the old annotator is released");

    assert_eq!(
        in_flight.await.expect("the proposal task completes"),
        HookDecision::AllowCall { spawn: None },
        "the in-flight call finishes with the deployment that started it"
    );
}

#[tokio::test]
async fn a_mapped_input_shows_the_annotator_one_argument() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, annotator) = serve_annotator().await;
    annotator.set("classifier", Answer::Wire(produced("trusted")));
    let config = http_policy(&url).replace(
        r#"name = "classifier"
audiences = ["internal"]"#,
        r#"name = "classifier"
inputs = { subject = "$tool_call.arguments.url" }
audiences = ["internal"]"#,
    );
    let runtime = open_runtime(&dir, &config).await;

    assert_eq!(
        propose(&runtime, fetch("https://a.example")).await,
        HookDecision::AllowCall { spawn: None }
    );
    let requests = annotator.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0]["artifact"]["args"],
        serde_json::json!({ "subject": "https://a.example" })
    );
}

#[tokio::test]
async fn every_annotation_failure_refuses_the_hook_and_appends_nothing() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, annotator) = serve_annotator().await;
    let runtime = open_runtime(&dir, &http_policy(&url)).await;
    let baseline = audit_len(&runtime);

    for failure in [
        Answer::Down,
        Answer::Malformed,
        // A missing answer envelope stands in for every parse-invalid body: the strict
        // decoder's own unit test enumerates the malformed shapes, and each reaches this
        // same operational refusal.
        Answer::Wire(serde_json::json!({ "version": 1 })),
    ] {
        annotator.set("classifier", failure);
        let decision = propose(&runtime, fetch("https://a.example")).await;
        let HookDecision::Refuse { detail } = decision else {
            panic!("an annotation failure is an operational refusal, got {decision:?}");
        };
        assert!(
            detail.contains("classifier"),
            "the refusal names the annotator: {detail}"
        );
        assert_eq!(
            audit_len(&runtime),
            baseline,
            "a no-answer appends nothing to the trajectory"
        );
    }

    // The deployment recovering makes the same proposal succeed: nothing durable recorded
    // the failures, and the fresh invocation consults again.
    annotator.set("classifier", Answer::Wire(produced("trusted")));
    assert_eq!(
        propose(&runtime, fetch("https://a.example")).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert!(audit_len(&runtime) > baseline);
}

#[cfg(unix)]
fn fake_claude(dir: &std::path::Path, script: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("fake-claude");
    std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).expect("the fake claude writes");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("the fake claude is executable");
    path
}

#[cfg(unix)]
fn builtin_policy(command: &std::path::Path, extra: &str) -> String {
    format!(
        r#"
[policy]
version = 1

[[policy.annotator]]
name = "classifier"
builtin = "claude-code"

[[policy.tool]]
name = "fetch"
description = "Fetches one URL and returns its body."
parameters = {{ type = "object", properties = {{ url = {{ type = "string" }} }}, required = ["url"] }}
annotator = "classifier"

[externals]
timeout_ms = 5000
max_body_bytes = 65536

[externals.claude_code]
command = "{command}"
{extra}
"#,
        command = command.display(),
    )
}

#[cfg(unix)]
const NEUTRAL_STRUCTURED: &str = r#"printf '%s' '{"structured_output":{"delta":{"trust":"trusted"},"requires":{"history":[],"attention":[]},"emits":[]}}'"#;

#[cfg(unix)]
#[tokio::test]
async fn the_claude_builtin_runs_the_configured_command_and_model() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let args_path = dir.path().join("args.txt");
    let command = fake_claude(
        dir.path(),
        &format!(
            "printf '%s\\n' \"$@\" > {args}\ncat > /dev/null\n{NEUTRAL_STRUCTURED}",
            args = args_path.display(),
        ),
    );
    let runtime = open_runtime(&dir, &builtin_policy(&command, "model = \"pinned-model\"")).await;

    assert_eq!(
        propose(&runtime, fetch("https://a.example")).await,
        HookDecision::AllowCall { spawn: None }
    );
    let args = std::fs::read_to_string(&args_path).expect("the fake captured its arguments");
    let args: Vec<&str> = args.lines().collect();
    let model = args
        .iter()
        .position(|arg| *arg == "--model")
        .map(|index| args[index + 1]);
    assert_eq!(model, Some("pinned-model"), "the deployment's model rides the argv");
    for expected in [
        "-p",
        "--safe-mode",
        "--disable-slash-commands",
        "--tools",
        "--permission-mode",
        "--no-session-persistence",
        "--output-format",
        "--json-schema",
        "--system-prompt",
    ] {
        assert!(args.contains(&expected), "missing claude argument {expected}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn the_builtin_timeout_is_its_own_budget_and_a_slow_consult_refuses_fast() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let command = fake_claude(dir.path(), "cat > /dev/null\nsleep 5");
    let runtime = open_runtime(&dir, &builtin_policy(&command, "timeout_ms = 150")).await;

    let started = Instant::now();
    let decision = propose(&runtime, fetch("https://a.example")).await;
    assert!(matches!(decision, HookDecision::Refuse { .. }), "got {decision:?}");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the consult budget is the builtin's own timeout, not the shared 5s"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn concurrent_claude_consults_are_gated_by_the_runtime_permit_pool() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    // A call owes one annotation, so concurrency comes from trajectories: five roots each
    // propose one annotated call at once. Five 300ms consults through a four-permit gate
    // are still two waves.
    let command = fake_claude(dir.path(), &format!("cat > /dev/null\nsleep 0.3\n{NEUTRAL_STRUCTURED}"));
    let runtime = open_runtime(&dir, &builtin_policy(&command, "")).await;

    let started = Instant::now();
    let mut proposals = Vec::new();
    for index in 0..5 {
        let root = TrajectoryId(format!("annotators-permit-{index}"));
        assert_eq!(
            hooks::handle(&runtime, HookEvent::SessionStart { root: root.clone() }).await,
            HookDecision::Ack
        );
        let runtime = Arc::clone(&runtime);
        proposals.push(tokio::spawn(async move {
            hooks::handle(
                &runtime,
                HookEvent::ToolCall {
                    actor: Actor { root, child: None },
                    call: fetch("https://a.example"),
                    spawn: false,
                },
            )
            .await
        }));
    }
    for proposal in proposals {
        assert_eq!(
            proposal.await.expect("the proposal task completes"),
            HookDecision::AllowCall { spawn: None }
        );
    }
    assert!(
        started.elapsed() >= Duration::from_millis(450),
        "five subprocess consults must not all run at once: {:?}",
        started.elapsed()
    );
}

/// One Ollama-shaped model endpoint on loopback: every `/api/chat` call is counted and
/// answered with `content`, the JSON text a structured-output consult expects.
async fn serve_ollama(content: &'static str) -> (String, Arc<Mutex<Vec<serde_json::Value>>>) {
    let requests: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&requests);
    let router = Router::new().route(
        "/api/chat",
        post(move |body: String| {
            let seen = Arc::clone(&seen);
            async move {
                let request: serde_json::Value = serde_json::from_str(&body).expect("the request is JSON");
                seen.lock().unwrap().push(request);
                serde_json::json!({
                    "model": "m",
                    "created_at": "2026-01-01T00:00:00Z",
                    "message": { "role": "assistant", "content": content },
                    "done": true,
                    "done_reason": "stop",
                })
                .to_string()
            }
        }),
    );
    (serve(router).await, requests)
}

/// An annotator that names `builtin = "llm"` on its declaration is served by the
/// deployment's `[externals.llm]` profile and by nothing under `[externals.annotators]`:
/// the endpoint bound for another annotator sees no request.
#[tokio::test]
async fn a_declared_llm_annotator_consults_the_llm_profile_and_no_binding() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (endpoint_url, annotator) = serve_annotator().await;
    let (llm_url, llm_requests) =
        serve_ollama(r#"{"delta":{"trust":"trusted"},"requires":{"history":[],"attention":[]},"emits":[]}"#).await;
    let config = format!(
        r#"
[policy]
version = 1

[[policy.annotator]]
name = "classifier"
builtin = "llm"

[[policy.annotator]]
name = "other"

[[policy.tool]]
name = "fetch"
description = "Fetches one URL and returns its body."
parameters = {{ type = "object", properties = {{ url = {{ type = "string" }} }}, required = ["url"] }}
annotator = "classifier"

[externals]
timeout_ms = 5000
max_body_bytes = 65536

[externals.annotators.other]
url = "{endpoint_url}"

[externals.llm]
provider = "ollama"
model = "m"
url = "{llm_url}"
"#
    );
    let runtime = open_runtime(&dir, &config).await;
    assert_eq!(
        propose(&runtime, fetch("https://a.example")).await,
        HookDecision::AllowCall { spawn: None }
    );
    let consults = llm_requests.lock().unwrap().clone();
    assert_eq!(consults.len(), 1, "the profile answered the one consult");
    assert_eq!(consults[0]["model"], "m", "the profile's model is the one consulted");
    assert!(
        annotator.requests().is_empty(),
        "the declared annotator never reached an annotator binding"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_builtin_annotator_never_touches_an_http_endpoint() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, annotator) = serve_annotator().await;
    let command = fake_claude(dir.path(), &format!("cat > /dev/null\n{NEUTRAL_STRUCTURED}"));
    let config = builtin_policy(&command, "")
        .replace(
            "[[policy.tool]]",
            "[[policy.annotator]]\nname = \"other\"\n\n[[policy.tool]]",
        )
        .replace(
            "[externals.claude_code]",
            &format!("[externals.annotators.other]\nurl = \"{url}\"\n\n[externals.claude_code]"),
        );
    let runtime = open_runtime(&dir, &config).await;
    assert_eq!(
        propose(&runtime, fetch("https://a.example")).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert!(
        annotator.requests().is_empty(),
        "the builtin annotator answered; the endpoint saw no request"
    );
}

/// The neutral annotation: no label change, no requirements, no effects.
fn neutral() -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "answer": { "delta": {}, "requires": { "history": [], "attention": [] }, "emits": [] }
    })
}

fn released_effects(runtime: &Runtime) -> Vec<Vec<String>> {
    runtime
        .audit(&root())
        .expect("the audit reads")
        .iter()
        .filter_map(|entry| match &entry.event {
            AuditEvent::Released { tool, effects, .. } if tool == "fetch" => Some(effects.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_neutral_annotation_admits_the_call_and_records_no_effects() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, annotator) = serve_annotator().await;
    annotator.set("classifier", Answer::Wire(neutral()));
    let runtime = open_runtime(&dir, &http_policy(&url)).await;

    assert_eq!(
        propose(&runtime, fetch("https://a.example")).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, fetch("https://a.example")).await;
    assert_eq!(
        released_effects(&runtime),
        vec![Vec::<String>::new()],
        "an annotation that answers the identity admits the call and commits nothing"
    );
}

#[tokio::test]
async fn a_produced_history_requirement_gates_on_the_effects_a_prior_release_committed() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, annotator) = serve_annotator().await;
    // Every produced annotation emits `egress` and requires that no prior `egress` was
    // committed: the first release succeeds and records the effect, and that record is
    // exactly what refuses the second call.
    annotator.set(
        "classifier",
        Answer::Wire(serde_json::json!({
            "version": 1,
            "answer": {
                "delta": {},
                "requires": { "history": [{ "excludes": "egress" }], "attention": [] },
                "emits": ["egress"],
            }
        })),
    );
    let config = http_policy(&url).replace(
        r#"name = "classifier"
audiences = ["internal"]"#,
        r#"name = "classifier"
audiences = ["internal"]
effects = ["egress"]"#,
    );
    let runtime = open_runtime(&dir, &config).await;

    assert_eq!(
        propose(&runtime, fetch("https://a.example")).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, fetch("https://a.example")).await;
    assert_eq!(released_effects(&runtime), vec![vec!["egress".to_string()]]);

    let decision = propose(&runtime, fetch("https://b.example")).await;
    assert!(
        matches!(decision, HookDecision::DenyCall { .. }),
        "the committed egress violates the produced no-prior requirement: {decision:?}"
    );
    assert_eq!(
        released_effects(&runtime),
        vec![vec!["egress".to_string()]],
        "the refused call released nothing"
    );
}

fn wildcard_policy(url: &str) -> String {
    format!(
        r#"
[policy]
version = 1

[[policy.annotator]]
name = "gatekeeper"

[[policy.tool]]
name = "read"
delta = {{}}

[[policy.tool]]
name = "*"
annotator = "gatekeeper"

[[policy.authority]]
name = "operator"
[policy.authority.permits]
attention = ["signoff"]

[externals]
timeout_ms = 2000
review_timeout_ms = 1000
max_body_bytes = 65536

[externals.annotators.gatekeeper]
url = "{url}"

[externals.authorities.operator]
builtin = "hitl"
"#
    )
}

fn call(tool: &str, arguments: serde_json::Value) -> ProposedCall {
    ProposedCall {
        tool: tool.to_string(),
        arguments: raw(arguments),
    }
}

/// Scenario 5: the policy writes `read` exactly and covers everything else with the
/// wildcard. An unwritten money-moving tool is annotated per call — the gatekeeper sees
/// the actual call — and its strict trust-and-attention contract blocks the call. The
/// exact declaration decides `read` without a consult.
#[tokio::test]
async fn the_wildcard_annotates_an_unwritten_tool_and_an_exact_declaration_never_consults() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, annotator) = serve_annotator().await;
    annotator.set(
        "gatekeeper",
        Answer::Wire(serde_json::json!({
            "version": 1,
            "answer": {
                "delta": {},
                "requires": { "trust": "trusted", "attention": ["signoff"], "history": [] },
                "emits": [],
            }
        })),
    );
    let runtime = open_runtime(&dir, &wildcard_policy(&url)).await;

    let decision = propose(
        &runtime,
        call("send_money_via_wire", serde_json::json!({"amount": 9000})),
    )
    .await;
    assert!(
        matches!(decision, HookDecision::DenyCall { .. }),
        "the produced attention requirement blocks the unwritten tool: {decision:?}"
    );
    let requests = annotator.requests();
    assert_eq!(requests.len(), 1, "the wildcard consulted once");
    // The annotation subject is the actual call, not the wildcard's spelling.
    assert_eq!(requests[0]["artifact"]["args"]["name"], "send_money_via_wire");
    assert_eq!(
        requests[0]["artifact"]["args"]["arguments"],
        serde_json::json!({"amount": 9000})
    );

    assert_eq!(
        propose(&runtime, call("read", serde_json::json!({"path": "a.txt"}))).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, call("read", serde_json::json!({"path": "a.txt"}))).await;
    assert_eq!(
        annotator.requests().len(),
        1,
        "the exact declaration decides without a consult"
    );
}

/// A produced restricting delta blocks a wildcard-covered call before release, and
/// proposing it again holds the block.
#[tokio::test]
async fn a_wildcard_calls_produced_narrowing_blocks_and_holds() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, annotator) = serve_annotator().await;
    annotator.set("gatekeeper", Answer::Wire(produced("suspicious")));
    let runtime = open_runtime(&dir, &wildcard_policy(&url)).await;

    for _ in 0..2 {
        let decision = propose(&runtime, call("ghost_tool", serde_json::json!({}))).await;
        assert!(
            matches!(decision, HookDecision::DenyCall { .. }),
            "the produced narrowing blocks the call: {decision:?}"
        );
    }
}

/// Without a wildcard, a tool the policy does not write has no contract at all: the hook
/// refuses it as a typed operational error naming the tool, and appends nothing.
#[tokio::test]
async fn a_tool_nothing_covers_refuses_the_hook_and_appends_nothing() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, _annotator) = serve_annotator().await;
    let runtime = open_runtime(&dir, &http_policy(&url)).await;
    let before = audit_len(&runtime);

    let decision = propose(&runtime, call("wrench", serde_json::json!({}))).await;
    match decision {
        HookDecision::Refuse { detail } => {
            assert!(detail.contains("wrench"), "the refusal names the tool: {detail}");
        }
        other => panic!("expected a typed refusal, got {other:?}"),
    }
    assert_eq!(audit_len(&runtime), before, "the refusal appends nothing");
}
