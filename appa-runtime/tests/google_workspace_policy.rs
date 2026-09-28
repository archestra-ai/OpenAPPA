//! Google Workspace battery: viewer-only untrusted Drive reads, trusted writes, reviewed sharing.
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
        tool: format!("mcp/claude_ai_Google_Drive/{tool}"),
        arguments: raw(args),
        cwd: None,
    }
}

/// A loopback audience source answering every collection with one fixed roster: the
/// battery labels reads `self` and bounds writes by `internal`, which both need a source.
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
    let target = dir.path().join("marketplace/batteries/google-workspace");
    std::fs::create_dir_all(&target).unwrap();
    for file in ["appa.toml", "audience-source.py"] {
        std::fs::copy(
            repo_root().join("marketplace/batteries/google-workspace").join(file),
            target.join(file),
        )
        .unwrap();
    }
    let path = dir.path().join("appa.toml");
    let source = members_source().await;
    std::fs::write(
        &path,
        format!(
            r#"include = ["marketplace/batteries/google-workspace/appa.toml"]

[policy]
version = 2

[policy.audience]
self = ["people:viewer"]
internal = ["people:members"]

[[policy.authority]]
name = "google-workspace-operator"
permits = {{ trust_below = "trusted", attention = ["google-workspace-review"] }}

[externals]
timeout_ms = 30000
review_timeout_ms = 600000
max_body_bytes = 1048576

[externals.authorities.google-workspace-operator]
builtin = "approve"

[externals.audience.people]
url = "{source}"
selectors = [
  {{ template = "viewer", feeds = "self" }},
  {{ template = "members", feeds = "internal" }},
]
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

/// A file read narrows the trajectory to the viewer and lowers its trust, so nothing built
/// from it is written back to Drive or shared.
#[tokio::test]
async fn a_file_read_keeps_its_content_with_the_viewer() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;

    let read = call("read_file_content", serde_json::json!({ "fileId": "1AbC" }));
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

    for write in [
        call("create_file", serde_json::json!({ "title": "Summary", "content": "..." })),
        call("update_file", serde_json::json!({ "fileId": "2DeF", "content": "..." })),
        call(
            "share_file",
            serde_json::json!({ "fileId": "1AbC", "emailAddress": "bob@partner.example", "role": "reader" }),
        ),
    ] {
        assert!(
            !matches!(propose(&runtime, write.clone()).await, HookDecision::AllowCall { .. }),
            "{}",
            write.tool
        );
    }
}

/// Trusted writes run outright and record `google-workspace.changed`; sharing waits for the
/// reviewer and records `google-workspace.sensitive`.
#[tokio::test]
async fn writes_record_their_effects_and_sharing_waits_for_review() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;

    for write in [
        call("create_file", serde_json::json!({ "title": "Plan", "content": "..." })),
        call("trash_file", serde_json::json!({ "fileId": "3GhI" })),
    ] {
        assert_eq!(
            propose(&runtime, write.clone()).await,
            HookDecision::AllowCall { spawn: None },
            "{}",
            write.tool
        );
        ran(&runtime, write).await;
    }

    let share = call(
        "share_file",
        serde_json::json!({ "fileId": "1AbC", "emailAddress": "bob@partner.example", "role": "reader" }),
    );
    let decision = propose(&runtime, share.clone()).await;
    assert!(!matches!(decision, HookDecision::AllowCall { .. }));
    assert!(matches!(
        runtime.execute_remedy(&actor(), offer_of(&decision)).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, share.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, share).await;

    let effects: Vec<_> = runtime
        .audit(&root())
        .unwrap()
        .into_iter()
        .filter_map(|entry| match entry.event {
            AuditEvent::Released { tool, effects, .. } if tool.starts_with("mcp/claude_ai_Google_Drive/") => {
                Some(effects)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        effects,
        vec![
            vec!["google-workspace.changed"],
            vec!["google-workspace.changed"],
            vec!["google-workspace.sensitive"],
        ]
    );
}
