//! The kagent adapter: the server-side derivation the runtime applies
//! to every call the kagent plugins send. Pure — no policy, no state,
//! no runtime calls; the compiler enforces the boundary, since this
//! crate depends only on `appa-runtime-api`.
//!
//! There is no client-side codec here. OpenAPPA owns both ends of the
//! kagent wire, so the plugins (`integrations/kagent/`) speak the
//! canonical hook wire of `appa-runtime-api` directly: one `WireEvent`
//! per `POST /hook`, one `WireDecision` back. What the runtime derives
//! from a call it derives itself, from the raw spelling alone
//! ([`adapter`]): the canonical identity and whether the call is the
//! spawn. Nothing a plugin sends is trusted for either.
//!
//! The plugin spells a tool from the inventory its entrypoint computes,
//! `<prefix>:<rest>`, and this is the bijection onto canonical identity
//! over the spellings it accepts; the adapter's inverse reads it right
//! to left, so the runtime can name a tool to this host without keeping
//! the spelling the call arrived under:
//!
//! | raw spelling | canonical | spawn |
//! |---|---|---|
//! | `appa:execute_remedy_plan` | `appa/execute_remedy_plan`, the runtime's control tool | no |
//! | `mcp:<toolset>/<tool>` | `mcp/<toolset>/<tool>` | no |
//! | `agent:<namespace>/<agent>` | `agent/<namespace>/<agent>` | yes |
//! | `builtin:<name>` | `host/kagent/<name>` | no |
//! | `gate:<name>` | `host/kagent-gate/<name>` | no |
//!
//! Every segment is `[A-Za-z0-9_.-]+`, and a namespace never contains
//! `__`. A spelling outside that domain is refused and the call
//! blocks: an unknown or missing prefix, `mcp:` or `agent:` without
//! its `/`, an empty or ill-formed segment, and `appa:` naming any
//! other tool.
//!
//! A kagent child's words reach its parent through the after-tool
//! callback only, and the child trajectory binds to its spawn through
//! `spawn_binding` at `child_start`, not through an argument the
//! parent spells. So no call names a child by its arguments and the
//! derivation's `names_children` is always empty.

use appa_runtime_api::{Actor, Adapter, AdapterName, CanonicalTool, Derived, ParseRefusal, ProposedCall};

/// The server-side derivation the runtime applies to every kagent call.
pub fn adapter() -> Adapter {
    Adapter {
        name: AdapterName::Kagent,
        derive,
        spell,
    }
}

/// The one tool the `appa:` prefix names.
const CONTROL_TOOL_NAME: &str = "execute_remedy_plan";

/// The control tool as the plugin's inventory spells it.
const CONTROL_TOOL_RAW: &str = "appa:execute_remedy_plan";

/// The inverse of [`derive`]'s mapping table over its range: the wire spelling the plugin's
/// inventory gives one canonical identity, which is what the runtime says where it names a
/// tool to this host. `None` for a canonical id no kagent spelling maps onto — another
/// host's namespace under the `host` family.
fn spell(canonical: &CanonicalTool) -> Option<String> {
    if canonical.is_control() {
        return Some(CONTROL_TOOL_RAW.to_string());
    }
    let mut segments = canonical.as_str().split('/');
    match (segments.next()?, segments.next()?, segments.next()?) {
        ("mcp", toolset, tool) => Some(format!("mcp:{toolset}/{tool}")),
        ("agent", namespace, agent) => Some(format!("agent:{namespace}/{agent}")),
        ("host", "kagent", name) => Some(format!("builtin:{name}")),
        ("host", "kagent-gate", name) => Some(format!("gate:{name}")),
        _ => None,
    }
}

/// The crate-level mapping table. `CanonicalTool::of` refuses an empty segment, a
/// character outside the grammar, and a namespace containing `__`.
fn derive(_: &Actor, call: &ProposedCall) -> Result<Derived, ParseRefusal> {
    let raw = call.tool.as_str();
    let refused = |detail: String| ParseRefusal::Malformed {
        detail: format!("tool {raw:?} is outside the kagent adapter's domain: {detail}"),
    };
    let Some((prefix, rest)) = raw.split_once(':') else {
        return Err(refused(
            "expected mcp:<toolset>/<tool>, agent:<namespace>/<agent>, builtin:<name>, gate:<name>, or appa:execute_remedy_plan"
                .to_string(),
        ));
    };
    let (family, namespace, tool, spawn) = match (prefix, rest.split_once('/')) {
        ("appa", _) if rest == CONTROL_TOOL_NAME => {
            return Ok(Derived {
                canonical: CanonicalTool::control(),
                spawn: false,
                names_children: Vec::new(),
            });
        }
        ("appa", _) => return Err(refused(format!("appa: names one tool, {CONTROL_TOOL_NAME}"))),
        ("mcp", Some((toolset, tool))) => ("mcp", toolset, tool, false),
        ("mcp", None) => return Err(refused("mcp: takes <toolset>/<tool>".to_string())),
        ("agent", Some((namespace, agent))) => ("agent", namespace, agent, true),
        ("agent", None) => return Err(refused("agent: takes <namespace>/<agent>".to_string())),
        ("builtin", _) => ("host", "kagent", rest, false),
        ("gate", _) => ("host", "kagent-gate", rest, false),
        (other, _) => {
            return Err(refused(format!(
                "{other:?} is not a prefix: mcp, agent, builtin, gate, or appa"
            )));
        }
    };
    let canonical = CanonicalTool::of(family, namespace, tool).map_err(|error| refused(error.to_string()))?;
    Ok(Derived {
        canonical,
        spawn,
        names_children: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use appa_runtime_api::TrajectoryId;

    fn derived(tool: &str) -> Result<Derived, ParseRefusal> {
        let actor = Actor {
            root: TrajectoryId("kagent:s1".to_string()),
            child: Some(TrajectoryId("kagent:s1:c1".to_string())),
        };
        (adapter().derive)(
            &actor,
            &ProposedCall {
                tool: tool.to_string(),
                arguments: serde_json::value::to_raw_value(&serde_json::json!({"path": "tasks/c1.output"}))
                    .expect("the fixture serializes"),
            },
        )
    }

    #[test]
    fn the_adapter_serves_kagent() {
        assert_eq!(adapter().name, AdapterName::Kagent);
    }

    #[test]
    fn each_raw_spelling_maps_onto_its_canonical_identity() {
        for (raw, expected, spawn) in [
            (CONTROL_TOOL_RAW, appa_runtime_api::CONTROL_TOOL, false),
            ("mcp:k8s/get_pods", "mcp/k8s/get_pods", false),
            ("mcp:a.b-c/T.o-o_l", "mcp/a.b-c/T.o-o_l", false),
            ("mcp:k8s/a__b", "mcp/k8s/a__b", false),
            ("agent:kagent/log-analyst", "agent/kagent/log-analyst", true),
            ("builtin:memory_persist", "host/kagent/memory_persist", false),
            ("gate:outer", "host/kagent-gate/outer", false),
        ] {
            let derived = derived(raw).unwrap_or_else(|refusal| panic!("{raw} derives: {refusal:?}"));
            assert_eq!(derived.canonical.as_str(), expected, "{raw}");
            assert_eq!(derived.spawn, spawn, "{raw}");
            assert_eq!(derived.canonical.is_control(), raw == CONTROL_TOOL_RAW, "{raw}");
            assert!(derived.names_children.is_empty(), "{raw}: no kagent call names a child");
            assert_eq!(
                (adapter().spell)(&derived.canonical).as_deref(),
                Some(raw),
                "the inverse spells {expected} back as the spelling the plugin sent"
            );
        }
    }

    /// A canonical id no kagent spelling derives to has no kagent spelling.
    #[test]
    fn a_canonical_id_outside_the_range_has_no_host_spelling() {
        for name in ["host/claude-code/Bash", "host/other/x"] {
            let canonical = CanonicalTool::parse(name).expect("the fixture is canonical");
            assert_eq!((adapter().spell)(&canonical), None, "{name}");
        }
    }

    #[test]
    fn a_spelling_outside_the_domain_is_a_named_refusal() {
        for raw in [
            "",
            "k8s_get_pods",
            "execute_remedy_plan",
            "mcp:",
            "mcp:k8s",
            "mcp:/get_pods",
            "mcp:k8s/",
            "mcp:a__b/x",
            "mcp:k8s/a/b",
            "mcp:k8s/get pods",
            "agent:log-analyst",
            "agent:kagent/",
            "agent:a__b/x",
            "builtin:",
            "builtin:a/b",
            "gate:a b",
            "gate:kagent/x",
            "appa:",
            "appa:other",
            "appa:execute_remedy_plan/x",
            "appa:execute_remedy_plan ",
            "other:x/y",
            ":x/y",
            "host:kagent/x",
            "mcp/k8s/get_pods",
            "appa/execute_remedy_plan",
            "mcp__k8s__get_pods",
        ] {
            match derived(raw) {
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

    mod laws {
        use super::*;
        use proptest::prelude::*;

        fn segment_chars() -> impl Strategy<Value = String> {
            "[A-Za-z0-9_.-]{0,10}"
        }

        fn prefix() -> impl Strategy<Value = String> {
            prop_oneof![
                Just("mcp".to_string()),
                Just("agent".to_string()),
                Just("builtin".to_string()),
                Just("gate".to_string()),
                Just("appa".to_string()),
                Just("other".to_string()),
                Just(String::new()),
            ]
        }

        fn raw_spelling() -> impl Strategy<Value = String> {
            prop_oneof![
                (prefix(), segment_chars(), segment_chars())
                    .prop_map(|(prefix, namespace, tool)| format!("{prefix}:{namespace}/{tool}")),
                (prefix(), segment_chars()).prop_map(|(prefix, name)| format!("{prefix}:{name}")),
                segment_chars(),
                Just(CONTROL_TOOL_RAW.to_string()),
            ]
        }

        proptest! {
            #[test]
            fn an_accepted_spelling_parses_back_and_is_control_only_when_registered(raw in raw_spelling()) {
                if let Ok(derived) = derived(&raw) {
                    let canonical = derived.canonical;
                    prop_assert_eq!(CanonicalTool::parse(canonical.as_str()), Ok(canonical.clone()));
                    prop_assert_eq!(canonical.is_control(), raw == CONTROL_TOOL_RAW);
                    prop_assert_eq!(derived.spawn, canonical.as_str().starts_with("agent/"), "{}", canonical);
                    prop_assert!(derived.names_children.is_empty());
                    if let Some(namespace) = canonical.as_str().split('/').nth(1) {
                        prop_assert!(!namespace.contains("__"), "{}", canonical);
                    }
                }
            }

            /// The inverse is total over the derivation's range and returns the exact
            /// spelling the plugin sent, so the runtime never has to keep one.
            #[test]
            fn the_inverse_spells_every_derived_identity_back(raw in raw_spelling()) {
                if let Ok(derived) = derived(&raw) {
                    let spelled = (adapter().spell)(&derived.canonical);
                    prop_assert_eq!(spelled.as_deref(), Some(raw.as_str()));
                }
            }

            #[test]
            fn the_map_is_injective(left in raw_spelling(), right in raw_spelling()) {
                if let (Ok(a), Ok(b)) = (derived(&left), derived(&right)) {
                    prop_assert_eq!(a.canonical == b.canonical, left == right, "{} vs {}", left, right);
                }
            }
        }
    }
}
