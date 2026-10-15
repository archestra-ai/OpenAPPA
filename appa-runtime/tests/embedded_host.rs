//! The runtime as a host embeds it: opened under an adapter the host defines, over a
//! hosted document composed from a root and a battery, so a battery's canonical rule
//! reaches the server the host aliased to it, the host's own spelling of that server is
//! what the model hears back, and a server the host did not alias is untouched by the
//! rule.

use std::sync::Arc;
use std::time::Duration;

use appa_eventlog::{Backend, LogStore};
use appa_runtime::api::{OpenError, RemedyOutcome, Runtime};
use appa_runtime::config::{Config, HostDefaults, HostedBattery};
use appa_runtime::hooks;
use appa_runtime_api::{
    Actor, Adapter, AdapterName, CanonicalTool, HookDecision, HookEvent, IdentifiedTool, ParseRefusal, ProposedCall,
    TrajectoryId,
};

const ROOT: &str = r#"
[aliases]
mcp = { github = ["github_prod"] }

[policy]
version = 2
"#;

const BATTERY: &str = r#"
[policy]
version = 2

[[policy.tool]]
name = "mcp/github/get_file_contents"
delta = { audience = ["internal"] }
"#;

/// The host's control tool as its model sees it.
const CONTROL_SPELLING: &str = "run_remedy";

/// The host spells an MCP server's tool `<server>.<tool>` and anything else by its bare
/// name; the mapping is a bijection over the spellings it accepts.
fn identify_tool(raw: &str) -> Result<IdentifiedTool, ParseRefusal> {
    let canonical = match raw {
        CONTROL_SPELLING => CanonicalTool::control(),
        spelled => match spelled.split_once('.') {
            Some((server, tool)) => CanonicalTool::of("mcp", server, tool),
            None => CanonicalTool::of("host", "testbed", spelled),
        }
        .map_err(|error| ParseRefusal::Malformed {
            detail: error.to_string(),
        })?,
    };
    Ok(IdentifiedTool { canonical, spawn: None })
}

fn spell(canonical: &CanonicalTool) -> Option<String> {
    if canonical.is_control() {
        return Some(CONTROL_SPELLING.to_string());
    }
    let mut segments = canonical.as_str().splitn(3, '/');
    match (segments.next(), segments.next(), segments.next()) {
        (Some("mcp"), Some(server), Some(tool)) => Some(format!("{server}.{tool}")),
        (Some("host"), Some("testbed"), Some(name)) if name != CONTROL_SPELLING => Some(name.to_string()),
        _ => None,
    }
}

fn names_children(_: &Actor, _: &ProposedCall) -> Vec<TrajectoryId> {
    Vec::new()
}

fn adapter() -> Adapter {
    Adapter {
        name: AdapterName::Embedded,
        identify_tool,
        names_children,
        spell,
        wildcard_covers_spawn: true,
        spells_server: |name| name.contains('.'),
    }
}

fn embedded_runtime() -> Runtime {
    open_under(adapter(), None).expect("the runtime opens under the host's adapter")
}

fn open_under(adapter: Adapter, target: Option<&str>) -> Result<Runtime, OpenError> {
    open_with_log(adapter, target).map(|(runtime, _)| runtime)
}

fn open_with_log(adapter: Adapter, target: Option<&str>) -> Result<(Runtime, Arc<LogStore>), OpenError> {
    let config = Config::hosted_composed(
        ROOT,
        &[HostedBattery {
            name: "github",
            base: BATTERY,
            target,
            token_env: &[],
        }],
        HostDefaults::new(Duration::from_millis(5000), 65_536),
        |_| None,
    )
    .expect("the hosted document composes");
    let store = Arc::new(LogStore::open(Backend::Memory).expect("an in-memory log opens"));
    let runtime = Runtime::open_with_store_as(config, Arc::clone(&store), None, adapter)?;
    Ok((runtime, store))
}

/// Every remedy tells the model to call the control tool by the host's spelling, so an
/// adapter that has none would dead-end the model on a name it cannot dispatch.
#[test]
fn an_adapter_that_spells_no_control_tool_is_refused() {
    let unspelled = Adapter {
        spell: |canonical| (!canonical.is_control()).then(|| spell(canonical)).flatten(),
        ..adapter()
    };
    assert!(matches!(
        open_under(unspelled, None),
        Err(OpenError::UnspelledControlTool)
    ));
}

/// A call as the host hands it over: identified through the adapter first, the way the wire
/// identifies a served host's calls.
fn call(actor: &Actor, raw: &str) -> HookEvent {
    let identified = identify_tool(raw).expect("the spelling is in the domain");
    HookEvent::ToolCall {
        call_id: None,
        actor: actor.clone(),
        call: ProposedCall {
            tool: identified.canonical.into_string(),
            arguments: serde_json::value::RawValue::from_string("{}".into()).expect("an object"),
            cwd: None,
        },
        spawn: identified.spawn,
        prompt: None,
        ruling: None,
    }
}

#[tokio::test]
async fn a_battery_rule_reaches_the_server_the_host_aliased_and_speaks_its_spelling() {
    let runtime = embedded_runtime();
    let actor = Actor {
        root: TrajectoryId("conversation-1".into()),
        child: None,
    };
    assert_eq!(
        hooks::handle(
            &runtime,
            HookEvent::SessionStart {
                root: actor.root.clone(),
                principal: None,
                address: None,
                title: None,
                launch: None,
                start: None,
            }
        )
        .await,
        HookDecision::Ack
    );

    // The root writes no wildcard, so a tool no rule names is refused. The battery's
    // namespace resolves to the server the host bound it to and nothing else: neither
    // another server nor the literal `github` is covered by the rule.
    for unaliased in [
        "github_dev.get_file_contents",
        "github.get_file_contents",
        "get_weather",
    ] {
        let decision = hooks::handle(&runtime, call(&actor, unaliased)).await;
        assert!(
            matches!(decision, HookDecision::Refuse { .. }),
            "{unaliased} is no tool the policy names: {decision:?}"
        );
    }

    let outcome = hooks::handle_embedded(&runtime, call(&actor, "github_prod.get_file_contents")).await;
    let HookDecision::DenyCall { offers, .. } = outcome.decision else {
        panic!("the aliased server's read must require acceptance under the battery rule");
    };
    assert!(!offers.is_empty(), "the block offers the remedy the rule names");
    let presentation = outcome.presentation.expect("a block carries its presentation");
    assert!(
        presentation.feedback.contains(CONTROL_SPELLING) && !presentation.feedback.contains("appa/"),
        "the model is told to take the remedy through the control tool as the host spells it: {}",
        presentation.feedback
    );

    let plan = serde_json::json!({ "offer_id": offers[0].id });
    assert_eq!(
        hooks::handle(
            &runtime,
            HookEvent::ToolCall {
                call_id: None,
                actor: actor.clone(),
                call: ProposedCall {
                    tool: appa_runtime_api::CONTROL_TOOL.into(),
                    arguments: serde_json::value::RawValue::from_string(plan.to_string()).expect("an object"),
                    cwd: None,
                },
                spawn: None,
                prompt: None,
                ruling: None,
            }
        )
        .await,
        HookDecision::PassControl
    );
    let executed = runtime
        .execute_embedded_remedy(&actor, serde_json::from_value(plan).expect("a plan"))
        .await;
    assert!(
        !matches!(executed, RemedyOutcome::Refused { .. }),
        "the accepted remedy executes: {executed:?}"
    );
    assert!(
        matches!(
            hooks::handle(&runtime, call(&actor, "github_prod.get_file_contents")).await,
            HookDecision::AllowCall { .. }
        ),
        "after the remedy the aliased read runs"
    );
}

#[tokio::test]
async fn identified_delivery_retries_replay_denied_calls_released_calls_and_closed_results() {
    let (runtime, store) = open_with_log(adapter(), None).expect("the host runtime opens");
    let actor = Actor {
        root: TrajectoryId("delivery-retry".into()),
        child: None,
    };
    let root = appa_eventlog::TrajectoryId::new(actor.root.0.clone());
    assert_eq!(
        hooks::handle(
            &runtime,
            HookEvent::SessionStart {
                root: actor.root.clone(),
                principal: None,
                address: None,
                title: None,
                launch: None,
                start: None,
            }
        )
        .await,
        HookDecision::Ack
    );
    let mut proposed = call(&actor, "github_prod.get_file_contents");
    let HookEvent::ToolCall { call_id, .. } = &mut proposed else {
        unreachable!()
    };
    *call_id = Some("denied".into());
    let first = hooks::handle_embedded(&runtime, proposed.clone()).await;
    let HookDecision::DenyCall { offers, .. } = &first.decision else {
        panic!("the initial narrowing requires acceptance: {:?}", first.decision);
    };
    let offer = offers[0].id.clone();
    let before = store.log(&root).expect("the decided log reads");
    let repeated = hooks::handle_embedded(&runtime, proposed.clone()).await;
    assert_eq!(
        repeated.decision, first.decision,
        "a delivery retry retains offer identities"
    );
    assert_eq!(
        store.log(&root).unwrap(),
        before,
        "a denied delivery retry appends nothing"
    );

    let plan = serde_json::json!({ "offer_id": offer });
    assert_eq!(
        hooks::handle(
            &runtime,
            HookEvent::ToolCall {
                actor: actor.clone(),
                call_id: None,
                spawn: None,
                prompt: None,
                ruling: None,
                call: ProposedCall {
                    tool: appa_runtime_api::CONTROL_TOOL.into(),
                    arguments: serde_json::value::RawValue::from_string(plan.to_string()).unwrap(),
                    cwd: None,
                },
            }
        )
        .await,
        HookDecision::PassControl
    );
    let accepted = runtime
        .execute_embedded_remedy(&actor, serde_json::from_value(plan).unwrap())
        .await;
    assert!(!matches!(accepted, RemedyOutcome::Refused { .. }), "{accepted:?}");
    let HookEvent::ToolCall { call_id, .. } = &mut proposed else {
        unreachable!()
    };
    *call_id = Some("released".into());
    let released = hooks::handle_embedded(&runtime, proposed.clone()).await;
    assert!(
        matches!(released.decision, HookDecision::AllowCall { .. }),
        "{:?}",
        released.decision
    );
    let before = store.log(&root).unwrap();
    assert_eq!(
        hooks::handle_embedded(&runtime, proposed.clone()).await.decision,
        released.decision
    );
    assert_eq!(
        store.log(&root).unwrap(),
        before,
        "a released delivery retry appends nothing"
    );

    let HookEvent::ToolCall { call, .. } = proposed else {
        unreachable!()
    };
    let result = HookEvent::ToolResult {
        actor: actor.clone(),
        call,
        call_id: Some("released".into()),
        outcome: appa_runtime_api::ToolOutcome::Success {
            body: appa_runtime_api::OutcomeBody::Available("original result".into()),
        },
    };
    let finished = hooks::handle_embedded(&runtime, result.clone()).await;
    assert!(
        !matches!(finished.decision, HookDecision::Refuse { .. }),
        "{:?}",
        finished.decision
    );
    let before = store.log(&root).unwrap();
    assert_eq!(
        hooks::handle_embedded(&runtime, result.clone()).await.decision,
        finished.decision
    );
    assert_eq!(
        store.log(&root).unwrap(),
        before,
        "a closed-result retry appends nothing"
    );

    let mut contradiction = result;
    let HookEvent::ToolResult { outcome, .. } = &mut contradiction else {
        unreachable!()
    };
    *outcome = appa_runtime_api::ToolOutcome::Failure {
        message: "contradictory report".into(),
    };
    let rejected = hooks::handle_embedded(&runtime, contradiction).await.decision;
    assert!(
        matches!(
            rejected,
            HookDecision::Block { .. } | HookDecision::ReplaceOutput { .. }
        ),
        "{rejected:?}"
    );
    assert_eq!(
        store.log(&root).unwrap(),
        before,
        "contradictory reports are not new outcomes"
    );
}

/// A battery's target policy composes before its base, so its variant of a tool the base
/// also names is the first match and decides the call.
#[tokio::test]
async fn a_target_policy_variant_decides_before_the_base() {
    const TARGET: &str =
        "[policy]\nversion = 2\n\n[[policy.tool]]\nname = \"mcp/github/get_file_contents\"\ndelta = {}\n";
    let actor = Actor {
        root: TrajectoryId("conversation-1".into()),
        child: None,
    };
    for (target, allowed) in [(None, false), (Some(TARGET), true)] {
        let runtime = open_under(adapter(), target).expect("the runtime opens");
        hooks::handle(
            &runtime,
            HookEvent::SessionStart {
                root: actor.root.clone(),
                principal: None,
                address: None,
                title: None,
                launch: None,
                start: None,
            },
        )
        .await;
        let decision = hooks::handle(&runtime, call(&actor, "github_prod.get_file_contents")).await;
        assert_eq!(
            matches!(decision, HookDecision::AllowCall { .. }),
            allowed,
            "{target:?}: {decision:?}"
        );
    }
}
