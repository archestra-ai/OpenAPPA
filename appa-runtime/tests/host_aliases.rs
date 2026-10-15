//! A host that serves Claude Code's tools under its own namespace reuses the shipped
//! `claude-code` battery through `[aliases] host`. Composition appends the battery's
//! rules after the root's, and alias resolution then rebinds every `host/claude-code/…`
//! id, root and battery alike, so a root rule still matches before the battery's.
mod common;

use std::sync::Arc;
use std::time::Duration;

use appa_eventlog::{Backend, LogStore};
use appa_runtime::api::Runtime;
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

async fn read(runtime: &Runtime, actor: &Actor, path: &str) -> HookDecision {
    let identified = identify_tool("Read").expect("Read is in the host's domain");
    hooks::handle(
        runtime,
        HookEvent::ToolCall {
            call_id: Some(format!("read {path}")),
            actor: actor.clone(),
            call: ProposedCall {
                tool: identified.canonical.into_string(),
                arguments: serde_json::value::RawValue::from_string(
                    serde_json::json!({ "file_path": path }).to_string(),
                )
                .expect("an object"),
                cwd: None,
            },
            spawn: None,
            prompt: None,
            ruling: None,
        },
    )
    .await
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
