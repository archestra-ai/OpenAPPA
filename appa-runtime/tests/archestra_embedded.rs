//! The runtime as Archestra embeds it: opened with the Archestra adapter over a
//! hosted document composed from a root and a battery, so a battery's canonical
//! rule reaches the catalog the host aliased to it, the host's own spelling of
//! that catalog is what the model hears back, and a catalog the host did not
//! alias is untouched by the rule.

use std::sync::Arc;
use std::time::Duration;

use appa_eventlog::{Backend, LogStore};
use appa_runtime::api::{RemedyOutcome, Runtime};
use appa_runtime::config::{Config, HostDefaults, HostedBattery};
use appa_runtime::hooks;
use appa_runtime_api::{Actor, HookDecision, HookEvent, ProposedCall, TrajectoryId};

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

fn embedded_runtime() -> Runtime {
    let config = Config::hosted_composed(
        ROOT,
        &[HostedBattery {
            name: "github",
            policy: BATTERY,
        }],
        HostDefaults {
            consult_timeout: Duration::from_millis(5000),
            max_body_bytes: 65_536,
        },
    )
    .expect("the hosted document composes");
    let store = Arc::new(LogStore::open(Backend::Memory).expect("an in-memory log opens"));
    Runtime::open_with_store_as(config, store, None, appa_adapter_archestra::adapter())
        .expect("the runtime opens under the Archestra adapter")
}

/// A call as Archestra hands it over: derived through the adapter first, the way the
/// wire derives a served host's calls.
fn call(actor: &Actor, raw: &str) -> HookEvent {
    let derived = (appa_adapter_archestra::adapter().derive)(raw).expect("the spelling is in the domain");
    HookEvent::ToolCall {
        call_id: None,
        actor: actor.clone(),
        call: ProposedCall {
            tool: derived.canonical.into_string(),
            arguments: serde_json::value::RawValue::from_string("{}".into()).expect("an object"),
        },
        spawn: derived.spawn,
        ruling: None,
    }
}

#[tokio::test]
async fn a_battery_rule_reaches_the_catalog_the_host_aliased_and_speaks_its_spelling() {
    let runtime = embedded_runtime();
    let actor = Actor {
        root: TrajectoryId("archestra:conversation-1".into()),
        child: None,
    };
    assert_eq!(
        hooks::handle(
            &runtime,
            HookEvent::SessionStart {
                root: actor.root.clone()
            }
        )
        .await,
        HookDecision::Ack
    );

    // The root writes no wildcard, so a tool no rule names is refused. The battery's
    // namespace resolves to the catalog the host bound it to and nothing else: neither
    // another catalog nor the literal `github` is covered by the rule.
    for unaliased in [
        "github_dev__get_file_contents",
        "github__get_file_contents",
        "get_weather",
    ] {
        let decision = hooks::handle(&runtime, call(&actor, unaliased)).await;
        assert!(
            matches!(decision, HookDecision::Refuse { .. }),
            "{unaliased} is no tool the policy names: {decision:?}"
        );
    }

    let outcome = hooks::handle_embedded(&runtime, call(&actor, "github_prod__get_file_contents")).await;
    let HookDecision::DenyCall { offers, .. } = outcome.decision else {
        panic!("the aliased catalog's read must require acceptance under the battery rule");
    };
    assert!(!offers.is_empty(), "the block offers the remedy the rule names");
    let presentation = outcome.presentation.expect("a block carries its presentation");
    assert!(
        presentation.feedback.contains("archestra__execute_remedy_plan") && !presentation.feedback.contains("appa/"),
        "the model is told to take the remedy through the control tool as Archestra spells it: {}",
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
            hooks::handle(&runtime, call(&actor, "github_prod__get_file_contents")).await,
            HookDecision::AllowCall { .. }
        ),
        "after the remedy the aliased read runs"
    );
}
