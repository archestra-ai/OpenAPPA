//! Tool identity, a bijection over the raw spellings it accepts; the
//! adapter's inverse reads the table right to left:
//!
//! | raw spelling | canonical |
//! |---|---|
//! | `mcp__appa__execute_remedy_plan` | `appa/execute_remedy_plan`, the runtime's control tool |
//! | `mcp__<server>__<tool>`, split at the first `__` after the prefix | `mcp/<server>/<tool>` |
//! | any other `[A-Za-z0-9_.-]+` | `host/claude-code/<name>` |
//!
//! A raw spelling outside that domain is refused and the call blocks:
//! `mcp__` with no second `__`, an empty server or tool segment, or a
//! character outside the segment grammar. The server segment never
//! contains `__`, because it is what precedes the first one. The spawn
//! tools `Agent` and `Task` are host tools, `host/claude-code/Agent` and
//! `host/claude-code/Task`; the `agent` family is not Claude Code's.
//!
//! Read right to left the table is partial. The control spelling
//! occupies a cell the `mcp` row would otherwise own, so
//! `mcp/appa/execute_remedy_plan` — an ordinary tool a policy may
//! declare on the runtime's own server — has no Claude Code spelling.
//! Where the runtime would name that tool it says the canonical id, never
//! a spelling that dispatches the control tool instead.

use appa_runtime_api::{CanonicalTool, Derived, ParseRefusal};

/// The registered spelling of the runtime's own control tool: `execute_remedy_plan`
/// on the `appa` MCP server the install registers. Only this spelling is the control
/// tool; a lookalike on another server is an ordinary checked call.
pub(crate) const CONTROL_TOOL_RAW: &str = "mcp__appa__execute_remedy_plan";

pub(crate) const MCP_PREFIX: &str = "mcp__";

/// The crate-level mapping table: the control spelling, then `mcp__<server>__<tool>` split
/// at the first `__` after the prefix, then `host/claude-code/<name>`. `CanonicalTool::of`
/// refuses an empty segment and a character outside the grammar, so the map is a bijection
/// over the spellings it accepts.
pub(crate) fn canonical(raw: &str) -> Result<CanonicalTool, ParseRefusal> {
    let refused = |detail: String| ParseRefusal::Malformed {
        detail: format!("tool {raw:?} is outside the Claude Code adapter's domain: {detail}"),
    };
    if raw == CONTROL_TOOL_RAW {
        return Ok(CanonicalTool::control());
    }
    match raw.strip_prefix(MCP_PREFIX) {
        Some(rest) => match rest.split_once("__") {
            Some((server, tool)) => CanonicalTool::of("mcp", server, tool).map_err(|error| refused(error.to_string())),
            None => Err(refused(format!("{MCP_PREFIX}<server>__<tool> names no tool segment"))),
        },
        None => CanonicalTool::of("host", "claude-code", raw).map_err(|error| refused(error.to_string())),
    }
}

/// The inverse of [`canonical`] over its range: what Claude Code calls the tool one
/// canonical identity names, which is the name the runtime says whenever it tells this
/// model to run something. Every answer is checked against [`canonical`], so a spelling
/// this returns is one that derives back to the identity it was asked about, and a
/// canonical id outside the derivation's range answers `None` — the caller says the
/// canonical id instead.
///
/// Three families of id have no Claude Code spelling. The `agent` family and another
/// host's namespace render nothing. `host/claude-code/<name>` whose name is itself an
/// `mcp__` spelling, and `mcp/appa/execute_remedy_plan` — an ordinary tool named
/// `execute_remedy_plan` on the runtime's own server — render a spelling Claude Code
/// dispatches to a different identity, so neither is a spelling of the id it came from.
pub(crate) fn spell(tool: &CanonicalTool) -> Option<String> {
    if tool.is_control() {
        return Some(CONTROL_TOOL_RAW.to_string());
    }
    let mut segments = tool.as_str().split('/');
    let raw = match (segments.next()?, segments.next()?, segments.next()?) {
        ("mcp", server, name) => format!("{MCP_PREFIX}{server}__{name}"),
        ("host", "claude-code", name) => name.to_string(),
        _ => return None,
    };
    (canonical(&raw).as_ref() == Ok(tool)).then_some(raw)
}

pub(crate) fn derive(raw: &str) -> Result<Derived, ParseRefusal> {
    Ok(Derived {
        canonical: canonical(raw)?,
        spawn: is_spawn_tool(raw),
    })
}

pub(crate) fn is_spawn_tool(tool: &str) -> bool {
    tool == "Agent" || tool == "Task"
}

pub(crate) fn is_mcp_tool(tool: &str) -> bool {
    tool.starts_with(MCP_PREFIX)
}
