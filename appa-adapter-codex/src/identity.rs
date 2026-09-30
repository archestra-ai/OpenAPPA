use appa_runtime_api::{CanonicalTool, IdentifiedTool, ParseRefusal};

const CONTROL: &str = "mcp__appa__execute_remedy_plan";

fn canonical(raw: &str) -> Result<CanonicalTool, ParseRefusal> {
    let invalid = |detail: String| ParseRefusal::Malformed {
        detail: format!("Codex tool {raw:?}: {detail}"),
    };
    if raw == CONTROL {
        return Ok(CanonicalTool::control());
    }
    // Bash is Codex's command transport. Policy names its eventual APPA-owned
    // wrapper, while the wire and reverse spelling retain the actual host tool.
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
        for raw in ["Bash", "apply_patch", "spawn_agent", "mcp__github__read_issue", CONTROL] {
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
}
