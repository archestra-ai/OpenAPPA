//! A tool the policy does not name is not refused for being unnamed: it is decided by the
//! label algebra like any other, once a cast covering undeclared tools answers the
//! requirement slots its policy leaves unwritten.

mod common;
use common::{offers, raw, serve};

use std::sync::{Arc, Mutex};

use appa_runtime::api::{RemedyOutcome, Runtime};
use appa_runtime::{config::Config, hooks};
use appa_runtime_api::{Actor, HookDecision, HookEvent, OutcomeBody, ProposedCall, ToolOutcome, TrajectoryId};
use axum::Router;
use axum::extract::State;
use axum::routing::post;

const DECLARED_TOOLS: &str = r#"
[policy]
version = 1

[[policy.tool]]
name = "read_page"
delta = { trust = "suspicious" }

[externals]
timeout_ms = 1000
max_body_bytes = 4096
"#;

fn constant_cast_policy() -> String {
    format!(
        r#"{DECLARED_TOOLS}
[[policy.cast]]
name = "undeclared-tools"
constant = {{ trust = "trusted", audience = ["public"], attention = [] }}
"#
    )
}

fn resolver_cast_policy() -> String {
    resolver_cast_policy_admitting(r#"["suspicious", "trusted"]"#)
}

fn resolver_cast_policy_admitting(trust: &str) -> String {
    format!(
        r#"{DECLARED_TOOLS}
[[policy.cast]]
name = "undeclared-tools"
resolver = {{ may_cast = {{ trust = {trust}, audience = ["public"] }} }}

[externals.casts]
"undeclared-tools" = {{ url = "CLASSIFIER_URL" }}
"#
    )
}

const UNDECLARED: &str = "mcp__claude_ai_Gmail__authenticate";

#[derive(Clone)]
struct Classifier {
    answer: Arc<Mutex<Option<serde_json::Value>>>,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl Classifier {
    fn answering(&self, answer: serde_json::Value) {
        *self.answer.lock().unwrap() = Some(answer);
    }

    fn requests(&self) -> Vec<serde_json::Value> {
        self.requests.lock().unwrap().clone()
    }
}

async fn serve_classifier() -> (String, Classifier) {
    let classifier = Classifier {
        answer: Arc::new(Mutex::new(None)),
        requests: Arc::new(Mutex::new(Vec::new())),
    };
    let router = Router::new()
        .route(
            "/classify",
            post(|State(classifier): State<Classifier>, body: String| async move {
                let request: serde_json::Value = serde_json::from_str(&body).expect("the request is JSON");
                classifier.requests.lock().unwrap().push(request);
                match classifier.answer.lock().unwrap().clone() {
                    Some(answer) => (
                        axum::http::StatusCode::OK,
                        serde_json::json!({ "version": 1, "answer": answer }).to_string(),
                    ),
                    None => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom".to_string()),
                }
            }),
        )
        .with_state(classifier.clone());
    (format!("{}/classify", serve(router).await), classifier)
}

fn root() -> TrajectoryId {
    TrajectoryId("undeclared-tool-test".to_string())
}

fn actor() -> Actor {
    Actor {
        root: root(),
        child: None,
    }
}

fn call(tool: &str) -> ProposedCall {
    ProposedCall {
        tool: tool.to_string(),
        arguments: raw(serde_json::json!({ "account": "me@example.com" })),
    }
}

async fn opened(dir: &tempfile::TempDir, policy: &str, url: &str) -> Arc<Runtime> {
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, policy.replace("CLASSIFIER_URL", url)).expect("the fixture writes");
    let config = Config::load(&path).expect("the fixture validates");
    let runtime = Arc::new(Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens"));
    assert_eq!(
        hooks::handle(&runtime, HookEvent::SessionStart { root: root() }).await,
        HookDecision::Ack
    );
    runtime
}

async fn propose(runtime: &Arc<Runtime>, tool: &str) -> HookDecision {
    hooks::handle(
        runtime,
        HookEvent::ToolCall {
            actor: actor(),
            call: call(tool),
            spawn: false,
        },
    )
    .await
}

async fn returned(runtime: &Arc<Runtime>, tool: &str, body: &str) -> HookDecision {
    hooks::handle(
        runtime,
        HookEvent::ToolResult {
            actor: actor(),
            call: call(tool),
            outcome: ToolOutcome::Success {
                body: OutcomeBody::Available(body.to_string()),
            },
        },
    )
    .await
}

/// The agent accepts the narrowing a suspicious read offers, so the session's trust is
/// `suspicious` and a floor of `trusted` refuses a call.
async fn narrowed_to_suspicious(runtime: &Arc<Runtime>) {
    let feedback = denied(propose(runtime, "read_page").await);
    let offer = offers(&feedback)
        .pop()
        .unwrap_or_else(|| panic!("the suspicious read offers its narrowing: {feedback}"));
    assert!(matches!(
        runtime.execute_remedy(&actor(), offer).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(runtime, "read_page").await,
        HookDecision::AllowCall { spawn: None },
        "the accepted narrowing releases the read"
    );
    assert_eq!(
        returned(runtime, "read_page", "the page said something").await,
        HookDecision::Ack
    );
}

fn denied(decision: HookDecision) -> String {
    match decision {
        HookDecision::DenyCall { feedback, .. } => feedback,
        other => panic!("the call is denied, got {other:?}"),
    }
}

#[tokio::test]
async fn without_a_covering_cast_an_undeclared_tool_is_denied_as_undeclared() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = opened(&dir, DECLARED_TOOLS, "").await;

    let feedback = denied(propose(&runtime, UNDECLARED).await);
    assert!(
        offers(&feedback).is_empty(),
        "nothing decided the call, so nothing is offered: {feedback}"
    );
    let feedback = denied(propose(&runtime, "read_page").await);
    assert!(
        !offers(&feedback).is_empty(),
        "the declared tool is decided by its label and offers its narrowing: {feedback}"
    );
}

#[tokio::test]
async fn a_constant_cast_releases_an_undeclared_tool_the_session_label_permits() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = opened(&dir, &constant_cast_policy(), "").await;

    assert_eq!(
        propose(&runtime, UNDECLARED).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert_eq!(
        returned(&runtime, UNDECLARED, "authenticated").await,
        HookDecision::Ack,
        "its result crosses like any unannotated result"
    );
}

#[tokio::test]
async fn a_constant_cast_holds_an_undeclared_tool_to_the_label_algebra() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = opened(&dir, &constant_cast_policy(), "").await;
    narrowed_to_suspicious(&runtime).await;

    denied(propose(&runtime, UNDECLARED).await);
}

#[tokio::test]
async fn a_resolver_cast_is_consulted_with_the_complete_call_and_its_answer_decides() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, classifier) = serve_classifier().await;
    classifier.answering(serde_json::json!({
        "requires.trust": "suspicious",
        "requires.audience": { "contains": "public" },
        "requires.attention": [],
    }));
    let runtime = opened(&dir, &resolver_cast_policy(), &url).await;
    narrowed_to_suspicious(&runtime).await;

    assert_eq!(
        propose(&runtime, UNDECLARED).await,
        HookDecision::AllowCall { spawn: None },
        "a floor of suspicious admits the suspicious session"
    );

    let requests = classifier.requests();
    assert_eq!(requests.len(), 1, "one consult per proposal: {requests:?}");
    let request = &requests[0];
    assert_eq!(request["kind"], serde_json::json!("requirement-cast"));
    assert_eq!(request["name"], serde_json::json!("undeclared-tools"));
    assert_eq!(
        request["declaration"]["returns"],
        serde_json::json!(["requires.trust", "requires.audience", "requires.attention"])
    );
    assert_eq!(
        request["artifact"]["args"],
        serde_json::json!({ "name": UNDECLARED, "arguments": { "account": "me@example.com" } }),
        "the cast judges the call as written"
    );
}

#[tokio::test]
async fn a_resolver_cast_answering_a_floor_the_session_misses_denies_the_call() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, classifier) = serve_classifier().await;
    classifier.answering(serde_json::json!({
        "requires.trust": "trusted",
        "requires.audience": { "contains": "public" },
        "requires.attention": [],
    }));
    let runtime = opened(&dir, &resolver_cast_policy(), &url).await;
    narrowed_to_suspicious(&runtime).await;

    denied(propose(&runtime, UNDECLARED).await);
    assert_eq!(classifier.requests().len(), 1);
}

#[tokio::test]
async fn an_answer_over_the_casts_ceiling_is_passed_over_and_the_dry_cascade_denies() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, classifier) = serve_classifier().await;
    classifier.answering(serde_json::json!({
        "requires.trust": "trusted",
        "requires.audience": { "contains": "public" },
        "requires.attention": [],
    }));
    let runtime = opened(&dir, &resolver_cast_policy_admitting(r#"["suspicious"]"#), &url).await;

    denied(propose(&runtime, UNDECLARED).await);
    assert_eq!(
        classifier.requests().len(),
        1,
        "the refused answer is superseded, not asked for again"
    );
}

#[tokio::test]
async fn a_resolver_cast_that_cannot_answer_grants_nothing() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, classifier) = serve_classifier().await;
    let runtime = opened(&dir, &resolver_cast_policy(), &url).await;

    denied(propose(&runtime, UNDECLARED).await);
    assert_eq!(classifier.requests().len(), 1, "the classifier was asked, and was down");
}
