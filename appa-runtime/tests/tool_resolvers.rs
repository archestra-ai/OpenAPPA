//! Tool-level dynamic resolvers over real boundaries: a loopback HTTP classifier, a fake
//! `claude` executable behind the command override, a real store, the real hook path.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use appa_runtime::api::Runtime;
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
struct Classifier {
    answers: Arc<Mutex<std::collections::BTreeMap<String, Answer>>>,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
    delays: Arc<Mutex<std::collections::BTreeMap<String, Duration>>>,
}

impl Classifier {
    fn set(&self, resolver: &str, answer: Answer) {
        self.answers.lock().unwrap().insert(resolver.to_string(), answer);
    }

    fn delay(&self, resolver: &str, delay: Duration) {
        self.delays.lock().unwrap().insert(resolver.to_string(), delay);
    }

    fn requests(&self) -> Vec<serde_json::Value> {
        self.requests.lock().unwrap().clone()
    }
}

async fn serve_classifier() -> (String, Classifier) {
    let classifier = Classifier {
        answers: Arc::new(Mutex::new(Default::default())),
        requests: Arc::new(Mutex::new(Vec::new())),
        delays: Arc::new(Mutex::new(Default::default())),
    };
    let router = Router::new()
        .route(
            "/resolve",
            post(|State(classifier): State<Classifier>, body: String| async move {
                let request: serde_json::Value = serde_json::from_str(&body).expect("the request is JSON");
                let resolver = request["resolver"].as_str().unwrap_or_default().to_string();
                classifier.requests.lock().unwrap().push(request);
                let delay = classifier.delays.lock().unwrap().get(&resolver).copied();
                if let Some(delay) = delay {
                    tokio::time::sleep(delay).await;
                }
                let answer = classifier.answers.lock().unwrap().get(&resolver).cloned();
                match answer {
                    Some(Answer::Wire(value)) => (axum::http::StatusCode::OK, value.to_string()),
                    Some(Answer::Malformed) => (axum::http::StatusCode::OK, "not json".to_string()),
                    Some(Answer::Down) | None => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom".to_string()),
                }
            }),
        )
        .with_state(classifier.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral loopback port binds");
    let addr = listener.local_addr().expect("the bound address is readable");
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("the stub serves");
    });
    (format!("http://{addr}/resolve"), classifier)
}

fn raw(value: serde_json::Value) -> Box<serde_json::value::RawValue> {
    serde_json::value::to_raw_value(&value).expect("the fixture serializes")
}

fn root() -> TrajectoryId {
    TrajectoryId("tool-resolvers-test".to_string())
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

[[policy.dynamic_resolver]]
name = "classifier"
returns = ["delta.trust"]

[[policy.tool]]
name = "fetch"
description = "Fetches one URL and returns its body."
parameters = {{ type = "object", properties = {{ url = {{ type = "string" }} }}, required = ["url"] }}
uses = [{{ resolver = "classifier" }}]

[externals]
timeout_ms = 2000
max_body_bytes = 65536

[externals.dynamic.classifier]
url = "{url}"
"#
    )
}

#[cfg(unix)]
fn command_policy(script: &str, matched_without_resolver: bool) -> String {
    let direct = if matched_without_resolver {
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

[[policy.dynamic_resolver]]
name = "classifier"
returns = ["delta.trust"]

{direct}
[[policy.tool]]
name = "fetch"
description = "Fetches one URL and returns its body."
parameters = {{ type = "object", properties = {{ url = {{ type = "string" }} }}, required = ["url"] }}
uses = [{{ resolver = "classifier" }}]

[externals]
timeout_ms = 5000
max_body_bytes = 65536

[externals.dynamic.classifier]
command = ["/bin/sh", "{script}"]
"#
    )
}

#[tokio::test]
async fn an_http_resolver_classifies_the_complete_call_and_a_fresh_proposal_consults_again() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, classifier) = serve_classifier().await;
    classifier.set(
        "classifier",
        Answer::Wire(serde_json::json!({ "version": 1, "result": { "delta.trust": "trusted" } })),
    );
    let runtime = open_runtime(&dir, &http_policy(&url)).await;

    assert_eq!(
        propose(&runtime, fetch("https://a.example")).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, fetch("https://a.example")).await;

    let requests = classifier.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request["version"], 1);
    assert_eq!(request["resolver"], "classifier");
    // The resolver declares no inputs, so `args` is the complete call.
    assert_eq!(
        request["args"],
        serde_json::json!({
            "name": "fetch",
            "description": "Fetches one URL and returns its body.",
            "arguments": { "url": "https://a.example" },
        })
    );
    for absent in ["tool", "input", "scope", "returns", "expects"] {
        assert!(request.get(absent).is_none(), "the request carries no {absent:?} key");
    }
    assert_eq!(request["trust_ranks"], serde_json::json!(["suspicious", "trusted"]));
    assert!(request["context"]["current_trust"].is_string());

    // Different canonical arguments are a different classification subject.
    assert_eq!(
        propose(&runtime, fetch("https://b.example")).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, fetch("https://b.example")).await;
    assert_eq!(classifier.requests().len(), 2);

    // And a fresh proposal of the first call is a new act: nothing durable answers for
    // it, so the resolver is consulted again.
    assert_eq!(
        propose(&runtime, fetch("https://a.example")).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert_eq!(classifier.requests().len(), 3);
}

#[tokio::test]
async fn only_the_first_matching_contract_runs_its_resolver() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, classifier) = serve_classifier().await;
    classifier.set(
        "classifier",
        Answer::Wire(serde_json::json!({ "version": 1, "result": { "delta.trust": "trusted" } })),
    );
    let config = format!(
        r#"
[policy]
version = 1

[[policy.dynamic_resolver]]
name = "classifier"
returns = ["delta.trust"]

[[policy.tool]]
name = "fetch(url:https://private*)"
uses = [{{ resolver = "classifier" }}]

[[policy.tool]]
name = "fetch"
delta = {{}}

[externals]
timeout_ms = 2000
max_body_bytes = 65536

[externals.dynamic.classifier]
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
        classifier.requests().is_empty(),
        "the fallback contract has no resolver"
    );
    ran(&runtime, public).await;

    assert_eq!(
        propose(&runtime, fetch("https://private.example")).await,
        HookDecision::AllowCall { spawn: None }
    );
    let requests = classifier.requests();
    assert_eq!(requests.len(), 1);
    // The matched contract declares no description, so the complete call carries none.
    assert_eq!(
        requests[0]["args"],
        serde_json::json!({ "name": "fetch", "arguments": { "url": "https://private.example" } })
    );
}

#[cfg(unix)]
#[tokio::test]
async fn command_resolvers_run_only_for_the_selected_contract_and_pick_up_script_edits() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let script = dir.path().join("resolver.sh");
    let calls = dir.path().join("calls.txt");
    std::fs::write(
        &script,
        "cat >> calls.txt\nprintf '%s' '{\"version\":1,\"result\":{\"delta.trust\":\"trusted\"}}'",
    )
    .expect("the resolver script writes");
    let runtime = open_runtime(&dir, &command_policy("resolver.sh", true)).await;

    let public = fetch("https://public.example");
    assert_eq!(
        propose(&runtime, public.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert!(!calls.exists(), "the earlier direct contract starts no resolver");
    ran(&runtime, public).await;

    let private = fetch("https://private.example");
    assert_eq!(
        propose(&runtime, private.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert!(calls.exists(), "the fallback contract runs its command resolver");
    ran(&runtime, private).await;

    std::fs::write(&script, "cat > /dev/null\nprintf 'not-json'").expect("the resolver edit writes");
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
        "cat > /dev/null\ntouch started\nwhile [ ! -f gate ]; do sleep 0.01; done\nprintf '%s' '{\"version\":1,\"result\":{\"delta.trust\":\"trusted\"}}'",
    )
    .expect("the old resolver writes");
    std::fs::write(dir.path().join("new.sh"), "cat > /dev/null\nexit 7").expect("the new resolver writes");
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
    std::fs::write(&gate, "go").expect("the old resolver is released");

    assert_eq!(
        in_flight.await.expect("the proposal task completes"),
        HookDecision::AllowCall { spawn: None },
        "the in-flight call finishes with the deployment that started it"
    );
}

#[tokio::test]
async fn a_mapped_input_shows_the_resolver_one_argument() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, classifier) = serve_classifier().await;
    classifier.set(
        "classifier",
        Answer::Wire(serde_json::json!({ "version": 1, "result": { "delta.trust": "trusted" } })),
    );
    let config = http_policy(&url)
        .replace(
            r#"name = "classifier"
returns = ["delta.trust"]"#,
            r#"name = "classifier"
inputs = ["subject"]
returns = ["delta.trust"]"#,
        )
        .replace(
            r#"uses = [{ resolver = "classifier" }]"#,
            r#"uses = [{ resolver = "classifier", inputs = { subject = "$tool_call.arguments.url" } }]"#,
        );
    let runtime = open_runtime(&dir, &config).await;

    assert_eq!(
        propose(&runtime, fetch("https://a.example")).await,
        HookDecision::AllowCall { spawn: None }
    );
    let requests = classifier.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0]["args"],
        serde_json::json!({ "subject": "https://a.example" })
    );
}

#[tokio::test]
async fn every_resolver_failure_refuses_the_hook_and_appends_nothing() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, classifier) = serve_classifier().await;
    let runtime = open_runtime(&dir, &http_policy(&url)).await;
    let baseline = audit_len(&runtime);

    for failure in [
        Answer::Down,
        Answer::Malformed,
        // Undeclared fields are exactly as unusable as transport failures.
        Answer::Wire(serde_json::json!({ "version": 1 })),
        Answer::Wire(serde_json::json!({ "version": 7, "delta": { "trust": "trusted" } })),
        Answer::Wire(serde_json::json!({ "version": 1, "delta": { "trust": "invented" } })),
    ] {
        classifier.set("classifier", failure);
        let decision = propose(&runtime, fetch("https://a.example")).await;
        let HookDecision::Refuse { detail } = decision else {
            panic!("a resolver failure is an operational refusal, got {decision:?}");
        };
        assert!(
            detail.contains("classifier"),
            "the refusal names the resolver: {detail}"
        );
        assert_eq!(
            audit_len(&runtime),
            baseline,
            "a no-answer appends nothing to the trajectory"
        );
    }

    // The deployment recovering makes the same proposal succeed: nothing durable recorded
    // the failures, and the fresh invocation consults again.
    classifier.set(
        "classifier",
        Answer::Wire(serde_json::json!({ "version": 1, "result": { "delta.trust": "trusted" } })),
    );
    assert_eq!(
        propose(&runtime, fetch("https://a.example")).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert!(audit_len(&runtime) > baseline);
}

fn two_resolver_policy(url: &str) -> String {
    format!(
        r#"
[policy]
version = 1

[[policy.dynamic_resolver]]
name = "alpha"
returns = ["delta.trust"]
[[policy.dynamic_resolver]]
name = "beta"
inputs = ["subject"]
returns = ["requires.trust"]

[[policy.tool]]
name = "fetch"
description = "Fetches one URL and returns its body."
parameters = {{ type = "object", properties = {{ url = {{ type = "string" }} }}, required = ["url"] }}
uses = [
  {{ resolver = "alpha" }},
  {{ resolver = "beta", inputs = {{ subject = "$tool_call.arguments.url" }} }},
]

[externals]
timeout_ms = 5000
max_body_bytes = 65536

[externals.dynamic.alpha]
url = "{url}"

[externals.dynamic.beta]
url = "{url}"
"#
    )
}

#[tokio::test]
async fn independent_consults_overlap_and_completion_order_never_moves_the_record() {
    let run = |slow: &'static str| async move {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let (url, classifier) = serve_classifier().await;
        classifier.set(
            "alpha",
            Answer::Wire(serde_json::json!({ "version": 1, "result": { "delta.trust": "trusted" } })),
        );
        classifier.set(
            "beta",
            Answer::Wire(serde_json::json!({ "version": 1, "result": { "requires.trust": "suspicious" } })),
        );
        // Both consults sleep: sequential execution would cost ~600ms, concurrent ~300ms.
        classifier.delay("alpha", Duration::from_millis(300));
        classifier.delay("beta", Duration::from_millis(300));
        classifier.delay(slow, Duration::from_millis(320));
        let runtime = open_runtime(&dir, &two_resolver_policy(&url)).await;
        let started = Instant::now();
        assert_eq!(
            propose(&runtime, fetch("https://a.example")).await,
            HookDecision::AllowCall { spawn: None }
        );
        let elapsed = started.elapsed();
        // Concurrent: the batch costs its slowest member, not the sum of both.
        assert!(
            elapsed < Duration::from_millis(550),
            "the two consults did not overlap: {elapsed:?}"
        );
        ran(&runtime, fetch("https://a.example")).await;
        format!("{:?}", runtime.audit(&root()).expect("the audit reads"))
    };
    let alpha_slow = run("alpha").await;
    let beta_slow = run("beta").await;
    assert_eq!(
        alpha_slow, beta_slow,
        "which consult finished first must not move the recorded trajectory"
    );
}

fn fake_claude(dir: &std::path::Path, script: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("fake-claude");
    std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).expect("the fake claude writes");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("the fake claude is executable");
    path
}

fn builtin_policy(command: &std::path::Path, extra: &str) -> String {
    format!(
        r#"
[policy]
version = 1

[[policy.dynamic_resolver]]
name = "classifier"
builtin = "claude-code"
returns = ["delta.trust"]

[[policy.tool]]
name = "fetch"
description = "Fetches one URL and returns its body."
parameters = {{ type = "object", properties = {{ url = {{ type = "string" }} }}, required = ["url"] }}
uses = [{{ resolver = "classifier" }}]

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

#[tokio::test]
async fn the_claude_builtin_runs_the_configured_command_and_model() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let args_path = dir.path().join("args.txt");
    let command = fake_claude(
        dir.path(),
        &format!(
            "printf '%s\\n' \"$@\" > {args}\ncat > /dev/null\nprintf '%s' '{{\"structured_output\":{{\"version\":1,\"result\":{{\"delta.trust\":\"trusted\"}}}}}}'",
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

#[tokio::test]
async fn concurrent_claude_consults_are_gated_by_the_runtime_permit_pool() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    // One destination has one owner, so a tool takes at most five resolvers, one per result.
    // Five 300ms consults through a four-permit gate are still two waves. Each resolver
    // declares a different result, so the fake answers by the name it reads on stdin.
    let command = fake_claude(
        dir.path(),
        r#"request=$(cat)
sleep 0.3
case "$request" in
  *classifier-0*) printf '%s' '{"structured_output":{"version":1,"result":{"delta.trust":"trusted"}}}' ;;
  *classifier-1*) printf '%s' '{"structured_output":{"version":1,"result":{"delta.audience":"public"}}}' ;;
  *classifier-2*) printf '%s' '{"structured_output":{"version":1,"result":{"requires.trust":"suspicious"}}}' ;;
  *classifier-3*) printf '%s' '{"structured_output":{"version":1,"result":{"requires.audience":{"within":"public"}}}}' ;;
  *) printf '%s' '{"structured_output":{"version":1,"result":{"requires.attention":[]}}}' ;;
esac
"#,
    );
    let results = [
        "delta.trust",
        "delta.audience",
        "requires.trust",
        "requires.audience",
        "requires.attention",
    ];
    let resolvers: String = results
        .iter()
        .enumerate()
        .map(|(index, result)| {
            format!(
                "[[policy.dynamic_resolver]]\nname = \"classifier-{index}\"\nbuiltin = \"claude-code\"\nreturns = [\"{result}\"]\n"
            )
        })
        .collect();
    let bindings: Vec<String> = (0..results.len())
        .map(|index| format!("{{ resolver = \"classifier-{index}\" }}"))
        .collect();
    let config = format!(
        r#"
[policy]
version = 1

{resolvers}
[[policy.tool]]
name = "fetch"
description = "Fetches one URL and returns its body."
parameters = {{ type = "object", properties = {{ url = {{ type = "string" }} }}, required = ["url"] }}
uses = [{bindings}]

[externals]
timeout_ms = 10000
max_body_bytes = 65536

[externals.claude_code]
command = "{command}"
"#,
        bindings = bindings.join(", "),
        command = command.display(),
    );
    let runtime = open_runtime(&dir, &config).await;
    let started = Instant::now();
    assert_eq!(
        propose(&runtime, fetch("https://a.example")).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert!(
        started.elapsed() >= Duration::from_millis(450),
        "five subprocess consults must not all run at once: {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn a_builtin_resolver_never_touches_an_http_endpoint() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, classifier) = serve_classifier().await;
    let command = fake_claude(
        dir.path(),
        "cat > /dev/null\nprintf '%s' '{\"structured_output\":{\"version\":1,\"result\":{\"delta.trust\":\"trusted\"}}}'",
    );
    let config = builtin_policy(&command, "")
        .replace(
            "[[policy.tool]]",
            "[[policy.dynamic_resolver]]\nname = \"other\"\nreturns = [\"delta.audience\"]\n\n[[policy.tool]]",
        )
        .replace(
            "[externals.claude_code]",
            &format!("[externals.dynamic.other]\nurl = \"{url}\"\n\n[externals.claude_code]"),
        );
    let runtime = open_runtime(&dir, &config).await;
    assert_eq!(
        propose(&runtime, fetch("https://a.example")).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert!(
        classifier.requests().is_empty(),
        "the builtin resolver answered; the endpoint saw no request"
    );
}
