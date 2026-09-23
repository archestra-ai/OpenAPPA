//! monday battery: internal reads and reviewed writes, like Linear and PostHog.
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
        tool: format!("mcp/monday/{tool}"),
        arguments: raw(args),
    }
}

async fn runtime(dir: &tempfile::TempDir, wildcard: bool, expand_audience: bool) -> Arc<Runtime> {
    runtime_with_monday_source(dir, wildcard, expand_audience, None).await
}

async fn runtime_with_monday_source(
    dir: &tempfile::TempDir,
    wildcard: bool,
    expand_audience: bool,
    monday_source: Option<&str>,
) -> Arc<Runtime> {
    let router = Router::new().route(
        "/audience",
        post(|body: String| async move {
            let request: serde_json::Value = serde_json::from_str(&body).unwrap();
            let members = if request["artifact"]["selector"] == "viewer" {
                vec!["alice@corp.example"]
            } else {
                vec!["alice@corp.example", "bob@corp.example"]
            };
            serde_json::json!({ "version": 1, "answer": { "members": members } }).to_string()
        }),
    );
    let source = serve(router).await;
    let fallback = if wildcard {
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
        let url = serve(router).await;
        format!(
            r#"
[[policy.annotator]]
name = "gatekeeper"
marks = ["signoff"]
[[policy.tool]]
name = "*"
annotator = "gatekeeper"
[externals.annotators.gatekeeper]
url = "{url}"
"#
        )
    } else {
        String::new()
    };
    let battery = "marketplace/batteries/monday/appa.toml";
    let target = dir.path().join(battery);
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    let policy = std::fs::read_to_string(repo_root().join(battery)).unwrap();
    let policy = if let Some(url) = monday_source {
        policy.replace(
            "command = [\"python3\", \"audience-source.py\"]\ntoken_env = \"APPA_PROVIDER_MONDAY_TOKEN\"",
            &format!("url = \"{url}\""),
        )
    } else {
        policy
    };
    std::fs::write(target, policy).unwrap();
    let expansion = if expand_audience {
        "audience_missing = [\"public\"]"
    } else {
        ""
    };
    let path = dir.path().join("appa.toml");
    std::fs::write(
        &path,
        format!(
            r#"include = [{battery:?}]
[policy]
version = 2
[policy.audience]
self = ["people:viewer"]
internal = ["people:members"]

# Read-only probes make both output dimensions independently observable.
[[policy.tool]]
name = "mcp/probe/public"
requires = {{ audience = {{ contains = ["public"] }} }}
[[policy.tool]]
name = "mcp/probe/trusted"
requires = {{ trust = "trusted" }}
[[policy.tool]]
name = "mcp/probe/trusted-internal"
delta = {{ audience = ["internal"] }}

[[policy.authority]]
name = "monday-operator"
[policy.authority.permits]
trust_below = "trusted"
attention = ["*"]
{expansion}

[externals]
timeout_ms = 30000
review_timeout_ms = 600000
max_body_bytes = 1048576
[externals.authorities.monday-operator]
builtin = "approve"
[externals.audience.people]
url = "{source}/audience"
selectors = [{{ template = "viewer", feeds = "self" }}, {{ template = "members", feeds = "internal" }}]
{fallback}
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

async fn accept_read(runtime: &Arc<Runtime>, read: ProposedCall) {
    let decision = propose(runtime, read.clone()).await;
    if !matches!(decision, HookDecision::AllowCall { .. }) {
        assert!(matches!(
            runtime.execute_remedy(&actor(), offer_of(&decision)).await,
            RemedyOutcome::Authorized { .. }
        ));
        assert_eq!(
            propose(runtime, read.clone()).await,
            HookDecision::AllowCall { spawn: None }
        );
    }
    ran(runtime, read).await;
}

async fn review_write(runtime: &Arc<Runtime>, write: ProposedCall) {
    let decision = propose(runtime, write.clone()).await;
    assert!(
        matches!(&decision, HookDecision::DenyCall { feedback, .. } if feedback.contains("monday-review")),
        "{decision:?}"
    );
    assert!(matches!(
        runtime.execute_remedy(&actor(), offer_of(&decision)).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(runtime, write.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(runtime, write).await;
}

fn effects(runtime: &Runtime) -> Vec<Vec<String>> {
    runtime
        .audit(&root())
        .unwrap()
        .into_iter()
        .filter_map(|entry| match entry.event {
            AuditEvent::Released { tool, effects, .. } if tool.starts_with("mcp/monday/") && !effects.is_empty() => {
                Some(effects)
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn ordinary_reads_keep_provider_options_and_classify_results() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir, false, false).await;
    for read in [
        call("get_user_context", serde_json::json!({})),
        call("get_assigned_items", serde_json::json!({ "limit": 1 })),
        call("get_user_mentions", serde_json::json!({ "limit": 1 })),
        call("get_user_recent_activity", serde_json::json!({ "limit": 1 })),
        call(
            "get_board_info",
            serde_json::json!({ "boardId": 1, "filters": { "columns": { "only": true } } }),
        ),
        call(
            "get_board_items_page",
            serde_json::json!({ "boardId": 1, "searchTerm": "plan", "includeItemDescription": true, "includeSubItems": true, "orderBy": [{ "columnId": "name", "direction": "asc" }] }),
        ),
        call(
            "get_updates",
            serde_json::json!({ "objectId": "1", "objectType": "Board", "includeReplies": true, "includeAssets": true, "includeItemUpdates": true }),
        ),
        call(
            "search",
            serde_json::json!({ "searchTerm": "plan", "searchType": "ITEMS" }),
        ),
        call("workspace_info", serde_json::json!({ "workspace_id": 1 })),
        call("list_users_and_teams", serde_json::json!({ "getMe": true })),
        call(
            "read_docs",
            serde_json::json!({ "type": "ids", "ids": ["1"], "include_comments": true }),
        ),
        call(
            "all_api_read",
            serde_json::json!({ "query": "query { boards { id } }", "variables": "{}" }),
        ),
    ] {
        accept_read(&runtime, read).await;
    }
    assert!(effects(&runtime).is_empty());
    let labels: Vec<_> = runtime
        .audit(&root())
        .unwrap()
        .into_iter()
        .filter_map(|entry| match entry.event {
            AuditEvent::Admitted { label } => Some(label),
            _ => None,
        })
        .collect();
    assert!(!labels.is_empty());
    for label in labels {
        assert_eq!(label.trust, "suspicious");
        assert_eq!(label.audience, "internal");
    }
    let public = ProposedCall {
        tool: "mcp/probe/public".into(),
        arguments: raw(serde_json::json!({})),
        cwd: None,
    };
    assert!(matches!(propose(&runtime, public).await, HookDecision::DenyCall { offers, .. } if offers.is_empty()));
    let trusted = ProposedCall {
        tool: "mcp/probe/trusted".into(),
        arguments: raw(serde_json::json!({})),
        cwd: None,
    };
    assert!(matches!(
        propose(&runtime, trusted).await,
        HookDecision::DenyCall { .. }
    ));
}

#[tokio::test]
async fn provider_schema_metadata_keeps_trust_until_unverified_result_is_read() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir, false, false).await;
    for read in [
        call("get_graphql_schema", serde_json::json!({ "operationType": "read" })),
        call("get_column_type_info", serde_json::json!({ "columnType": "status" })),
    ] {
        accept_read(&runtime, read).await;
    }
    let labels: Vec<_> = runtime
        .audit(&root())
        .unwrap()
        .into_iter()
        .filter_map(|entry| match entry.event {
            AuditEvent::Admitted { label } => Some(label),
            _ => None,
        })
        .collect();
    assert_eq!(labels.len(), 2);
    for label in labels {
        assert_eq!(label.trust, "trusted");
        assert_eq!(label.audience, "internal");
    }
    let trusted = ProposedCall {
        tool: "mcp/probe/trusted".into(),
        arguments: raw(serde_json::json!({})),
        cwd: None,
    };
    assert_eq!(
        propose(&runtime, trusted.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, trusted.clone()).await;
    accept_read(
        &runtime,
        call("get_type_details", serde_json::json!({ "typeName": "Board" })),
    )
    .await;
    assert!(matches!(
        propose(&runtime, trusted).await,
        HookDecision::DenyCall { .. }
    ));
}

#[tokio::test]
async fn aggregate_statistics_keep_trust_but_entity_details_lower_it() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir, false, false).await;
    accept_read(
        &runtime,
        call(
            "get_automation_statistics",
            serde_json::json!({ "breakdown": "totals", "boardId": "1" }),
        ),
    )
    .await;
    accept_read(
        &runtime,
        call(
            "get_automation_statistics",
            serde_json::json!({ "breakdown": "by_entity", "accountWide": true, "runStatus": "success" }),
        ),
    )
    .await;
    let trusts: Vec<_> = runtime
        .audit(&root())
        .unwrap()
        .into_iter()
        .filter_map(|entry| match entry.event {
            AuditEvent::Admitted { label } => Some(label.trust),
            _ => None,
        })
        .collect();
    assert_eq!(trusts, ["trusted", "suspicious"]);
}

#[tokio::test]
async fn reviewed_identifier_only_writes_keep_trust_until_a_broader_result() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir, false, false).await;
    for write in [
        call("create_form", serde_json::json!({ "destination_workspace_id": "1" })),
        call("move_object", serde_json::json!({ "objectType": "Folder", "id": "1" })),
        call(
            "get_asset_upload_url",
            serde_json::json!({ "fileName": "plan.pdf", "contentType": "application/pdf", "fileSize": 100 }),
        ),
    ] {
        review_write(&runtime, write).await;
    }
    review_write(
        &runtime,
        call("move_object", serde_json::json!({ "objectType": "Board", "id": "1" })),
    )
    .await;
    let trusts: Vec<_> = runtime
        .audit(&root())
        .unwrap()
        .into_iter()
        .filter_map(|entry| match entry.event {
            AuditEvent::Admitted { label } => Some(label.trust),
            _ => None,
        })
        .collect();
    assert_eq!(trusts, ["trusted", "trusted", "trusted", "suspicious"]);
    assert_eq!(
        effects(&runtime),
        vec![
            vec!["monday.sensitive".to_owned()],
            vec!["monday.sensitive".to_owned()],
            vec!["monday.changed".to_owned()],
            vec!["monday.sensitive".to_owned()],
        ]
    );
}

#[tokio::test]
async fn internal_reads_can_flow_to_reviewed_writes_without_audience_expansion() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir, false, false).await;
    accept_read(
        &runtime,
        call("get_board_items_page", serde_json::json!({ "boardId": 1 })),
    )
    .await;
    for write in [
        call(
            "create_item",
            serde_json::json!({ "boardId": 1, "name": "Internal plan", "columnValues": "{\"status\":\"Done\"}", "groupId": "topics", "parentItemId": 2 }),
        ),
        call(
            "create_item",
            serde_json::json!({ "boardId": 1, "name": "Copy", "columnValues": "{}", "duplicateFromItemId": 2, "createLabelsIfMissing": true }),
        ),
        call(
            "create_update",
            serde_json::json!({ "itemId": 1, "body": "Internal summary", "parentId": 2, "mentionsList": "[]" }),
        ),
        call(
            "change_item_column_values",
            serde_json::json!({ "boardId": 1, "itemId": 1, "columnValues": "{\"status\":\"Done\"}" }),
        ),
    ] {
        review_write(&runtime, write).await;
    }
    assert_eq!(effects(&runtime), vec![vec!["monday.changed".to_owned()]; 4]);
    let admitted = runtime
        .audit(&root())
        .unwrap()
        .into_iter()
        .filter_map(|entry| match entry.event {
            AuditEvent::Admitted { label } => Some(label),
            _ => None,
        })
        .next_back()
        .unwrap();
    assert_eq!(admitted.trust, "suspicious");
    assert_eq!(admitted.audience, "internal");
    // Approval of a write does not declassify its result or subsequent calls.
    assert!(
        matches!(propose(&runtime, call("get_monday_knowledge", serde_json::json!({ "query": "Internal summary", "kind": "general" }))).await, HookDecision::DenyCall { offers, .. } if offers.is_empty())
    );
}

#[tokio::test]
async fn trusted_internal_creation_needs_review_but_no_public_audience() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir, false, false).await;
    accept_read(
        &runtime,
        ProposedCall {
            tool: "mcp/probe/trusted-internal".into(),
            arguments: raw(serde_json::json!({})),
            cwd: None,
        },
    )
    .await;
    review_write(
        &runtime,
        call(
            "create_item",
            serde_json::json!({ "boardId": 1, "name": "Internal plan", "columnValues": "{}" }),
        ),
    )
    .await;
}

#[tokio::test]
async fn structural_and_opaque_operations_are_reviewable_under_a_host_wildcard() {
    for (name, args) in [
        (
            "create_board",
            serde_json::json!({ "boardName": "Plan", "boardKind": "private" }),
        ),
        (
            "move_object",
            serde_json::json!({ "objectType": "Board", "id": "1", "workspaceId": "2" }),
        ),
        (
            "all_monday_api",
            serde_json::json!({ "query": "mutation { delete_board(board_id: 1) { id } }", "variables": "{}" }),
        ),
        (
            "all_api_write",
            serde_json::json!({ "query": "mutation { delete_board(board_id: 1) { id } }", "variables": "{}" }),
        ),
        (
            "execute_code",
            serde_json::json!({ "description": "Print a test message", "code": "print('reviewed')", "language": "python" }),
        ),
        ("run_action", serde_json::json!({ "id": "1" })),
        (
            "publish_workflow",
            serde_json::json!({ "workflowObjectId": 1, "workflowDraftId": 2 }),
        ),
        (
            "vibe_publication",
            serde_json::json!({ "app_id": 1, "action": "publish" }),
        ),
        ("delete_view", serde_json::json!({ "viewId": "1", "boardId": "1" })),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(&dir, true, false).await;
        review_write(&runtime, call(name, args)).await;
        assert_eq!(effects(&runtime), vec![vec!["monday.sensitive"]]);
    }
}

#[tokio::test]
async fn notification_requires_its_recipient_to_read_the_input() {
    let router = Router::new().route(
        "/",
        post(|body: String| async move {
            let request: serde_json::Value = serde_json::from_str(&body).unwrap();
            let selector = request["artifact"]["selector"].as_str().unwrap();
            let member = match selector {
                "user/1" => "alice@corp.example",
                "user/2" => "outsider@corp.example",
                other => panic!("unexpected monday selector {other}"),
            };
            serde_json::json!({ "version": 1, "answer": { "members": [member] } }).to_string()
        }),
    );
    let source = serve(router).await;
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime_with_monday_source(&dir, false, false, Some(&source)).await;
    accept_read(&runtime, call("get_board_info", serde_json::json!({ "boardId": 1 }))).await;

    let notification = |user_id| {
        call(
            "create_notification",
            serde_json::json!({
                "user_id": user_id,
                "target_id": "1",
                "target_type": "Project",
                "text": "Internal plan",
            }),
        )
    };
    assert!(matches!(
        propose(&runtime, notification("2")).await,
        HookDecision::DenyCall { offers, .. } if offers.is_empty()
    ));
    review_write(&runtime, notification("1")).await;
    assert_eq!(effects(&runtime), vec![vec!["monday.sensitive"]]);
}

#[tokio::test]
async fn mixed_tool_read_actions_do_not_use_the_reviewed_write_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir, true, false).await;
    for (name, action) in [
        ("manage_agent", "get"),
        ("manage_agent_triggers", "list"),
        ("manage_agent_knowledge", "list"),
    ] {
        accept_read(
            &runtime,
            call(name, serde_json::json!({ "action": action, "agent_id": "1" })),
        )
        .await;
    }
    assert!(effects(&runtime).is_empty());
    for (name, args) in [
        ("manage_agent", serde_json::json!({ "action": "run", "agent_id": "1" })),
        (
            "manage_agent_triggers",
            serde_json::json!({ "action": "add", "agent_id": "1", "block_reference_id": "example-trigger" }),
        ),
        (
            "manage_agent_knowledge",
            serde_json::json!({ "action": "remove", "agent_id": "1", "resource_id": "2", "scope_type": "BOARD" }),
        ),
    ] {
        review_write(&runtime, call(name, args)).await;
    }
    assert_eq!(effects(&runtime), vec![vec!["monday.sensitive".to_owned()]; 3]);
}

#[tokio::test]
async fn public_documentation_does_not_narrow_a_fresh_public_trajectory() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir, false, false).await;
    accept_read(
        &runtime,
        call(
            "get_monday_knowledge",
            serde_json::json!({ "query": "How do boards work?", "kind": "general" }),
        ),
    )
    .await;
    let public = ProposedCall {
        tool: "mcp/probe/public".into(),
        arguments: raw(serde_json::json!({})),
        cwd: None,
    };
    assert_eq!(propose(&runtime, public).await, HookDecision::AllowCall { spawn: None });
}

#[tokio::test]
async fn external_submissions_require_explicit_audience_approval() {
    for name in ["create_form_submission", "submit_bug_or_feature_request"] {
        let dir = tempfile::tempdir().unwrap();
        let without_expansion = runtime(&dir, false, false).await;
        accept_read(
            &without_expansion,
            call("get_board_info", serde_json::json!({ "boardId": 1 })),
        )
        .await;
        let args = if name == "create_form_submission" {
            serde_json::json!({ "form_token": "example", "answers": [{ "question_id": "name", "name": "Example" }], "form_timezone_offset": 0 })
        } else {
            serde_json::json!({ "kind": "bug", "title": "Example", "description": "Example" })
        };
        assert!(
            matches!(propose(&without_expansion, call(name, args.clone())).await, HookDecision::DenyCall { offers, .. } if offers.is_empty())
        );
        let dir = tempfile::tempdir().unwrap();
        let with_expansion = runtime(&dir, false, true).await;
        review_write(&with_expansion, call(name, args)).await;
    }
}

#[tokio::test]
async fn external_agent_credentials_stay_with_the_viewer() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir, false, false).await;
    review_write(
        &runtime,
        call(
            "connect_external_agent",
            serde_json::json!({ "custom": { "name": "Example" } }),
        ),
    )
    .await;
    let admitted = runtime
        .audit(&root())
        .unwrap()
        .into_iter()
        .filter_map(|entry| match entry.event {
            AuditEvent::Admitted { label } => Some(label),
            _ => None,
        })
        .next_back()
        .unwrap();
    assert_eq!(admitted.audience, "self");
    assert!(
        matches!(propose(&runtime, call("create_item", serde_json::json!({ "boardId": 1, "name": "Secret", "columnValues": "{}" }))).await, HookDecision::DenyCall { offers, .. } if offers.is_empty())
    );
}

#[tokio::test]
async fn unknown_tools_follow_the_deployments_fallback() {
    for wildcard in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(&dir, wildcard, false).await;
        let unknown = ProposedCall {
            tool: "mcp/monday/future_unknown_tool".into(),
            arguments: raw(serde_json::json!({})),
            cwd: None,
        };
        match propose(&runtime, unknown).await {
            HookDecision::Refuse { .. } if !wildcard => {}
            HookDecision::DenyCall { offers, feedback, .. } => {
                assert_eq!(!offers.is_empty(), wildcard);
                if wildcard {
                    assert!(feedback.contains("signoff"));
                }
            }
            other => panic!("unexpected unknown-tool decision: {other:?}"),
        }
    }
}
