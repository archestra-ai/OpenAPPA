//! Server-side identification for AppenCode, the OpenCode V2 plugin.
//! The plugin obtains namespaces from the registry, never by splitting OpenCode's
//! flattened dispatch names. Namespaced tools cross as `mcp__<namespace>__<name>`.
//! Only the plugin-owned delegation tool starts checked child trajectories.

use appa_runtime_api::{Adapter, AdapterName, CanonicalTool, IdentifiedTool, ParseRefusal, SpawnKind, SpawnLaunch};

const CONTROL: &str = "mcp__appa__execute_remedy_plan";

pub fn adapter() -> Adapter {
    Adapter {
        name: AdapterName::OpenCode,
        identify_tool,
        names_children: |_, _| Vec::new(),
        spell,
        wildcard_covers_spawn: false,
        spells_server: |name| name.starts_with("mcp__"),
        spawn_launch: SpawnLaunch::Conformed(launch_fields),
    }
}

/// A background `appa_subagent` launch replies with the task handle the plugin generated, a
/// UUID, under `output` and `content`. Only a handle of that form stays.
fn launch_fields(report: Option<serde_json::Value>, _spawned: &str) -> serde_json::Value {
    let handles = match report {
        Some(serde_json::Value::Object(fields)) => fields
            .into_iter()
            .filter(|(key, field)| {
                matches!(key.as_str(), "output" | "content") && field.as_str().is_some_and(is_handle)
            })
            .collect(),
        _ => serde_json::Map::new(),
    };
    serde_json::Value::Object(handles)
}

/// Whether `text` is a UUID in its hyphenated form, as `crypto.randomUUID` spells one.
fn is_handle(text: &str) -> bool {
    text.len() == 36
        && text.char_indices().all(|(at, c)| match at {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

fn identify_tool(raw: &str) -> Result<IdentifiedTool, ParseRefusal> {
    let malformed = |detail: String| ParseRefusal::Malformed {
        detail: format!("tool {raw:?} is outside the OpenCode adapter's domain: {detail}"),
    };
    let canonical = if raw == CONTROL {
        CanonicalTool::control()
    } else if let Some(rest) = raw.strip_prefix("mcp__") {
        let (server, tool) = rest
            .split_once("__")
            .ok_or_else(|| malformed("expected mcp__<namespace>__<tool>".into()))?;
        CanonicalTool::of("mcp", server, tool).map_err(|error| malformed(error.to_string()))?
    } else {
        CanonicalTool::of("host", "opencode", raw).map_err(|error| malformed(error.to_string()))?
    };
    Ok(IdentifiedTool {
        canonical,
        spawn: (raw == "appa_subagent").then_some(SpawnKind::Single),
    })
}

fn spell(tool: &CanonicalTool) -> Option<String> {
    if tool.is_control() {
        return Some(CONTROL.into());
    }
    let mut parts = tool.as_str().split('/');
    let raw = match (parts.next()?, parts.next()?, parts.next()?) {
        ("mcp", server, name) => format!("mcp__{server}__{name}"),
        ("host", "opencode", name) => name.to_string(),
        _ => return None,
    };
    (identify_tool(&raw).ok()?.canonical == *tool).then_some(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn namespaces_and_control_identity_do_not_follow_flattened_names() {
        for (raw, canonical) in [
            ("read", "host/opencode/read"),
            ("subagent", "host/opencode/subagent"),
            ("appa_collect", "host/opencode/appa_collect"),
            ("appa_cancel", "host/opencode/appa_cancel"),
            ("appa_execute_remedy_plan", "host/opencode/appa_execute_remedy_plan"),
            ("mcp__github_team__create_issue", "mcp/github_team/create_issue"),
            ("mcp__other__execute_remedy_plan", "mcp/other/execute_remedy_plan"),
            (CONTROL, "appa/execute_remedy_plan"),
        ] {
            let identified = identify_tool(raw).unwrap();
            assert_eq!(identified.canonical.as_str(), canonical);
            assert_eq!(identified.spawn, None);
            assert_eq!(spell(&identified.canonical).as_deref(), Some(raw));
        }
        let spawn = identify_tool("appa_subagent").unwrap();
        assert_eq!(spawn.canonical.as_str(), "host/opencode/appa_subagent");
        assert_eq!(spawn.spawn, Some(SpawnKind::Single));
        assert_eq!(spell(&spawn.canonical).as_deref(), Some("appa_subagent"));
        assert_eq!(identify_tool("mcp__other__appa_subagent").unwrap().spawn, None);
        for raw in ["", "mcp__", "mcp__server", "mcp____tool", "mcp__x__", "a/b", "a b"] {
            assert!(identify_tool(raw).is_err(), "{raw}");
        }
        for canonical in [
            "host/amp/read",
            "host/opencode/mcp__a__b",
            "mcp/appa/execute_remedy_plan",
        ] {
            assert_eq!(spell(&CanonicalTool::parse(canonical).unwrap()), None);
        }
    }

    #[test]
    fn a_launch_reply_keeps_only_the_task_handle() {
        let handle = "0f8fad5b-d9cb-469f-a165-70867728950e";
        assert_eq!(
            launch_fields(
                Some(serde_json::json!({ "output": handle, "content": handle, "note": "the child's bytes" })),
                "child"
            ),
            serde_json::json!({ "output": handle, "content": handle })
        );
        assert_eq!(
            launch_fields(
                Some(serde_json::json!({ "output": "the child's bytes", "content": [handle] })),
                "child"
            ),
            serde_json::json!({})
        );
        assert_eq!(launch_fields(None, "child"), serde_json::json!({}));
    }

    proptest! {
        #[test]
        fn identity_round_trip_and_only_the_registered_control_is_reserved(
            raw in prop_oneof![
                "[A-Za-z0-9_.-]{1,30}",
                "mcp__[A-Za-z0-9.-]{1,10}__[A-Za-z0-9_.-]{1,20}",
                Just(CONTROL.to_string()),
            ]
        ) {
            let identified = identify_tool(&raw).unwrap();
            prop_assert_eq!(identified.canonical.is_control(), raw == CONTROL);
            prop_assert_eq!(spell(&identified.canonical), Some(raw));
        }
    }
}
