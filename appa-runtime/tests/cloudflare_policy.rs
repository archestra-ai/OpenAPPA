//! Cloudflare battery: public documentation reads and internal Workers logs under one
//! namespace, with no writes.
mod common;

use appa_runtime::{
    api::{AuditEvent, Runtime},
    config::Config,
    hooks,
};
use appa_runtime_api::{HookDecision, HookEvent, ProposedCall};
use axum::{Router, routing::post};
use common::{actor, offer_of, propose, ran, raw, repo_root, root, serve};
use std::sync::Arc;

fn call(tool: &str, args: serde_json::Value) -> ProposedCall {
    ProposedCall {
        tool: format!("mcp/cloudflare/{tool}"),
        arguments: raw(args),
        cwd: None,
    }
}

/// A loopback audience source answering every collection with one fixed roster: the
/// Workers logs are `internal`, which needs a source to narrow onto.
async fn members_source() -> String {
    let router = Router::new().route(
        "/audience",
        post(|_body: String| async move {
            serde_json::json!({ "version": 1, "answer": { "members": ["alice@corp.example"] } }).to_string()
        }),
    );
    format!("{}/audience", serve(router).await)
}

async fn runtime(dir: &tempfile::TempDir) -> Arc<Runtime> {
    let target = dir.path().join("marketplace/batteries/cloudflare");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::copy(
        repo_root().join("marketplace/batteries/cloudflare/appa.toml"),
        target.join("appa.toml"),
    )
    .unwrap();
    let path = dir.path().join("appa.toml");
    let source = members_source().await;
    std::fs::write(
        &path,
        format!(
            r#"include = ["marketplace/batteries/cloudflare/appa.toml"]

[policy]
version = 2

[policy.audience]
internal = ["people:members"]

[externals]
timeout_ms = 30000
max_body_bytes = 1048576

[externals.audience.people]
url = "{source}"
selectors = [{{ template = "members", feeds = "internal" }}]
"#
        ),
    )
    .unwrap();
    let runtime = Arc::new(Runtime::open(Config::load(&path).unwrap(), dir.path().join("runtime.db"), None).unwrap());
    assert_eq!(
        hooks::handle(
            &runtime,
            HookEvent::SessionStart {
                root: root(),
                principal: None
            }
        )
        .await,
        HookDecision::Ack
    );
    runtime
}

/// Documentation reads stay public, so they run outright. Reading Workers logs
/// narrows the trajectory to internal, and the documentation search then refuses that
/// input.
#[tokio::test]
async fn public_reads_run_until_the_workers_logs_narrow_the_trajectory() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;

    // The first read admits the trust fall to `suspicious`; the trajectory stays public.
    let docs = call(
        "search_cloudflare_documentation",
        serde_json::json!({ "query": "durable objects alarms" }),
    );
    let offer = offer_of(&propose(&runtime, docs.clone()).await);
    assert!(matches!(
        runtime.execute_remedy(&actor(), offer).await,
        appa_runtime::api::RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, docs.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, docs).await;

    // Still public, so the migration guide runs outright.
    let guide = call("migrate_pages_to_workers_guide", serde_json::json!({}));
    assert_eq!(
        propose(&runtime, guide.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, guide).await;

    let logs = call(
        "query_worker_observability",
        serde_json::json!({ "query": { "view": "events", "limit": 5 } }),
    );
    let offer = offer_of(&propose(&runtime, logs.clone()).await);
    assert!(matches!(
        runtime.execute_remedy(&actor(), offer).await,
        appa_runtime::api::RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, logs.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, logs).await;

    // Internal already: another account-scoped read runs outright.
    let code = call(
        "workers_get_worker_code",
        serde_json::json!({ "scriptName": "payments-api" }),
    );
    assert_eq!(
        propose(&runtime, code.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, code).await;

    // The public-input rule refuses internal input.
    let search = call(
        "search_cloudflare_documentation",
        serde_json::json!({ "query": "workers logs" }),
    );
    assert!(!matches!(
        propose(&runtime, search).await,
        HookDecision::AllowCall { .. }
    ));

    // Every tool that ran here reads: none of them recorded an effect.
    let effects: Vec<Vec<String>> = runtime
        .audit(&root())
        .unwrap()
        .into_iter()
        .filter_map(|entry| match entry.event {
            AuditEvent::Released { tool, effects, .. } if tool.starts_with("mcp/cloudflare/") => Some(effects),
            _ => None,
        })
        .collect();
    assert_eq!(effects.len(), 4);
    assert!(effects.iter().all(Vec::is_empty));
}
