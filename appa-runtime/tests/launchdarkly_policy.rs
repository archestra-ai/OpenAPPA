//! LaunchDarkly battery: coarse internal reads; every write needs review, deletes are sensitive.
mod common;

use appa_runtime::{
    api::{AuditEvent, AuditLabel, RemedyOutcome, Runtime},
    config::Config,
    hooks,
};
use appa_runtime_api::{HookDecision, HookEvent, ProposedCall};
use axum::{Router, routing::post};
use common::{actor, offer_of, propose, ran, raw, repo_root, root, serve};
use std::sync::Arc;

fn call(tool: &str, request: serde_json::Value) -> ProposedCall {
    ProposedCall {
        tool: format!("mcp/launchdarkly/{tool}"),
        arguments: raw(serde_json::json!({ "request": request })),
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
    let target = dir.path().join("marketplace/batteries/launchdarkly");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::copy(
        repo_root().join("marketplace/batteries/launchdarkly/appa.toml"),
        target.join("appa.toml"),
    )
    .unwrap();
    let path = dir.path().join("appa.toml");
    let source = members_source().await;
    std::fs::write(
        &path,
        format!(
            r#"include = ["marketplace/batteries/launchdarkly/appa.toml"]

[policy]
version = 2

[policy.audience]
internal = ["people:members"]

[[policy.authority]]
name = "launchdarkly-operator"
permits = {{ trust_below = "trusted", attention = ["launchdarkly-review"] }}

[externals]
timeout_ms = 30000
review_timeout_ms = 600000
max_body_bytes = 1048576

[externals.authorities.launchdarkly-operator]
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

/// A flag read narrows the trajectory to internal and enters `suspicious`: what it
/// returned cannot then be written back to LaunchDarkly as trusted configuration.
#[tokio::test]
async fn a_read_is_internal_and_suspicious() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;

    let read = call(
        "get-feature-flag",
        serde_json::json!({ "projectKey": "acme", "featureFlagKey": "checkout-v2" }),
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

    // Internal already: the audit log and the AI Config reads run outright.
    for read in [
        call(
            "get-audit-log-entries",
            serde_json::json!({ "spec": "proj/acme:env/*:flag/checkout-v2" }),
        ),
        call("list-ai-configs", serde_json::json!({ "projectKey": "acme" })),
    ] {
        assert_eq!(
            propose(&runtime, read.clone()).await,
            HookDecision::AllowCall { spawn: None },
            "{}",
            read.tool
        );
        ran(&runtime, read).await;
    }

    // Every read admitted its result as suspicious data the organization's members may see.
    let admitted: Vec<AuditLabel> = runtime
        .audit(&root())
        .unwrap()
        .into_iter()
        .filter_map(|entry| match entry.event {
            AuditEvent::Admitted { label } => Some(label),
            _ => None,
        })
        .collect();
    assert_eq!(
        admitted,
        vec![
            AuditLabel {
                trust: "suspicious".to_string(),
                audience: "internal".to_string()
            };
            3
        ]
    );

    // Suspicious configuration cannot be written back, reviewer or not.
    for write in [
        call(
            "update-feature-flag",
            serde_json::json!({ "projectKey": "acme", "featureFlagKey": "checkout-v2", "patchWithComment": {} }),
        ),
        call(
            "delete-feature-flag",
            serde_json::json!({ "projectKey": "acme", "featureFlagKey": "checkout-v2" }),
        ),
    ] {
        assert!(
            !matches!(propose(&runtime, write.clone()).await, HookDecision::AllowCall { .. }),
            "{}",
            write.tool
        );
    }
}

/// A trusted write still waits for the reviewer, and records its effect: the sensitive
/// kind for a delete, the ordinary kind for a change.
#[tokio::test]
async fn a_write_needs_review_and_records_its_effect() {
    for (write, effect) in [
        (
            call(
                "create-feature-flag",
                serde_json::json!({ "projectKey": "acme", "featureFlagBody": { "key": "checkout-v2", "name": "Checkout v2" } }),
            ),
            "launchdarkly.changed",
        ),
        (
            call(
                "delete-ai-config",
                serde_json::json!({ "projectKey": "acme", "configKey": "support-bot" }),
            ),
            "launchdarkly.sensitive",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(&dir).await;
        let decision = propose(&runtime, write.clone()).await;
        assert!(!matches!(decision, HookDecision::AllowCall { .. }), "{}", write.tool);
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
                AuditEvent::Released { tool, effects, .. } if tool.starts_with("mcp/launchdarkly/") => Some(effects),
                _ => None,
            })
            .collect();
        assert_eq!(effects, vec![vec![effect]]);
    }
}
