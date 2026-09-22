//! PostHog battery: internal analytics reads, public documentation input, reviewed writes.
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

fn call(tool: &str, args: serde_json::Value) -> ProposedCall {
    ProposedCall {
        tool: format!("mcp/posthog/{tool}"),
        arguments: raw(args),
        cwd: None,
    }
}

/// A loopback audience source answering every collection with one fixed roster: the
/// battery labels reads `internal`, which needs a source to narrow onto.
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
    let target = dir.path().join("marketplace/batteries/posthog");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::copy(
        repo_root().join("marketplace/batteries/posthog/appa.toml"),
        target.join("appa.toml"),
    )
    .unwrap();
    let path = dir.path().join("appa.toml");
    let source = members_source().await;
    std::fs::write(
        &path,
        format!(
            r#"include = ["marketplace/batteries/posthog/appa.toml"]

[policy]
version = 2

[policy.audience]
internal = ["people:members"]

[[policy.authority]]
name = "posthog-operator"
permits = {{ trust_below = "trusted", attention = ["posthog-review"] }}

[externals]
timeout_ms = 30000
review_timeout_ms = 600000
max_body_bytes = 1048576

[externals.authorities.posthog-operator]
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

/// A HogQL query narrows the trajectory to internal and lowers it to suspicious: the
/// documentation lookup then refuses that input, and what was read cannot be written back.
#[tokio::test]
async fn a_read_is_internal_and_suspicious() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;

    let read = call(
        "query-run",
        serde_json::json!({
            "query": {
                "kind": "DataVisualizationNode",
                "source": { "kind": "HogQLQuery", "query": "select count() from events" }
            }
        }),
    );
    let offer = offer_of(&propose(&runtime, read.clone()).await);
    assert!(matches!(
        runtime.execute_remedy(&actor(), offer).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, read.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, read).await;

    // Internal already: another read runs outright.
    let errors = call("list-errors", serde_json::json!({ "status": "active" }));
    assert_eq!(
        propose(&runtime, errors.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, errors).await;

    // The documentation lookup sends its query to a public endpoint.
    assert!(!matches!(
        propose(
            &runtime,
            call("docs-search", serde_json::json!({ "query": "how do cohorts work" }))
        )
        .await,
        HookDecision::AllowCall { .. }
    ));

    // Suspicious analytics data cannot be written back into PostHog.
    for write in [
        call(
            "insight-create-from-query",
            serde_json::json!({ "data": { "name": "Signups" } }),
        ),
        call(
            "update-feature-flag",
            serde_json::json!({ "flagKey": "new-checkout", "data": { "active": false } }),
        ),
    ] {
        assert!(
            !matches!(propose(&runtime, write.clone()).await, HookDecision::AllowCall { .. }),
            "{}",
            write.tool
        );
    }
}

/// A trusted write needs the reviewer and records its effect, the sensitive kind for a
/// feature flag change.
#[tokio::test]
async fn a_write_needs_review_and_records_its_effect() {
    for (write, effect) in [
        (
            call("dashboard-create", serde_json::json!({ "data": { "name": "Growth" } })),
            "posthog.changed",
        ),
        (
            call(
                "update-feature-flag",
                serde_json::json!({ "flagKey": "new-checkout", "data": { "active": false } }),
            ),
            "posthog.sensitive",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(&dir).await;
        let decision = propose(&runtime, write.clone()).await;
        assert!(!matches!(decision, HookDecision::AllowCall { .. }));
        assert!(matches!(
            runtime.execute_remedy(&actor(), offer_of(&decision)).await,
            RemedyOutcome::Authorized { .. }
        ));
        assert_eq!(
            propose(&runtime, write.clone()).await,
            HookDecision::AllowCall { spawn: None }
        );
        ran(&runtime, write).await;
        let effects: Vec<_> = runtime
            .audit(&root())
            .unwrap()
            .into_iter()
            .filter_map(|entry| match entry.event {
                AuditEvent::Released { tool, effects, .. } if tool.starts_with("mcp/posthog/") => Some(effects),
                _ => None,
            })
            .collect();
        assert_eq!(effects, vec![vec![effect]]);
    }
}
