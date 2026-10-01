//! Notion battery: coarse internal reads, trusted writes, reviewed structural changes.
mod common;

use appa_runtime::api::{AuditEvent, RemedyOutcome, Runtime};
use appa_runtime_api::{HookDecision, ProposedCall};
use common::{actor, battery_runtime, members_source, offer_of, propose, ran, raw, root};
use std::sync::Arc;

fn call(tool: &str, args: serde_json::Value) -> ProposedCall {
    ProposedCall {
        tool: format!("mcp/notion/{tool}"),
        arguments: raw(args),
        cwd: None,
    }
}

async fn runtime(dir: &tempfile::TempDir) -> Arc<Runtime> {
    let source = members_source().await;
    battery_runtime(
        dir.path(),
        "notion",
        &format!(
            r#"[policy]
version = 2

[policy.audience]
internal = ["people:members"]

[[policy.authority]]
name = "notion-operator"
permits = {{ trust_below = "trusted", attention = ["notion-review"] }}

[externals]
timeout_ms = 30000
review_timeout_ms = 600000
max_body_bytes = 1048576

[externals.authorities.notion-operator]
builtin = "approve"

[externals.audience.people]
url = "{source}"
selectors = [{{ template = "members", feeds = "internal" }}]
"#
        ),
    )
    .await
}

/// A page read narrows the trajectory to internal and keeps its trust, so the page can be
/// written back; a search reaches connected sources such as mail and lowers the trust, so
/// nothing built from it is written as trusted, nor fetched from a URL into the workspace.
#[tokio::test]
async fn a_page_read_keeps_trust_and_a_search_lowers_it() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;

    let read = call(
        "notion-fetch",
        serde_json::json!({ "id": "https://www.notion.so/acme/Plan-1" }),
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

    let summary = call(
        "notion-create-pages",
        serde_json::json!({ "pages": [{ "properties": { "title": "Summary" } }] }),
    );
    assert_eq!(
        propose(&runtime, summary.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, summary).await;

    let search = call("notion-search", serde_json::json!({ "query": "budget" }));
    let offer = offer_of(&propose(&runtime, search.clone()).await);
    assert!(matches!(
        runtime.execute_remedy(&actor(), offer).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, search.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, search).await;

    for write in [
        call(
            "notion-create-pages",
            serde_json::json!({ "pages": [{ "properties": { "title": "Summary" } }] }),
        ),
        call(
            "notion-create-attachment",
            serde_json::json!({ "filename": "x.html", "source_url": "https://example.com/x.html" }),
        ),
    ] {
        assert!(
            !matches!(propose(&runtime, write.clone()).await, HookDecision::AllowCall { .. }),
            "{}",
            write.tool
        );
    }
}

/// A trusted write inside the workspace runs outright and records `notion.changed`; a
/// structural change waits for the reviewer and records `notion.sensitive`.
#[tokio::test]
async fn writes_record_their_effects_and_structural_changes_wait_for_review() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;

    let create = call(
        "notion-create-pages",
        serde_json::json!({ "pages": [{ "properties": { "title": "Plan" } }] }),
    );
    assert_eq!(
        propose(&runtime, create.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, create).await;

    let mv = call(
        "notion-move-pages",
        serde_json::json!({ "page_or_database_ids": ["1"], "new_parent": { "page_id": "2" } }),
    );
    let decision = propose(&runtime, mv.clone()).await;
    assert!(!matches!(decision, HookDecision::AllowCall { .. }));
    assert!(matches!(
        runtime.execute_remedy(&actor(), offer_of(&decision)).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, mv.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, mv).await;

    let effects: Vec<_> = runtime
        .audit(&root())
        .unwrap()
        .into_iter()
        .filter_map(|entry| match entry.event {
            AuditEvent::Released { tool, effects, .. } if tool.starts_with("mcp/notion/") => Some(effects),
            _ => None,
        })
        .collect();
    assert_eq!(effects, vec![vec!["notion.changed"], vec!["notion.sensitive"]]);
}
