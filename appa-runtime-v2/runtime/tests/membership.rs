
use std::sync::{Arc, Mutex};

use appa_runtime_api::{Actor, HookDecision, HookEvent, OutcomeBody, ProposedCall, ToolOutcome, TrajectoryId};
use appa_runtime_v2::api::{RemedyOutcome, Runtime};
use appa_runtime_v2::{config::Config, hooks};
use axum::Router;
use axum::extract::State;
use axum::routing::post;

const POLICY: &str = r#"
[policy]
version = 1

[policy.membership]
name = "directory"

[[policy.tool]]
name = "read_hr"
delta = { audience = { exactly = ["alice", "bob"] } }

[[policy.tool]]
name = "send"
parameters = { type = "object", properties = { to = { type = "string" } }, required = ["to"] }
requires = { audience = { includes = ["$to"] } }
effects = ["egress"]
delta = {}

[externals]
timeout_ms = 1000
max_body_bytes = 4096

[externals.membership]
url = "MEMBERSHIP_URL"
"#;

#[derive(Clone)]
enum Answer {
    Readers(Vec<&'static str>),
    Down,
}

#[derive(Clone)]
struct Directory {
    answer: Arc<Mutex<Answer>>,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl Directory {
    fn set(&self, answer: Answer) {
        *self.answer.lock().unwrap() = answer;
    }

    fn requests(&self) -> Vec<serde_json::Value> {
        self.requests.lock().unwrap().clone()
    }
}

async fn serve_directory() -> (String, Directory) {
    let directory = Directory {
        answer: Arc::new(Mutex::new(Answer::Readers(vec![]))),
        requests: Arc::new(Mutex::new(Vec::new())),
    };
    let router = Router::new()
        .route(
            "/membership",
            post(|State(directory): State<Directory>, body: String| async move {
                let request: serde_json::Value = serde_json::from_str(&body).expect("the request is JSON");
                directory.requests.lock().unwrap().push(request);
                match directory.answer.lock().unwrap().clone() {
                    Answer::Readers(readers) => (
                        axum::http::StatusCode::OK,
                        serde_json::json!({ "version": 1, "readers": readers }).to_string(),
                    ),
                    Answer::Down => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom".to_string()),
                }
            }),
        )
        .with_state(directory.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral loopback port binds");
    let addr = listener.local_addr().expect("the bound address is readable");
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("the stub serves");
    });
    (format!("http://{addr}/membership"), directory)
}

fn raw(value: serde_json::Value) -> Box<serde_json::value::RawValue> {
    serde_json::value::to_raw_value(&value).expect("the fixture serializes")
}

fn root() -> TrajectoryId {
    TrajectoryId("membership-test".to_string())
}

fn actor() -> Actor {
    Actor {
        root: root(),
        child: None,
    }
}

fn read_hr() -> ProposedCall {
    ProposedCall {
        tool: "read_hr".to_string(),
        arguments: raw(serde_json::json!({})),
    }
}

fn send(to: &str) -> ProposedCall {
    ProposedCall {
        tool: "send".to_string(),
        arguments: raw(serde_json::json!({ "to": to })),
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

fn last_offer(feedback: &str) -> appa_runtime_v2::api::OfferId {
    feedback
        .lines()
        .filter_map(|line| {
            let after = line.split("offer_id:").nth(1)?;
            let rest = after.trim_start().strip_prefix('"')?;
            Some(appa_runtime_v2::api::OfferId(rest[..rest.find('"')?].to_string()))
        })
        .next_back()
        .unwrap_or_else(|| panic!("no offer id in feedback: {feedback}"))
}

async fn narrowed(dir: &tempfile::TempDir, membership_url: &str) -> Arc<Runtime> {
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, POLICY.replace("MEMBERSHIP_URL", membership_url)).expect("the fixture writes");
    let config = Config::load(&path).expect("the fixture validates");
    let runtime = Arc::new(Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens"));
    assert_eq!(
        hooks::handle(&runtime, HookEvent::SessionStart { root: root() }).await,
        HookDecision::Ack
    );
    let blocked = propose(&runtime, read_hr()).await;
    let HookDecision::DenyCall { feedback } = blocked else {
        panic!("the narrowing read is offered for acceptance, got {blocked:?}");
    };
    assert!(matches!(
        runtime.execute_remedy(last_offer(&feedback)).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, read_hr()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, read_hr()).await;
    runtime
}

fn audit_len(runtime: &Runtime) -> usize {
    runtime.audit(&root()).expect("the audit reads").len()
}

#[tokio::test]
async fn a_group_argument_is_checked_against_the_directorys_answer() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, directory) = serve_directory().await;
    let runtime = narrowed(&dir, &url).await;

    directory.set(Answer::Readers(vec!["alice"]));
    assert_eq!(
        propose(&runtime, send("@team")).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, send("@team")).await;
    let requests = directory.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["version"], 1);
    assert_eq!(requests[0]["resolver"], "directory");
    assert_eq!(requests[0]["group"], "team");

    directory.set(Answer::Readers(vec!["alice", "carol"]));
    assert!(matches!(
        propose(&runtime, send("@team")).await,
        HookDecision::DenyCall { .. }
    ));
    assert_eq!(directory.requests().len(), 2);

    directory.set(Answer::Readers(vec![]));
    assert_eq!(
        propose(&runtime, send("@nobody")).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, send("@nobody")).await;
    assert_eq!(directory.requests()[2]["group"], "nobody");
}

#[tokio::test]
async fn no_answer_leaves_the_call_unchecked_and_the_log_unchanged() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, directory) = serve_directory().await;
    let runtime = narrowed(&dir, &url).await;
    let before = audit_len(&runtime);

    directory.set(Answer::Down);
    assert!(matches!(
        propose(&runtime, send("@team")).await,
        HookDecision::DenyCall { .. }
    ));
    assert_eq!(audit_len(&runtime), before, "no answer is no engine act");

    directory.set(Answer::Readers(vec!["bob"]));
    assert_eq!(
        propose(&runtime, send("@team")).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert!(audit_len(&runtime) > before);
    ran(&runtime, send("@team")).await;
}

#[tokio::test]
async fn public_and_literal_arguments_never_consult_the_directory() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, directory) = serve_directory().await;
    let runtime = narrowed(&dir, &url).await;

    assert!(matches!(
        propose(&runtime, send("public")).await,
        HookDecision::DenyCall { .. }
    ));
    assert_eq!(
        propose(&runtime, send("alice")).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, send("alice")).await;
    assert!(matches!(
        propose(&runtime, send("mallory")).await,
        HookDecision::DenyCall { .. }
    ));
    assert!(directory.requests().is_empty(), "neither spelling names a group");
}

#[test]
fn a_registered_membership_resolver_must_be_bound() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let path = dir.path().join("appa.toml");
    let unbound = POLICY.replace("[externals.membership]\nurl = \"MEMBERSHIP_URL\"\n", "");
    std::fs::write(&path, unbound).expect("the fixture writes");
    let config = Config::load(&path).expect("the file validates");
    assert!(matches!(
        Runtime::open(config, dir.path().join("appa.db"), None),
        Err(appa_runtime_v2::api::OpenError::UnboundExternal { .. })
    ));
}
