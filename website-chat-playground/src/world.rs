//! Which tools exist, decided by the enabled systems — not by the policy.

use std::collections::BTreeSet;
use std::time::Duration;

use appa_runtime::config::{Binding, ExternalBindings};

use crate::systems::System;

use crate::params::{InjectError, tool_parameters};

pub fn system_tools(system: System) -> Vec<String> {
    system.tools().iter().map(|tool| tool.to_string()).collect()
}

/// Every tool the enabled systems provide, in a stable order. One system's
/// checkbox is exactly one system's tools — nothing appears that is not
/// written under a system in the UI.
pub fn expected_tools(enabled: &BTreeSet<System>) -> Vec<String> {
    enabled.iter().copied().flat_map(system_tools).collect()
}

#[derive(Debug)]
pub struct MergedPolicy {
    /// TOML for the loader: the submitted policy, filtered to available tools,
    /// extended with neutral contracts, with argument schemas injected.
    pub toml: String,
    pub defaulted: Vec<String>,
    pub dropped: Vec<String>,
}

pub fn merge_policy(policy: &str, enabled: &BTreeSet<System>) -> Result<MergedPolicy, InjectError> {
    let mut value: toml::Value = toml::from_str(policy)?;
    let available: BTreeSet<String> = expected_tools(enabled).into_iter().collect();

    let mut declared = BTreeSet::new();
    let mut dropped = Vec::new();

    let table = value
        .as_table_mut()
        .ok_or_else(|| InjectError::Schema("the policy is not a TOML table".to_string()))?;
    let mut tools = match table.remove("tool") {
        Some(toml::Value::Array(tools)) => tools,
        Some(other) => {
            table.insert("tool".to_string(), other);
            Vec::new()
        }
        None => Vec::new(),
    };

    // An ordered contract is spelled `tool(arg:pattern)`; availability is the base tool's.
    let base_of = |name: &str| name.split('(').next().unwrap_or(name).to_string();
    let mut wildcard = false;
    tools.retain(|tool| {
        let Some(name) = tool.get("name").and_then(|name| name.as_str()) else {
            return true; // let the loader report a nameless contract
        };
        if name == "*" {
            wildcard = true;
            return true;
        }
        let base = base_of(name);
        if available.contains(&base) {
            declared.insert(base);
            true
        } else {
            dropped.push(name.to_string());
            false
        }
    });

    // Tools the systems provide that the policy is silent about: neutral contract — unless
    // the policy writes a wildcard, which covers exactly those calls; a neutral contract
    // would shadow it (an exact declaration always wins over the wildcard).
    let defaulted: Vec<String> = match wildcard {
        true => Vec::new(),
        false => available
            .iter()
            .filter(|name| !declared.contains(*name))
            .cloned()
            .collect(),
    };
    for name in &defaulted {
        let mut contract = toml::value::Table::new();
        contract.insert("name".to_string(), toml::Value::String(name.clone()));
        contract.insert("delta".to_string(), toml::Value::Table(toml::value::Table::new()));
        tools.push(toml::Value::Table(contract));
    }

    for tool in &mut tools {
        let Some(contract) = tool.as_table_mut() else { continue };
        if contract.contains_key("parameters") {
            continue;
        }
        let Some(name) = contract.get("name").and_then(|name| name.as_str()) else {
            continue;
        };
        // The wildcard carries no metadata by construction; a selector's schema is its base
        // tool's.
        if name == "*" {
            continue;
        }
        if let Some(schema) = tool_parameters(&base_of(name)) {
            let schema = toml::Value::try_from(schema).map_err(|error| InjectError::Schema(error.to_string()))?;
            contract.insert("parameters".to_string(), schema);
        }
    }

    let mut deployment = toml::value::Table::new();
    deployment.insert("dispatch".to_string(), toml::Value::String("enforced".to_string()));
    // Every tool the systems serve is confined — declared, defaulted, or wildcard-covered —
    // so the host can withhold any raw result a sanitizer derivation stands in for.
    deployment.insert(
        "confined_results".to_string(),
        toml::Value::Array(available.iter().cloned().map(toml::Value::String).collect()),
    );
    table.insert("deployment".to_string(), toml::Value::Table(deployment));

    if !tools.is_empty() {
        table.insert("tool".to_string(), toml::Value::Array(tools));
    }

    Ok(MergedPolicy {
        toml: toml::to_string(&value)?,
        defaulted,
        dropped,
    })
}

pub const TOOLS_PATH: &str = "/tools";
pub const AUTHORITY_PATH: &str = "/authority";
pub const SANITIZER_PATH: &str = "/sanitizer";
pub const ANNOTATOR_PATH: &str = "/annotator";
/// The one annotator the playground implements: the email recipient directory.
pub const DIRECTORY_ANNOTATOR: &str = "email-recipient-readers";

const CONSULT_TIMEOUT: Duration = Duration::from_secs(300);
const REVIEW_WINDOW: Duration = Duration::from_secs(365 * 24 * 60 * 60);
const MAX_CONSULT_BYTES: usize = 256 * 1024;

/// Bind every component the visitor's policy registered to this session's
/// own endpoint.
///
/// The policy says what a component may do; the deployment says who
/// performs it, and on this box the deployment is never the
/// visitor's to write. That split is what makes the playground safe to
/// expose: a submitted policy has nowhere to put a URL, so it cannot make
/// this process call one. Every authority is the visitor at the approval
/// desk, every sanitizer is their own model, and the one directory is the
/// service's.
///
/// Enumerating is the whole job: a component the policy registers and this
/// misses would be refused by `Runtime::open` as unbound, which
/// is a contained failure but a failure — a visitor's own policy would not
/// start.
pub fn externals_for(policy: &appa_policy::Config, base: &str) -> ExternalBindings {
    let registry = policy.registry_config();
    let endpoint = |path: String| Binding::Url {
        url: path,
        token_env: None,
    };
    let mut bindings = ExternalBindings::new(CONSULT_TIMEOUT, MAX_CONSULT_BYTES);
    bindings.review_timeout_ms = REVIEW_WINDOW.as_millis() as u64;
    bindings.authorities = registry
        .authorities
        .iter()
        .map(|authority| {
            let name = authority.name.as_str().to_string();
            let url = format!("{base}{AUTHORITY_PATH}/{name}");
            (name, endpoint(url))
        })
        .collect();
    bindings.sanitizers = registry
        .sanitizers
        .iter()
        .filter(|sanitizer| !sanitizer.name.is_attest_schema())
        .map(|sanitizer| {
            let name = sanitizer.name.as_str().to_string();
            let url = format!("{base}{SANITIZER_PATH}/{name}");
            (name, endpoint(url))
        })
        .collect();
    // The playground implements one annotator. Binding only it lets a visitor policy
    // that names any other refuse at open — where the message names the annotator —
    // instead of failing operationally on every call.
    bindings.annotators = policy
        .annotator_names()
        .filter(|name| name.as_str() == DIRECTORY_ANNOTATOR)
        .map(|name| (name.as_str().to_string(), endpoint(format!("{base}{ANNOTATOR_PATH}"))))
        .collect();
    bindings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn systems(list: &str) -> BTreeSet<System> {
        list.split(',')
            .map(|name| System::parse(name.trim()).unwrap())
            .collect()
    }

    #[test]
    fn a_system_contributes_its_own_tools() {
        assert_eq!(
            system_tools(System::Crm),
            vec!["list_customers", "create_customer_data"]
        );
        assert_eq!(system_tools(System::Github), vec!["list_issues", "create_issue"]);
    }

    #[test]
    fn systems_compose_without_extras() {
        assert_eq!(expected_tools(&systems("crm,github")).len(), 4);
    }

    #[test]
    fn an_enabled_system_without_a_contract_still_gets_its_tools() {
        let merged = merge_policy("version = 2\n", &systems("crm")).unwrap();
        assert_eq!(merged.defaulted, vec!["create_customer_data", "list_customers"]);
        assert!(merged.dropped.is_empty());
        let value: toml::Value = toml::from_str(&merged.toml).unwrap();
        assert_eq!(value["tool"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn an_ordered_contract_counts_for_its_base_tool() {
        let merged = merge_policy(
            "version = 2\n[[tool]]\nname = \"list_customers(query:vip-*)\"\ndelta = {}\n",
            &systems("crm"),
        )
        .unwrap();
        assert!(merged.dropped.is_empty(), "the selector's base tool is available");
        assert_eq!(
            merged.defaulted,
            vec!["create_customer_data"],
            "the selector declares list_customers; only the other tool defaults"
        );
        let value: toml::Value = toml::from_str(&merged.toml).unwrap();
        let confined: Vec<&str> = value["deployment"]["confined_results"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|name| name.as_str())
            .collect();
        assert!(
            confined.contains(&"list_customers") && !confined.iter().any(|name| name.contains('(')),
            "coverage names the base tool, never the selector spelling: {confined:?}"
        );
    }

    #[test]
    fn a_wildcard_is_retained_and_covers_the_undeclared_tools() {
        let merged = merge_policy(
            "version = 2\n[[annotator]]\nname = \"acl\"\n[[tool]]\nname = \"*\"\nannotator = \"acl\"\n",
            &systems("crm"),
        )
        .unwrap();
        assert!(merged.dropped.is_empty());
        assert!(
            merged.defaulted.is_empty(),
            "a neutral contract would shadow the wildcard for every available tool"
        );
        let value: toml::Value = toml::from_str(&merged.toml).unwrap();
        let tools = value["tool"].as_array().unwrap();
        assert_eq!(tools.len(), 1, "only the wildcard is declared: {tools:?}");
        let confined: Vec<&str> = value["deployment"]["confined_results"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|name| name.as_str())
            .collect();
        assert_eq!(
            confined,
            vec!["create_customer_data", "list_customers"],
            "wildcard-covered tools stay confined; the wildcard itself is never an entry"
        );
    }

    #[test]
    fn a_contract_keeps_its_terms_and_gains_a_schema() {
        let merged = merge_policy(
            r#"
version = 2
[[tool]]
name  = "list_customers"
delta = { audience = ["crm"] }
"#,
            &systems("crm"),
        )
        .unwrap();
        let value: toml::Value = toml::from_str(&merged.toml).unwrap();
        let contract = value["tool"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"].as_str() == Some("list_customers"))
            .unwrap();
        assert!(contract["delta"]["audience"].as_array().is_some(), "terms survive");
        assert!(contract.get("parameters").is_some(), "schema injected");
        assert_eq!(merged.defaulted, vec!["create_customer_data"]);
    }

    #[test]
    fn a_contract_for_a_disabled_system_is_dropped_and_reported() {
        let merged = merge_policy(
            r#"
version = 2
[[tool]]
name  = "list_issues"
delta = {}
"#,
            &systems("crm"),
        )
        .unwrap();
        assert_eq!(merged.dropped, vec!["list_issues"]);
        let value: toml::Value = toml::from_str(&merged.toml).unwrap();
        let names: Vec<&str> = value["tool"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert!(!names.contains(&"list_issues"));
    }

    #[test]
    fn no_systems_leaves_no_tools() {
        let merged = merge_policy("version = 2\n", &BTreeSet::new()).unwrap();
        assert!(merged.defaulted.is_empty());
        let value: toml::Value = toml::from_str(&merged.toml).unwrap();
        assert!(value.get("tool").is_none());
    }
}
