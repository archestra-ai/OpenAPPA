mod common;
use common::{actor, audit_len, last_offer, propose, ran, raw, root, serve};

use std::sync::{Arc, Mutex};

use appa_runtime::api::{RemedyOutcome, Runtime};
use appa_runtime::{config::Config, hooks};
use appa_runtime_api::{HookDecision, HookEvent, ProposedCall};
use axum::Router;
use axum::extract::State;
use axum::routing::post;

const POLICY: &str = r#"
[policy]
version = 2

[[policy.audience.group]]
name = "team"
from = ["slack:user-group/team"]

[[policy.audience.group]]
name = "nobody"
from = ["slack:user-group/nobody"]

[[policy.tool]]
name = "read_hr"
delta = { audience = ["alice@corp.example", "bob@corp.example"] }

[[policy.tool]]
name = "send"
parameters = { type = "object", properties = { to = { type = "string" } }, required = ["to"] }
requires = { audience = { contains = ["$to"] } }
effects = ["egress"]
delta = {}

[[policy.tool]]
name = "send_capped"
requires = { audience = { within = ["alice@corp.example", "@team"] } }
effects = ["egress"]
delta = {}

[externals]
timeout_ms = 1000
max_body_bytes = 4096

[externals.audience.slack]
url = "AUDIENCE_URL"
"#;

#[derive(Clone)]
enum Answer {
    Members(Vec<(&'static str, Option<&'static str>)>),
    /// Answer a member lookup with claims echoing the member asked, carrying this email.
    Claims(Option<&'static str>),
    /// Answer a member lookup with claims for a different member than the one asked.
    ForeignClaims,
    Down,
}

#[derive(Clone)]
struct Source {
    answer: Arc<Mutex<Answer>>,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl Source {
    fn set(&self, answer: Answer) {
        *self.answer.lock().unwrap() = answer;
    }

    fn requests(&self) -> Vec<serde_json::Value> {
        self.requests.lock().unwrap().clone()
    }
}

async fn serve_source() -> (String, Source) {
    let source = Source {
        answer: Arc::new(Mutex::new(Answer::Members(vec![]))),
        requests: Arc::new(Mutex::new(Vec::new())),
    };
    let router = Router::new()
        .route(
            "/audience",
            post(|State(source): State<Source>, body: String| async move {
                let request: serde_json::Value = serde_json::from_str(&body).expect("the request is JSON");
                let member = request["artifact"]["member"].as_str().map(str::to_string);
                source.requests.lock().unwrap().push(request);
                let claims = |id: &str, email: Option<&str>| {
                    let claims = match email {
                        Some(email) => serde_json::json!({ "id": id, "verified_email": email }),
                        None => serde_json::json!({ "id": id }),
                    };
                    (
                        axum::http::StatusCode::OK,
                        serde_json::json!({ "version": 1, "answer": { "claims": claims } }).to_string(),
                    )
                };
                match source.answer.lock().unwrap().clone() {
                    Answer::Members(members) => {
                        let members: Vec<serde_json::Value> = members
                            .into_iter()
                            .map(|(id, email)| match email {
                                Some(email) => serde_json::json!({ "id": id, "verified_email": email }),
                                None => serde_json::json!({ "id": id }),
                            })
                            .collect();
                        (
                            axum::http::StatusCode::OK,
                            serde_json::json!({ "version": 1, "answer": { "members": members } }).to_string(),
                        )
                    }
                    Answer::Claims(email) => claims(&member.expect("a claims answer serves a member lookup"), email),
                    Answer::ForeignClaims => claims("slack:U-other", None),
                    Answer::Down => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom".to_string()),
                }
            }),
        )
        .with_state(source.clone());
    (format!("{}/audience", serve(router).await), source)
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

async fn narrowed(dir: &tempfile::TempDir, audience_url: &str) -> Arc<Runtime> {
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, POLICY.replace("AUDIENCE_URL", audience_url)).expect("the fixture writes");
    let config = Config::load(&path).expect("the fixture validates");
    let runtime = Arc::new(Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens"));
    assert_eq!(
        hooks::handle(&runtime, HookEvent::SessionStart { root: root() }).await,
        HookDecision::Ack
    );
    let blocked = propose(&runtime, read_hr()).await;
    let HookDecision::DenyCall { feedback, .. } = blocked else {
        panic!("the narrowing read is offered for acceptance, got {blocked:?}");
    };
    assert!(matches!(
        runtime.execute_remedy(&actor(), last_offer(&feedback)).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, read_hr()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, read_hr()).await;
    runtime
}

#[tokio::test]
async fn a_group_argument_is_checked_against_the_sources_answer() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let runtime = narrowed(&dir, &url).await;

    // Slack Alice's verified email canonicalizes to the same principal the delta wrote.
    source.set(Answer::Members(vec![("slack:U-alice", Some("alice@corp.example"))]));
    assert_eq!(
        propose(&runtime, send("@team")).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, send("@team")).await;
    let requests = source.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["version"], 1);
    assert_eq!(requests[0]["kind"], "audience");
    assert_eq!(requests[0]["name"], "slack");
    assert_eq!(
        requests[0]["declaration"]["templates"],
        serde_json::json!(["viewer", "full-members", "user-group/<handle>"])
    );
    assert_eq!(
        requests[0]["artifact"],
        serde_json::json!({ "selector": "user-group/team" })
    );

    // A member without a verified email keeps its qualified identity, which the
    // narrowed audience does not hold.
    source.set(Answer::Members(vec![
        ("slack:U-alice", Some("alice@corp.example")),
        ("slack:U-carol", None),
    ]));
    assert!(matches!(
        propose(&runtime, send("@team")).await,
        HookDecision::DenyCall { .. }
    ));
    assert_eq!(source.requests().len(), 2);

    // An empty member list is a complete answer.
    source.set(Answer::Members(vec![]));
    assert_eq!(
        propose(&runtime, send("@nobody")).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, send("@nobody")).await;
    assert_eq!(
        source.requests()[2]["artifact"],
        serde_json::json!({ "selector": "user-group/nobody" })
    );
}

#[tokio::test]
async fn no_answer_leaves_the_call_unchecked_and_the_log_unchanged() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let runtime = narrowed(&dir, &url).await;
    let before = audit_len(&runtime);

    source.set(Answer::Down);
    assert!(matches!(
        propose(&runtime, send("@team")).await,
        HookDecision::DenyCall { .. }
    ));
    assert_eq!(audit_len(&runtime), before, "no answer is no engine act");

    source.set(Answer::Members(vec![("slack:U-bob", Some("bob@corp.example"))]));
    assert_eq!(
        propose(&runtime, send("@team")).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert!(audit_len(&runtime) > before);
    ran(&runtime, send("@team")).await;
}

#[tokio::test]
async fn an_unconfigured_group_argument_fails_operationally() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let runtime = narrowed(&dir, &url).await;
    let before = audit_len(&runtime);

    // `@offsite` is supplied dynamically and configured nowhere: an operational
    // refusal that consults nothing and decides nothing.
    assert!(matches!(
        propose(&runtime, send("@offsite")).await,
        HookDecision::DenyCall { .. }
    ));
    assert!(source.requests().is_empty());
    assert_eq!(audit_len(&runtime), before);
}

#[tokio::test]
async fn public_and_literal_arguments_never_consult_the_source() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let runtime = narrowed(&dir, &url).await;

    assert!(matches!(
        propose(&runtime, send("public")).await,
        HookDecision::DenyCall { .. }
    ));
    assert_eq!(
        propose(&runtime, send("alice@corp.example")).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, send("alice@corp.example")).await;
    assert!(matches!(
        propose(&runtime, send("mallory")).await,
        HookDecision::DenyCall { .. }
    ));
    assert!(source.requests().is_empty(), "no spelling here names a group");
}

#[test]
fn a_referenced_audience_source_must_be_bound() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let path = dir.path().join("appa.toml");
    let unbound = POLICY.replace("[externals.audience.slack]\nurl = \"AUDIENCE_URL\"\n", "");
    std::fs::write(&path, unbound).expect("the fixture writes");
    let config = Config::load(&path).expect("the file validates");
    assert!(matches!(
        Runtime::open(config, dir.path().join("appa.db"), None),
        Err(appa_runtime::api::OpenError::UnboundExternal { .. })
    ));
}

#[tokio::test]
async fn a_qualified_recipient_is_checked_through_a_member_lookup() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let runtime = narrowed(&dir, &url).await;

    // The lookup canonicalizes the recipient to the principal the narrowed audience holds.
    source.set(Answer::Claims(Some("bob@corp.example")));
    assert_eq!(
        propose(&runtime, send("slack:U-bob")).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, send("slack:U-bob")).await;
    let requests = source.requests();
    assert_eq!(requests.len(), 1, "one act, one lookup per member");
    assert_eq!(requests[0]["kind"], "audience");
    assert_eq!(requests[0]["artifact"], serde_json::json!({ "member": "slack:U-bob" }));
}

#[tokio::test]
async fn foreign_lookup_claims_are_no_answer_and_are_not_re_asked() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let runtime = narrowed(&dir, &url).await;
    let before = audit_len(&runtime);

    // Claims for a different member than the one asked are a broken answer: the call is
    // refused operationally after exactly one consult, and no decision is recorded.
    source.set(Answer::ForeignClaims);
    assert!(matches!(
        propose(&runtime, send("slack:U-bob")).await,
        HookDecision::DenyCall { .. }
    ));
    assert_eq!(source.requests().len(), 1, "a broken answer is not re-asked");
    assert_eq!(audit_len(&runtime), before);
}

#[tokio::test]
async fn a_bare_provider_prefix_recipient_is_a_literal_reader() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let runtime = narrowed(&dir, &url).await;

    // "slack:" names no member, so it denotes itself: the check decides without any
    // consult instead of looping against the source.
    assert!(matches!(
        propose(&runtime, send("slack:")).await,
        HookDecision::DenyCall { .. }
    ));
    assert!(source.requests().is_empty(), "a bare prefix is never looked up");
}

fn send_capped() -> ProposedCall {
    ProposedCall {
        tool: "send_capped".to_string(),
        arguments: raw(serde_json::json!({})),
    }
}

#[tokio::test]
async fn a_cap_written_with_a_group_is_read_per_act_from_the_source() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let runtime = narrowed(&dir, &url).await;
    let before = audit_len(&runtime);

    source.set(Answer::Members(vec![("slack:U-bob", Some("bob@corp.example"))]));
    assert_eq!(
        propose(&runtime, send_capped()).await,
        HookDecision::AllowCall { spawn: None }
    );
    let requests = source.requests();
    assert_eq!(requests.len(), 1, "one act, one consult per selector");
    assert_eq!(
        requests[0]["artifact"],
        serde_json::json!({ "selector": "user-group/team" })
    );
    ran(&runtime, send_capped()).await;

    source.set(Answer::Members(vec![("slack:U-carol", Some("carol@corp.example"))]));
    let HookDecision::DenyCall { feedback, .. } = propose(&runtime, send_capped()).await else {
        panic!("the moved cap blocks");
    };
    assert!(
        !feedback.contains("carol"),
        "the model hears a directory member: {feedback}"
    );
    assert_eq!(source.requests().len(), 2);

    let undecided = audit_len(&runtime);
    source.set(Answer::Down);
    assert!(matches!(
        propose(&runtime, send_capped()).await,
        HookDecision::DenyCall { .. }
    ));
    assert_eq!(audit_len(&runtime), undecided);
    assert!(audit_len(&runtime) > before);

    let audit = serde_json::to_string(&runtime.audit(&root()).expect("the audit reads")).expect("the audit serializes");
    assert!(!audit.contains("carol"), "the audit leaks a directory member: {audit}");
    drop(runtime);

    // The reopened deployment replays the log — its pinned answers included — without
    // consulting the source; only the fresh act reads it again.
    let config = Config::load(&dir.path().join("appa.toml")).expect("the fixture validates");
    let reopened = Arc::new(Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment reopens"));
    source.set(Answer::Members(vec![("slack:U-bob", Some("bob@corp.example"))]));
    assert_eq!(
        propose(&reopened, send_capped()).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert_eq!(source.requests().len(), 4);
}
