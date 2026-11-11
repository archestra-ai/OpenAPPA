//! Archestra battery: what each tool adds to the session and what it requires, and which of
//! a sharing tool's first-match rules a call selects.
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
/// The root adds one read per label a probe needs and one sink per requirement.
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
trust_chain = ["suspicious", "trusted"]

[policy.audience]
internal = ["archestra:members"]

[[policy.tool]]
name = "read_note"
delta = { audience = ["@archestra:team/t1"] }

[[policy.tool]]
name = "read_web"
delta = { trust = "suspicious" }

[[policy.tool]]
name = "publish"
delta = {}
requires = { audience = { contains = ["public"] } }

[[policy.tool]]
name = "act"
delta = {}
requires = { trust = "trusted" }

[externals]
timeout_ms = 30000
max_body_bytes = 1048576
"#,
    )
    .unwrap();
    session_runtime(dir.path(), &path).await
}

/// `read` first narrows the session to its label, then runs.
async fn narrowed_by(runtime: &Arc<Runtime>, read: ProposedCall) {
    let offer = offer_of(&propose(runtime, read.clone()).await);
    assert!(matches!(
        runtime.execute_remedy(&actor(), offer).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(runtime, read.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(runtime, read).await;
}

/// The session read a note only team `t1` (alice) may see.
async fn narrowed_to_team_t1(runtime: &Arc<Runtime>) {
    narrowed_by(runtime, call("read_note", serde_json::json!({}))).await;
}

/// Whether `call` runs, after taking every remedy its first denial offers.
async fn runs_after_remedies(runtime: &Arc<Runtime>, call: ProposedCall) -> bool {
    let decision = match propose(runtime, call.clone()).await {
        HookDecision::DenyCall { feedback, .. } => {
            for offer in offers(&feedback) {
                runtime.execute_remedy(&actor(), offer).await;
            }
            propose(runtime, call.clone()).await
        }
        decision => decision,
    };
    let runs = matches!(decision, HookDecision::AllowCall { .. });
    if runs {
        ran(runtime, call).await;
    }
    runs
}

/// What a call's result adds to a fresh session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Adds {
    Nothing,
    Internal,
    InternalSuspicious,
}

/// What a call needs of the session before it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Needs {
    Nothing,
    Trusted,
    Internal,
    TrustedInternal,
}

/// The contract a composed policy gives `call`, read from how sessions treat it: a fresh
/// one, one that read suspicious data, and one narrowed to a single team.
async fn contract(call: &ProposedCall) -> (Adds, Needs) {
    let dir = tempfile::tempdir().unwrap();
    let fresh = runtime(&dir).await;
    let adds = match propose(&fresh, call.clone()).await {
        HookDecision::AllowCall { .. } => {
            ran(&fresh, call.clone()).await;
            Adds::Nothing
        }
        _ => {
            narrowed_by(&fresh, call.clone()).await;
            let public = runs_after_remedies(&fresh, self::call("publish", serde_json::json!({}))).await;
            let trusted = runs_after_remedies(&fresh, self::call("act", serde_json::json!({}))).await;
            match (public, trusted) {
                (false, true) => Adds::Internal,
                (false, false) => Adds::InternalSuspicious,
                other => panic!("{}: unexpected narrowing {other:?}", call.tool),
            }
        }
    };

    let dir = tempfile::tempdir().unwrap();
    let suspicious = runtime(&dir).await;
    narrowed_by(&suspicious, self::call("read_web", serde_json::json!({}))).await;
    let trusted = !runs_after_remedies(&suspicious, call.clone()).await;

    let dir = tempfile::tempdir().unwrap();
    let team = runtime(&dir).await;
    narrowed_to_team_t1(&team).await;
    let internal = !runs_after_remedies(&team, call.clone()).await;

    let needs = match (trusted, internal) {
        (false, false) => Needs::Nothing,
        (true, false) => Needs::Trusted,
        (false, true) => Needs::Internal,
        (true, true) => Needs::TrustedInternal,
    };
    (adds, needs)
}

const RUN: &str = "00000000-0000-4000-8000-000000000001";

/// Representative calls of each class, with the arguments they send. `initialGrantPrincipals`
/// and `transferModes` are the string arrays the Archestra proxy derives from `initialGrants`
/// and `environment`.
struct Class {
    adds: Adds,
    needs: Needs,
    /// Each call's tool and the arguments it sends.
    calls: &'static [(&'static str, &'static str)],
}

const TABLE: &[Class] = &[
    // The session's own surfaces, and writes that carry no session data.
    Class {
        adds: Adds::Nothing,
        needs: Needs::Nothing,
        calls: &[
            ("search_tools", r#"{"query": "github"}"#),
            ("ask_user", r#"{"question": "Apply?"}"#),
            ("validate_guardrails_policy", r#"{"content": ""}"#),
            ("reload_mcp_server_tools", r#"{"id": "s"}"#),
            ("cancel_agent_run", r#"{"run_id": "r"}"#),
            ("cancel_run", r#"{"run_id": "r"}"#),
            ("scaffold_app", r#"{"name": "a", "initialGrantPrincipals": []}"#),
        ],
    },
    // The organization's records, written by its own members and agents.
    Class {
        adds: Adds::Internal,
        needs: Needs::Nothing,
        calls: &[
            ("get_openappa_yell", r#"{"id": "y"}"#),
            ("list_openappa_consults", r#"{"sessionId": "s"}"#),
            ("preview_guardrails_policy_change", r#"{"edits": []}"#),
            ("read_file", r#"{"id": "f"}"#),
            ("get_agent", r#"{"id": "a"}"#),
            ("get_agent_run", r#"{"run_id": "r"}"#),
            ("get_run", r#"{"run_id": "r"}"#),
            ("get_mcp_servers", "{}"),
            ("get_mcp_server_tools", r#"{"mcpServerId": "s"}"#),
            ("inspect_guardrails_server", r#"{"id": "s"}"#),
            ("get_mcp_server_logs", r#"{"id": "s"}"#),
            ("list_llm_sessions", r#"{"limit": 10}"#),
        ],
    },
    // Text strangers can write.
    Class {
        adds: Adds::InternalSuspicious,
        needs: Needs::Nothing,
        calls: &[
            ("get_schedule_trigger_run_transcript", r#"{"run_id": "r"}"#),
            ("query_knowledge_sources", r#"{"query": "invoices"}"#),
            ("list_llm_interactions", r#"{"limit": 10}"#),
            ("get_llm_interaction", r#"{"interaction_id": "i"}"#),
            ("get_llm_session_logs", r#"{"session_id": "s"}"#),
        ],
    },
    // Guardrails, destructive and control-plane writes.
    Class {
        adds: Adds::Nothing,
        needs: Needs::Trusted,
        calls: &[
            ("update_guardrails_policy", r#"{"edits": []}"#),
            ("connect_guardrails_repository", r#"{"repository": "acme/policy"}"#),
            ("sync_guardrails_repository", "{}"),
            (
                "bind_guardrails_credential",
                r#"{"variable": "APPA_PROVIDER_X", "key": "k"}"#,
            ),
            ("delete_workspace", r#"{"run_id": "r"}"#),
            ("delete_runtime_credential", r#"{"id": "c"}"#),
            ("delete_plugin", r#"{"id": "p"}"#),
            ("unassign_knowledge_base_from_agent", r#"{"agent_id": "a"}"#),
            ("create_mcp_server", r#"{"name": "s"}"#),
            ("create_agent", r#"{"name": "a"}"#),
            ("create_agent", r#"{"name": "a", "initialGrantPrincipals": []}"#),
            ("create_plugin", r#"{"name": "a"}"#),
            ("create_knowledge_base", r#"{"name": "k"}"#),
        ],
    },
    // Writes other members read or run, writes a later read returns as trusted, creates that
    // share with someone, and credential transfers that carry a value or send no modes.
    Class {
        adds: Adds::Nothing,
        needs: Needs::TrustedInternal,
        calls: &[
            ("post_agent_run_message", r#"{"run_id": "r", "text": "done"}"#),
            ("edit_agent", r#"{"id": "a", "systemPrompt": "p"}"#),
            ("create_schedule_trigger", r#"{"message_template": "m"}"#),
            ("publish_app", r#"{"appId": "a", "scope": "org"}"#),
            ("post_run_message", r#"{"run_id": "r", "text": "done"}"#),
            ("create_agent", ORGANIZATION),
            (
                "create_plugin",
                r#"{"name": "a", "initialGrantPrincipals": ["team:t1"]}"#,
            ),
            ("create_knowledge_base", ORGANIZATION),
            ("create_mcp_gateway", ORGANIZATION),
            ("create_mcp_server", ORGANIZATION),
            ("scaffold_app", ORGANIZATION),
            ("create_knowledge_connector", r#"{"name": "c"}"#),
            ("update_knowledge_base", r#"{"id": "k", "name": "n"}"#),
            ("refine_app", r#"{"appId": "a", "spec": {}}"#),
            ("save_file", r#"{"filename": "n.md", "content": "n"}"#),
            ("edit_file", r#"{"id": "f", "content": "n"}"#),
            ("copy_file", r#"{"id": "f"}"#),
            ("download_file", r#"{"path": "out.csv"}"#),
            (
                "transfer_credential",
                r#"{"agent_id": "a", "transferModes": ["value"]}"#,
            ),
            ("transfer_credential", r#"{"agent_id": "a", "transferModes": []}"#),
            ("transfer_credential", r#"{"agent_id": "a"}"#),
        ],
    },
    // Writes into a run, and credential declarations an agent's readers see.
    Class {
        adds: Adds::Nothing,
        needs: Needs::Internal,
        calls: &[
            ("start_agent_run", r#"{"agent_id": "a", "message": "m"}"#),
            ("start_run", r#"{"agent_id": "a", "message": "m"}"#),
            ("transfer_credential", r#"{"agent_id": "a", "transferModes": ["link"]}"#),
        ],
    },
];

/// A create that shares its resource with the whole organization.
const ORGANIZATION: &str = r#"{"name": "a", "initialGrantPrincipals": ["organization"]}"#;

#[tokio::test]
async fn each_tool_adds_and_requires_what_its_class_declares() {
    for &Class { adds, needs, calls } in TABLE {
        for &(tool, args) in calls {
            let call = archestra(tool, serde_json::from_str(args).unwrap());
            assert_eq!(contract(&call).await, (adds, needs), "{tool} {args}");
        }
    }
}

#[tokio::test]
async fn a_share_runs_only_when_everyone_it_reaches_may_already_read() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;
    narrowed_to_team_t1(&runtime).await;

    let allowed = [
        archestra(
            "publish_app",
            serde_json::json!({ "appId": "a", "scope": "team", "teams": ["t1"] }),
        ),
        archestra(
            "update_knowledge_connector",
            serde_json::json!({ "id": "c", "visibility": "team-scoped", "team_ids": ["t1"] }),
        ),
        archestra(
            "update_knowledge_connector",
            serde_json::json!({ "id": "c", "team_ids": ["t1"] }),
        ),
        archestra(
            "update_knowledge_connector",
            serde_json::json!({ "id": "c", "name": "renamed" }),
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
        archestra("publish_app", serde_json::json!({ "appId": "a", "scope": "org" })),
        archestra(
            "publish_app",
            serde_json::json!({ "appId": "a", "scope": "team", "teams": ["t1", "t2"] }),
        ),
        archestra(
            "update_knowledge_connector",
            serde_json::json!({ "id": "c", "visibility": "org-wide" }),
        ),
        archestra(
            "update_knowledge_connector",
            serde_json::json!({ "id": "c", "visibility": "auto-sync-permissions" }),
        ),
        // A list sent without a visibility replaces who the connector is shared with.
        archestra(
            "update_knowledge_connector",
            serde_json::json!({ "id": "c", "team_ids": ["t2"] }),
        ),
        // A malformed list is refused, never read as no list.
        archestra(
            "update_knowledge_connector",
            serde_json::json!({ "id": "c", "team_ids": ["t1", 1] }),
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
        assert!(!runs_after_remedies(&team, write).await, "{tool}");
    }

    let dir = tempfile::tempdir().unwrap();
    let organization = runtime(&dir).await;
    narrowed_by(
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
        narrowed_by(&runtime, read).await;
        assert_eq!(
            propose(
                &runtime,
                archestra("publish_app", serde_json::json!({ "appId": "a", "scope": "org" })),
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

/// A persistent file reads back as trusted, so a session that read text strangers wrote
/// cannot save one.
#[tokio::test]
async fn a_suspicious_session_cannot_save_a_file() {
    let save = archestra("save_file", serde_json::json!({ "filename": "n.md", "content": "n" }));
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;
    narrowed_by(
        &runtime,
        archestra("query_knowledge_sources", serde_json::json!({ "query": "invoices" })),
    )
    .await;
    assert!(!runs_after_remedies(&runtime, save).await);
}
