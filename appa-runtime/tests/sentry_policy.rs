//! Sentry battery: the catalog proxy is selected by its inner tool name; writes need review.
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
        tool: format!("mcp/sentry/{tool}"),
        arguments: raw(args),
    }
}

fn execute(name: &str) -> ProposedCall {
    call(
        "execute_sentry_tool",
        serde_json::json!({ "name": name, "arguments": {} }),
    )
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
    let target = dir.path().join("marketplace/batteries/sentry");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::copy(
        repo_root().join("marketplace/batteries/sentry/appa.toml"),
        target.join("appa.toml"),
    )
    .unwrap();
    let path = dir.path().join("appa.toml");
    let source = members_source().await;
    std::fs::write(
        &path,
        format!(
            r#"include = ["marketplace/batteries/sentry/appa.toml"]

[policy]
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

/// A catalog read behind the proxy narrows the trajectory like the listed reads; the
/// documentation tools then refuse an internal query, and a name outside the catalog is
/// not a read at all.
#[tokio::test]
async fn the_proxy_is_read_by_its_inner_tool_name() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;

    let read = execute("get_issue_details");
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
                AuditEvent::Released { tool, effects, .. } if tool.starts_with("mcp/sentry/") => Some(effects),
                _ => None,
            })
            .collect();
        assert_eq!(effects, vec![vec![effect]]);
    }
}
