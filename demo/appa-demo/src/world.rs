//! Which tools exist, decided by the enabled systems — not by the policy.

use std::collections::BTreeSet;

use crate::systems::System;

use crate::params::{InjectError, tool_parameters};

/// The only dynamic-resolver URL a visitor policy may name. Session creation
/// replaces it with the session's loopback endpoint after the SSRF lint.
pub const DYNAMIC_RESOLVER_PLACEHOLDER: &str = "http://demo.invalid/dynamic-resolver";

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

    if !tools.is_empty() {
        table.insert("tool".to_string(), toml::Value::Array(tools));
    }

    Ok(MergedPolicy {
        toml: toml::to_string(&value)?,
        defaulted,
        dropped,
    })
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Rebound {
    pub authorities: usize,
    pub sanitizers: usize,
    pub dynamic_resolvers: usize,
}

/// Replace the implementations the harness hosts with this session's own endpoints: `hitl`
/// authorities with the approval desk, `hosted` sanitizers with the derivation endpoint, and the
/// reserved dynamic-resolver placeholder with the directory endpoint.
pub fn bind_hosted(
    merged_toml: &str,
    authority_url: &str,
    sanitizer_base: &str,
    dynamic_resolver_url: &str,
) -> Result<(String, Rebound), toml::de::Error> {
    let mut value: toml::Value = toml::from_str(merged_toml)?;
    let mut rebound = Rebound::default();

    let resolver = |url: String, timeout_ms: i64| {
        let mut resolver = toml::value::Table::new();
        resolver.insert("url".to_string(), toml::Value::String(url));
        resolver.insert("timeout_ms".to_string(), toml::Value::Integer(timeout_ms));
        let mut implementation = toml::value::Table::new();
        implementation.insert("resolver".to_string(), toml::Value::Table(resolver));
        toml::Value::Table(implementation)
    };
    const UNBOUNDED_MS: i64 = 365 * 24 * 60 * 60 * 1000;
    const DERIVATION_MS: i64 = 300_000;
    let builtin_is = |entry: &toml::Value, name: &str| {
        entry
            .get("implementation")
            .and_then(|imp| imp.get("builtin"))
            .and_then(|found| found.as_str())
            == Some(name)
    };

    if let Some(authorities) = value.get_mut("authority").and_then(|list| list.as_array_mut()) {
        for authority in authorities {
            if !builtin_is(authority, "hitl") {
                continue;
            }
            let Some(table) = authority.as_table_mut() else {
                continue;
            };
            table.insert(
                "implementation".to_string(),
                resolver(authority_url.to_string(), UNBOUNDED_MS),
            );
            rebound.authorities += 1;
        }
    }

    if let Some(sanitizers) = value.get_mut("sanitizer").and_then(|list| list.as_array_mut()) {
        for sanitizer in sanitizers {
            if !builtin_is(sanitizer, "hosted") {
                continue;
            }
            let Some(name) = sanitizer.get("name").and_then(|name| name.as_str()).map(str::to_string) else {
                continue;
            };
            let Some(table) = sanitizer.as_table_mut() else {
                continue;
            };
            table.insert(
                "implementation".to_string(),
                resolver(format!("{sanitizer_base}/{name}"), DERIVATION_MS),
            );
            rebound.sanitizers += 1;
        }
    }

    if let Some(resolvers) = value.get_mut("dynamic_resolver").and_then(|list| list.as_array_mut()) {
        for dynamic_resolver in resolvers {
            let Some(url) = dynamic_resolver
                .get_mut("resolver")
                .and_then(|resolver| resolver.get_mut("url"))
            else {
                continue;
            };
            if url.as_str() != Some(DYNAMIC_RESOLVER_PLACEHOLDER) {
                continue;
            }
            *url = toml::Value::String(dynamic_resolver_url.to_string());
            rebound.dynamic_resolvers += 1;
        }
    }

    Ok((
        toml::to_string(&value).expect("a just-parsed policy re-serializes"),
        rebound,
    ))
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
    fn only_the_harness_hosted_builtins_are_rebound() {
        let policy = r#"
version = 1
[[dynamic_resolver]]
name = "directory"
resolver = { url = "http://demo.invalid/dynamic-resolver", timeout_ms = 5000 }

[[authority]]
name = "treasurer"
mandate.attends = ["human-approval"]
implementation.builtin = "hitl"

[[authority]]
name = "auto"
mandate.can_waive = ["egress"]
implementation.builtin = "approve"

[[sanitizer]]
name = "digest"
on = ["tool_output"]
mandate.trust = { from = "suspicious", to = "trusted" }
implementation.builtin = "hosted"

[[sanitizer]]
name = "scrub"
on = ["tool_output"]
mandate.audience = { from = { includes = ["internal"] }, to = { exactly = ["public"] } }
implementation.builtin = "redact-numbers"
"#;
        let (bound, rebound) = bind_hosted(
            policy,
            "http://127.0.0.1:5555/authority",
            "http://127.0.0.1:5555/sanitizer",
            "http://127.0.0.1:5555/dynamic-resolver",
        )
        .unwrap();
        assert_eq!(
            rebound,
            Rebound {
                authorities: 1,
                sanitizers: 1,
                dynamic_resolvers: 1,
            }
        );
        let value: toml::Value = toml::from_str(&bound).unwrap();

        let dynamic_resolvers = value["dynamic_resolver"].as_array().unwrap();
        assert_eq!(
            dynamic_resolvers[0]["resolver"]["url"].as_str(),
            Some("http://127.0.0.1:5555/dynamic-resolver")
        );

        let authorities = value["authority"].as_array().unwrap();
        assert_eq!(
            authorities[0]["implementation"]["resolver"]["url"].as_str(),
            Some("http://127.0.0.1:5555/authority")
        );
        assert!(authorities[0]["implementation"].get("builtin").is_none());
        assert_eq!(authorities[1]["implementation"]["builtin"].as_str(), Some("approve"));

        let sanitizers = value["sanitizer"].as_array().unwrap();
        assert_eq!(
            sanitizers[0]["implementation"]["resolver"]["url"].as_str(),
            Some("http://127.0.0.1:5555/sanitizer/digest")
        );
        assert_eq!(
            sanitizers[1]["implementation"]["builtin"].as_str(),
            Some("redact-numbers")
        );
    }

    #[test]
    fn no_systems_leaves_no_tools() {
        let merged = merge_policy("version = 1\n", &BTreeSet::new()).unwrap();
        assert!(merged.defaulted.is_empty());
        let value: toml::Value = toml::from_str(&merged.toml).unwrap();
        assert!(value.get("tool").is_none());
    }
}
