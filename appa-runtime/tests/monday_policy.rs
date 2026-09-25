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
        cwd: None,
    }
}

async fn runtime(dir: &tempfile::TempDir, expand_audience: bool) -> Arc<Runtime> {
    runtime_with_monday_source(dir, expand_audience, None).await
}

async fn runtime_with_monday_source(
    dir: &tempfile::TempDir,
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

[[policy.tool]]
name = "mcp/probe/public"
requires = {{ audience = {{ contains = ["public"] }} }}

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
"#
        ),
    )
    .unwrap();
    let runtime = Arc::new(Runtime::open(Config::load(&path).unwrap(), dir.path().join("runtime.db"), None).unwrap());
    assert_eq!(
        hooks::handle(
            &runtime,
            HookEvent::SessionStart {
                root: root(),
                principal: None
            }
        )
        .await,
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
async fn account_reads_keep_trust_and_meeting_content_lowers_it() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir, false).await;
    accept_read(&runtime, call("get_graphql_schema", serde_json::json!({}))).await;
    accept_read(
        &runtime,
        call(
            "get_automation_statistics",
            serde_json::json!({ "breakdown": "by_entity", "accountWide": true, "runStatus": "success" }),
        ),
    )
    .await;
    accept_read(
        &runtime,
        call("get_meetings_content", serde_json::json!({ "meetingIds": ["1"] })),
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
    assert_eq!(trusts, ["trusted", "trusted", "suspicious"]);
}

#[tokio::test]
async fn reviewed_writes_keep_trust_and_record_their_effects() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir, false).await;
    review_write(
        &runtime,
        call("create_board", serde_json::json!({ "boardName": "Plan" })),
    )
    .await;
    review_write(
        &runtime,
        call("move_object", serde_json::json!({ "objectType": "Folder", "id": "1" })),
    )
    .await;
    review_write(
        &runtime,
        call("move_object", serde_json::json!({ "objectType": "Board", "id": "1" })),
    )
    .await;
    review_write(
        &runtime,
        call(
            "create_item",
            serde_json::json!({ "boardId": 1, "name": "Plan", "columnValues": "{}" }),
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
    assert_eq!(trusts, ["trusted"; 4]);
    assert_eq!(
        effects(&runtime),
        vec![
            vec!["monday.sensitive"],
            vec!["monday.sensitive"],
            vec!["monday.sensitive"],
            vec!["monday.changed"],
        ]
    );
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
    let runtime = runtime_with_monday_source(&dir, false, Some(&source)).await;
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
    let runtime = runtime(&dir, false).await;
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
    let runtime = runtime(&dir, false).await;
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
        let without_expansion = runtime(&dir, false).await;
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
        let with_expansion = runtime(&dir, true).await;
        review_write(&with_expansion, call(name, args)).await;
    }
}

#[tokio::test]
async fn external_agent_credentials_stay_with_the_viewer() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir, false).await;
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
