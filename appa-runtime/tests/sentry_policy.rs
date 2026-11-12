//! Sentry battery: the catalog proxy is selected by its inner tool name; writes need review.
use crate::common;

use appa_runtime::api::{AuditEvent, RemedyOutcome, Runtime};
use appa_runtime_api::{HookDecision, ProposedCall};
use common::{actor, battery_runtime, members_source, propose, ran, raw, root, unsanitized_offer_of};
use std::sync::Arc;

fn call(tool: &str, args: serde_json::Value) -> ProposedCall {
    ProposedCall {
        tool: format!("mcp/sentry/{tool}"),
        arguments: raw(args),
        cwd: None,
    }
}

fn execute(name: &str) -> ProposedCall {
    call(
        "execute_sentry_tool",
        serde_json::json!({ "name": name, "arguments": {} }),
    )
}

async fn runtime(dir: &tempfile::TempDir) -> Arc<Runtime> {
    runtime_binding(dir, "").await
}

async fn runtime_binding(dir: &tempfile::TempDir, bindings: &str) -> Arc<Runtime> {
    let source = members_source().await;
    battery_runtime(
        dir.path(),
        "sentry",
        &format!(
            r#"[policy]
version = 2

[policy.audience]
internal = ["people:members"]

[[policy.authority]]
name = "sentry-operator"
permits = {{ trust_below = "trusted", attention = ["sentry-review"] }}

[externals]
timeout_ms = 30000
review_timeout_ms = 600000
max_body_bytes = 1048576

[externals.authorities.sentry-operator]
# Approves when asked. The stock "approve" is applied at the block, which this test reads.
command = ["/bin/sh", "-c", 'cat > /dev/null; printf "%s" "{{\"version\":1,\"answer\":{{\"ruling\":\"approve\"}}}}"']

[externals.audience.people]
url = "{source}"
selectors = [{{ template = "members", feeds = "internal" }}]
{bindings}"#
        ),
    )
    .await
}

/// Public redaction is offered only on issue and event records. A DSN, an attachment, or
/// a name outside the catalog returns nothing a model could safely release to `public`,
/// so those results offer instruction stripping alone.
#[tokio::test]
async fn public_redaction_is_offered_only_on_issue_and_event_reads() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime_binding(
        &dir,
        r#"
[externals.sanitizers."sentry.strip-instructions"]
builtin = "claude-code"

[externals.sanitizers."sentry.redact-for-public"]
builtin = "claude-code"
"#,
    )
    .await;
    let offered = |decision: &HookDecision, sanitizer: &str| match decision {
        HookDecision::DenyCall { feedback, .. } => feedback.contains(&format!("Use sanitizer {sanitizer}'s result")),
        other => panic!("the call narrows the trajectory, got {other:?}"),
    };

    for record in [
        execute("get_issue_details"),
        execute("search_issue_events"),
        call("search_events", serde_json::json!({ "organizationSlug": "acme" })),
    ] {
        let decision = propose(&runtime, record.clone()).await;
        assert!(
            offered(&decision, "sentry.redact-for-public"),
            "{}: {decision:?}",
            record.arguments
        );
        assert!(
            offered(&decision, "sentry.strip-instructions"),
            "{}: {decision:?}",
            record.arguments
        );
    }
    for other in [
        execute("find_dsns"),
        execute("get_event_attachment"),
        execute("not_a_catalog_tool"),
    ] {
        let decision = propose(&runtime, other.clone()).await;
        assert!(
            !offered(&decision, "sentry.redact-for-public"),
            "{}: {decision:?}",
            other.arguments
        );
    }
    let dsns = propose(&runtime, execute("find_dsns")).await;
    assert!(offered(&dsns, "sentry.strip-instructions"), "{dsns:?}");
}

/// A catalog read behind the proxy narrows the trajectory like the listed reads; the
/// documentation tools then refuse an internal query, and a name outside the catalog is
/// not a read at all.
#[tokio::test]
async fn the_proxy_is_read_by_its_inner_tool_name() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;

    let read = execute("get_issue_details");
    let offer = unsanitized_offer_of(&propose(&runtime, read.clone()).await);
    assert!(matches!(
        runtime.execute_remedy(&actor(), offer).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, read.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, read).await;

    // Internal already: the listed read and another catalog read run outright.
    for read in [
        call("search_issues", serde_json::json!({ "organizationSlug": "acme" })),
        execute("find_teams"),
    ] {
        assert_eq!(
            propose(&runtime, read.clone()).await,
            HookDecision::AllowCall { spawn: None },
            "{}",
            read.tool
        );
        ran(&runtime, read).await;
    }

    // A documentation lookup sends its input to a public endpoint.
    assert!(!matches!(
        propose(&runtime, execute("search_docs")).await,
        HookDecision::AllowCall { .. }
    ));

    // Suspicious content cannot be written back; a name outside the catalog is no read.
    for write in [
        call(
            "update_issue",
            serde_json::json!({ "issueId": "1", "status": "resolved" }),
        ),
        execute("create_team"),
        execute("not_a_catalog_tool"),
    ] {
        assert!(
            !matches!(propose(&runtime, write.clone()).await, HookDecision::AllowCall { .. }),
            "{}",
            write.tool
        );
    }
}

/// A trusted write needs the reviewer and records its effect, the sensitive kind for a
/// project change.
#[tokio::test]
async fn a_write_needs_review_and_records_its_effect() {
    for (write, effect) in [
        (execute("add_issue_note"), "sentry.changed"),
        (execute("update_project"), "sentry.sensitive"),
        (execute("not_a_catalog_tool"), "sentry.sensitive"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(&dir).await;
        let decision = propose(&runtime, write.clone()).await;
        assert!(!matches!(decision, HookDecision::AllowCall { .. }));
        assert!(matches!(
            runtime.execute_remedy(&actor(), unsanitized_offer_of(&decision)).await,
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
                AuditEvent::Released { tool, effects, .. } if tool.starts_with("mcp/sentry/") => Some(effects),
                _ => None,
            })
            .collect();
        assert_eq!(effects, vec![vec![effect]]);
    }
}
