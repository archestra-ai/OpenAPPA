//! A subagent's return declaration through the Claude Code hooks: declared at the spawn's
//! block without review, or right before the spawn so the spawn meets no block for it.
mod common;
use common::{claude_event, claude_hook, including_session_tool_rules, offers, repo_root};

use std::path::Path;

use appa_runtime::api::{
    ExecuteRemedyPlanArgs, LabelArgs, LabelSpelling, OfferId, RemedyArguments, RemedyOutcome, Runtime,
};
use appa_runtime::config::Config;
use appa_runtime::hooks;
use appa_runtime_api::{Actor, HookDecision, HookEvent, OfferedReturn};

/// `claude -p` on Claude Code 2.1.257 starting one subagent with `Agent`.
fn events() -> Vec<serde_json::Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hooks-sync.jsonl");
    std::fs::read_to_string(path)
        .expect("the recorded hook fixture is readable")
        .lines()
        .map(|line| serde_json::from_str(line).expect("each fixture line is JSON"))
        .collect()
}

fn hook(events: &[serde_json::Value], name: &str, tool: &str, subagent: bool) -> serde_json::Value {
    events
        .iter()
        .find(|event| {
            event["hook_event_name"] == name
                && event["tool_name"] == tool
                && event.get("agent_id").is_some() == subagent
        })
        .unwrap_or_else(|| panic!("the fixture carries a {name} {tool}"))
        .clone()
}

/// The shipped root with deterministic `Read` and `Bash` tools; `bash_delta` is what the
/// recorded Bash output carries.
fn deployment(bash_delta: &str) -> Runtime {
    let example = std::fs::read_to_string(repo_root().join("marketplace/plugins/claude-code/default.appa.toml"))
        .expect("the shipped example is readable");
    let (policy, externals) = example
        .split_once("[externals]")
        .expect("the example carries an [externals] table");
    let text = format!(
        "{policy}[[policy.tool]]\nname = \"host/claude-code/Read\"\ndelta = {{}}\n\
         [[policy.tool]]\nname = \"host/claude-code/Bash\"\n{bash_delta}\n[externals]{externals}"
    );
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, including_session_tool_rules(dir.path(), &text)).expect("the deployment writes");
    let config = Config::load(&path).expect("the deployment loads");
    Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens")
}

fn parsed(event: &serde_json::Value) -> HookEvent {
    claude_event(event).expect("the recorded event maps to a hook event")
}

fn actor_of(event: &serde_json::Value) -> Actor {
    let HookEvent::ToolCall { actor, .. } = parsed(event) else {
        panic!("the event is a tool call");
    };
    actor
}

/// The spawn again under another call id and prompt: another subagent call.
fn another(spawn: &serde_json::Value) -> serde_json::Value {
    let mut spawn = spawn.clone();
    spawn["tool_use_id"] = serde_json::json!("toolu_test_second_spawn");
    spawn["tool_input"]["prompt"] = serde_json::json!("A second, different errand.");
    spawn
}

/// The model's control call declaring ahead of `spawn`, through its own PreToolUse hook: the
/// hook opens the session and vouches for the call, as it does before the tool runs.
async fn control_hook(runtime: &Runtime, spawn: &serde_json::Value, label: serde_json::Value) {
    let mut control = spawn.clone();
    control["tool_name"] = serde_json::json!("mcp__appa__execute_remedy_plan");
    control["tool_use_id"] = serde_json::json!("toolu_test_declare_ahead");
    control["tool_input"] = serde_json::json!({ "label": label });
    let (status, answer) = claude_hook(runtime, &control).await;
    assert_eq!(status, 200, "{answer}");
}

fn ahead(label: LabelArgs) -> ExecuteRemedyPlanArgs {
    ExecuteRemedyPlanArgs {
        offer_id: None,
        label: Some(label),
        return_schema: None,
    }
}

fn this_label() -> LabelArgs {
    LabelArgs {
        trust: None,
        audience: None,
    }
}

#[tokio::test]
async fn a_spawn_blocked_only_for_its_return_declares_without_review_and_is_steered_to_declare_ahead() {
    let runtime = deployment("delta = {}");
    let spawn = hook(&events(), "PreToolUse", "Agent", false);
    let HookDecision::DenyCall {
        feedback,
        offers,
        review,
        ..
    } = hooks::handle(&runtime, parsed(&spawn)).await
    else {
        panic!("an undeclared spawn is blocked");
    };
    assert!(review.is_empty(), "no plan asks anyone: {review:?}");
    assert!(
        !offers.is_empty() && offers.iter().all(|offer| offer.returns.is_some()),
        "every plan declares the return: {offers:?}"
    );
    assert!(!feedback.contains('<'), "no call carries a placeholder: {feedback}");
    assert!(
        feedback.contains("Next time, declare right before the subagent call: execute_remedy_plan(label: {trust: \""),
        "the block steers toward declaring ahead, with a ready call: {feedback}"
    );
    let offer = offers
        .iter()
        .find(|offer| offer.returns == Some(OfferedReturn::AsSpoken))
        .expect("the bare declaration is offered");
    let declared = runtime
        .execute_remedy_with(
            &actor_of(&spawn),
            OfferId(offer.id.clone()),
            RemedyArguments {
                label: Some(LabelSpelling::default()),
                return_schema: None,
            },
        )
        .await;
    assert!(matches!(declared, RemedyOutcome::Authorized { .. }), "{declared:?}");
    assert!(
        matches!(
            hooks::handle(&runtime, parsed(&spawn)).await,
            HookDecision::AllowCall { spawn: Some(_) }
        ),
        "the retried spawn releases"
    );
}

#[tokio::test]
async fn a_declaration_ahead_releases_the_next_spawn_without_a_block_and_only_that_spawn() {
    let runtime = deployment("delta = {}");
    let spawn = hook(&events(), "PreToolUse", "Agent", false);
    control_hook(&runtime, &spawn, serde_json::json!({})).await;
    let declared = runtime
        .execute_embedded_remedy(&actor_of(&spawn), ahead(this_label()))
        .await;
    let RemedyOutcome::Returned { value } = declared else {
        panic!("the declaration is taken, got {declared:?}");
    };
    assert!(value.starts_with("[appa] Declared."), "{value}");
    let decision = hooks::handle(&runtime, parsed(&spawn)).await;
    assert!(
        matches!(decision, HookDecision::AllowCall { spawn: Some(_) }),
        "the declared spawn releases at once: {decision:?}"
    );
    assert!(
        matches!(
            hooks::handle(&runtime, parsed(&another(&spawn))).await,
            HookDecision::DenyCall { .. }
        ),
        "the declaration covered one spawn"
    );
}

#[tokio::test]
async fn a_declaration_ahead_with_an_unknown_rank_is_refused() {
    let runtime = deployment("delta = {}");
    let spawn = hook(&events(), "PreToolUse", "Agent", false);
    control_hook(&runtime, &spawn, serde_json::json!({})).await;
    let declared = runtime
        .execute_embedded_remedy(
            &actor_of(&spawn),
            ahead(LabelArgs {
                trust: Some("no-such-rank".to_string()),
                audience: None,
            }),
        )
        .await;
    assert!(matches!(declared, RemedyOutcome::Refused { .. }), "{declared:?}");
    assert!(
        matches!(
            hooks::handle(&runtime, parsed(&spawn)).await,
            HookDecision::DenyCall { .. }
        ),
        "nothing was held for the spawn"
    );
}

#[tokio::test]
async fn a_declaration_ahead_lapses_when_a_read_narrows_the_session_and_the_block_says_so() {
    let runtime = deployment("delta = { trust = \"suspicious\" }");
    let events = events();
    let spawn = hook(&events, "PreToolUse", "Agent", false);
    control_hook(&runtime, &spawn, serde_json::json!({})).await;
    let declared = runtime
        .execute_embedded_remedy(&actor_of(&spawn), ahead(this_label()))
        .await;
    assert!(matches!(declared, RemedyOutcome::Returned { .. }), "{declared:?}");

    // The parent runs a read that lowers its trust before it spawns.
    let mut read = hook(&events, "PreToolUse", "Bash", true);
    read.as_object_mut().unwrap().remove("agent_id");
    read.as_object_mut().unwrap().remove("agent_type");
    let (_, answer) = claude_hook(&runtime, &read).await;
    if answer["hookSpecificOutput"]["permissionDecision"] == "deny" {
        let reason = answer["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .expect("the block names its offers");
        let accepted = runtime
            .execute_remedy(
                &actor_of(&read),
                offers(reason).pop().expect("the narrowing is offered"),
            )
            .await;
        assert!(matches!(accepted, RemedyOutcome::Authorized { .. }), "{accepted:?}");
        let (_, answer) = claude_hook(&runtime, &read).await;
        assert_ne!(answer["hookSpecificOutput"]["permissionDecision"], "deny", "{answer}");
    }
    let mut result = hook(&events, "PostToolUse", "Bash", true);
    result.as_object_mut().unwrap().remove("agent_id");
    result.as_object_mut().unwrap().remove("agent_type");
    claude_hook(&runtime, &result).await;
    let root = actor_of(&spawn).root;
    assert_eq!(runtime.status(&root).expect("the root answers").trust, "suspicious");

    let HookDecision::DenyCall { feedback, .. } = hooks::handle(&runtime, parsed(&spawn)).await else {
        panic!("a lapsed declaration leaves the spawn blocked");
    };
    assert!(
        feedback.starts_with("[appa] The return you declared ahead lapsed"),
        "the block says the declaration lapsed: {feedback}"
    );
}
