//! A host that serves Claude Code's tools under its own namespace reuses the shipped
//! `claude-code` battery through `[aliases] host`. Composition appends the battery's
//! rules after the root's, and alias resolution then rebinds every `host/claude-code/…`
//! id, root and battery alike, so a root rule still matches before the battery's.
mod common;

use std::sync::Arc;
use std::time::Duration;

use appa_eventlog::{Backend, LogStore};
use appa_runtime::api::{RemedyOutcome, Runtime};
use appa_runtime::config::{Config, HostDefaults, HostedBattery};
use appa_runtime::hooks;
use appa_runtime_api::{
    Actor, Adapter, AdapterName, CanonicalTool, HookDecision, HookEvent, IdentifiedTool, ParseRefusal, ProposedCall,
    TrajectoryId,
};
use common::repo_root;

const CONTROL_SPELLING: &str = "run_remedy";

/// Every root rule names the host's tool; the `.env.local` one is spelled under the
/// battery's namespace and reaches the host through the same alias.
const RULES: &str = r#"
[policy]
version = 2

[[policy.tool]]
name = "host/archestra/Read(file_path:.env.example)"
delta = {}

[[policy.tool]]
name = "host/claude-code/Read(file_path:.env.local)"
delta = {}
"#;

fn identify_tool(raw: &str) -> Result<IdentifiedTool, ParseRefusal> {
    let canonical = match raw {
        CONTROL_SPELLING => CanonicalTool::control(),
        name => CanonicalTool::of("host", "archestra", name).map_err(|error| ParseRefusal::Malformed {
            detail: error.to_string(),
        })?,
    };
    Ok(IdentifiedTool { canonical, spawn: None })
}

fn spell(canonical: &CanonicalTool) -> Option<String> {
    if canonical.is_control() {
        return Some(CONTROL_SPELLING.to_string());
    }
    canonical.as_str().strip_prefix("host/archestra/").map(str::to_string)
}

fn adapter() -> Adapter {
    Adapter {
        name: AdapterName::Embedded,
        identify_tool,
        names_children: |_, _| Vec::new(),
        spell,
        wildcard_covers_spawn: true,
        spells_server: |_| false,
    }
}

async fn session(aliases: &str) -> (Runtime, Actor) {
    let battery = std::fs::read_to_string(repo_root().join("marketplace/batteries/claude-code/appa.toml"))
        .expect("the shipped battery is readable");
    let config = Config::hosted_composed(
        &format!("{aliases}{RULES}"),
        &[HostedBattery {
            name: "claude-code",
            base: &battery,
            target: None,
            token_env: &[],
        }],
        HostDefaults::new(Duration::from_millis(5000), 65_536),
        |_| None,
    )
    .expect("the root composes with the shipped battery");
    let store = Arc::new(LogStore::open(Backend::Memory).expect("an in-memory log opens"));
    let runtime = Runtime::open_with_store_as(config, store, None, adapter()).expect("the runtime opens");
    let actor = Actor {
        root: TrajectoryId("aliased".into()),
        child: None,
    };
    let started = hooks::handle(
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
    assert_eq!(started, HookDecision::Ack);
    (runtime, actor)
}

fn proposed(tool: &str, arguments: serde_json::Value) -> ProposedCall {
    ProposedCall {
        tool: identify_tool(tool)
            .expect("the tool is in the host's domain")
            .canonical
            .into_string(),
        arguments: serde_json::value::RawValue::from_string(arguments.to_string()).expect("an object"),
        cwd: None,
    }
}

async fn propose(runtime: &Runtime, actor: &Actor, call: ProposedCall, id: &str) -> HookDecision {
    hooks::handle(
        runtime,
        HookEvent::ToolCall {
            call_id: Some(id.to_string()),
            actor: actor.clone(),
            call,
            spawn: None,
            prompt: None,
            ruling: None,
        },
    )
    .await
}

async fn read(runtime: &Runtime, actor: &Actor, path: &str) -> HookDecision {
    let call = proposed("Read", serde_json::json!({ "file_path": path }));
    propose(runtime, actor, call, &format!("read {path}")).await
}

#[tokio::test]
async fn a_host_alias_routes_the_hosts_reads_through_the_batterys_rules_after_the_roots() {
    let (runtime, actor) = session("[aliases]\nhost = { claude-code = [\"archestra\"] }\n").await;

    let credential = read(&runtime, &actor, ".env").await;
    assert!(
        matches!(&credential, HookDecision::DenyCall { offers, .. } if !offers.is_empty()),
        "the battery's credential rule narrows a .env read, and the narrowing waits for acceptance: {credential:?}"
    );
    for (path, why) in [
        ("src/main.rs", "the battery's bare Read rule"),
        (
            ".env.example",
            "the root rule on the host's id precedes the battery's .env rule",
        ),
        (
            ".env.local",
            "the root rule spelled under the battery's namespace is rebound too",
        ),
    ] {
        assert_eq!(
            read(&runtime, &actor, path).await,
            HookDecision::AllowCall { spawn: None },
            "{path}: {why}"
        );
    }
}

#[tokio::test]
async fn without_the_alias_no_battery_rule_names_the_hosts_tools() {
    let (runtime, actor) = session("").await;
    for path in [".env", "src/main.rs"] {
        let decision = read(&runtime, &actor, path).await;
        assert!(
            matches!(&decision, HookDecision::DenyCall { offers, .. } if offers.is_empty()),
            "{path}: only the root's selected rule names host/archestra/Read: {decision:?}"
        );
    }
    assert_eq!(
        read(&runtime, &actor, ".env.example").await,
        HookDecision::AllowCall { spawn: None },
        "the root's own rule on the host's id applies without any alias"
    );
}

/// A host alias rebinds only what the battery declares. The protected-peer exceptions for
/// `SendMessage` belong to a root whose runtime verifies peer addresses, so under the alias
/// every send is the battery's public sink.
#[tokio::test]
async fn an_aliased_send_to_a_socket_address_is_a_public_sink() {
    let (runtime, actor) = session("[aliases]\nhost = { claude-code = [\"archestra\"] }\n").await;
    let send = || {
        proposed(
            "SendMessage",
            serde_json::json!({ "to": "uds:/tmp/peer.sock", "message": "hi" }),
        )
    };
    assert_eq!(
        propose(&runtime, &actor, send(), "send while public").await,
        HookDecision::AllowCall { spawn: None }
    );

    let HookDecision::DenyCall { offers, .. } = read(&runtime, &actor, ".env").await else {
        panic!("the .env read narrows the session");
    };
    let plan = serde_json::json!({ "offer_id": offers[0].id });
    let control = ProposedCall {
        tool: appa_runtime_api::CONTROL_TOOL.into(),
        arguments: serde_json::value::RawValue::from_string(plan.to_string()).expect("an object"),
        cwd: None,
    };
    assert_eq!(
        propose(&runtime, &actor, control, "accept").await,
        HookDecision::PassControl
    );
    let accepted = runtime
        .execute_embedded_remedy(&actor, serde_json::from_value(plan).expect("a plan"))
        .await;
    assert!(!matches!(accepted, RemedyOutcome::Refused { .. }), "{accepted:?}");
    let read_env = proposed("Read", serde_json::json!({ "file_path": ".env" }));
    assert_eq!(
        propose(&runtime, &actor, read_env.clone(), "read .env accepted").await,
        HookDecision::AllowCall { spawn: None }
    );
    hooks::handle(
        &runtime,
        HookEvent::ToolResult {
            actor: actor.clone(),
            call: read_env,
            call_id: Some("read .env accepted".into()),
            outcome: appa_runtime_api::ToolOutcome::Success {
                body: appa_runtime_api::OutcomeBody::Available("TOKEN=secret".into()),
            },
        },
    )
    .await;

    let narrowed = propose(&runtime, &actor, send(), "send while self").await;
    assert!(
        matches!(narrowed, HookDecision::DenyCall { .. }),
        "a self-narrowed session cannot send to an unverified socket: {narrowed:?}"
    );
}
