//! A return declared ahead is recorded in the family's log, so every runtime process serving
//! the store sees it: a host with several replicas.
mod common;
use common::{claude_event, claude_hook, including_session_tool_rules, repo_root};

use std::path::Path;

use appa_runtime::api::{
    EmbeddedPresentationOptions, ExecuteRemedyPlanArgs, LabelArgs, RemedyAttempt, RemedyOutcome, Runtime,
};
use appa_runtime::config::Config;
use appa_runtime::hooks;
use appa_runtime_api::{Actor, HookDecision, HookEvent};

fn spawn_event() -> serde_json::Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hooks-sync.jsonl");
    std::fs::read_to_string(path)
        .expect("fixture readable")
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("json"))
        .find(|e| e["hook_event_name"] == "PreToolUse" && e["tool_name"] == "Agent" && e.get("agent_id").is_none())
        .expect("fixture carries the parent's Agent PreToolUse")
}

fn replica(dir: &Path) -> Runtime {
    let example = std::fs::read_to_string(repo_root().join("marketplace/plugins/claude-code/default.appa.toml"))
        .expect("shipped example readable");
    let (policy, externals) = example.split_once("[externals]").expect("externals table");
    let text = format!(
        "{policy}[[policy.tool]]\nname = \"host/claude-code/Read\"\ndelta = {{}}\n\
         [[policy.tool]]\nname = \"host/claude-code/Bash\"\ndelta = {{}}\n[externals]{externals}"
    );
    let path = dir.join("appa.toml");
    std::fs::write(&path, including_session_tool_rules(dir, &text)).expect("writes");
    let config = Config::load(&path).expect("loads");
    Runtime::open(config, dir.join("appa.db"), None).expect("opens")
}

fn parsed(event: &serde_json::Value) -> HookEvent {
    claude_event(event).expect("maps")
}

fn actor_of(event: &serde_json::Value) -> Actor {
    let HookEvent::ToolCall { actor, .. } = parsed(event) else {
        panic!("a tool call");
    };
    actor
}

#[tokio::test]
async fn a_declaration_ahead_on_one_replica_releases_the_spawn_proposed_on_another() {
    let dir = tempfile::tempdir().expect("temp dir");
    let declaring = replica(dir.path());
    let spawning = replica(dir.path());
    let spawn = spawn_event();

    let mut control = spawn.clone();
    control["tool_name"] = serde_json::json!("mcp__appa__execute_remedy_plan");
    control["tool_use_id"] = serde_json::json!("toolu_test_declare_ahead");
    control["tool_input"] = serde_json::json!({ "label": {} });
    let (status, answer) = claude_hook(&declaring, &control).await;
    assert_eq!(status, 200, "{answer}");

    let declared = declaring
        .execute_embedded_remedy_with_options(
            &actor_of(&spawn),
            ExecuteRemedyPlanArgs {
                offer_id: None,
                label: Some(LabelArgs {
                    trust: None,
                    audience: None,
                }),
                return_schema: None,
            },
            EmbeddedPresentationOptions::default(),
            Some(RemedyAttempt("remedy:toolu_test_declare_ahead".to_string())),
        )
        .await;
    assert!(matches!(declared, RemedyOutcome::Returned { .. }), "{declared:?}");

    let decision = hooks::handle(&spawning, parsed(&spawn)).await;
    assert!(
        matches!(decision, HookDecision::AllowCall { spawn: Some(_) }),
        "the spawn the store-wide session declared for releases on any replica: {decision:#?}"
    );
}
