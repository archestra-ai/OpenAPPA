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

use appa_runtime_api::{CanonicalTool, IdentifiedTool, ParseRefusal};

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
/// this returns is one that maps back to the identity it was asked about, and a
/// canonical id outside the mapping's range answers `None` — the caller says the
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

pub(crate) fn identify_tool(raw: &str) -> Result<IdentifiedTool, ParseRefusal> {
    Ok(IdentifiedTool {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter;
    use crate::fixtures::*;
    use appa_runtime_api::{CanonicalTool, ParseRefusal};
    #[test]
    fn each_raw_spelling_maps_onto_its_canonical_identity() {
        for (raw, expected) in [
            ("Bash", "host/claude-code/Bash"),
            ("Agent", "host/claude-code/Agent"),
            ("Task", "host/claude-code/Task"),
            ("mcp__github__create_issue", "mcp/github/create_issue"),
            ("mcp__github__a__b", "mcp/github/a__b"),
            ("mcp__appa__other", "mcp/appa/other"),
            (
                "mcp__appa-guide__execute_remedy_plan",
                "mcp/appa-guide/execute_remedy_plan",
            ),
            ("mcp__a.b-c__T.o-o_l", "mcp/a.b-c/T.o-o_l"),
            ("mcp_x", "host/claude-code/mcp_x"),
            (CONTROL_TOOL_RAW, appa_runtime_api::CONTROL_TOOL),
        ] {
            let canonical = canonical(raw).unwrap_or_else(|refusal| panic!("{raw} maps: {refusal:?}"));
            assert_eq!(canonical.as_str(), expected, "{raw}");
            assert_eq!(canonical.is_control(), raw == CONTROL_TOOL_RAW, "{raw}");
            assert_eq!(
                (adapter().spell)(&canonical).as_deref(),
                Some(raw),
                "the inverse spells {expected} back as the name Claude Code dispatches"
            );
        }
    }

    /// A canonical id outside the mapping's range has no Claude Code spelling.
    /// `mcp/appa/execute_remedy_plan` is the ordinary tool a policy may declare on the
    /// runtime's own server: its rendering is the reserved control spelling, which names
    /// another tool, so it has none.
    #[test]
    fn a_canonical_id_outside_the_range_has_no_host_spelling() {
        for name in [
            "agent/kagent/log-analyst",
            "host/kagent/memory_persist",
            "host/kagent-gate/outer",
            "host/claude-code/mcp__github__x",
            "mcp/appa/execute_remedy_plan",
        ] {
            let canonical = CanonicalTool::parse(name).expect("the fixture is canonical");
            assert_eq!((adapter().spell)(&canonical), None, "{name}");
        }
    }

    #[test]
    fn a_spelling_outside_the_domain_is_a_named_refusal() {
        for raw in [
            "",
            "mcp__",
            "mcp__github",
            "mcp____x",
            "mcp__github__",
            "mcp__git hub__x",
            "mcp__github__x(y)",
            "Bash(command:ls)",
            "a/b",
            "host/claude-code/Bash",
            "agent/kagent/x",
            "appa/execute_remedy_plan",
            "*",
        ] {
            match canonical(raw) {
                Err(ParseRefusal::Malformed { detail }) => {
                    assert!(
                        detail.contains(&format!("{raw:?}")),
                        "the refusal names {raw:?}: {detail}"
                    );
                }
                other => panic!("{raw:?} must be refused, got {other:?}"),
            }
        }
    }

    #[test]
    fn identification_carries_the_canonical_identity_and_spawn() {
        for tool in ["Agent", "Task"] {
            let identified = identified(tool).expect("identifies");
            assert!(identified.spawn, "{tool} is the spawn");
            assert_eq!(identified.canonical.as_str(), format!("host/claude-code/{tool}"));
        }
        assert!(!identified("Bash").expect("identifies").spawn);
        assert!(matches!(identified("mcp__github"), Err(ParseRefusal::Malformed { .. })));
    }

    mod laws {
        use super::*;
        use proptest::prelude::*;

        fn segment_chars() -> impl Strategy<Value = String> {
            "[A-Za-z0-9_.-]{0,10}"
        }

        fn raw_spelling() -> impl Strategy<Value = String> {
            prop_oneof![
                segment_chars(),
                (segment_chars(), segment_chars()).prop_map(|(server, tool)| format!("mcp__{server}__{tool}")),
                (segment_chars(), segment_chars(), segment_chars())
                    .prop_map(|(server, tool, more)| format!("mcp__{server}__{tool}__{more}")),
                segment_chars().prop_map(|rest| format!("mcp__{rest}")),
                Just(CONTROL_TOOL_RAW.to_string()),
            ]
        }

        /// Every canonical identity a policy may declare, including the ones no Claude
        /// Code spelling maps to: the control tool's own server and name, another
        /// host's namespace, and a host tool named like an `mcp__` spelling.
        fn canonical_id() -> impl Strategy<Value = CanonicalTool> {
            let family = prop_oneof![Just("mcp"), Just("host"), Just("agent")];
            let namespace = prop_oneof![
                Just("appa".to_string()),
                Just("claude-code".to_string()),
                Just("kagent".to_string()),
                segment_chars(),
            ];
            let name = prop_oneof![
                Just("execute_remedy_plan".to_string()),
                Just("mcp__github__x".to_string()),
                segment_chars(),
            ];
            prop_oneof![
                (family, namespace, name).prop_filter_map("a canonical identity", |(family, namespace, name)| {
                    CanonicalTool::of(family, &namespace, &name).ok()
                }),
                raw_spelling().prop_filter_map("an accepted spelling", |raw| canonical(&raw).ok()),
            ]
        }

        proptest! {
            #[test]
            fn an_accepted_spelling_parses_back_and_is_control_only_when_registered(raw in raw_spelling()) {
                if let Ok(canonical) = canonical(&raw) {
                    prop_assert_eq!(CanonicalTool::parse(canonical.as_str()), Ok(canonical.clone()));
                    prop_assert_eq!(canonical.is_control(), raw == CONTROL_TOOL_RAW);
                    prop_assert!(!canonical.as_str().starts_with("agent/"), "{}", canonical);
                }
            }

            /// The inverse is total over the mapping's range and returns the exact
            /// spelling Claude Code dispatches, so the runtime never has to keep one.
            #[test]
            fn the_inverse_spells_every_mapped_identity_back(raw in raw_spelling()) {
                if let Ok(canonical) = canonical(&raw) {
                    let spelled = (adapter().spell)(&canonical);
                    prop_assert_eq!(spelled.as_deref(), Some(raw.as_str()));
                }
            }

            /// The other direction, over every canonical identity a policy may declare:
            /// a spelling the inverse yields is one Claude Code dispatches to the very
            /// identity it was asked about. An identity whose rendering would name
            /// another tool is spelled `None` instead, never that rendering.
            #[test]
            fn a_spelled_identity_is_the_one_its_spelling_maps_to(tool in canonical_id()) {
                if let Some(spelled) = (adapter().spell)(&tool) {
                    prop_assert_eq!(canonical(&spelled), Ok(tool));
                }
            }

            #[test]
            fn an_mcp_server_segment_never_contains_a_double_underscore(server in segment_chars(), tool in segment_chars()) {
                if let Ok(canonical) = canonical(&format!("mcp__{server}__{tool}")) {
                    let namespace = canonical.as_str().split('/').nth(1).expect("a namespace segment");
                    prop_assert!(!namespace.contains("__"), "{}", canonical);
                    prop_assert!(canonical.as_str().starts_with("mcp/"), "{}", canonical);
                }
            }

            #[test]
            fn the_map_is_injective(left in raw_spelling(), right in raw_spelling()) {
                if let (Ok(a), Ok(b)) = (canonical(&left), canonical(&right)) {
                    prop_assert_eq!(a == b, left == right, "{} vs {}", left, right);
                }
            }
        }
    }
}
