//! PostHog battery: internal analytics reads, public documentation input, reviewed writes.
mod common;

use appa_runtime::api::{AuditEvent, RemedyOutcome, Runtime};
use appa_runtime_api::{HookDecision, ProposedCall};
use common::{actor, battery_runtime, members_source, offer_of, propose, ran, raw, root};
use std::sync::Arc;

fn call(tool: &str, args: serde_json::Value) -> ProposedCall {
    ProposedCall {
        tool: format!("mcp/posthog/{tool}"),
        arguments: raw(args),
        cwd: None,
    }
}

async fn runtime(dir: &tempfile::TempDir) -> Arc<Runtime> {
    let source = members_source().await;
    battery_runtime(
        dir.path(),
        "posthog",
        &format!(
            r#"[policy]
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
    .await
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
