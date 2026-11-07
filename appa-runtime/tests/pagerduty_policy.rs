//! PagerDuty battery: internal reads of operational data, every `manage_*` write reviewed.
mod common;

use appa_runtime::api::{AuditEvent, RemedyOutcome, Runtime};
use appa_runtime_api::{HookDecision, ProposedCall};
use common::{actor, battery_runtime, members_source, propose, ran, raw, root, unsanitized_offer_of};
use std::sync::Arc;

fn call(tool: &str, request: serde_json::Value) -> ProposedCall {
    ProposedCall {
        tool: format!("mcp/pagerduty/{tool}"),
        arguments: raw(serde_json::json!({ "request": request })),
        cwd: None,
    }
}

/// The deployment with the battery's sanitizers bound, as a deployment that runs them
/// binds them. Nothing consults them here.
async fn runtime(dir: &tempfile::TempDir) -> Arc<Runtime> {
    runtime_binding(
        dir,
        r#"
[externals.sanitizers."pagerduty.strip-instructions"]
builtin = "claude-code"

[externals.sanitizers."pagerduty.redact-for-public"]
builtin = "claude-code"
"#,
    )
    .await
}

async fn runtime_binding(dir: &tempfile::TempDir, bindings: &str) -> Arc<Runtime> {
    let source = members_source().await;
    battery_runtime(
        dir.path(),
        "pagerduty",
        &format!(
            r#"[policy]
version = 2

[policy.audience]
internal = ["people:members"]

[[policy.authority]]
name = "pagerduty-operator"
permits = {{ trust_below = "trusted", attention = ["pagerduty-review"] }}

[externals]
timeout_ms = 30000
review_timeout_ms = 600000
max_body_bytes = 1048576

[externals.authorities.pagerduty-operator]
builtin = "approve"

[externals.audience.people]
url = "{source}"
selectors = [{{ template = "members", feeds = "internal" }}]
{bindings}"#
        ),
    )
    .await
}

/// A deployment that binds none of the battery's sanitizers decides as if the battery
/// declared none: the read offers only the narrowing.
#[tokio::test]
async fn an_unbound_sanitizer_is_never_offered() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime_binding(&dir, "").await;
    let read = call(
        "browse_incidents",
        serde_json::json!({ "action": "list", "statuses": ["triggered"] }),
    );
    let blocked = propose(&runtime, read).await;
    let HookDecision::DenyCall { feedback, offers, .. } = &blocked else {
        panic!("an incident read narrows the trajectory, got {blocked:?}");
    };
    assert_eq!(offers.len(), 1, "the narrowing alone: {feedback}");
    assert!(!feedback.contains("sanitizer pagerduty."), "{feedback}");
}

/// An incident read narrows the trajectory to internal unless one of the battery's
/// sanitizers settles it; once it has narrowed, further reads run outright and what was
/// read cannot be written back as trusted.
#[tokio::test]
async fn a_read_is_internal_and_suspicious() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;

    let read = call(
        "browse_incidents",
        serde_json::json!({ "action": "list", "statuses": ["triggered"] }),
    );
    let blocked = propose(&runtime, read.clone()).await;
    // The read is confined, so the battery's sanitizers stand beside the plain narrowing:
    // one keeps the session's trust, the other its `public` audience.
    let HookDecision::DenyCall { feedback, offers, .. } = &blocked else {
        panic!("an incident read narrows the trajectory, got {blocked:?}");
    };
    assert_eq!(offers.len(), 3, "the narrowing and two sanitizers: {feedback}");
    for sanitizer in ["pagerduty.strip-instructions", "pagerduty.redact-for-public"] {
        assert!(
            feedback.contains(&format!("Use sanitizer {sanitizer}'s result")),
            "{sanitizer} is offered: {feedback}"
        );
    }
    assert!(matches!(
        runtime.execute_remedy(&actor(), unsanitized_offer_of(&blocked)).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, read.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, read).await;

    // Internal already: the other reads run without a further decision.
    for read in [
        call("browse_schedules", serde_json::json!({ "action": "list_oncalls" })),
        call("browse_activity", serde_json::json!({ "action": "list_log_entries" })),
    ] {
        assert_eq!(
            propose(&runtime, read.clone()).await,
            HookDecision::AllowCall { spawn: None },
            "{}",
            read.tool
        );
        ran(&runtime, read).await;
    }

    // Suspicious incident text cannot be written back into PagerDuty.
    for write in [
        call(
            "manage_incidents",
            serde_json::json!({ "action": "add_note", "incident_id": "P1", "note": "on it" }),
        ),
        call(
            "manage_status_pages",
            serde_json::json!({ "action": "create_post", "title": "Degraded" }),
        ),
    ] {
        assert!(
            !matches!(propose(&runtime, write.clone()).await, HookDecision::AllowCall { .. }),
            "{}",
            write.tool
        );
    }
}

/// Every write waits for the reviewer and records its effect: the operational write path
/// records `pagerduty.changed`, a configuration or public-facing change
/// `pagerduty.sensitive`.
#[tokio::test]
async fn a_write_needs_review_and_records_its_effect() {
    for (write, effect) in [
        (
            call(
                "manage_incidents",
                serde_json::json!({ "action": "update", "incident_id": "P1", "status": "acknowledged" }),
            ),
            "pagerduty.changed",
        ),
        (
            call(
                "manage_teams",
                serde_json::json!({ "action": "add_member", "team_id": "T1", "user_id": "U1" }),
            ),
            "pagerduty.sensitive",
        ),
        (
            call(
                "manage_event_orchestrations",
                serde_json::json!({ "action": "append_router_rule", "orchestration_id": "O1" }),
            ),
            "pagerduty.sensitive",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(&dir).await;
        let decision = propose(&runtime, write.clone()).await;
        assert!(!matches!(decision, HookDecision::AllowCall { .. }), "{}", write.tool);
        assert!(matches!(
            runtime.execute_remedy(&actor(), unsanitized_offer_of(&decision)).await,
            RemedyOutcome::Authorized { .. }
        ));
        assert_eq!(
            propose(&runtime, write.clone()).await,
            HookDecision::AllowCall { spawn: None }
        );
        ran(&runtime, write.clone()).await;
        let effects: Vec<_> = runtime
            .audit(&root())
            .unwrap()
            .into_iter()
            .filter_map(|entry| match entry.event {
                AuditEvent::Released { tool, effects, .. } if tool.starts_with("mcp/pagerduty/") => Some(effects),
                _ => None,
            })
            .collect();
        assert_eq!(effects, vec![vec![effect]], "{}", write.tool);
    }
}
