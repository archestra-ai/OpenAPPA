//! Load outcomes of tool declarations at the TOML entry point: what a declared tool's
//! omitted `delta` means, and that requirement-only declarations load.

use appa_engine::contract::{Delta, ToolAnnotation};
use appa_policy::Config;

/// A tool declaration under the default trust chain, with the given TOML lines after its name.
fn tool_policy(name: &str, body: &str) -> String {
    format!("version = 2\n\n[[tool]]\nname = \"{name}\"\n{body}\n")
}

/// The one annotation a policy here registers under `name`; these policies never order several,
/// and every tool here is statically declared.
fn contract<'a>(config: &'a Config, name: &str) -> &'a ToolAnnotation {
    config
        .engine()
        .registry()
        .tools()
        .find(|tool| tool.name().as_str() == name)
        .unwrap_or_else(|| panic!("{name} registers"))
        .declared()
        .unwrap_or_else(|| panic!("{name} is declared"))
}

/// A declared tool with a label requirement and no `delta` used to be refused: its output was
/// Unknown forever, so the requirement could never hold. Declaring the tool now says the
/// deployment knows it, its unwritten dimensions restrict nothing, and the requirement is
/// ordinary.
#[test]
fn a_declared_tool_with_a_label_requirement_and_no_delta_loads_neutral() {
    let cases = [
        ("trust floor", "requires = { trust = \"trusted\" }"),
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

/// The `jev` builtin labels a call and never emits, so a wildcard it serves records no effect
/// a prior can wait for; another Annotator's omitted effect bound admits every policy effect.
#[test]
fn a_jev_annotator_emits_nothing_a_prior_can_wait_for() {
    use appa_engine::fact::EffectKind;
    use appa_engine::registry::{RequirementKind, UnsatisfiableRequirement};
    use appa_engine::value::ToolName;

    let policy = |builtin: &str| {
        format!(
            "version = 2\n\n[[annotator]]\nname = \"classifier\"\n{builtin}\n\n\
             [[tool]]\nname = \"*\"\nannotator = \"classifier\"\n\n\
             [[tool]]\nname = \"deploy\"\nrequires = {{ effects = {{ contains = [\"backup.completed\"] }} }}\n"
        )
    };
    let unsatisfiable = |builtin| {
        Config::from_toml_str(&policy(builtin))
            .expect("the policy loads")
            .unsatisfiable_requirements()
    };

    assert_eq!(
        unsatisfiable("builtin = \"jev\""),
        [UnsatisfiableRequirement {
            tool: ToolName::new("deploy"),
            requirement: RequirementKind::Prior(EffectKind::new("backup.completed")),
        }]
    );
    assert_eq!(unsatisfiable(""), []);
}

/// Selection is first match wins, so a rule an earlier selector always selects first states no
/// requirement; a selector with another matcher leaves the rule its calls.
#[test]
fn a_rule_an_earlier_selector_always_selects_first_states_no_requirement() {
    use appa_engine::names::MarkName;
    use appa_engine::registry::{RequirementKind, UnsatisfiableRequirement};
    use appa_engine::value::ToolName;

    let unsatisfiable = |selector: &str| {
        Config::from_toml_str(&format!(
            "version = 2\n\n[[tool]]\nname = \"mcp/*/read({selector})\"\n\n\
             [[tool]]\nname = \"mcp/demo/read(path:private*)\"\nrequires = {{ attention = [\"signoff\"] }}\n"
        ))
        .expect("the policy loads")
        .unsatisfiable_requirements()
    };

    assert_eq!(unsatisfiable("path:private*"), []);
    assert_eq!(
        unsatisfiable("path:public*"),
        [UnsatisfiableRequirement {
            tool: ToolName::new("mcp/demo/read"),
            requirement: RequirementKind::Attention(MarkName::new("signoff")),
        }]
    );
}
