//! Resolve authored names without treating a policy as a tool inventory.
//!
//! This module makes no network requests and grants no permission. Hosts supply facts;
//! the same resolver is used by preflight and by served-policy compilation.

use std::collections::{BTreeMap, BTreeSet};

use appa_runtime_api::inventory::{InventorySource, InventoryStatus, ToolInventory};
use appa_runtime_api::{Adapter, AdapterName, CanonicalTool};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum ToolStatus {
    Valid,
    Invalid { reason: String },
    Unknown { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCheck {
    pub tool: String,
    #[serde(flatten)]
    pub status: ToolStatus,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ValidationReport {
    pub tools: Vec<ToolCheck>,
    pub sources: Vec<InventorySource>,
    pub errors: Vec<String>,
    pub diagnostics: Vec<String>,
    pub inventory_complete: bool,
    pub tools_may_change: bool,
    pub wildcard: bool,
    /// Previously accepted identities in this actor's scope, including names
    /// omitted from later listings. Hosts use these to isolate invalid additions.
    #[serde(default)]
    pub accepted_tools: Vec<appa_runtime_api::inventory::ObservedTool>,
    /// Whether this actor has opened, independently of an empty tool catalogue.
    #[serde(default)]
    pub actor_opened: bool,
}

impl ValidationReport {
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty()
            && !self
                .tools
                .iter()
                .any(|tool| matches!(tool.status, ToolStatus::Invalid { .. }))
    }

    pub fn summary(&self) -> String {
        let count = |status: fn(&ToolStatus) -> bool| self.tools.iter().filter(|tool| status(&tool.status)).count();
        format!(
            "tools: {} valid, {} invalid, {} unknown; inventory: {}; new tools: {}; wildcard: {}",
            count(|status| matches!(status, ToolStatus::Valid)),
            count(|status| matches!(status, ToolStatus::Invalid { .. })),
            count(|status| matches!(status, ToolStatus::Unknown { .. })),
            if self.inventory_complete {
                "complete snapshot"
            } else {
                "partial or unavailable"
            },
            if self.tools_may_change {
                "possible"
            } else {
                "not reported"
            },
            if self.wildcard {
                "present (ordinary tools only)"
            } else {
                "absent"
            },
        )
    }
}

pub(crate) fn valid_host_trajectory_id(id: &str) -> bool {
    !id.is_empty() && !id.chars().any(char::is_control)
}

/// Read-only host preflight on the same boundary as /hook. A successful request
/// returns a report even when its tools are invalid; only malformed requests or
/// unavailable pinned policies are HTTP failures. Hook admission checks again.
pub fn answer(runtime: &crate::api::Runtime, adapter: Adapter, body: &[u8]) -> (u16, serde_json::Value) {
    use appa_runtime_api::inventory::{MAX_INVENTORY_BYTES, ValidationRequest};
    let error = |status, message: &str| (status, serde_json::json!({"error": message}));
    if body.len() > MAX_INVENTORY_BYTES {
        return error(413, "validation request exceeds its size limit");
    }
    let request: ValidationRequest = match serde_json::from_slice(body) {
        Ok(request) => request,
        Err(_) => return error(400, "invalid validation request"),
    };
    if request.protocol != appa_runtime_api::PROTOCOL || request.adapter != adapter.name {
        return error(409, "validation protocol or adapter does not match this runtime");
    }
    if [&request.root_id, &request.child_id]
        .into_iter()
        .flatten()
        .any(|id| !valid_host_trajectory_id(id))
        || (request.child_id.is_some() && request.root_id.is_none())
    {
        return error(400, "root_id must be a nonempty host trajectory ID");
    }
    let actor = request.root_id.as_deref().map(|id| {
        let root = adapter.name.root(id);
        let child = request
            .child_id
            .as_ref()
            .map(|child| appa_runtime_api::TrajectoryId(format!("{}:{child}", root.0)));
        appa_runtime_api::Actor { root, child }
    });
    match runtime.preflight_inventory(actor.as_ref(), adapter, &request.inventory) {
        Ok(report) => (
            200,
            serde_json::to_value(report).expect("a validation report serializes"),
        ),
        Err(crate::api::EventError::UnknownTrajectory) => error(404, "the requested family has not opened"),
        Err(_) => error(409, "the pinned policy could not be validated"),
    }
}

pub struct ResolvedPolicy {
    pub policy: toml::Value,
    pub report: ValidationReport,
}

fn bare(name: &str) -> (&str, &str) {
    match name.find('(') {
        Some(index) => (&name[..index], &name[index..]),
        None => (name, ""),
    }
}

fn rule_matches(rule: &str, canonical: &str) -> bool {
    rule == canonical
        || rule.strip_prefix("mcp/*/").is_some_and(|leaf| {
            canonical
                .strip_prefix("mcp/")
                .and_then(|rest| rest.split_once('/'))
                .is_some_and(|(_, tool)| tool == leaf)
        })
}

fn native_variants(name: &str) -> Vec<String> {
    let (base, selector) = bare(name);
    match base.strip_prefix("mcp/*/") {
        Some(leaf) => vec![
            name.to_string(),
            format!("host/kagent/{leaf}{selector}"),
            format!("host/kagent-gate/{leaf}{selector}"),
        ],
        None => vec![name.to_string()],
    }
}

/// Resolve precise native names without an inventory. Short kagent names remain
/// server-independent rules; observations establish identity, not rule contents.
pub fn precise_name(name: &str, adapter: Adapter) -> Option<CanonicalTool> {
    if let Ok(canonical) = CanonicalTool::parse(name) {
        return Some(canonical);
    }
    if name == "*" {
        return None;
    }
    if let Ok(derived) = (adapter.derive)(name) {
        return Some(derived.canonical);
    }
    if adapter.name == AdapterName::Kagent
        && let Some((namespace, agent)) = name.split_once("__NS__")
    {
        return CanonicalTool::of("agent", &namespace.replace('_', "-"), &agent.replace('_', "-")).ok();
    }
    None
}

/// `server_aliases` belongs to deployment configuration, never to a battery. Its values
/// are configured connection identities, not DNS/provider guesses.
pub fn resolve(
    policy: &toml::Value,
    adapter: Adapter,
    inventory: &ToolInventory,
    server_aliases: &BTreeMap<String, String>,
) -> ResolvedPolicy {
    let mut policy = policy.clone();
    let mut report = ValidationReport {
        sources: inventory.sources.clone(),
        inventory_complete: !inventory.sources.is_empty()
            && inventory
                .sources
                .iter()
                .all(|source| source.status == InventoryStatus::Complete),
        tools_may_change: inventory.sources.is_empty() || inventory.sources.iter().any(|source| source.dynamic),
        ..ValidationReport::default()
    };
    report.tools_may_change |= !report.inventory_complete;
    let observed = match inventory.identities(adapter) {
        Ok(observed) => observed,
        Err(error) => {
            report.errors.push(match error {
                appa_runtime_api::ParseRefusal::Unreadable { detail }
                | appa_runtime_api::ParseRefusal::Malformed { detail } => detail,
            });
            return ResolvedPolicy { policy, report };
        }
    };
    for (alias, target) in server_aliases {
        if CanonicalTool::of("mcp", alias, "tool").is_err() || CanonicalTool::of("mcp", target, "tool").is_err() {
            report.errors.push(format!(
                "server alias {alias:?} or its target is not a valid connection identity"
            ));
        }
    }
    let resolve_name = |name: &str, server: Option<&str>| -> Result<Option<String>, String> {
        let (name, selector) = bare(name);
        if name == "*" {
            return if server.is_some() {
                Err("a wildcard annotator cannot select one server".into())
            } else {
                Ok(Some("*".into()))
            };
        }
        let target = server.map(|server| server_aliases.get(server).map(String::as_str).unwrap_or(server));
        let qualified = precise_name(name, adapter).map(|id| {
            let parts: Vec<_> = id.as_str().split('/').collect();
            match parts.as_slice() {
                ["mcp", namespace, tool] => server_aliases
                    .get(*namespace)
                    .and_then(|target| CanonicalTool::of("mcp", target, tool).ok())
                    .unwrap_or(id),
                _ => id,
            }
        });
        if let Some(target) = target {
            if name.contains('/') || (adapter.name == AdapterName::ClaudeCode && name.starts_with("mcp__")) {
                return Err(format!(
                    "tool {name:?} conflicts with server selector {target:?}; use a short tool name with server"
                ));
            }
            let id = CanonicalTool::of("mcp", target, name).map_err(|error| error.to_string())?;
            return Ok(Some(format!("{id}{selector}")));
        }
        if qualified.is_none()
            && (name.contains('/') || (adapter.name == AdapterName::ClaudeCode && name.starts_with("mcp__")))
        {
            return Err(format!("tool {name:?} has an invalid qualified identity"));
        }
        if qualified.is_none() && adapter.name == AdapterName::Kagent {
            CanonicalTool::of("mcp", "server", name).map_err(|error| error.to_string())?;
            return Ok(Some(format!("mcp/*/{name}{selector}")));
        }
        Ok(qualified.map(|id| format!("{id}{selector}")))
    };
    let mut covered = BTreeSet::new();
    let mut declared_names: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut unresolved = BTreeSet::new();
    if let Some(tools) = policy.get_mut("tool").and_then(toml::Value::as_array_mut) {
        for mut tool in std::mem::take(tools) {
            let Some(table) = tool.as_table_mut() else {
                tools.push(tool);
                continue;
            };
            let server = table.remove("server");
            if server.as_ref().is_some_and(|value| value.as_str().is_none()) {
                report.errors.push("tool server must be a string".into());
                continue;
            }
            let Some(name) = table.get("name").and_then(toml::Value::as_str).map(str::to_owned) else {
                tools.push(tool);
                continue;
            };
            match resolve_name(&name, server.as_ref().and_then(toml::Value::as_str)) {
                Ok(Some(resolved)) => {
                    report.wildcard |= resolved == "*";
                    let names = native_variants(&resolved);
                    if resolved != "*"
                        && !observed
                            .iter()
                            .any(|(_, id, _)| names.iter().any(|name| rule_matches(bare(name).0, id.as_str())))
                    {
                        unresolved.insert(name.clone());
                    }
                    for name in names {
                        covered.insert(bare(&name).0.to_owned());
                        let mut variant = table.clone();
                        variant.insert("name".into(), toml::Value::String(name));
                        tools.push(toml::Value::Table(variant));
                    }
                    declared_names.entry(bare(&name).0.to_owned()).or_default().extend(
                        native_variants(&resolved)
                            .into_iter()
                            .map(|name| bare(&name).0.to_owned()),
                    );
                }
                Ok(None) => {
                    unresolved.insert(name.clone());
                    report
                        .diagnostics
                        .push(format!("rule {name:?} is inactive until its tool can be identified"));
                    tools.push(tool);
                }
                Err(error) => report.errors.push(error),
            }
        }
    }
    if let Some(deployment) = policy.get_mut("deployment").and_then(toml::Value::as_table_mut) {
        // These are the exact tool-name arrays understood by the policy compiler.
        for field in ["confined_results", "provider_run_tools", "assumed_tools"] {
            if let Some(names) = deployment.get_mut(field).and_then(toml::Value::as_array_mut) {
                for value in std::mem::take(names) {
                    let Some(name) = value.as_str().map(str::to_owned) else {
                        names.push(value);
                        continue;
                    };
                    if let Some(declarations) = declared_names.get(&name) {
                        names.extend(declarations.iter().cloned().map(toml::Value::String));
                        continue;
                    }
                    match resolve_name(&name, None) {
                        Ok(Some(resolved)) => {
                            names.extend(native_variants(&resolved).into_iter().map(toml::Value::String))
                        }
                        Ok(None) => {
                            report
                                .diagnostics
                                .push(format!("deployment.{field}: {name:?} has not been observed"));
                            names.push(value);
                        }
                        Err(error) => report.errors.push(error),
                    }
                }
            }
        }
    }
    for (host, identity, spawn) in &observed {
        if identity.is_control() {
            continue;
        }
        let declared = covered.iter().any(|rule| rule_matches(rule, identity.as_str()));
        let covered = declared || (report.wildcard && (!spawn || adapter.name == AdapterName::ClaudeCode));
        report.tools.push(ToolCheck {
            tool: host.clone(),
            status: if covered {
                ToolStatus::Valid
            } else {
                ToolStatus::Invalid {
                    reason: format!(
                        "{} is not covered by policy{}",
                        identity,
                        if *spawn {
                            "; delegation requires an explicit contract"
                        } else {
                            "; add a contract or wildcard annotator"
                        }
                    ),
                }
            },
        });
    }
    for name in unresolved {
        report.tools.push(ToolCheck {
            tool: name,
            status: ToolStatus::Unknown {
                reason: "no matching host observation".into(),
            },
        });
    }
    ResolvedPolicy { policy, report }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kagent_literal_mcp_prefix_is_not_claude_wire_syntax() {
        use appa_runtime_api::inventory::ObservedTool;
        let policy: toml::Value = toml::from_str("version=2\n[[tool]]\nname='mcp__github__get_me'\n").unwrap();
        let inventory = ToolInventory {
            tools: vec![ObservedTool {
                name: "mcp__github__get_me".into(),
                tool: "mcp:fixture/mcp__github__get_me".into(),
            }],
            ..ToolInventory::default()
        };
        let resolved = resolve(&policy, appa_adapter_kagent::adapter(), &inventory, &BTreeMap::new());
        assert!(resolved.report.is_valid(), "{:?}", resolved.report);
        assert!(matches!(resolved.report.tools[0].status, ToolStatus::Valid));
        assert_eq!(
            resolved.policy["tool"][0]["name"].as_str(),
            Some("mcp/*/mcp__github__get_me")
        );
    }
    use appa_runtime_api::inventory::ObservedTool;

    fn inventory(tools: &[(&str, &str)]) -> ToolInventory {
        ToolInventory {
            tools: tools
                .iter()
                .map(|(name, tool)| ObservedTool {
                    name: (*name).into(),
                    tool: (*tool).into(),
                })
                .collect(),
            sources: vec![InventorySource {
                server: "demo".into(),
                status: InventoryStatus::Complete,
                dynamic: true,
                detail: None,
            }],
        }
    }

    fn policy(name: &str) -> toml::Value {
        toml::from_str(&format!("version = 2\n[[tool]]\nname = {name:?}\ndelta = {{}}\n")).unwrap()
    }

    #[test]
    fn native_claude_names_and_selectors_resolve_without_rewriting_input() {
        let original = policy("Bash(command:cargo test*)");
        let result = resolve(
            &original,
            appa_adapter_claude_code::adapter(),
            &inventory(&[("Bash", "Bash")]),
            &BTreeMap::new(),
        );
        assert!(result.report.is_valid());
        assert_eq!(original["tool"][0]["name"].as_str(), Some("Bash(command:cargo test*)"));
        assert_eq!(
            result.policy["tool"][0]["name"].as_str(),
            Some("host/claude-code/Bash(command:cargo test*)")
        );
    }

    #[test]
    fn remote_agent_names_restore_hyphens_in_both_components() {
        let adapter = appa_adapter_kagent::adapter();
        assert_eq!(
            precise_name("my_team__NS__log_analyst", adapter).unwrap().as_str(),
            "agent/my-team/log-analyst"
        );
    }

    #[test]
    fn opening_rule_registry_does_not_depend_on_observation_time() {
        let authored = policy("read_secret");
        let adapter = appa_adapter_kagent::adapter();
        let mut unavailable = inventory(&[]);
        unavailable.sources[0].status = InventoryStatus::Unavailable;
        let before = resolve(&authored, adapter, &unavailable, &BTreeMap::new());
        let after = resolve(
            &authored,
            adapter,
            &inventory(&[("read_secret", "mcp:demo/read_secret")]),
            &BTreeMap::new(),
        );
        assert!(before.report.is_valid());
        assert!(after.report.is_valid());
        assert_eq!(
            before.policy, after.policy,
            "discovery may add identity evidence, not change the opening rule registry"
        );
    }

    #[test]
    fn kagent_short_names_need_observations_not_policy_as_inventory() {
        let original = policy("read_secret");
        let unknown = resolve(
            &original,
            appa_adapter_kagent::adapter(),
            &ToolInventory::default(),
            &BTreeMap::new(),
        );
        assert!(unknown.report.is_valid());
        assert!(!unknown.report.inventory_complete);
        assert!(matches!(unknown.report.tools[0].status, ToolStatus::Unknown { .. }));
        let known = resolve(
            &original,
            appa_adapter_kagent::adapter(),
            &inventory(&[("read_secret", "mcp:demo/read_secret")]),
            &BTreeMap::new(),
        );
        assert!(known.report.is_valid());
        assert_eq!(known.policy["tool"][0]["name"].as_str(), Some("mcp/*/read_secret"));
        assert_eq!(known.report.tools[0].status, ToolStatus::Valid);
    }

    #[test]
    fn known_uncovered_tools_are_errors_but_unavailable_connections_are_not() {
        let mut facts = inventory(&[("write", "mcp:demo/write")]);
        let bad = resolve(
            &policy("read"),
            appa_adapter_kagent::adapter(),
            &facts,
            &BTreeMap::new(),
        );
        assert!(!bad.report.is_valid());
        facts.tools.clear();
        facts.sources[0].status = InventoryStatus::Unavailable;
        let partial = resolve(
            &policy("read"),
            appa_adapter_kagent::adapter(),
            &facts,
            &BTreeMap::new(),
        );
        assert!(partial.report.is_valid());
        assert!(!partial.report.inventory_complete);
    }

    #[test]
    fn wildcard_covers_ordinary_tools_not_ambiguous_dispatch_or_kagent_spawns() {
        let wildcard: toml::Value = toml::from_str("[[tool]]\nname = '*'\nannotator = 'classify'").unwrap();
        let facts = inventory(&[("read", "mcp:demo/read")]);
        assert!(
            resolve(&wildcard, appa_adapter_kagent::adapter(), &facts, &BTreeMap::new())
                .report
                .is_valid()
        );
        let facts = inventory(&[("read", "mcp:demo/read"), ("read", "mcp:other/read")]);
        assert!(
            !resolve(&wildcard, appa_adapter_kagent::adapter(), &facts, &BTreeMap::new())
                .report
                .is_valid()
        );
        let facts = inventory(&[("child", "agent:demo/child")]);
        assert!(
            !resolve(&wildcard, appa_adapter_kagent::adapter(), &facts, &BTreeMap::new())
                .report
                .is_valid()
        );
    }

    #[test]
    fn server_qualification_limits_a_native_rule_without_guessing_provider() {
        let facts = inventory(&[("read", "mcp:demo/read")]);
        let mut authored = policy("read");
        assert!(
            resolve(&authored, appa_adapter_kagent::adapter(), &facts, &BTreeMap::new())
                .report
                .is_valid()
        );
        authored["tool"][0]
            .as_table_mut()
            .unwrap()
            .insert("server".into(), toml::Value::String("demo".into()));
        let resolved = resolve(&authored, appa_adapter_kagent::adapter(), &facts, &BTreeMap::new());
        assert_eq!(resolved.policy["tool"][0]["name"].as_str(), Some("mcp/demo/read"));
        assert!(resolved.report.is_valid());
        assert!(resolved.policy["tool"][0].get("server").is_none());
        let other = inventory(&[("read", "mcp:other/read")]);
        assert!(
            !resolve(&authored, appa_adapter_kagent::adapter(), &other, &BTreeMap::new())
                .report
                .is_valid()
        );
    }

    #[test]
    fn declaration_order_and_deployment_references_are_preserved() {
        let authored: toml::Value = toml::from_str("[[tool]]\nname = 'Bash(command:git*)'\ndelta = {}\n[[tool]]\nname = 'Bash'\ndelta = {}\n[deployment]\nconfined_results = ['Bash']").unwrap();
        let resolved = resolve(
            &authored,
            appa_adapter_claude_code::adapter(),
            &ToolInventory::default(),
            &BTreeMap::new(),
        );
        assert_eq!(
            resolved.policy["tool"][0]["name"].as_str(),
            Some("host/claude-code/Bash(command:git*)")
        );
        assert_eq!(
            resolved.policy["tool"][1]["name"].as_str(),
            Some("host/claude-code/Bash")
        );
        assert_eq!(
            resolved.policy["deployment"]["confined_results"][0].as_str(),
            Some("host/claude-code/Bash")
        );
    }

    #[test]
    fn native_deployment_references_follow_explicit_server_qualifiers() {
        let authored: toml::Value = toml::from_str("version = 2\n[[tool]]\nname = 'read(path:private*)'\nserver = 'demo'\n[[tool]]\nname = 'read'\nserver = 'demo'\n[deployment]\nconfined_results = ['read']\n").unwrap();
        let resolved = resolve(
            &authored,
            appa_adapter_kagent::adapter(),
            &ToolInventory::default(),
            &BTreeMap::new(),
        );
        assert_eq!(
            resolved.policy["deployment"]["confined_results"],
            toml::Value::Array(vec![toml::Value::String("mcp/demo/read".into())])
        );
        let compiled = appa_policy::Config::from_toml_str(&toml::to_string(&resolved.policy).unwrap()).unwrap();
        assert!(
            compiled
                .engine()
                .profile()
                .confines_result(&appa_engine::value::ToolName::new("mcp/demo/read"))
        );
    }
}
