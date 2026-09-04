//! Spawn coverage on the kagent adapter: an agent called as a tool runs as a child only
//! under a contract that names it. The wildcard covers every ordinary call the policy does
//! not write; under `SpawnCoverage::Declared` it covers no spawn.

mod common;

use std::sync::{Arc, Mutex};

use appa_runtime::api::{LabelSpelling, OfferId, RemedyArguments, RemedyOutcome, Runtime, SpawnCoverage};
use appa_runtime::{config::Config, hooks};
use appa_runtime_api::{Actor, HookDecision, HookEvent, OfferedReturn, ProposedCall};
use axum::Router;
use axum::extract::State;
use axum::routing::post;
use common::{propose, ran, raw, root, serve};

/// An annotator that lets everything through unchanged, counting its consults.
async fn permissive_annotator() -> (String, Arc<Mutex<usize>>) {
    let consults = Arc::new(Mutex::new(0usize));
    let router = Router::new()
        .route(
            "/annotate",
            post(|State(consults): State<Arc<Mutex<usize>>>, _body: String| async move {
                *consults.lock().unwrap() += 1;
                axum::Json(serde_json::json!({
                    "version": 1,
                    "answer": {
                        "delta": {},
                        "requires": { "history": [], "attention": [] },
                        "emits": [],
                    }
                }))
            }),
        )
        .with_state(Arc::clone(&consults));
    (format!("{}/annotate", serve(router).await), consults)
}

fn policy(url: &str) -> String {
    format!(
        r#"
[policy]
version = 2

[[policy.annotator]]
name = "gatekeeper"

# The one agent this deployment delegates to, by the name kagent dispatches it under.
[[policy.tool]]
name = "kagent__NS__log_analyst"
delta = {{}}

# Everything else the policy does not write.
[[policy.tool]]
name = "*"
annotator = "gatekeeper"

[policy.deployment]
context_control = true

[externals]
timeout_ms = 2000
max_body_bytes = 65536

[externals.annotators.gatekeeper]
url = "{url}"
"#
    )
}

async fn open(dir: &tempfile::TempDir, config_toml: &str, coverage: SpawnCoverage) -> Arc<Runtime> {
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, config_toml).expect("the fixture writes");
    let config = Config::load(&path).expect("the fixture validates");
    let runtime = Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens");
    let runtime = Arc::new(runtime.with_spawn_coverage(coverage));
    assert_eq!(
        hooks::handle(&runtime, HookEvent::SessionStart { root: root() }).await,
        HookDecision::Ack
    );
    runtime
}

fn call(tool: &str) -> ProposedCall {
    ProposedCall {
        tool: tool.to_string(),
        arguments: raw(serde_json::json!({ "request": "summarize the crash logs" })),
    }
}

fn acting() -> Actor {
    Actor {
        root: root(),
        child: None,
    }
}

async fn spawn(runtime: &Arc<Runtime>, tool: &str) -> HookDecision {
    hooks::handle(
        runtime,
        HookEvent::ToolCall {
            actor: acting(),
            call: call(tool),
            spawn: true,
            ruling: None,
        },
    )
    .await
}

/// A marked spawn is held until the parent declares the child's return; the bare
/// declaration — as spoken, floored at the parent's label — releases it with its fork.
async fn declare_and_release(runtime: &Arc<Runtime>, held: HookDecision, tool: &str) -> HookDecision {
    let HookDecision::DenyCall { offers, .. } = held else {
        panic!("a marked spawn is held on the return menu, got {held:?}");
    };
    let offer = offers
        .iter()
        .find(|offer| offer.returns == Some(OfferedReturn::AsSpoken))
        .expect("the menu offers the return as spoken");
    let declared = runtime
        .execute_remedy_with(
            &acting(),
            OfferId(offer.id.clone()),
            RemedyArguments {
                label: Some(LabelSpelling::default()),
                return_schema: None,
            },
        )
        .await;
    assert!(
        matches!(declared, RemedyOutcome::Authorized { .. }),
        "the declaration approves the spawn, got {declared:?}"
    );
    spawn(runtime, tool).await
}

#[tokio::test]
async fn under_declared_coverage_an_agent_the_policy_never_names_cannot_spawn() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, consults) = permissive_annotator().await;
    let runtime = open(&dir, &policy(&url), SpawnCoverage::Declared).await;

    match spawn(&runtime, "kagent__NS__release_manager").await {
        HookDecision::DenyCall { feedback, review, .. } => {
            assert!(
                feedback.contains("not declared by the policy"),
                "the model reads why the delegation is denied: {feedback}"
            );
            assert!(review.is_empty(), "nothing to review: no contract, no offer");
        }
        other => panic!("an unnamed agent spawns nothing, got {other:?}"),
    }
    assert_eq!(
        *consults.lock().unwrap(),
        0,
        "no consult stands in for a missing declaration"
    );

    // The same name as an ordinary call is the wildcard's to cover, as before.
    assert_eq!(
        propose(&runtime, call("kagent__NS__release_manager")).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert_eq!(*consults.lock().unwrap(), 1, "the wildcard annotated the ordinary call");
    ran(&runtime, call("kagent__NS__release_manager")).await;

    // The agent the policy names spawns once its return is declared, with its fork.
    let held = spawn(&runtime, "kagent__NS__log_analyst").await;
    assert!(
        matches!(
            declare_and_release(&runtime, held, "kagent__NS__log_analyst").await,
            HookDecision::AllowCall { spawn: Some(_) }
        ),
        "a named agent releases as a spawn"
    );
    assert_eq!(
        *consults.lock().unwrap(),
        1,
        "the named contract decides without a consult"
    );
}

#[tokio::test]
async fn under_wildcard_coverage_the_annotator_covers_a_spawn_as_any_call() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, consults) = permissive_annotator().await;
    let runtime = open(&dir, &policy(&url), SpawnCoverage::Wildcard).await;

    let held = spawn(&runtime, "kagent__NS__release_manager").await;
    assert!(
        matches!(&held, HookDecision::DenyCall { offers, .. } if !offers.is_empty()),
        "the wildcard covers the spawn under the default coverage: it is held on the return menu, got {held:?}"
    );
    assert_eq!(*consults.lock().unwrap(), 1);
    assert!(
        matches!(
            declare_and_release(&runtime, held, "kagent__NS__release_manager").await,
            HookDecision::AllowCall { spawn: Some(_) }
        ),
        "the declared spawn releases with its fork"
    );
}
