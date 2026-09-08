//! The policy reference's TOML examples are held to the loader.
//!
//! `website/content/docs/contracts.md` is a golden file: what it shows a reader typing has to
//! be what this crate accepts. Nothing else in the test suite reads it, so a dialect change
//! that landed in the loader and not in the guide would otherwise ship unnoticed.

use std::path::PathBuf;

use appa_policy::Config;

fn policy_reference() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crate sits one level under the repository root")
        .join("website/content/docs/contracts.md");
    std::fs::read_to_string(path).expect("the policy reference is readable")
}

/// Every fenced TOML block, in document order.
fn toml_fences(reference: &str) -> Vec<&str> {
    let mut fences = Vec::new();
    let mut rest = reference;
    while let Some((_, after)) = rest.split_once("```toml\n") {
        let (fence, tail) = after.split_once("```").expect("a fenced block closes");
        fences.push(fence);
        rest = tail;
    }
    fences
}

/// Extract the policy from deployment examples. The reference also shows a
/// selector line and an authority-permits table separately from their context.
fn as_policy(mut table: toml::Table) -> Option<String> {
    assert!(
        table
            .keys()
            .all(|key| matches!(key.as_str(), "policy" | "externals" | "include" | "name"))
    );
    let mut policy = match table.remove("policy") {
        Some(toml::Value::Table(policy)) => policy,
        Some(_) => panic!("policy must be a table"),
        None if table.contains_key("name") => {
            toml::Table::from_iter([("tool".into(), toml::Value::Array(vec![toml::Value::Table(table)]))])
        }
        None => return None, // Deployment bindings only; checked as TOML above.
    };
    if let Some(toml::Value::Table(authority)) = policy.get("authority") {
        let mut authority = authority.clone();
        authority.insert("name".into(), toml::Value::String("example".into()));
        policy.insert(
            "authority".into(),
            toml::Value::Array(vec![toml::Value::Table(authority)]),
        );
    }
    policy.entry("version").or_insert(toml::Value::Integer(2));
    if policy.len() == 2 && policy.contains_key("deployment") {
        // The deployment-only example refers to the ticket tool introduced above.
        let context: toml::Table = "[[tool]]\nname = 'get_ticket_from_crm'\n".parse().unwrap();
        policy.insert("tool".into(), context["tool"].clone());
    }
    // A standalone sanitizer-permissions example needs a confined result on
    // which to operate; complete examples declare their own deployment table.
    if policy.contains_key("sanitizer") && !policy.contains_key("deployment") {
        let context: toml::Table =
            "[deployment]\nconfined_results = ['example_read']\n[[tool]]\nname = 'example_read'\n"
                .parse()
                .unwrap();
        policy.insert("deployment".into(), context["deployment"].clone());
        policy.entry("tool").or_insert(context["tool"].clone());
    }
    // The wildcard example explicitly refers readers to the annotator section
    // for this separate declaration. Supply that declared surrounding context.
    if policy.get("tool").and_then(toml::Value::as_array).is_some_and(|tools| {
        tools
            .iter()
            .any(|tool| tool.get("annotator").and_then(toml::Value::as_str) == Some("classify_unknown_tool"))
    }) {
        let context: toml::Table =
            "[[annotator]]\nname = 'classify_unknown_tool'\nranks = []\naudiences = []\nmarks = []\neffects = []\n"
                .parse()
                .unwrap();
        policy.insert("annotator".into(), context["annotator"].clone());
    }
    Some(toml::to_string(&policy).unwrap())
}

#[test]
fn every_toml_fence_in_the_policy_reference_loads() {
    let reference = policy_reference();
    let fences = toml_fences(&reference);
    assert!(!fences.is_empty(), "the policy reference shows TOML examples");
    for fence in fences {
        let table: toml::Table = fence
            .parse()
            .unwrap_or_else(|error| panic!("a fence is not valid TOML: {error}\n{fence}"));
        let Some(policy) = as_policy(table) else { continue };
        if let Err(error) = Config::from_toml_str(&policy) {
            panic!("a policy example does not load: {error}\n{policy}");
        }
    }
}
