//! A `tool_input` rewrite is judged by the ordered declaration its rewritten arguments select,
//! over real boundaries: a loopback HTTP annotator and sanitizer, a real store, the real hook
//! path.

mod common;
use common::{actor, offer_of, propose, ran, raw, root, serve};

use std::sync::{Arc, Mutex};

use appa_runtime::api::{AuditEvent, RemedyOutcome, Runtime};
use appa_runtime::{config::Config, hooks};
use appa_runtime_api::{HookDecision, HookEvent, ProposedCall};
use axum::Router;
use axum::extract::State;
use axum::routing::post;

/// The public contract at 0 routes through an annotator that owns the recipients the call
/// requires; the private contract at 1 requires the partner desk and records a classified
/// read. `read_hr` narrows the audience to `hr` first, so a call that needs the partner desk
/// blocks and the `redirect` sanitizer — `hr` to `partner` — is offered.
fn policy(base: &str) -> String {
    format!(
        r#"
[policy]
version = 2

[[policy.annotator]]
name = "classify"
audiences = ["partner"]

[[policy.tool]]
name = "read_hr"
delta = {{ audience = ["hr"] }}

[[policy.tool]]
name = "read_file(path:public/*)"
parameters = {{ type = "object", properties = {{ path = {{ type = "string" }} }}, required = ["path"] }}
annotator = "classify"

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

[externals.annotators.classify]
url = "{base}/resolve"

[externals.sanitizers.redirect]
url = "{base}/sanitize"
"#
    )
}

#[derive(Clone)]
struct Stubs {
    /// Every request the annotator received.
    consults: Arc<Mutex<Vec<serde_json::Value>>>,
    /// Every request the sanitizer received.
    sanitizations: Arc<Mutex<Vec<serde_json::Value>>>,
    /// The argument object the sanitizer answers with.
    rewrite: Arc<Mutex<serde_json::Value>>,
}

async fn serve_stubs(rewrite: serde_json::Value) -> (String, Stubs) {
    let stubs = Stubs {
        consults: Arc::new(Mutex::new(Vec::new())),
        sanitizations: Arc::new(Mutex::new(Vec::new())),
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
                    "answer": {
                        "delta": {},
                        "requires": {
                            "audience": { "contains": ["partner"] },
                            "history": [],
                            "attention": [],
                        },
                        "emits": [],
                    },
                }))
            }),
        )
        .route(
            "/sanitize",
            post(|State(stubs): State<Stubs>, body: String| async move {
                let request: serde_json::Value = serde_json::from_str(&body).expect("the request is JSON");
                stubs.sanitizations.lock().unwrap().push(request);
                let rewrite = stubs.rewrite.lock().unwrap().to_string();
                axum::Json(serde_json::json!({ "version": 1, "answer": { "body": rewrite } }))
            }),
        )
        .with_state(stubs.clone());
    (serve(router).await, stubs)
}

fn read_hr() -> ProposedCall {
    ProposedCall {
        tool: "read_hr".to_string(),
        arguments: raw(serde_json::json!({})),
        cwd: None,
    }
}

fn read_file(path: &str) -> ProposedCall {
    ProposedCall {
        tool: "read_file".to_string(),
        arguments: raw(serde_json::json!({ "path": path })),
        cwd: None,
    }
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
    let accept = offer_of(&propose(&runtime, read_hr()).await);
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
async fn a_rewrite_into_the_public_contract_consults_its_annotator_about_the_rewritten_call() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (runtime, stubs) = narrowed(&dir, serde_json::json!({ "path": "public/q3.md" })).await;

    let blocked = propose(&runtime, read_file("private/q3.md")).await;
    assert!(
        stubs.consults.lock().unwrap().is_empty(),
        "the private contract is static and owes no annotation"
    );
    let hop = offer_of(&blocked);

    assert_eq!(
        rewritten_path(runtime.execute_remedy(&actor(), hop).await),
        "public/q3.md"
    );
    let consults = stubs.consults.lock().unwrap().clone();
    assert_eq!(
        consults.len(),
        1,
        "the public declaration's annotator is consulted once, about the rewrite"
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
    let hop = offer_of(&blocked);

    assert_eq!(
        rewritten_path(runtime.execute_remedy(&actor(), hop).await),
        "private/q3.md"
    );
    assert_eq!(
        stubs.consults.lock().unwrap().len(),
        1,
        "the private contract is static, and the proposal's annotation is not carried"
    );

    assert_eq!(
        propose(&runtime, read_file("private/q3.md")).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, read_file("private/q3.md")).await;
    assert_eq!(released_effects(&runtime), vec![vec!["classified".to_string()]]);
}

#[tokio::test]
async fn a_rewrite_within_the_public_contract_is_annotated_afresh() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (runtime, stubs) = narrowed(&dir, serde_json::json!({ "path": "public/q4.md" })).await;

    let hop = offer_of(&propose(&runtime, read_file("public/q3.md")).await);
    assert_eq!(
        rewritten_path(runtime.execute_remedy(&actor(), hop).await),
        "public/q4.md"
    );
    // The sanitizer was asked over the wire about the call it rewrites: the input point,
    // the callee and its arguments, and the schema the rewrite must still satisfy.
    let sanitizations = stubs.sanitizations.lock().unwrap().clone();
    assert_eq!(sanitizations.len(), 1);
    assert_eq!(sanitizations[0]["declaration"]["on"], "tool_input");
    assert_eq!(
        sanitizations[0]["declaration"]["parameters"]["required"],
        serde_json::json!(["path"])
    );
    assert_eq!(sanitizations[0]["artifact"]["tool"], "read_file");
    assert_eq!(
        sanitizations[0]["artifact"]["body"],
        serde_json::json!({ "path": "public/q3.md" }).to_string()
    );
    let consults = stubs.consults.lock().unwrap().clone();
    assert_eq!(
        consults.len(),
        2,
        "a rewrite is a new call: the annotator is consulted about the rewritten arguments"
    );
    assert_eq!(
        consults[1]["artifact"]["args"],
        serde_json::json!({ "name": "read_file", "arguments": { "path": "public/q4.md" } })
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

    let hop = offer_of(&propose(&runtime, read_file("private/q3.md")).await);
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
