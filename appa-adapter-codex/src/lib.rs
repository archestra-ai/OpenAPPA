//! Pure translation between Codex hook JSON and the APPA runtime wire.

mod identity;
mod parse;
mod render;

use appa_runtime_api::{Adapter, AdapterName, Codec};

pub fn codec() -> Codec {
    Codec {
        parse: parse::parse,
        render: render::render,
        withholding: render::withholding,
    }
}

pub fn adapter() -> Adapter {
    Adapter {
        name: AdapterName::Codex,
        identify_tool: identity::identify_tool,
        names_children: |_actor, _call| Vec::new(),
        spell: identity::spell,
        wildcard_covers_spawn: true,
        spells_server: |name| name.starts_with("mcp__"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use appa_runtime_api::{CanonicalTool, HookEvent};

    #[test]
    fn observed_bash_and_authored_policy_identity_round_trip() {
        let body = br#"{"hook_event_name":"PreToolUse","session_id":"s1","tool_name":"Bash","tool_use_id":"c1","tool_input":{"command":"ls"}}"#;
        let HookEvent::ToolCall { call, .. } = (codec().parse)(body).unwrap().unwrap() else {
            panic!("Codex Bash parses as a proposed call")
        };
        assert_eq!(call.tool, "Bash", "the wire keeps the observed host spelling");
        let adapter = adapter();
        let identity = (adapter.identify_tool)(&call.tool).unwrap().canonical;
        assert_eq!(identity, CanonicalTool::parse("host/codex/appa_exec").unwrap());
        assert_eq!((adapter.spell)(&identity).as_deref(), Some("Bash"));
    }
}
