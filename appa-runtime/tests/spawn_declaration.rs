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
    deployment_where(AGENT_RULE, AGENT_RULE, bash_delta)
}

/// The shipped rule for `Agent`.
const AGENT_RULE: &str = "name = \"host/claude-code/Agent\"\ndelta = {}";

/// [`deployment`] with the shipped rule `from` replaced by `to`.
fn deployment_where(from: &str, to: &str, bash_delta: &str) -> Runtime {
    let example = std::fs::read_to_string(repo_root().join("marketplace/plugins/claude-code/default.appa.toml"))
        .expect("the shipped example is readable");
    assert!(example.contains(from), "the shipped example carries {from}");
    let example = example.replace(from, to);
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
    control_hook_as(runtime, spawn, label, DECLARING_CALL).await;
}

/// The `tool_use_id` of the control call [`control_hook`] makes.
const DECLARING_CALL: &str = "toolu_test_declare_ahead";

async fn control_hook_as(runtime: &Runtime, spawn: &serde_json::Value, label: serde_json::Value, call: &str) {
    let mut control = spawn.clone();
    control["tool_name"] = serde_json::json!("mcp__appa__execute_remedy_plan");
    control["tool_use_id"] = serde_json::json!(call);
    control["tool_input"] = serde_json::json!({ "label": label });
    let (status, answer) = claude_hook(runtime, &control).await;
    assert_eq!(status, 200, "{answer}");
    assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "allow", "{answer}");
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

/// The root's own turn ends, as Claude Code's `Stop` hook reports it.
fn stop(events: &[serde_json::Value]) -> serde_json::Value {
    events
        .iter()
        .find(|event| event["hook_event_name"] == "Stop")
        .expect("the fixture carries the root's Stop")
        .clone()
}

#[tokio::test]
async fn a_declaration_ahead_does_not_outlive_its_turn() {
    let runtime = deployment("delta = {}");
    let events = events();
    let spawn = hook(&events, "PreToolUse", "Agent", false);
    control_hook(&runtime, &spawn, serde_json::json!({})).await;
    let declared = runtime
        .execute_embedded_remedy(&actor_of(&spawn), ahead(this_label()))
        .await;
    assert!(matches!(declared, RemedyOutcome::Returned { .. }), "{declared:?}");
    let (status, answer) = claude_hook(&runtime, &stop(&events)).await;
    assert_eq!(status, 200, "{answer}");

    let HookDecision::DenyCall { feedback, .. } = hooks::handle(&runtime, parsed(&spawn)).await else {
        panic!("a spawn in the next turn needs its own declaration");
    };
    assert!(
        feedback.starts_with("[appa] Blocked:"),
        "the earlier turn's declaration left no notice: {feedback}"
    );
}

#[tokio::test]
async fn a_declaration_ahead_that_no_offer_fits_is_dropped_and_the_block_says_so() {
    // The spawn needs a person's review beside its declaration.
    let runtime = deployment_where(
        AGENT_RULE,
        "name = \"host/claude-code/Agent\"\nrequires = { attention = [\"human-approval\"] }\ndelta = {}",
        "delta = {}",
    );
    let spawn = hook(&events(), "PreToolUse", "Agent", false);
    let HookDecision::DenyCall { offers, .. } = hooks::handle(&runtime, parsed(&spawn)).await else {
        panic!("an undeclared spawn is blocked");
    };
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

    // The declaration stands, so the retry is planned on the review alone.
    control_hook(&runtime, &spawn, serde_json::json!({})).await;
    let ahead = runtime
        .execute_embedded_remedy(&actor_of(&spawn), ahead(this_label()))
        .await;
    assert!(matches!(ahead, RemedyOutcome::Returned { .. }), "{ahead:?}");
    let HookDecision::DenyCall { feedback, offers, .. } = hooks::handle(&runtime, parsed(&spawn)).await else {
        panic!("the review still blocks the spawn");
    };
    assert!(!offers.is_empty(), "the review is offered");
    assert!(
        feedback.starts_with(
            "[appa] The return you declared ahead does not fit this call, so it was not applied. Apply a plan below."
        ),
        "the block says the declaration was not applied: {feedback}"
    );
}

/// `claude -p` running a `Workflow` script that starts subagents: a fan-out spawn.
fn workflow_events() -> Vec<serde_json::Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hooks-workflow-pipeline.jsonl");
    std::fs::read_to_string(path)
        .expect("the recorded hook fixture is readable")
        .lines()
        .map(|line| serde_json::from_str(line).expect("each fixture line is JSON"))
        .collect()
}

#[tokio::test]
async fn a_fan_out_spawn_drops_a_declaration_ahead_and_declares_at_its_block() {
    let runtime = deployment("delta = {}");
    let workflow = hook(&workflow_events(), "PreToolUse", "Workflow", false);
    control_hook(&runtime, &workflow, serde_json::json!({})).await;
    let declared = runtime
        .execute_embedded_remedy(&actor_of(&workflow), ahead(this_label()))
        .await;
    assert!(matches!(declared, RemedyOutcome::Returned { .. }), "{declared:?}");

    let HookDecision::DenyCall { feedback, offers, .. } = hooks::handle(&runtime, parsed(&workflow)).await else {
        panic!("the fan-out spawn declares at its block");
    };
    assert!(!offers.is_empty(), "its declarations are offered");
    assert!(
        feedback.starts_with(
            "[appa] A return declared ahead covers a single subagent call, so it was not applied. A call that \
             starts several subagents declares at its block. Apply a plan below."
        ),
        "the block says why the declaration was not applied: {feedback}"
    );
    assert!(
        !feedback.contains("Next time, declare right before"),
        "a fan-out spawn is not steered to declare ahead: {feedback}"
    );

    // The dropped declaration does not reach a later single spawn.
    let mut single = workflow.clone();
    single["tool_name"] = serde_json::json!("Agent");
    single["tool_use_id"] = serde_json::json!("toolu_test_single_after_workflow");
    single["tool_input"] = serde_json::json!({
        "description": "One errand",
        "prompt": "Say hello.",
        "subagent_type": "general-purpose",
    });
    let HookDecision::DenyCall { feedback, .. } = hooks::handle(&runtime, parsed(&single)).await else {
        panic!("the single spawn needs its own declaration");
    };
    assert!(feedback.starts_with("[appa] Blocked:"), "{feedback}");
}

/// The daemon's control tool, served over MCP as `appa runtime` serves it to Claude Code.
#[cfg(feature = "daemon")]
mod over_mcp {
    use super::*;
    use rmcp::service::{ClientLifecycleMode, ClientServiceExt};
    use std::sync::Arc;

    async fn served(runtime: Arc<Runtime>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port is available");
        let url = format!(
            "http://{}/mcp",
            listener.local_addr().expect("the socket has an address")
        );
        let app = axum::Router::new().nest_service(
            "/mcp",
            appa_runtime::mcp::service(runtime, appa_runtime::yell::Harness::ClaudeCode),
        );
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        url
    }

    /// One `execute_remedy_plan` call declaring this session's label, naming `call` in its
    /// `_meta` as Claude Code does: the reply's text, and whether it failed.
    async fn declare_over_mcp(url: &str, call: Option<&str>) -> (String, bool) {
        let transport = rmcp::transport::StreamableHttpClientTransport::from_uri(url.to_string());
        let client =
            ().serve_with_lifecycle(transport, ClientLifecycleMode::Initialize)
                .await
                .expect("the client starts");
        let mut params = rmcp::model::CallToolRequestParams::default();
        params.name = "execute_remedy_plan".into();
        params.arguments = serde_json::json!({ "label": {} }).as_object().cloned();
        params.meta = call.map(|call| {
            rmcp::model::RequestMetaObject::from(
                serde_json::json!({ "claudecode/toolUseId": call })
                    .as_object()
                    .cloned()
                    .expect("the meta is an object"),
            )
        });
        let result = client.call_tool(params).await.expect("the control tool answers");
        client.cancel().await.ok();
        (format!("{:?}", result.content), result.is_error == Some(true))
    }

    /// The spawn again in another Claude Code session.
    fn in_another_session(spawn: &serde_json::Value) -> serde_json::Value {
        let mut spawn = spawn.clone();
        spawn["session_id"] = serde_json::json!("5d3c1d0e-7f0a-4c55-9a6e-2b1f6f0c9a11");
        spawn["transcript_path"] = serde_json::json!("/recorded/5d3c1d0e-7f0a-4c55-9a6e-2b1f6f0c9a11.jsonl");
        spawn["tool_use_id"] = serde_json::json!("toolu_test_other_session_spawn");
        spawn
    }

    #[tokio::test]
    async fn a_declaration_ahead_over_mcp_releases_the_spawn_its_hook_vouched_for() {
        let runtime = Arc::new(deployment("delta = {}"));
        let url = served(Arc::clone(&runtime)).await;
        let spawn = hook(&events(), "PreToolUse", "Agent", false);
        control_hook(&runtime, &spawn, serde_json::json!({})).await;
        let (text, failed) = declare_over_mcp(&url, Some(DECLARING_CALL)).await;
        assert!(!failed && text.contains("[appa] Declared."), "{text}");
        assert!(
            matches!(
                hooks::handle(&runtime, parsed(&spawn)).await,
                HookDecision::AllowCall { spawn: Some(_) }
            ),
            "the spawn meets no block for its return"
        );
    }

    #[tokio::test]
    async fn two_sessions_declaring_alike_at_once_both_declare() {
        let runtime = Arc::new(deployment("delta = {}"));
        let url = served(Arc::clone(&runtime)).await;
        let first = hook(&events(), "PreToolUse", "Agent", false);
        let second = in_another_session(&first);
        control_hook_as(&runtime, &first, serde_json::json!({}), "toolu_test_first_declares").await;
        control_hook_as(&runtime, &second, serde_json::json!({}), "toolu_test_second_declares").await;
        for call in ["toolu_test_first_declares", "toolu_test_second_declares"] {
            let (text, failed) = declare_over_mcp(&url, Some(call)).await;
            assert!(!failed && text.contains("[appa] Declared."), "{call}: {text}");
        }
        for spawn in [&first, &second] {
            assert!(
                matches!(
                    hooks::handle(&runtime, parsed(spawn)).await,
                    HookDecision::AllowCall { spawn: Some(_) }
                ),
                "each session's spawn releases under its own declaration"
            );
        }
    }

    #[tokio::test]
    async fn a_declaration_that_names_no_call_is_refused_with_the_way_through_the_block() {
        let runtime = Arc::new(deployment("delta = {}"));
        let url = served(Arc::clone(&runtime)).await;
        let spawn = hook(&events(), "PreToolUse", "Agent", false);
        control_hook(&runtime, &spawn, serde_json::json!({})).await;
        // A Claude Code that sends no `_meta` names no call, so the hook's vouch is not found.
        let (text, failed) = declare_over_mcp(&url, None).await;
        assert!(failed, "{text}");
        assert!(
            text.contains("Call the subagent tool now, and apply the declaration plan its block offers."),
            "{text}"
        );
        assert!(
            matches!(
                hooks::handle(&runtime, parsed(&spawn)).await,
                HookDecision::DenyCall { .. }
            ),
            "nothing was declared"
        );
    }
}
