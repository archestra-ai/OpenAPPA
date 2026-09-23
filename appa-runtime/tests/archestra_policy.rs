//! Archestra battery: every share is checked against who it makes the data readable by,
//! whichever of a tool's first-match rules the call selects.
mod common;

use appa_runtime::{
    api::{RemedyOutcome, Runtime},
    config::Config,
    hooks,
};
use appa_runtime_api::{HookDecision, HookEvent, ProposedCall};
use axum::{Router, routing::post};
use common::{actor, offer_of, propose, ran, raw, repo_root, root, serve};
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
    let (rules, binding) = shipped
        .split_once("[externals.audience.archestra]")
        .expect("the battery binds its source");
    let selectors = binding
        .lines()
        .skip_while(|line| !line.starts_with("selectors"))
        .collect::<Vec<_>>()
        .join("\n");
    let source = archestra_source().await;
    let target = dir.path().join("batteries/archestra");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(
        target.join("appa.toml"),
        format!("{rules}[externals.audience.archestra]\nurl = \"{source}\"\n{selectors}\n"),
    )
    .unwrap();
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
    let runtime = Arc::new(Runtime::open(Config::load(&path).unwrap(), dir.path().join("runtime.db"), None).unwrap());
    assert_eq!(
        hooks::handle(&runtime, HookEvent::SessionStart { root: root() }).await,
        HookDecision::Ack
    );
    runtime
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
