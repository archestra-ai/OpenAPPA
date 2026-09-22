//! monday battery: bounded internal reads and autonomous public-input writes.
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

const SUPPORTED_TOOLS: &[&str] = &[
    "get_user_context",
    "get_board_info",
    "get_board_items_page",
    "get_updates",
    "create_item",
];

// Tool names from the authenticated 2026-09-22 tools/list capture, independent
// of the policy so an omitted contract cannot disappear from coverage.
fn discovered_tools() -> Vec<String> {
    serde_json::from_str(include_str!("fixtures/monday-tools.json")).expect("the discovery fixture is valid JSON")
}

fn call(tool: &str, args: serde_json::Value) -> ProposedCall {
    ProposedCall {
        tool: format!("mcp/monday/{tool}"),
        arguments: raw(args),
    }
}

/// The battery deliberately has no monday ACL helper. This fixture supplies the
/// deployment's explicit internal cohort so audience narrowing is deterministic.
async fn members_source() -> String {
    let router = Router::new().route(
        "/audience",
        post(|_body: String| async move {
            serde_json::json!({ "version": 1, "answer": { "members": ["alice@corp.example"] } }).to_string()
        }),
    );
    format!("{}/audience", serve(router).await)
}

/// A deployment-level wildcard is intentionally reviewable here so the
/// composition test can distinguish an exact terminal block from an omitted
/// future tool that the deployment has chosen to cover.
async fn wildcard_annotator() -> String {
    let router = Router::new().route(
        "/",
        post(|_body: String| async move {
            serde_json::json!({
                "version": 1,
                "answer": {
                    "delta": {},
                    "requires": { "trust": "trusted", "attention": ["signoff"], "history": [] },
                    "emits": []
                }
            })
            .to_string()
        }),
    );
    serve(router).await
}

async fn runtime(dir: &tempfile::TempDir) -> Arc<Runtime> {
    let target = dir.path().join("marketplace/batteries/monday");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::copy(
        repo_root().join("marketplace/batteries/monday/appa.toml"),
        target.join("appa.toml"),
    )
    .unwrap();
    let source = members_source().await;
    let path = dir.path().join("appa.toml");
    std::fs::write(
        &path,
        format!(
            r#"include = ["marketplace/batteries/monday/appa.toml"]

[policy]
version = 2

[policy.audience]
internal = ["people:members"]

[externals]
timeout_ms = 30000
review_timeout_ms = 600000
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
        hooks::handle(&runtime, HookEvent::SessionStart { root: root() }).await,
        HookDecision::Ack
    );
    runtime
}

async fn runtime_with_wildcard(dir: &tempfile::TempDir) -> Arc<Runtime> {
    let target = dir.path().join("marketplace/batteries/monday");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::copy(
        repo_root().join("marketplace/batteries/monday/appa.toml"),
        target.join("appa.toml"),
    )
    .unwrap();
    let source = members_source().await;
    let annotator = wildcard_annotator().await;
    let path = dir.path().join("appa.toml");
    std::fs::write(
        &path,
        format!(
            r#"include = ["marketplace/batteries/monday/appa.toml"]

[policy]
version = 2

[policy.audience]
internal = ["people:members"]

[[policy.annotator]]
name = "gatekeeper"

[[policy.tool]]
name = "*"
annotator = "gatekeeper"

[[policy.authority]]
name = "operator"
[policy.authority.permits]
attention = ["signoff"]

[externals]
timeout_ms = 30000
review_timeout_ms = 600000
max_body_bytes = 1048576

[externals.audience.people]
url = "{source}"
selectors = [{{ template = "members", feeds = "internal" }}]

[externals.annotators.gatekeeper]
url = "{annotator}"

[externals.authorities.operator]
builtin = "hitl"
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

#[tokio::test]
async fn bounded_reads_consult_internal_and_reject_escape_variants() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;

    let board = call(
        "get_board_info",
        serde_json::json!({ "boardId": 1, "filters": { "columns": { "only": true } } }),
    );
    let decision = propose(&runtime, board.clone()).await;
    assert!(matches!(decision, HookDecision::DenyCall { .. }));
    assert!(matches!(
        runtime.execute_remedy(&actor(), offer_of(&decision)).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, board.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, board).await;

    let items = call(
        "get_board_items_page",
        serde_json::json!({ "boardId": 1, "itemIds": [1, 2], "includeColumns": true }),
    );
    assert_eq!(
        propose(&runtime, items.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, items).await;

    let updates = call(
        "get_updates",
        serde_json::json!({
            "objectId": "1",
            "objectType": "Item",
            "includeReplies": false,
            "includeAssets": false
        }),
    );
    assert_eq!(
        propose(&runtime, updates.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, updates).await;

    for escaped in [
        call("get_board_info", serde_json::json!({ "boardId": 1, "unknown": true })),
        call(
            "get_board_info",
            serde_json::json!({ "boardId": 1, "filters": { "columns": { "unknown": true } } }),
        ),
        call(
            "get_board_items_page",
            serde_json::json!({ "boardId": 1, "searchTerm": "secret" }),
        ),
        call(
            "get_board_items_page",
            serde_json::json!({ "boardId": 1, "itemIds": vec![1; 100] }),
        ),
        call(
            "get_updates",
            serde_json::json!({ "objectId": "1", "objectType": "Board" }),
        ),
    ] {
        assert!(
            !matches!(propose(&runtime, escaped.clone()).await, HookDecision::AllowCall { .. }),
            "escaped call was admitted: {}",
            escaped.tool
        );
    }
}

#[tokio::test]
async fn public_input_writes_run_without_a_default_review_and_record_effects() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;

    let create = call(
        "create_item",
        serde_json::json!({ "boardId": 1, "name": "OpenAPPA monday smoke", "columnValues": "{}" }),
    );
    assert_eq!(
        propose(&runtime, create.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, create).await;

    let admitted = runtime
        .audit(&root())
        .unwrap()
        .into_iter()
        .filter_map(|entry| match entry.event {
            AuditEvent::Admitted { label } => Some(label),
            _ => None,
        })
        .next_back()
        .expect("the write result is admitted");
    assert_eq!(admitted.audience, "public");

    let effects: Vec<_> = runtime
        .audit(&root())
        .unwrap()
        .into_iter()
        .filter_map(|entry| match entry.event {
            AuditEvent::Released { tool, effects, .. } if tool.starts_with("mcp/monday/") => Some(effects),
            _ => None,
        })
        .collect();
    assert_eq!(effects, vec![vec!["monday.changed"]]);
}

#[tokio::test]
async fn a_restricted_read_cannot_flow_to_an_unknown_monday_destination() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;

    let read = call("get_board_items_page", serde_json::json!({ "boardId": 1 }));
    let decision = propose(&runtime, read.clone()).await;
    assert!(matches!(
        runtime.execute_remedy(&actor(), offer_of(&decision)).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, read.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, read).await;

    for write in [
        call(
            "create_item",
            serde_json::json!({ "boardId": 1, "name": "must refuse", "columnValues": "{}" }),
        ),
        call(
            "create_update",
            serde_json::json!({ "itemId": 1, "body": "must refuse" }),
        ),
    ] {
        assert!(
            !matches!(propose(&runtime, write.clone()).await, HookDecision::AllowCall { .. }),
            "restricted content reached {}",
            write.tool
        );
    }
}

#[tokio::test]
async fn writes_reject_structural_or_opaque_variants() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;

    for rejected in [
        call(
            "create_item",
            serde_json::json!({ "boardId": 1, "name": "x", "columnValues": "{}", "parentItemId": 2 }),
        ),
        call(
            "create_item",
            serde_json::json!({ "boardId": 1, "name": "x", "columnValues": "{}", "duplicateFromItemId": 2 }),
        ),
        call(
            "create_item",
            serde_json::json!({ "boardId": 1, "name": "x", "columnValues": "{}", "createLabelsIfMissing": true }),
        ),
        call(
            "create_item",
            serde_json::json!({ "boardId": 1, "name": "x", "columnValues": r#"{"status":"Done"}"# }),
        ),
        call(
            "create_update",
            serde_json::json!({ "itemId": 1, "body": "x", "mentionsList": "[]" }),
        ),
        call(
            "create_update",
            serde_json::json!({ "itemId": 1, "body": "x", "parentId": 2 }),
        ),
        call("create_update", serde_json::json!({ "itemId": 1, "body": "x" })),
        call(
            "change_item_column_values",
            serde_json::json!({ "boardId": 1, "itemId": 1, "columnValues": "{}" }),
        ),
        call(
            "all_monday_api",
            serde_json::json!({ "query": "mutation { delete_board(board_id: 1) { id } }" }),
        ),
        call(
            "all_api_read",
            serde_json::json!({ "query": "query { boards { id } }" }),
        ),
        call(
            "all_api_write",
            serde_json::json!({ "query": "mutation { delete_board(board_id: 1) { id } }" }),
        ),
        call("execute_code", serde_json::json!({ "code": "delete everything" })),
        call("run_action", serde_json::json!({ "actionId": "1" })),
    ] {
        assert!(
            !matches!(
                propose(&runtime, rejected.clone()).await,
                HookDecision::AllowCall { .. }
            ),
            "unsafe variant was admitted: {}",
            rejected.tool
        );
    }
}

#[tokio::test]
async fn future_tools_are_refused_without_a_root_wildcard() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;
    let decision = propose(
        &runtime,
        call("future_unknown_tool", serde_json::json!({ "marker": "unknown" })),
    )
    .await;
    assert!(!matches!(decision, HookDecision::AllowCall { .. }));
    // A deployment-level `name = "*"` would cover future names; this battery
    // intentionally supplies no such catchall. The exact blocked rules above
    // remain necessary when a deployment chooses to add one.
}

#[tokio::test]
async fn exact_refusals_stay_terminal_under_a_reviewable_root_wildcard() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime_with_wildcard(&dir).await;
    for name in discovered_tools()
        .iter()
        .filter(|name| !SUPPORTED_TOOLS.contains(&name.as_str()))
    {
        let decision = propose(&runtime, call(name, serde_json::json!({}))).await;
        match decision {
            HookDecision::DenyCall {
                offers,
                review,
                feedback,
            } => {
                assert!(offers.is_empty(), "blocked tool received a remedy: {name}");
                assert!(review.is_empty(), "blocked tool received review: {name}");
                assert!(
                    feedback.contains("blocked"),
                    "terminal block lost its marker: {name}: {feedback}"
                );
            }
            other => panic!("exact refusal was opened by the wildcard for {name}: {other:?}"),
        }
    }
}

#[tokio::test]
async fn an_unknown_future_tool_is_covered_by_a_root_wildcard() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime_with_wildcard(&dir).await;
    let decision = propose(
        &runtime,
        call("future_unknown_tool", serde_json::json!({ "marker": "unknown" })),
    )
    .await;
    match decision {
        HookDecision::DenyCall {
            offers,
            review,
            feedback,
        } => {
            assert!(!offers.is_empty(), "wildcard did not produce a reviewable remedy");
            assert!(!review.is_empty(), "wildcard did not expose the configured review");
            assert!(
                feedback.contains("signoff"),
                "wildcard response was not consulted: {feedback}"
            );
        }
        other => panic!("future tool did not use the root wildcard: {other:?}"),
    }
}

#[test]
fn every_discovered_tool_has_an_explicit_contract() {
    let discovered = discovered_tools();
    assert_eq!(discovered.len(), 96);
    let expected: std::collections::BTreeSet<_> = discovered.iter().map(String::as_str).collect();
    assert_eq!(expected.len(), 96, "discovery contains duplicate names");

    let policy: toml::Value = toml::from_str(include_str!("../../marketplace/batteries/monday/appa.toml")).unwrap();
    let contracts = policy["policy"]["tool"].as_array().unwrap();
    let actual: std::collections::BTreeSet<_> = contracts
        .iter()
        .map(|contract| contract["name"].as_str().unwrap().strip_prefix("mcp/monday/").unwrap())
        .collect();
    assert_eq!(contracts.len(), expected.len());
    assert_eq!(actual, expected);

    let supported = contracts
        .iter()
        .filter(|contract| contract["requires"].get("attention").is_none())
        .map(|contract| contract["name"].as_str().unwrap().strip_prefix("mcp/monday/").unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(supported, SUPPORTED_TOOLS.iter().copied().collect());

    for contract in contracts {
        let name = contract["name"].as_str().unwrap().strip_prefix("mcp/monday/").unwrap();
        if !SUPPORTED_TOOLS.contains(&name) {
            assert_eq!(
                contract["requires"]["attention"].as_array().unwrap(),
                &[toml::Value::String("blocked".into())],
                "unsupported tool lacks an exact terminal block: {name}"
            );
        }
    }
}
