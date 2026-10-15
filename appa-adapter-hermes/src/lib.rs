//! Server-side tool identification for the Hermes Agent plugin in `integrations/hermes`.
//! The plugin speaks the shared hook wire. Builtins and plugin tools retain their
//! raw names under `host/hermes`; MCP names (`mcp__<server>__<tool>`, which Hermes
//! builds from names sanitized to `[A-Za-z0-9_]`) identify their server separately.
//!
//! `delegate_task` is an ordinary tool call. The plugin checks a subagent's calls
//! on its parent's trajectory, so no child trajectory or checked return exists.

use appa_runtime_api::{Adapter, AdapterName, CanonicalTool, IdentifiedTool, ParseRefusal};

const CONTROL: &str = "mcp__appa__execute_remedy_plan";

pub fn adapter() -> Adapter {
    Adapter {
        name: AdapterName::Hermes,
        identify_tool,
        names_children: |_, _| Vec::new(),
        spell,
        wildcard_covers_spawn: false,
        spells_server: |name| name.starts_with("mcp__"),
    }
}

fn identify_tool(raw: &str) -> Result<IdentifiedTool, ParseRefusal> {
    let malformed = |detail: String| ParseRefusal::Malformed {
        detail: format!("tool {raw:?} is outside the Hermes adapter's domain: {detail}"),
    };
    let canonical = if raw == CONTROL {
        CanonicalTool::control()
    } else if let Some(rest) = raw.strip_prefix("mcp__") {
        let (server, tool) = rest
            .split_once("__")
            .ok_or_else(|| malformed("expected mcp__<server>__<tool>".into()))?;
        CanonicalTool::of("mcp", server, tool).map_err(|error| malformed(error.to_string()))?
    } else {
        CanonicalTool::of("host", "hermes", raw).map_err(|error| malformed(error.to_string()))?
    };
    Ok(IdentifiedTool { canonical, spawn: None })
}

fn spell(tool: &CanonicalTool) -> Option<String> {
    if tool.is_control() {
        return Some(CONTROL.into());
    }
    let mut parts = tool.as_str().split('/');
    let raw = match (parts.next()?, parts.next()?, parts.next()?) {
        ("mcp", server, name) => format!("mcp__{server}__{name}"),
        ("host", "hermes", name) => name.to_string(),
        _ => return None,
    };
    // A control or MCP spelling cannot also name an ordinary host tool.
    (identify_tool(&raw).ok()?.canonical == *tool).then_some(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn identities_and_control_lookalikes() {
        for (raw, canonical) in [
            ("terminal", "host/hermes/terminal"),
            ("delegate_task", "host/hermes/delegate_task"),
            ("plugin__example__send", "host/hermes/plugin__example__send"),
            ("mcp__github__create_issue", "mcp/github/create_issue"),
            (
                "mcp__google_workspace__read_file_content",
                "mcp/google_workspace/read_file_content",
            ),
            (CONTROL, "appa/execute_remedy_plan"),
            ("execute_remedy_plan", "host/hermes/execute_remedy_plan"),
            ("mcp__other__execute_remedy_plan", "mcp/other/execute_remedy_plan"),
        ] {
            let identified = identify_tool(raw).unwrap();
            assert_eq!(identified.canonical.as_str(), canonical);
            assert_eq!(identified.spawn, None);
            assert_eq!(spell(&identified.canonical).as_deref(), Some(raw));
        }
        for raw in ["", "mcp__", "mcp__server", "mcp____tool", "mcp__x__", "a/b", "a b"] {
            assert!(identify_tool(raw).is_err(), "{raw}");
        }
        for canonical in [
            "host/claude-code/Bash",
            "mcp/appa/execute_remedy_plan",
            "host/hermes/mcp__a__b",
        ] {
            assert_eq!(spell(&CanonicalTool::parse(canonical).unwrap()), None);
        }
    }

    proptest! {
        #[test]
        fn accepted_names_round_trip_and_only_the_registered_tool_is_control(
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
