//! The three Cloudflare batteries composed together: public Radar and documentation
//! reads, internal Workers logs, and the one reviewed URL scan.
mod common;

use appa_runtime::{
    api::{AuditEvent, RemedyOutcome, Runtime},
    config::Config,
    hooks,
};
use appa_runtime_api::{HookDecision, HookEvent, ProposedCall};
use axum::{Router, routing::post};
use common::{actor, offer_of, propose, ran, raw, repo_root, root, serve};
use std::sync::Arc;

const BATTERIES: [&str; 3] = ["cloudflare-docs", "cloudflare-radar", "cloudflare-observability"];

fn call(namespace: &str, tool: &str, args: serde_json::Value) -> ProposedCall {
    ProposedCall {
        tool: format!("mcp/{namespace}/{tool}"),
        arguments: raw(args),
    }
}

/// A loopback audience source answering every collection with one fixed roster: the
/// Workers logs and the URL Scanner reads are `internal`, which needs a source to
/// narrow onto.
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
    let mut includes = String::new();
    for battery in BATTERIES {
        let target = dir.path().join("marketplace/batteries").join(battery);
        std::fs::create_dir_all(&target).unwrap();
        std::fs::copy(
            repo_root()
                .join("marketplace/batteries")
                .join(battery)
                .join("appa.toml"),
            target.join("appa.toml"),
        )
        .unwrap();
        includes.push_str(&format!("\"marketplace/batteries/{battery}/appa.toml\", "));
    }
    let path = dir.path().join("appa.toml");
    let source = members_source().await;
    std::fs::write(
        &path,
        format!(
            r#"include = [{includes}]

[policy]
version = 2

[policy.audience]
internal = ["people:members"]

[[policy.authority]]
name = "cloudflare-operator"
permits = {{ trust_below = "trusted", attention = ["cloudflare-radar-review"] }}

[externals]
timeout_ms = 30000
review_timeout_ms = 600000
max_body_bytes = 1048576

[externals.authorities.cloudflare-operator]
builtin = "approve"

[externals.audience.people]
url = "{source}"
selectors = [{{ template = "members", feeds = "internal" }}]
"#
        ),
    )
    .unwrap();
    let runtime = Arc::new(Runtime::open(Config::load(&path).unwrap(), dir.path().join("runtime.db"), None).unwrap());
    assert_eq!(
        hooks::handle(&runtime, HookEvent::SessionStart { root: root() }).await,
        HookDecision::Ack
    );
    runtime
}

/// Radar and documentation reads stay public, so they run outright. Reading Workers
/// logs narrows the trajectory to internal, and the documentation search and the URL
/// scan then refuse that input while a Radar read, which carries no audience bound,
/// keeps running.
#[tokio::test]
async fn public_reads_run_until_the_workers_logs_narrow_the_trajectory() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;

    // The first read admits the trust fall to `suspicious`; the trajectory stays public.
    let outages = call(
        "cloudflare-radar",
        "get_outages",
        serde_json::json!({ "dateRange": "7d" }),
    );
    let offer = offer_of(&propose(&runtime, outages.clone()).await);
    assert!(matches!(
        runtime.execute_remedy(&actor(), offer).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, outages.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, outages).await;

    // Still public, so a documentation search runs outright.
    let docs = call(
        "cloudflare-docs",
        "search_cloudflare_documentation",
        serde_json::json!({ "query": "durable objects alarms" }),
    );
    assert_eq!(
        propose(&runtime, docs.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, docs).await;

    let logs = call(
        "cloudflare-observability",
        "query_worker_observability",
        serde_json::json!({ "query": { "view": "events", "limit": 5 } }),
    );
    let offer = offer_of(&propose(&runtime, logs.clone()).await);
    assert!(matches!(
        runtime.execute_remedy(&actor(), offer).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, logs.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, logs).await;

    // Internal already: another account-scoped read runs outright.
    let code = call(
        "cloudflare-observability",
        "workers_get_worker_code",
        serde_json::json!({ "scriptName": "payments-api" }),
    );
    assert_eq!(
        propose(&runtime, code.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, code).await;

    // A Radar read carries no audience bound, so internal data does not stop it.
    let radar = call(
        "cloudflare-radar",
        "get_bgp_leaks",
        serde_json::json!({ "dateRange": "7d" }),
    );
    assert_eq!(
        propose(&runtime, radar.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, radar).await;

    // The public-input rules refuse internal input, on either server carrying them.
    for public_input in [
        call(
            "cloudflare-docs",
            "search_cloudflare_documentation",
            serde_json::json!({ "query": "workers logs" }),
        ),
        call(
            "cloudflare-observability",
            "search_cloudflare_documentation",
            serde_json::json!({ "query": "workers logs" }),
        ),
        call(
            "cloudflare-radar",
            "create_url_scan",
            serde_json::json!({ "url": "https://example.com/" }),
        ),
    ] {
        assert!(
            !matches!(
                propose(&runtime, public_input.clone()).await,
                HookDecision::AllowCall { .. }
            ),
            "{}",
            public_input.tool
        );
    }

    // Every tool that ran here reads: none of them recorded an effect.
    assert!(released_effects(&runtime).iter().all(Vec::is_empty));
}

/// Submitting a URL to be scanned needs the reviewer even on an untouched trajectory,
/// and records the sensitive effect.
#[tokio::test]
async fn a_url_scan_needs_review_and_records_its_effect() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;

    let scan = call(
        "cloudflare-radar",
        "create_url_scan",
        serde_json::json!({ "url": "https://example.com/" }),
    );
    let decision = propose(&runtime, scan.clone()).await;
    assert!(!matches!(decision, HookDecision::AllowCall { .. }));
    assert!(matches!(
        runtime.execute_remedy(&actor(), offer_of(&decision)).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, scan.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, scan).await;

    assert_eq!(released_effects(&runtime), vec![vec!["cloudflare-radar.sensitive"]]);
}

fn released_effects(runtime: &Arc<Runtime>) -> Vec<Vec<String>> {
    runtime
        .audit(&root())
        .unwrap()
        .into_iter()
        .filter_map(|entry| match entry.event {
            AuditEvent::Released { tool, effects, .. } if tool.starts_with("mcp/cloudflare-") => Some(effects),
            _ => None,
        })
        .collect()
}
