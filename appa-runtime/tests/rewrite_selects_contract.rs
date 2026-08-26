//! A `tool_input` rewrite is judged by the ordered contract its rewritten arguments select, over
//! real boundaries: a loopback HTTP resolver and sanitizer, a real store, the real hook path.

mod common;
use common::{raw, serve};

use std::sync::{Arc, Mutex};

use appa_runtime::api::{AuditEvent, OfferId, RemedyOutcome, Runtime};
use appa_runtime::{config::Config, hooks};
use appa_runtime_api::{Actor, HookDecision, HookEvent, OutcomeBody, ProposedCall, ToolOutcome, TrajectoryId};
use axum::Router;
use axum::extract::State;
use axum::routing::post;

/// The public contract at 0 uses a resolver that owns the recipients the call requires; the
/// private contract at 1 requires the partner desk and records a classified read. `read_hr`
/// narrows the audience to `hr` first, so a call that needs the partner desk blocks and the
/// `redirect` sanitizer — `hr` to `partner` — is offered.
fn policy(base: &str) -> String {
    format!(
        r#"
[policy]
version = 1

[[policy.dynamic_resolver]]
name = "classify"
returns = ["requires.audience"]

[[policy.tool]]
name = "read_hr"
delta = {{ audience = ["hr"] }}

[[policy.tool]]
name = "read_file(path:public/*)"
parameters = {{ type = "object", properties = {{ path = {{ type = "string" }} }}, required = ["path"] }}
uses = [{{ resolver = "classify" }}]
delta = {{}}

[[policy.tool]]
name = "read_file(path:private/*)"
parameters = {{ type = "object", properties = {{ path = {{ type = "string" }} }}, required = ["path"] }}
requires = {{ audience = {{ contains = ["partner"] }} }}
effects = ["classified"]
delta = {{}}

[[policy.sanitizer]]
name = "redirect"
on = ["tool_input"]
permits = {{ audience = {{ from = ["hr"], to = ["partner"] }} }}

[externals]
timeout_ms = 2000
max_body_bytes = 65536

[externals.dynamic.classify]
url = "{base}/resolve"

[externals.sanitizers.redirect]
url = "{base}/sanitize"
"#
    )
}

#[derive(Clone)]
struct Stubs {
    /// Every request the resolver received.
    consults: Arc<Mutex<Vec<serde_json::Value>>>,
    /// The argument object the sanitizer answers with.
    rewrite: Arc<Mutex<serde_json::Value>>,
}

async fn serve_stubs(rewrite: serde_json::Value) -> (String, Stubs) {
    let stubs = Stubs {
        consults: Arc::new(Mutex::new(Vec::new())),
        rewrite: Arc::new(Mutex::new(rewrite)),
    };
    let router = Router::new()
        .route(
            "/resolve",
            post(|State(stubs): State<Stubs>, body: String| async move {
                let request: serde_json::Value = serde_json::from_str(&body).expect("the request is JSON");
                stubs.consults.lock().unwrap().push(request);
                axum::Json(serde_json::json!({
                    "version": 1,
                    "answer": { "requires.audience": { "contains": ["partner"] } },
                }))
            }),
        )
        .route(
            "/sanitize",
            post(|State(stubs): State<Stubs>| async move {
                let rewrite = stubs.rewrite.lock().unwrap().to_string();
                axum::Json(serde_json::json!({ "version": 1, "answer": { "body": rewrite } }))
            }),
        )
        .with_state(stubs.clone());
    (serve(router).await, stubs)
}

fn root() -> TrajectoryId {
    TrajectoryId("rewrite-selects-contract".to_string())
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

fn read_file(path: &str) -> ProposedCall {
    ProposedCall {
        tool: "read_file".to_string(),
        arguments: raw(serde_json::json!({ "path": path })),
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

fn last_offer(decision: &HookDecision) -> OfferId {
    let HookDecision::DenyCall { feedback, .. } = decision else {
        panic!("expected a deny carrying feedback, got {decision:?}")
    };
    feedback
        .lines()
        .filter_map(|line| {
            let after = line.split("offer_id:").nth(1)?;
            let rest = after.trim_start().strip_prefix('"')?;
            Some(OfferId(rest[..rest.find('"')?].to_string()))
        })
        .next_back()
        .unwrap_or_else(|| panic!("no offer id in feedback: {feedback}"))
}

/// A runtime whose audience is narrowed to `hr`, with the sanitizer answering `rewrite`.
async fn narrowed(dir: &tempfile::TempDir, rewrite: serde_json::Value) -> (Arc<Runtime>, Stubs) {
    let (base, stubs) = serve_stubs(rewrite).await;
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, policy(&base)).expect("the fixture writes");
    let config = Config::load(&path).expect("the fixture validates");
    let runtime = Arc::new(Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens"));
    assert_eq!(
        hooks::handle(&runtime, HookEvent::SessionStart { root: root() }).await,
        HookDecision::Ack
    );
    let accept = last_offer(&propose(&runtime, read_hr()).await);
    assert!(matches!(
        runtime.execute_remedy(&actor(), accept).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, read_hr()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, read_hr()).await;
    (runtime, stubs)
}

fn rewritten_path(outcome: RemedyOutcome) -> String {
    let RemedyOutcome::Substituted { call } = outcome else {
        panic!("the input sanitizer's hop substitutes the call, got {outcome:?}")
    };
    assert_eq!(call.tool, "read_file");
    serde_json::from_str::<serde_json::Value>(call.arguments.get()).expect("canonical JSON")["path"]
        .as_str()
        .expect("a path")
        .to_string()
}

fn released_effects(runtime: &Runtime) -> Vec<Vec<String>> {
    runtime
        .audit(&root())
        .expect("the audit reads")
        .iter()
        .filter_map(|entry| match &entry.event {
            AuditEvent::Released { tool, effects, .. } if tool == "read_file" => Some(effects.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_rewrite_into_the_public_contract_consults_its_resolver_about_the_rewritten_call() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (runtime, stubs) = narrowed(&dir, serde_json::json!({ "path": "public/q3.md" })).await;

    let blocked = propose(&runtime, read_file("private/q3.md")).await;
    assert!(
        stubs.consults.lock().unwrap().is_empty(),
        "the private contract uses no resolver"
    );
    let hop = last_offer(&blocked);

    assert_eq!(
        rewritten_path(runtime.execute_remedy(&actor(), hop).await),
        "public/q3.md"
    );
    let consults = stubs.consults.lock().unwrap().clone();
    assert_eq!(
        consults.len(),
        1,
        "the public contract's resolver is consulted once, about the rewrite"
    );
    assert_eq!(
        consults[0]["artifact"]["args"],
        serde_json::json!({ "name": "read_file", "arguments": { "path": "public/q3.md" } })
    );

    assert_eq!(
        propose(&runtime, read_file("public/q3.md")).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, read_file("public/q3.md")).await;
    assert_eq!(
        released_effects(&runtime),
        vec![Vec::<String>::new()],
        "the release records the public contract's effects, not the classified read"
    );
}

#[tokio::test]
async fn a_rewrite_into_the_private_contract_records_the_classified_read_and_consults_nothing() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (runtime, stubs) = narrowed(&dir, serde_json::json!({ "path": "private/q3.md" })).await;

    let blocked = propose(&runtime, read_file("public/q3.md")).await;
    assert_eq!(stubs.consults.lock().unwrap().len(), 1, "the proposal is classified");
    let hop = last_offer(&blocked);

    assert_eq!(
        rewritten_path(runtime.execute_remedy(&actor(), hop).await),
        "private/q3.md"
    );
    assert_eq!(
        stubs.consults.lock().unwrap().len(),
        1,
        "the private contract uses no resolver, and the proposal's answer is not carried"
    );

    assert_eq!(
        propose(&runtime, read_file("private/q3.md")).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, read_file("private/q3.md")).await;
    assert_eq!(released_effects(&runtime), vec![vec!["classified".to_string()]]);
}

#[tokio::test]
async fn a_rewrite_within_the_public_contract_keeps_the_proposals_answer() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (runtime, stubs) = narrowed(&dir, serde_json::json!({ "path": "public/q4.md" })).await;

    let hop = last_offer(&propose(&runtime, read_file("public/q3.md")).await);
    assert_eq!(
        rewritten_path(runtime.execute_remedy(&actor(), hop).await),
        "public/q4.md"
    );
    assert_eq!(
        stubs.consults.lock().unwrap().len(),
        1,
        "the answer about the proposal rides through a rewrite that stays in its contract"
    );
    assert_eq!(
        propose(&runtime, read_file("public/q4.md")).await,
        HookDecision::AllowCall { spawn: None }
    );
}

#[tokio::test]
async fn a_rewrite_no_contract_selects_leaves_the_offer_standing() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (runtime, stubs) = narrowed(&dir, serde_json::json!({ "path": "shared/q3.md" })).await;

    let hop = last_offer(&propose(&runtime, read_file("private/q3.md")).await);
    for _ in 0..2 {
        assert!(matches!(
            runtime.execute_remedy(&actor(), hop.clone()).await,
            RemedyOutcome::NoAnswer { .. }
        ));
    }
    assert!(
        stubs.consults.lock().unwrap().is_empty(),
        "nothing was minted, so nothing was consulted"
    );
    assert!(released_effects(&runtime).is_empty(), "no dispatch opened");
    assert!(matches!(
        propose(&runtime, read_file("shared/q3.md")).await,
        HookDecision::DenyCall { .. }
    ));
}
