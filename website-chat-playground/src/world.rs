//! Which tools exist, decided by the enabled systems — not by the policy.

use std::collections::BTreeSet;
use std::time::Duration;

use appa_runtime_v2::config::{Endpoint, Externals, Implementation};

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

    tools.retain(|tool| {
        let Some(name) = tool.get("name").and_then(|name| name.as_str()) else {
            return true; // let the loader report a nameless contract
        };
        if available.contains(name) {
            declared.insert(name.to_string());
            true
        } else {
            dropped.push(name.to_string());
            false
        }
    });

    // Tools the systems provide that the policy is silent about: neutral contract.
    let defaulted: Vec<String> = available
        .iter()
        .filter(|name| !declared.contains(*name))
        .cloned()
        .collect();
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
        if let Some(schema) = tool_parameters(name) {
            let schema = toml::Value::try_from(schema).map_err(|error| InjectError::Schema(error.to_string()))?;
            contract.insert("parameters".to_string(), schema);
        }
    }

    let mut deployment = toml::value::Table::new();
    deployment.insert("dispatch".to_string(), toml::Value::String("enforced".to_string()));
    deployment.insert(
        "confined_results".to_string(),
        toml::Value::Array(tools.iter().filter_map(|tool| tool.get("name").cloned()).collect()),
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
pub const DYNAMIC_RESOLVER_PATH: &str = "/dynamic-resolver";
pub const MEMBERSHIP_PATH: &str = "/membership";

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
pub fn externals_for(policy: &appa_policy::Config, base: &str) -> Externals {
    let registry = policy.registry_config();
    let endpoint = |path: String| Implementation::Resolver(Endpoint { url: path, token: None });
    Externals {
        timeout: CONSULT_TIMEOUT,
        review_timeout: REVIEW_WINDOW,
        max_body_bytes: MAX_CONSULT_BYTES,
        authorities: registry
            .authorities
            .iter()
            .map(|authority| {
                let name = authority.name.as_str().to_string();
                let url = format!("{base}{AUTHORITY_PATH}/{name}");
                (name, endpoint(url))
            })
            .collect(),
        sanitizers: registry
            .sanitizers
            .iter()
            .map(|sanitizer| {
                let name = sanitizer.name.as_str().to_string();
                let url = format!("{base}{SANITIZER_PATH}/{name}");
                (name, endpoint(url))
            })
            .collect(),
        dynamic: Some(Endpoint {
            url: format!("{base}{DYNAMIC_RESOLVER_PATH}"),
            token: None,
        }),
        membership: Some(Endpoint {
            url: format!("{base}{MEMBERSHIP_PATH}"),
            token: None,
        }),
    }
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
        let merged = merge_policy("version = 1\n", &systems("crm")).unwrap();
        assert_eq!(merged.defaulted, vec!["create_customer_data", "list_customers"]);
        assert!(merged.dropped.is_empty());
        let value: toml::Value = toml::from_str(&merged.toml).unwrap();
        assert_eq!(value["tool"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_contract_keeps_its_terms_and_gains_a_schema() {
        let merged = merge_policy(
            r#"
version = 1
[[tool]]
name  = "list_customers"
delta = { audience = { exactly = ["crm"] } }
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
        assert!(contract["delta"]["audience"].get("exactly").is_some(), "terms survive");
        assert!(contract.get("parameters").is_some(), "schema injected");
        assert_eq!(merged.defaulted, vec!["create_customer_data"]);
    }

    #[test]
    fn a_contract_for_a_disabled_system_is_dropped_and_reported() {
        let merged = merge_policy(
            r#"
version = 1
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
        let merged = merge_policy("version = 1\n", &BTreeSet::new()).unwrap();
        assert!(merged.defaulted.is_empty());
        let value: toml::Value = toml::from_str(&merged.toml).unwrap();
        assert!(value.get("tool").is_none());
    }
}
