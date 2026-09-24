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
[server_aliases]
github = ["github_prod"]

[policy]
version = 2
"#;

const BATTERY: &str = r#"
[policy]
version = 2

[[policy.tool]]
name = "mcp/github/get_file_contents"
delta = { audience = ["internal"] }
requires = { audience = { within = ["internal"] } }
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
    Ok(IdentifiedTool {
        canonical,
        spawn: false,
    })
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
    open_under(adapter()).expect("the runtime opens under the host's adapter")
}

fn open_under(adapter: Adapter) -> Result<Runtime, OpenError> {
    let config = Config::hosted_composed(
        ROOT,
        &[HostedBattery {
            name: "github",
            policy: BATTERY,
            token_env: &[],
        }],
        HostDefaults {
            consult_timeout: Duration::from_millis(5000),
            max_body_bytes: 65_536,
        },
        |_| None,
    )
    .expect("the hosted document composes");
    let store = Arc::new(LogStore::open(Backend::Memory).expect("an in-memory log opens"));
    Runtime::open_with_store_as(config, store, None, adapter)
}

/// Every remedy tells the model to call the control tool by the host's spelling, so an
/// adapter that has none would dead-end the model on a name it cannot dispatch.
#[test]
fn an_adapter_that_spells_no_control_tool_is_refused() {
    let unspelled = Adapter {
        spell: |canonical| (!canonical.is_control()).then(|| spell(canonical)).flatten(),
        ..adapter()
    };
    assert!(matches!(open_under(unspelled), Err(OpenError::UnspelledControlTool)));
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
                spawn: false,
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
