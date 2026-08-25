//! Load outcomes of the Unknown surface: an omitted `delta`, a pending-cast dimension, the
//! reserved `"unknown"` token, and the shape of a `[[cast]]` — pinned at the TOML entry point.

use appa_engine::contract::{Delta, ToolContract};
use appa_engine::label::Dimension;
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

#[test]
fn an_unannotated_tool_with_a_label_requirement_is_refused() {
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
        assert!(
            matches!(
                Config::from_toml_str(&policy),
                Err(ConfigError::Registry(LoadError::UnannotatedWithLabelRequirement(tool))) if tool == "send"
            ),
            "an unannotated tool with a {case} requirement must be refused"
        );
    }
}

#[test]
fn an_unannotated_tool_with_history_or_attention_requirements_loads() {
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
            .unwrap_or_else(|error| panic!("an unannotated tool with a {case} requirement loads: {error}"));
        let contract = contract(&config, "send");
        assert_eq!(contract.delta, None, "{case}: the tool stays unannotated");
    }
}

#[test]
fn a_delta_pending_in_both_dimensions_is_refused() {
    let policy = format!(
        "{}\n[deployment]\nconfined_results = [\"scan\"]\n",
        tool_policy("scan", "delta = { trust = \"unknown\", audience = \"unknown\" }")
    );
    assert!(matches!(
        Config::from_toml_str(&policy),
        Err(ConfigError::Registry(LoadError::DualPendingCast(tool))) if tool == "scan"
    ));
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

#[test]
fn a_pending_cast_tool_loads_only_when_its_result_is_confined() {
    let unconfined = tool_policy("scan", "delta = { trust = \"unknown\" }");
    assert!(matches!(
        Config::from_toml_str(&unconfined),
        Err(ConfigError::Registry(LoadError::PendingCastUnconfined { tool })) if tool == "scan"
    ));

    let confined = format!("{unconfined}\n[deployment]\nconfined_results = [\"scan\"]\n");
    let config = Config::from_toml_str(&confined).expect("a confined pending-cast tool loads");
    let contract = contract(&config, "scan");
    assert_eq!(contract.pending_cast_dim(), Some(Dimension::Trust));
}

#[test]
fn an_empty_delta_and_an_omitted_delta_load_as_different_contracts() {
    let omitted = Config::from_toml_str(&tool_policy("read", "")).expect("an unannotated tool loads");
    let neutral = Config::from_toml_str(&tool_policy("read", "delta = {}")).expect("a neutral tool loads");

    let omitted = contract(&omitted, "read");
    let neutral = contract(&neutral, "read");
    assert_eq!(omitted.delta, None);
    assert_eq!(neutral.delta, Some(Delta::NONE));
    assert_ne!(omitted.delta, neutral.delta);
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
