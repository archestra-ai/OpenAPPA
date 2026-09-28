//! Grain battery: meeting content is suspicious, and what is read back as meeting content
//! can still be written from it.
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
        tool: format!("mcp/claude_ai_Grain/{tool}"),
        arguments: raw(args),
        cwd: None,
    }
}

/// Runs `call`, accepting the label change it offers when it first changes the trajectory.
async fn accept_and_run(runtime: &Arc<Runtime>, call: ProposedCall) {
    let decision = propose(runtime, call.clone()).await;
    if !matches!(decision, HookDecision::AllowCall { .. }) {
        assert!(matches!(
            runtime.execute_remedy(&actor(), offer_of(&decision)).await,
            RemedyOutcome::Authorized { .. }
        ));
        assert_eq!(
            propose(runtime, call.clone()).await,
            HookDecision::AllowCall { spawn: None },
            "{}",
            call.tool
        );
    }
    ran(runtime, call).await;
}

async fn runtime(dir: &tempfile::TempDir) -> Arc<Runtime> {
    let router = Router::new().route(
        "/audience",
        post(|_body: String| async move {
            serde_json::json!({ "version": 1, "answer": { "members": ["alice@corp.example"] } }).to_string()
        }),
    );
    let source = format!("{}/audience", serve(router).await);
    let target = dir.path().join("marketplace/batteries/grain");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::copy(
        repo_root().join("marketplace/batteries/grain/appa.toml"),
        target.join("appa.toml"),
    )
    .unwrap();
    let path = dir.path().join("appa.toml");
    std::fs::write(
        &path,
        format!(
            r#"include = ["marketplace/batteries/grain/appa.toml"]

[policy]
version = 2

[policy.audience]
internal = ["people:members"]

[externals]
timeout_ms = 30000
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

/// A transcript carries what outside participants said and lowers the trust. A clip is
/// read back as meeting content, so it can still be cut from the transcript; a smart
/// topic is read back with the settings, so it cannot.
#[tokio::test]
async fn a_transcript_is_suspicious_and_a_clip_can_still_be_cut_from_it() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;

    accept_and_run(&runtime, call("workspace_settings", serde_json::json!({}))).await;
    accept_and_run(
        &runtime,
        call("fetch_meeting_transcript", serde_json::json!({ "meeting_id": "m1" })),
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

    let clip = call(
        "create_clip",
        serde_json::json!({ "recording_id": "m1", "start_ms": 0, "end_ms": 1000 }),
    );
    assert_eq!(
        propose(&runtime, clip.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, clip).await;

    let topic = call("create_smart_topic", serde_json::json!({ "name": "pricing" }));
    assert!(!matches!(
        propose(&runtime, topic).await,
        HookDecision::AllowCall { .. }
    ));
}
