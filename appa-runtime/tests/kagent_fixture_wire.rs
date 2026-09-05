//! The shared kagent fixture, read by the runtime the way `/hook` reads it.
//!
//! `marketplace/adapters/kagent/fixtures/wire-events.jsonl` is the one sample of the
//! canonical wire the two plugin lanes and the runtime agree on: the Python and
//! Go suites render it, and this suite admits it. Every line goes through
//! `WireEvent::read` and the served kagent adapter, so a plugin that changes the
//! bytes it posts fails here as well as in its own lane.

use appa_runtime_api::{AdapterName, CanonicalTool, HookEvent, OutcomeBody, ToolOutcome, WireEvent};

const FIXTURES: &str = include_str!("../../marketplace/adapters/kagent/fixtures/wire-events.jsonl");

/// One fixture line: the case name the plugin lanes use, and the posted bytes.
struct Posted {
    name: String,
    body: Vec<u8>,
}

fn posted() -> Vec<Posted> {
    let lines: Vec<Posted> = FIXTURES
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let parsed: serde_json::Value = serde_json::from_str(line).expect("each fixture line is one JSON object");
            Posted {
                name: parsed["name"].as_str().expect("each case is named").to_string(),
                body: serde_json::to_vec(&parsed["wire"]).expect("the wire event re-serializes"),
            }
        })
        .collect();
    assert!(!lines.is_empty(), "the fixture carries cases");
    lines
}

/// The family the runtime derives from each of kagent's raw spellings, and
/// whether the call is a spawn.
fn expected(raw: &str) -> (String, bool) {
    match raw.split_once(':') {
        Some(("mcp", rest)) => (format!("mcp/{rest}"), false),
        Some(("agent", rest)) => (format!("agent/{rest}"), true),
        Some(("builtin", name)) => (format!("host/kagent/{name}"), false),
        Some(("gate", name)) => (format!("host/kagent-gate/{name}"), false),
        Some(("appa", _)) => (CanonicalTool::control().as_str().to_string(), false),
        other => panic!("the fixture spells a tool the adapter cannot derive: {other:?}"),
    }
}

#[test]
fn every_fixture_event_is_admitted_under_the_served_kagent_adapter() {
    let adapter = appa_adapter_kagent::adapter();
    for Posted { name, body } in posted() {
        let wire = WireEvent::read(&body).unwrap_or_else(|refusal| panic!("`{name}` reads: {refusal:?}"));
        let accepted = wire
            .into_event(&adapter)
            .unwrap_or_else(|refusal| panic!("`{name}` is admitted: {refusal:?}"));
        match name.as_str() {
            "ping" => assert!(accepted.is_none(), "a ping opens no trajectory"),
            _ => assert!(accepted.is_some(), "`{name}` carries an event"),
        }
    }
}

#[test]
fn each_raw_spelling_arrives_as_the_canonical_tool_the_policy_names() {
    let adapter = appa_adapter_kagent::adapter();
    let mut derived = 0;
    for Posted { name, body } in posted() {
        let raw = serde_json::from_slice::<serde_json::Value>(&body).expect("the fixture body is JSON")["tool"]
            .as_str()
            .map(str::to_string);
        let Some(raw) = raw else { continue };
        let accepted = WireEvent::read(&body)
            .and_then(|wire| wire.into_event(&adapter))
            .unwrap_or_else(|refusal| panic!("`{name}` is admitted: {refusal:?}"))
            .expect("an event naming a tool is never a ping");
        let (call, spawn) = match &accepted.event {
            HookEvent::ToolCall { call, spawn, .. } => (call, *spawn),
            HookEvent::ToolResult { call, .. } | HookEvent::SpawnResult { call, .. } => (call, false),
            other => panic!("`{name}` names a tool on {other:?}"),
        };
        let (canonical, spawns) = expected(&raw);
        assert_eq!(call.tool, canonical, "`{name}` derives its canonical tool");
        assert!(
            CanonicalTool::parse(&call.tool).is_ok(),
            "`{name}` derives a tool the policy can name"
        );
        if matches!(accepted.event, HookEvent::ToolCall { .. }) {
            assert_eq!(spawn, spawns, "`{name}` derives whether the call forks a child");
        }
        assert!(
            accepted.names_children.is_empty(),
            "kagent binds a child by its spawn binding, never by an argument"
        );
        derived += 1;
    }
    assert!(derived >= 5, "the fixture exercises every raw class");
}

/// The three successes the fixture spells arrive as three different
/// outcomes: a body that is JSON `null`, a body the plugin did not carry,
/// and an ordinary body.
#[test]
fn each_success_encoding_arrives_as_its_own_outcome() {
    let adapter = appa_adapter_kagent::adapter();
    let outcome = |case: &str| {
        let Posted { name, body } = posted()
            .into_iter()
            .find(|posted| posted.name == case)
            .unwrap_or_else(|| panic!("the fixture carries `{case}`"));
        let accepted = WireEvent::read(&body)
            .and_then(|wire| wire.into_event(&adapter))
            .unwrap_or_else(|refusal| panic!("`{name}` is admitted: {refusal:?}"))
            .expect("a tool result is never a ping");
        match accepted.event {
            HookEvent::ToolResult { outcome, .. } => outcome,
            other => panic!("`{name}` is a tool result, got {other:?}"),
        }
    };
    assert_eq!(
        outcome("tool_result_null_body"),
        ToolOutcome::Success {
            body: OutcomeBody::Available("null".to_string())
        },
    );
    assert_eq!(
        outcome("tool_result_without_body"),
        ToolOutcome::Success {
            body: OutcomeBody::Unavailable
        },
    );
    assert_eq!(
        outcome("tool_result_success"),
        ToolOutcome::Success {
            body: OutcomeBody::Available(r#"{"scaled":true}"#.to_string())
        },
    );
}

#[test]
fn the_runtime_prefixes_the_trajectory_ids_the_plugin_leaves_bare() {
    let adapter = appa_adapter_kagent::adapter();
    let prefix = format!("{}:", AdapterName::Kagent.prefix());
    let mut seen = 0;
    for Posted { name, body } in posted() {
        let raw: serde_json::Value = serde_json::from_slice(&body).expect("the fixture body is JSON");
        let Some(root) = raw["root_id"].as_str() else { continue };
        assert!(
            !root.starts_with(&prefix),
            "`{name}` posts the bare session id the ADK dispatches"
        );
        let accepted = WireEvent::read(&body)
            .and_then(|wire| wire.into_event(&adapter))
            .unwrap_or_else(|refusal| panic!("`{name}` is admitted: {refusal:?}"))
            .expect("an event naming a trajectory is never a ping");
        let root_id = match &accepted.event {
            HookEvent::SessionStart { root }
            | HookEvent::ChildStart { root, .. }
            | HookEvent::ChildEnd { root, .. } => root.0.clone(),
            HookEvent::Prompt { actor, .. }
            | HookEvent::TurnEnd { actor }
            | HookEvent::ToolCall { actor, .. }
            | HookEvent::ToolResult { actor, .. }
            | HookEvent::SpawnResult { actor, .. } => actor.root.0.clone(),
        };
        assert_eq!(
            root_id,
            format!("{prefix}{root}"),
            "`{name}` lands under the host prefix"
        );
        seen += 1;
    }
    assert!(seen > 1, "the fixture carries events naming a trajectory");
}
