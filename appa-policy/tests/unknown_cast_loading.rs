//! Load outcomes of the Unknown surface: what a declared tool's omitted `delta` means, a
//! pending-cast dimension, the reserved `"unknown"` token, and the shape of a `[[cast]]` — pinned
//! at the TOML entry point.

use appa_engine::contract::{Delta, RequirementSlot, ToolContract};
use appa_engine::label::{Dim, Dimension};
use appa_engine::registry::LoadError;
use appa_policy::{Config, ConfigError};

/// A tool contract under the default trust chain, with the given TOML lines after its name.
fn tool_policy(name: &str, body: &str) -> String {
    format!("version = 1\n\n[[tool]]\nname = \"{name}\"\n{body}\n")
}

/// The one contract a policy here registers under `name`; these policies never order several.
fn contract<'a>(config: &'a Config, name: &str) -> &'a ToolContract {
    config
        .registry()
        .tools()
        .find(|tool| tool.name.as_str() == name)
        .unwrap_or_else(|| panic!("{name} registers"))
}

/// A declared tool with a label requirement and no `delta` used to be refused: its output was
/// Unknown forever, so the requirement could never hold. Declaring the tool now says the
/// deployment knows it, its unwritten dimensions restrict nothing, and the requirement is
/// ordinary.
#[test]
fn a_declared_tool_with_a_label_requirement_and_no_delta_loads_neutral() {
    let cases = [
        ("trust floor", "requires = { trust = \"trusted\" }"),
        ("audience cap", "requires = { audience = { within = [\"public\"] } }"),
        (
            "audience includes",
            "requires = { audience = { contains = [\"alice\"] } }",
        ),
    ];
    for (case, requires) in cases {
        let policy = tool_policy("send", requires);
        let config = Config::from_toml_str(&policy)
            .unwrap_or_else(|error| panic!("a declared tool with a {case} requirement loads: {error}"));
        assert_eq!(
            contract(&config, "send").delta,
            Delta::NONE,
            "{case}: an omitted delta is neutral"
        );
    }
}

#[test]
fn a_declared_tool_with_history_or_attention_requirements_loads() {
    let cases = [
        (
            "history",
            "requires = { effects = { contains = [\"backup.completed\"], excludes = [\"email.sent\"] } }",
        ),
        ("attention", "requires = { attention = [\"operator-signoff\"] }"),
    ];
    for (case, requires) in cases {
        let policy = tool_policy("send", requires);
        let config = Config::from_toml_str(&policy)
            .unwrap_or_else(|error| panic!("a declared tool with a {case} requirement loads: {error}"));
        let contract = contract(&config, "send");
        assert_eq!(contract.delta, Delta::NONE, "{case}: an omitted delta is neutral");
    }
}

/// One cast answers one dimension, so a confined result point cannot wait on two. Unconfined,
/// nothing waits and both dimensions travel Unknown.
#[test]
fn a_delta_unknown_in_both_dimensions_is_refused_only_at_a_confined_result_point() {
    let policy = format!(
        "{}\n[deployment]\nconfined_results = [\"scan\"]\n",
        tool_policy("scan", "delta = { trust = \"unknown\", audience = \"unknown\" }")
    );
    assert!(matches!(
        Config::from_toml_str(&policy),
        Err(ConfigError::Registry(LoadError::DualPendingCast(tool))) if tool == "scan"
    ));

    let unconfined = tool_policy("scan", "delta = { trust = \"unknown\", audience = \"unknown\" }");
    let config = Config::from_toml_str(&unconfined).expect("an unconfined result point may leave both Unknown");
    assert_eq!(contract(&config, "scan").pending_cast_dim(), Some(Dimension::Trust));
}

#[test]
fn a_pending_cast_dimension_with_a_requirement_on_it_is_refused() {
    let policy = format!(
        "{}\n[deployment]\nconfined_results = [\"scan\"]\n",
        tool_policy(
            "scan",
            "delta = { trust = \"unknown\" }\nrequires = { trust = \"trusted\" }"
        )
    );
    assert!(matches!(
        Config::from_toml_str(&policy),
        Err(ConfigError::Registry(LoadError::PendingCastWithRequirement { tool, dimension: Dimension::Trust }))
            if tool == "scan"
    ));
}

/// `"unknown"` in a requirement slot is the lazy form: the policy states no requirement, and a
/// cast establishes it at the proposal. Each slot is its own monad — the others stay as written,
/// and an omitted slot is still empty.
#[test]
fn an_unknown_requirement_slot_loads_and_stays_unknown() {
    let all = Config::from_toml_str(&tool_policy(
        "send",
        "requires = { trust = \"unknown\", audience = \"unknown\", attention = \"unknown\" }",
    ))
    .expect("every requirement slot may be unknown");
    let requires = &contract(&all, "send").requires;
    assert_eq!(requires.label.trust_floor, Some(Dim::Unknown));
    assert_eq!(requires.label.audience, Dim::Unknown);
    assert_eq!(requires.attention, Dim::Unknown);
    assert_eq!(
        requires.unknown_slots().collect::<Vec<_>>(),
        [
            RequirementSlot::Trust,
            RequirementSlot::Audience,
            RequirementSlot::Attention
        ]
    );

    let one = Config::from_toml_str(&tool_policy("send", "requires = { trust = \"unknown\" }"))
        .expect("one unknown slot loads");
    let requires = &contract(&one, "send").requires;
    assert_eq!(requires.label.trust_floor, Some(Dim::Unknown));
    assert_eq!(requires.label.audience, Dim::Known(vec![]));
    assert_eq!(requires.attention, Dim::Known(vec![]));
    assert_eq!(requires.unknown_slots().collect::<Vec<_>>(), [RequirementSlot::Trust]);

    assert!(matches!(
        Config::from_toml_str(&tool_policy("send", "requires = { audience = \"nobody\" }")),
        Err(ConfigError::BadAudience { .. })
    ));
}

/// `"unknown"` names the dimension the contract does not describe. Confining the result point
/// is a separate deployment choice: it asks for a cast before the model reads the result, and
/// without it the value carries its Unknown to whichever sink consumes it.
#[test]
fn an_unknown_dimension_loads_whether_or_not_its_result_point_is_confined() {
    let unconfined = tool_policy("scan", "delta = { trust = \"unknown\" }");
    let config = Config::from_toml_str(&unconfined).expect("an unconfined Unknown dimension loads");
    assert_eq!(contract(&config, "scan").pending_cast_dim(), Some(Dimension::Trust));
    assert!(
        !config
            .registry()
            .profile()
            .confines_result(&contract(&config, "scan").name)
    );

    let confined = format!("{unconfined}\n[deployment]\nconfined_results = [\"scan\"]\n");
    let config = Config::from_toml_str(&confined).expect("a confined pending-cast tool loads");
    let contract = contract(&config, "scan");
    assert_eq!(contract.pending_cast_dim(), Some(Dimension::Trust));
    assert!(config.registry().profile().confines_result(&contract.name));
}

/// `delta = {}` writes out what an omitted `delta` already says. The two spellings are one
/// contract, and Unknown is reached only by asking for it: `"unknown"` on a dimension, or a
/// resolver that owns one.
#[test]
fn an_empty_delta_and_an_omitted_delta_load_as_the_same_contract() {
    let omitted = Config::from_toml_str(&tool_policy("read", "")).expect("a tool with no delta loads");
    let neutral = Config::from_toml_str(&tool_policy("read", "delta = {}")).expect("a neutral tool loads");

    assert_eq!(contract(&omitted, "read").delta, Delta::NONE);
    assert_eq!(contract(&neutral, "read").delta, contract(&omitted, "read").delta);
}

#[test]
fn a_resolver_cast_declares_its_audience_cap() {
    // `constant` xor `resolver` is pinned beside the loader's unit tests; the ceiling's shape is
    // structural, so an omitted cap never reaches the semantic conversions.
    let cases = [
        ("no audience", "resolver = { may_cast = { trust = [\"trusted\"] } }"),
        (
            "audience without cap",
            "resolver = { may_cast = { trust = [\"trusted\"], audience = {} } }",
        ),
    ];
    for (case, resolver) in cases {
        let policy = format!("version = 1\n\n[[cast]]\nname = \"classifier\"\n{resolver}\n");
        assert!(
            matches!(Config::from_toml_str(&policy), Err(ConfigError::Parse(_))),
            "a resolver cast with {case} must be refused"
        );
    }
}
