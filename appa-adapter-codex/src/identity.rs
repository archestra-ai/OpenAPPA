use appa_runtime_api::{CanonicalTool, IdentifiedTool, ParseRefusal};

const CONTROL: &str = "mcp__appa__execute_remedy_plan";

fn canonical(raw: &str) -> Result<CanonicalTool, ParseRefusal> {
    let invalid = |detail: String| ParseRefusal::Malformed {
        detail: format!("Codex tool {raw:?}: {detail}"),
    };
    if raw == CONTROL {
        return Ok(CanonicalTool::control());
    }
    // Session inventories use callable names. Hook events use host names.
    // Both names resolve to the same policy identity.
    let raw = match raw {
        "exec_command" => "Bash",
        "web__run" => "webrun",
        "image_gen__imagegen" => "image_genimagegen",
        name => name,
    };
    // Policy names the APPA command wrapper. Reverse translation retains Bash.
    if raw == "Bash" {
        return CanonicalTool::of("host", "codex", "appa_exec").map_err(|error| invalid(error.to_string()));
    }
    if raw == "appa_exec" {
        return Err(invalid("appa_exec is a policy identity, not a Codex tool".into()));
    }
    if let Some(rest) = raw.strip_prefix("mcp__") {
        let (server, tool) = rest
            .split_once("__")
            .ok_or_else(|| invalid("missing MCP tool name".into()))?;
        CanonicalTool::of("mcp", server, tool).map_err(|error| invalid(error.to_string()))
    } else {
        CanonicalTool::of("host", "codex", raw).map_err(|error| invalid(error.to_string()))
    }
}

pub(crate) fn identify_tool(raw: &str) -> Result<IdentifiedTool, ParseRefusal> {
    Ok(IdentifiedTool {
        canonical: canonical(raw)?,
        spawn: raw == "spawn_agent",
    })
}

pub(crate) fn spell(tool: &CanonicalTool) -> Option<String> {
    if tool.is_control() {
        return Some(CONTROL.into());
    }
    let mut parts = tool.as_str().split('/');
    let raw = match (parts.next()?, parts.next()?, parts.next()?, parts.next()) {
        ("mcp", server, name, None) => format!("mcp__{server}__{name}"),
        ("host", "codex", "appa_exec", None) => "Bash".to_owned(),
        ("host", "codex", name, None) => name.to_owned(),
        _ => return None,
    };
    (canonical(&raw).as_ref() == Ok(tool)).then_some(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_reversible_over_accepted_spellings() {
        for raw in [
            "Bash",
            "webrun",
            "image_genimagegen",
            "apply_patch",
            "spawn_agent",
            "mcp__github__read_issue",
            CONTROL,
        ] {
            let tool = canonical(raw).unwrap();
            assert_eq!(spell(&tool).as_deref(), Some(raw));
        }
        assert!(canonical("mcp__github").is_err());
        assert_eq!(
            spell(&CanonicalTool::parse("mcp/appa/execute_remedy_plan").unwrap()),
            None
        );
        assert!(identify_tool("spawn_agent").unwrap().spawn);
        let bash = CanonicalTool::parse("host/codex/appa_exec").unwrap();
        assert_eq!(identify_tool("Bash").unwrap().canonical, bash);
        assert_eq!(spell(&bash).as_deref(), Some("Bash"));
        assert!(identify_tool("appa_exec").is_err());
    }

    #[test]
    fn callable_names_resolve_to_recorded_hook_identities() {
        for (callable, hook, policy) in [
            ("exec_command", "Bash", "host/codex/appa_exec"),
            ("web__run", "webrun", "host/codex/webrun"),
            (
                "image_gen__imagegen",
                "image_genimagegen",
                "host/codex/image_genimagegen",
            ),
        ] {
            let identity = identify_tool(callable).unwrap();
            assert_eq!(identity.canonical.as_str(), policy);
            assert_eq!(identity.canonical, identify_tool(hook).unwrap().canonical);
            assert!(!identity.spawn);
            assert_eq!(spell(&identity.canonical).as_deref(), Some(hook));
        }
    }

    #[test]
    fn callable_aliases_leave_mcp_and_control_identities_intact() {
        for tool in ["exec_command", "web__run", "image_gen__imagegen"] {
            let raw = format!("mcp__example__{tool}");
            let identity = identify_tool(&raw).unwrap().canonical;
            assert_eq!(identity.as_str(), format!("mcp/example/{tool}"));
            assert_eq!(spell(&identity).as_deref(), Some(raw.as_str()));
        }
        assert!(identify_tool(CONTROL).unwrap().canonical.is_control());
        assert_eq!(spell(&CanonicalTool::control()).as_deref(), Some(CONTROL));
    }
}
