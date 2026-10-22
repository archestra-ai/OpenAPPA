//! Archestra battery: every share is checked against who it makes the data readable by,
//! whichever of a tool's first-match rules the call selects.
mod common;

use appa_runtime::api::{RemedyOutcome, Runtime};
use appa_runtime_api::{HookDecision, ProposedCall};
use axum::{Router, routing::post};
use common::{actor, offer_of, offers, propose, ran, raw, repo_root, serve, session_runtime};
use std::sync::Arc;

fn call(tool: &str, args: serde_json::Value) -> ProposedCall {
    ProposedCall {
        tool: tool.to_string(),
        arguments: raw(args),
        cwd: None,
    }
}

fn archestra(tool: &str, args: serde_json::Value) -> ProposedCall {
    call(&format!("mcp/archestra/{tool}"), args)
}

/// A loopback source for the `archestra` collections: alice is in `t1`, bob in `t2`.
async fn archestra_source() -> String {
    let router = Router::new().route(
        "/audience",
        post(|body: String| async move {
            let request: serde_json::Value = serde_json::from_str(&body).unwrap();
            let members = match request["artifact"]["selector"].as_str().unwrap() {
                "members" => vec!["alice@corp.example", "bob@corp.example"],
                "team/t1" | "user/u-alice" => vec!["alice@corp.example"],
                "team/t2" | "user/u-bob" => vec!["bob@corp.example"],
                other => panic!("unexpected selector {other}"),
            };
            serde_json::json!({ "version": 1, "answer": { "members": members } }).to_string()
        }),
    );
    format!("{}/audience", serve(router).await)
}

/// The shipped rules, with the battery's own `command` binding swapped for the loopback
/// source: the helper has its own tests, and a root cannot rebind a battery's provider.
async fn runtime(dir: &tempfile::TempDir) -> Arc<Runtime> {
    let shipped = std::fs::read_to_string(repo_root().join("marketplace/batteries/archestra/appa.toml")).unwrap();
    let mut battery: toml_edit::DocumentMut = shipped.parse().unwrap();
    let binding = battery["externals"]["audience"]["archestra"]
        .as_table_mut()
        .expect("the battery binds its source");
    binding.remove("command");
    binding.remove("token_env");
    binding.insert("url", toml_edit::value(archestra_source().await));
    let target = dir.path().join("batteries/archestra");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("appa.toml"), battery.to_string()).unwrap();
    let path = dir.path().join("appa.toml");
    std::fs::write(
        &path,
        r#"include = ["batteries/archestra/appa.toml"]

[policy]
version = 2

[policy.audience]
internal = ["archestra:members"]

[[policy.tool]]
name = "read_note"
delta = { audience = ["@archestra:team/t1"] }

[externals]
timeout_ms = 30000
max_body_bytes = 1048576
"#,
    )
    .unwrap();
    session_runtime(dir.path(), &path).await
}

/// The session read a note only team `t1` (alice) may see.
async fn narrowed_to_team_t1(runtime: &Arc<Runtime>) {
    let note = call("read_note", serde_json::json!({}));
    let offer = offer_of(&propose(runtime, note.clone()).await);
    assert!(matches!(
        runtime.execute_remedy(&actor(), offer).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(runtime, note.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(runtime, note).await;
}

#[tokio::test]
async fn a_share_runs_only_when_everyone_it_reaches_may_already_read() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;
    narrowed_to_team_t1(&runtime).await;

    let allowed = [
        archestra("search_tools", serde_json::json!({ "query": "github" })),
        archestra("search_files", serde_json::json!({ "query": "notes" })),
        archestra("load_skill", serde_json::json!({ "name": "release" })),
        archestra(
            "yell",
            serde_json::json!({ "message": "blocked", "with_trajectory": false }),
        ),
        archestra(
            "set_project_share",
            serde_json::json!({ "visibility": "team", "team_ids": ["t1"] }),
        ),
        archestra("set_project_share", serde_json::json!({ "visibility": "none" })),
        archestra(
            "create_knowledge_base",
            serde_json::json!({ "name": "n", "visibility": "private" }),
        ),
        archestra("edit_agent", serde_json::json!({ "id": "a", "name": "renamed" })),
        archestra(
            "update_plugin",
            serde_json::json!({ "id": "p", "userIds": ["u-alice"] }),
        ),
        archestra(
            "add_team_member",
            serde_json::json!({ "team_id": "t1", "user": "u-alice" }),
        ),
    ];
    for share in allowed {
        let tool = share.tool.clone();
        assert_eq!(
            propose(&runtime, share.clone()).await,
            HookDecision::AllowCall { spawn: None },
            "{tool}"
        );
        ran(&runtime, share).await;
    }

    let denied = [
        // The organization and another team hold bob.
        archestra("set_project_share", serde_json::json!({ "visibility": "organization" })),
        archestra(
            "create_knowledge_connector",
            serde_json::json!({ "name": "c", "visibility": "auto-sync-permissions" }),
        ),
        archestra(
            "set_project_share",
            serde_json::json!({ "visibility": "team", "team_ids": ["t1", "t2"] }),
        ),
        // A new knowledge base is org-wide unless it says otherwise.
        archestra(
            "create_knowledge_base",
            serde_json::json!({ "name": "n", "teamIds": ["t1"] }),
        ),
        archestra(
            "create_knowledge_base",
            serde_json::json!({ "name": "n", "visibility": "private", "teamIds": ["t2"] }),
        ),
        // A list sent without a scope replaces who the resource is shared with.
        archestra("edit_agent", serde_json::json!({ "id": "a", "teams": ["t2"] })),
        archestra(
            "update_knowledge_base",
            serde_json::json!({ "id": "k", "teamIds": ["t2"] }),
        ),
        archestra("update_plugin", serde_json::json!({ "id": "p", "userIds": ["u-bob"] })),
        archestra(
            "update_plugin",
            serde_json::json!({ "id": "p", "teamIds": ["t1"], "userIds": ["u-alice"] }),
        ),
        // A malformed list is refused, never read as no list.
        archestra(
            "update_knowledge_base",
            serde_json::json!({ "id": "k", "teamIds": ["t1", 1] }),
        ),
        archestra(
            "add_team_member",
            serde_json::json!({ "team_id": "t2", "user": "u-bob" }),
        ),
    ];
    for share in denied {
        let tool = share.tool.clone();
        assert!(
            !matches!(propose(&runtime, share).await, HookDecision::AllowCall { .. }),
            "{tool}"
        );
    }
}

const RUN: &str = "00000000-0000-4000-8000-000000000001";

/// Each call that puts session data into a run or its workspace.
fn into_a_run() -> [ProposedCall; 5] {
    [
        archestra(
            "start_agent_run",
            serde_json::json!({ "agent_id": "a", "message": "summarize the notes" }),
        ),
        archestra(
            "steer_agent_run",
            serde_json::json!({ "run_id": RUN, "message": "also list owners" }),
        ),
        archestra(
            "write_workspace_file",
            serde_json::json!({ "task_id": RUN, "path": "notes.md", "content": "notes" }),
        ),
        archestra(
            "transfer_workspace_file",
            serde_json::json!({
                "task_id": RUN, "direction": "upload", "path": "notes.md",
                "local_path": "/tmp/notes.md", "size": 5, "sha256": "0".repeat(64),
            }),
        ),
        archestra(
            "transfer_workspace_file",
            serde_json::json!({ "task_id": RUN, "path": "notes.md", "local_path": "/tmp/notes.md" }),
        ),
    ]
}

/// Each call that brings a run's or its workspace's data into the session.
fn out_of_a_run() -> [ProposedCall; 5] {
    [
        archestra("get_agent_run", serde_json::json!({ "run_id": RUN })),
        archestra(
            "read_workspace_file",
            serde_json::json!({ "task_id": RUN, "path": "notes.md" }),
        ),
        archestra(
            "transfer_workspace_file",
            serde_json::json!({
                "task_id": RUN, "direction": "download", "path": "notes.md", "local_path": "/tmp/notes.md",
            }),
        ),
        archestra(
            "transfer_workspace_file",
            serde_json::json!({ "task_id": RUN, "path": "notes.md", "local_path": "/tmp/notes.md" }),
        ),
        archestra("list_agent_runs", serde_json::json!({ "agent_ids": ["a"] })),
    ]
}

/// In a fresh session, `call` first narrows the session to the organization, then runs.
async fn narrows_to_internal(runtime: &Arc<Runtime>, call: ProposedCall) {
    let tool = call.tool.clone();
    let offer = offer_of(&propose(runtime, call.clone()).await);
    assert!(
        matches!(
            runtime.execute_remedy(&actor(), offer).await,
            RemedyOutcome::Authorized { .. }
        ),
        "{tool}"
    );
    assert_eq!(
        propose(runtime, call.clone()).await,
        HookDecision::AllowCall { spawn: None },
        "{tool}"
    );
    ran(runtime, call).await;
}

/// A run and its workspace are one container the organization may read: data a team
/// narrower than the organization may see cannot enter it.
#[tokio::test]
async fn session_data_enters_a_run_only_when_the_organization_may_read_it() {
    let dir = tempfile::tempdir().unwrap();
    let team = runtime(&dir).await;
    narrowed_to_team_t1(&team).await;
    // Not even after taking every remedy offered, such as narrowing to what is read back.
    for write in into_a_run() {
        let tool = write.tool.clone();
        let decision = propose(&team, write.clone()).await;
        let HookDecision::DenyCall { feedback, .. } = &decision else {
            panic!("{tool}: {decision:?}")
        };
        for offer in offers(feedback) {
            team.execute_remedy(&actor(), offer).await;
        }
        assert!(
            !matches!(propose(&team, write).await, HookDecision::AllowCall { .. }),
            "{tool}"
        );
    }
    let cancel = archestra("cancel_agent_run", serde_json::json!({ "run_id": RUN }));
    assert_eq!(
        propose(&team, cancel.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&team, cancel).await;

    let dir = tempfile::tempdir().unwrap();
    let organization = runtime(&dir).await;
    narrows_to_internal(
        &organization,
        archestra("list_agent_runs", serde_json::json!({ "agent_ids": ["a"] })),
    )
    .await;
    for write in into_a_run() {
        let tool = write.tool.clone();
        assert_eq!(
            propose(&organization, write.clone()).await,
            HookDecision::AllowCall { spawn: None },
            "{tool}"
        );
        ran(&organization, write).await;
    }
}

/// What leaves a run narrows the session to the organization and keeps it trusted, so an
/// org-wide share still runs.
#[tokio::test]
async fn reading_a_run_narrows_the_session_to_internal_and_keeps_trust() {
    for read in out_of_a_run() {
        let tool = read.tool.clone();
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(&dir).await;
        narrows_to_internal(&runtime, read).await;
        assert_eq!(
            propose(
                &runtime,
                archestra("set_project_share", serde_json::json!({ "visibility": "organization" })),
            )
            .await,
            HookDecision::AllowCall { spawn: None },
            "{tool}"
        );
    }
}

/// `direction` selects the transfer's rule: a download only reads, an upload only writes.
#[tokio::test]
async fn a_transfer_direction_selects_its_rule() {
    let transfer = |direction: &str| {
        archestra(
            "transfer_workspace_file",
            serde_json::json!({
                "task_id": RUN, "direction": direction, "path": "notes.md",
                "local_path": "/tmp/notes.md", "size": 5, "sha256": "0".repeat(64),
            }),
        )
    };

    // A download requires nothing, so a team-only session runs it once narrowed.
    let dir = tempfile::tempdir().unwrap();
    let team = runtime(&dir).await;
    narrowed_to_team_t1(&team).await;
    let download = transfer("download");
    let offer = offer_of(&propose(&team, download.clone()).await);
    assert!(matches!(
        team.execute_remedy(&actor(), offer).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(propose(&team, download).await, HookDecision::AllowCall { spawn: None });

    // An upload adds no label, so a fresh session runs it without narrowing.
    let dir = tempfile::tempdir().unwrap();
    let fresh = runtime(&dir).await;
    assert_eq!(
        propose(&fresh, transfer("upload")).await,
        HookDecision::AllowCall { spawn: None }
    );
}
