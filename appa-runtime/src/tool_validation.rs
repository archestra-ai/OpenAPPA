//! Resolve authored names without treating a policy as a tool inventory.
//!
//! This module makes no network requests and grants no permission. Hosts supply facts;
//! the same resolver is used by preflight and by served-policy compilation.

use std::collections::{BTreeMap, BTreeSet};

use appa_runtime_api::inventory::{InventorySource, InventoryStatus, ToolInventory};
use appa_runtime_api::{Adapter, AdapterName, CanonicalTool};
use serde::{Deserialize, Serialize};

use crate::config::AliasFamily;

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
        .any(|id| id.is_empty() || id.chars().any(char::is_control))
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
    if let Ok(identified) = (adapter.identify_tool)(name) {
        return Some(identified.canonical);
    }
    if adapter.name == AdapterName::Kagent
        && let Some((namespace, agent)) = name.split_once(appa_adapter_kagent::AGENT_SPELLING_SEPARATOR)
    {
        return CanonicalTool::of("agent", &namespace.replace('_', "-"), &agent.replace('_', "-")).ok();
    }
    None
}

/// Whether an authored name already spells a server: as a canonical id, or in the host's
/// own grammar, so a `server` selector beside it would name the server twice.
fn spells_server(adapter: Adapter, name: &str) -> bool {
    name.contains('/') || (adapter.spells_server)(name)
}

/// `aliases` belongs to deployment configuration, never to a battery. Its values are
/// identities the deployment reports, not DNS/provider guesses. A namespace bound to
/// several identities resolves each rule under it to one canonical id per identity: the
/// rule covers every server or host the deployment bound it to. Each family's aliases
/// rebind only ids of that family, after composition, so root and battery rules alike
/// resolve and the root's still match first.
pub fn resolve(
    policy: &toml::Value,
    adapter: Adapter,
    inventory: &ToolInventory,
    aliases: &crate::config::Aliases,
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
            report.errors.push(crate::api::refusal_detail(error));
            return ResolvedPolicy { policy, report };
        }
    };
    for family in AliasFamily::ALL {
        let kind = family.kind();
        for (alias, targets) in aliases.of(family) {
            if CanonicalTool::of(kind, alias, "tool").is_err() {
                report
                    .errors
                    .push(format!("{kind} alias {alias:?} is not a valid {kind} namespace"));
            }
            if targets.is_empty() {
                report.errors.push(format!("{kind} alias {alias:?} binds nothing"));
            }
            let mut seen = BTreeSet::new();
            for target in targets {
                if CanonicalTool::of(kind, target, "tool").is_err() {
                    report.errors.push(format!(
                        "{kind} alias {alias:?} target {target:?} is not a valid {kind} identity"
                    ));
                }
                if !seen.insert(target) {
                    report
                        .errors
                        .push(format!("{kind} alias {alias:?} binds {target:?} twice"));
                }
            }
        }
    }
    // The identities a rule spelled `<family>/<namespace>/…` (or, for MCP,
    // `server = "<namespace>"`) names: the namespace's bound identities, else the namespace.
    let targets_of = |family: AliasFamily, namespace: &str| -> Vec<String> {
        match aliases.of(family).get(namespace) {
            Some(targets) if !targets.is_empty() => targets.clone(),
            _ => vec![namespace.to_owned()],
        }
    };
    // The canonical ids one rule resolves to, `None` while its tool cannot be identified.
    let resolve_name = |name: &str, server: Option<&str>| -> Result<Option<Vec<String>>, String> {
        let (name, selector) = bare(name);
        if name == "*" {
            return if server.is_some() {
                Err("a wildcard annotator cannot select one server".into())
            } else {
                Ok(Some(vec!["*".into()]))
            };
        }
        if let Some(server) = server {
            if spells_server(adapter, name) {
                return Err(format!(
                    "tool {name:?} conflicts with server selector {server:?}; use a short tool name with server"
                ));
            }
            return targets_of(AliasFamily::Mcp, server)
                .iter()
                .map(|target| {
                    CanonicalTool::of("mcp", target, name)
                        .map(|id| format!("{id}{selector}"))
                        .map_err(|error| error.to_string())
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Some);
        }
        let qualified = precise_name(name, adapter).map(|id| {
            let parts: Vec<_> = id.as_str().split('/').collect();
            match parts.as_slice() {
                [kind, namespace, tool] => match AliasFamily::of_kind(kind) {
                    Some(family) if aliases.of(family).contains_key(*namespace) => targets_of(family, namespace)
                        .iter()
                        .filter_map(|target| CanonicalTool::of(kind, target, tool).ok())
                        .map(|id| format!("{id}{selector}"))
                        .collect(),
                    _ => vec![format!("{id}{selector}")],
                },
                _ => vec![format!("{id}{selector}")],
            }
        });
        if qualified.is_none() && spells_server(adapter, name) {
            return Err(format!("tool {name:?} has an invalid qualified identity"));
        }
        if qualified.is_none() && adapter.name == AdapterName::Kagent {
            CanonicalTool::of("mcp", "server", name).map_err(|error| error.to_string())?;
            return Ok(Some(vec![format!("mcp/*/{name}{selector}")]));
        }
        Ok(qualified.filter(|ids| !ids.is_empty()))
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
                    let wildcard = resolved.iter().any(|id| id == "*");
                    report.wildcard |= wildcard;
                    let names: Vec<String> = resolved.iter().flat_map(|id| native_variants(id)).collect();
                    // A rule bound to several servers is identified once any of them is observed.
                    if !wildcard
                        && !observed
                            .iter()
                            .any(|(_, id, _)| names.iter().any(|name| rule_matches(bare(name).0, id.as_str())))
                    {
                        unresolved.insert(name.clone());
                    }
                    for name in &names {
                        covered.insert(bare(name).0.to_owned());
                        let mut variant = table.clone();
                        variant.insert("name".into(), toml::Value::String(name.clone()));
                        tools.push(toml::Value::Table(variant));
                    }
                    declared_names
                        .entry(bare(&name).0.to_owned())
                        .or_default()
                        .extend(names.iter().map(|name| bare(name).0.to_owned()));
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
                        Ok(Some(resolved)) => names.extend(
                            resolved
                                .iter()
                                .flat_map(|id| native_variants(id))
                                .map(toml::Value::String),
                        ),
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
        let covered = declared || (report.wildcard && (spawn.is_none() || adapter.wildcard_covers_spawn));
        report.tools.push(ToolCheck {
            tool: host.clone(),
            status: if covered {
                ToolStatus::Valid
            } else {
                ToolStatus::Invalid {
                    reason: format!(
                        "{} is not covered by policy{}",
                        identity,
                        if spawn.is_some() {
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
    use crate::config::Aliases;

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
        let resolved = resolve(
            &policy,
            appa_adapter_kagent::adapter(),
            &inventory,
            &crate::config::Aliases::default(),
        );
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
            &crate::config::Aliases::default(),
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
    fn a_remote_agent_spelling_names_its_namespace_so_a_server_selector_conflicts() {
        let authored: toml::Value =
            toml::from_str("version = 2\n[[tool]]\nname = 'my_team__NS__log_analyst'\nserver = 'demo'\ndelta = {}\n")
                .unwrap();
        let result = resolve(
            &authored,
            appa_adapter_kagent::adapter(),
            &inventory(&[]),
            &crate::config::Aliases::default(),
        );
        assert!(
            result
                .report
                .errors
                .iter()
                .any(|error| error.contains("conflicts with server selector")),
            "{:?}",
            result.report
        );
    }

    #[test]
    fn opening_rule_registry_does_not_depend_on_observation_time() {
        let authored = policy("read_secret");
        let adapter = appa_adapter_kagent::adapter();
        let mut unavailable = inventory(&[]);
        unavailable.sources[0].status = InventoryStatus::Unavailable;
        let before = resolve(&authored, adapter, &unavailable, &crate::config::Aliases::default());
        let after = resolve(
            &authored,
            adapter,
            &inventory(&[("read_secret", "mcp:demo/read_secret")]),
            &crate::config::Aliases::default(),
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
            &crate::config::Aliases::default(),
        );
        assert!(unknown.report.is_valid());
        assert!(!unknown.report.inventory_complete);
        assert!(matches!(unknown.report.tools[0].status, ToolStatus::Unknown { .. }));
        let known = resolve(
            &original,
            appa_adapter_kagent::adapter(),
            &inventory(&[("read_secret", "mcp:demo/read_secret")]),
            &crate::config::Aliases::default(),
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
            &crate::config::Aliases::default(),
        );
        assert!(!bad.report.is_valid());
        facts.tools.clear();
        facts.sources[0].status = InventoryStatus::Unavailable;
        let partial = resolve(
            &policy("read"),
            appa_adapter_kagent::adapter(),
            &facts,
            &crate::config::Aliases::default(),
        );
        assert!(partial.report.is_valid());
        assert!(!partial.report.inventory_complete);
    }

    #[test]
    fn wildcard_covers_ordinary_tools_not_ambiguous_dispatch_or_kagent_spawns() {
        let wildcard: toml::Value = toml::from_str("[[tool]]\nname = '*'\nannotator = 'classify'").unwrap();
        let facts = inventory(&[("read", "mcp:demo/read")]);
        assert!(
            resolve(
                &wildcard,
                appa_adapter_kagent::adapter(),
                &facts,
                &crate::config::Aliases::default()
            )
            .report
            .is_valid()
        );
        let facts = inventory(&[("read", "mcp:demo/read"), ("read", "mcp:other/read")]);
        assert!(
            !resolve(
                &wildcard,
                appa_adapter_kagent::adapter(),
                &facts,
                &crate::config::Aliases::default()
            )
            .report
            .is_valid()
        );
        let facts = inventory(&[("child", "agent:demo/child")]);
        assert!(
            !resolve(
                &wildcard,
                appa_adapter_kagent::adapter(),
                &facts,
                &crate::config::Aliases::default()
            )
            .report
            .is_valid()
        );
    }

    #[test]
    fn server_qualification_limits_a_native_rule_without_guessing_provider() {
        let facts = inventory(&[("read", "mcp:demo/read")]);
        let mut authored = policy("read");
        assert!(
            resolve(
                &authored,
                appa_adapter_kagent::adapter(),
                &facts,
                &crate::config::Aliases::default()
            )
            .report
            .is_valid()
        );
        authored["tool"][0]
            .as_table_mut()
            .unwrap()
            .insert("server".into(), toml::Value::String("demo".into()));
        let resolved = resolve(
            &authored,
            appa_adapter_kagent::adapter(),
            &facts,
            &crate::config::Aliases::default(),
        );
        assert_eq!(resolved.policy["tool"][0]["name"].as_str(), Some("mcp/demo/read"));
        assert!(resolved.report.is_valid());
        assert!(resolved.policy["tool"][0].get("server").is_none());
        let other = inventory(&[("read", "mcp:other/read")]);
        assert!(
            !resolve(
                &authored,
                appa_adapter_kagent::adapter(),
                &other,
                &crate::config::Aliases::default()
            )
            .report
            .is_valid()
        );
    }

    /// A namespace bound to several servers resolves each rule under it, spelled
    /// canonically or through `server`, to one id per server; the rule is identified
    /// once any of them is observed, and a deployment reference to it names them all.
    #[test]
    fn a_namespace_bound_to_several_servers_covers_each_of_them() {
        let aliases = mcp_aliases("databricks", &["genie", "sql"]);
        let authored: toml::Value = toml::from_str(
            "version = 2\n[[tool]]\nname = 'mcp/databricks/genie_ask'\ndelta = {}\n[[tool]]\nname = 'execute_sql'\nserver = 'databricks'\ndelta = {}\n[deployment]\nconfined_results = ['mcp/databricks/genie_ask']\n",
        )
        .unwrap();
        let facts = inventory(&[("genie_ask", "mcp:genie/genie_ask")]);
        let resolved = resolve(&authored, appa_adapter_kagent::adapter(), &facts, &aliases);
        assert!(resolved.report.is_valid(), "{:?}", resolved.report);
        let names: Vec<_> = resolved.policy["tool"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        assert_eq!(
            names,
            vec![
                "mcp/genie/genie_ask",
                "mcp/sql/genie_ask",
                "mcp/genie/execute_sql",
                "mcp/sql/execute_sql"
            ]
        );
        assert_eq!(
            resolved.policy["deployment"]["confined_results"],
            toml::Value::Array(vec![
                toml::Value::String("mcp/genie/genie_ask".into()),
                toml::Value::String("mcp/sql/genie_ask".into()),
            ])
        );
        assert!(
            !resolved
                .report
                .tools
                .iter()
                .any(|tool| tool.tool == "mcp/databricks/genie_ask"),
            "a rule observed on one bound server is not reported unknown"
        );

        let unseen = inventory(&[("other", "mcp:other/other")]);
        let unresolved = resolve(&authored, appa_adapter_kagent::adapter(), &unseen, &aliases);
        assert!(
            unresolved.report.tools.iter().any(
                |tool| tool.tool == "mcp/databricks/genie_ask" && matches!(tool.status, ToolStatus::Unknown { .. })
            )
        );

        for broken in [
            mcp_aliases("databricks", &[]),
            mcp_aliases("databricks", &["genie", "genie"]),
            mcp_aliases("databricks", &["not a server"]),
        ] {
            assert!(
                !resolve(&authored, appa_adapter_kagent::adapter(), &facts, &broken)
                    .report
                    .is_valid(),
                "{broken:?}"
            );
        }
    }

    fn mcp_aliases(namespace: &str, targets: &[&str]) -> Aliases {
        Aliases {
            mcp: BTreeMap::from([(namespace.to_owned(), targets.iter().map(|t| (*t).to_owned()).collect())]),
            host: BTreeMap::new(),
        }
    }

    fn host_aliases(namespace: &str, targets: &[&str]) -> Aliases {
        Aliases {
            mcp: BTreeMap::new(),
            host: BTreeMap::from([(namespace.to_owned(), targets.iter().map(|t| (*t).to_owned()).collect())]),
        }
    }

    fn resolved_names(resolved: &ResolvedPolicy) -> Vec<&str> {
        resolved.policy["tool"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect()
    }

    /// A host alias rebinds a battery's `host/<namespace>/…` rules, selectors and
    /// deployment references included, and leaves MCP ids alone; an MCP alias of the same
    /// namespace leaves host ids alone.
    #[test]
    fn each_alias_family_rebinds_only_its_own_ids() {
        let authored: toml::Value = toml::from_str(
            "version = 2\n[[tool]]\nname = 'host/claude-code/Read(file_path:.env*)'\ndelta = { audience = ['self'] }\n[[tool]]\nname = 'host/claude-code/Read'\ndelta = {}\n[[tool]]\nname = 'mcp/claude-code/Read'\ndelta = {}\n[[tool]]\nname = 'Read'\nserver = 'claude-code'\ndelta = {}\n[deployment]\nconfined_results = ['host/claude-code/Read']\n",
        )
        .unwrap();
        let adapter = appa_adapter_kagent::adapter();
        let host = resolve(
            &authored,
            adapter,
            &ToolInventory::default(),
            &host_aliases("claude-code", &["archestra"]),
        );
        assert!(host.report.errors.is_empty(), "{:?}", host.report);
        assert_eq!(
            resolved_names(&host),
            vec![
                "host/archestra/Read(file_path:.env*)",
                "host/archestra/Read",
                "mcp/claude-code/Read",
                "mcp/claude-code/Read",
            ]
        );
        assert_eq!(
            host.policy["deployment"]["confined_results"],
            toml::Value::Array(vec![toml::Value::String("host/archestra/Read".into())])
        );

        let mcp = resolve(
            &authored,
            adapter,
            &ToolInventory::default(),
            &mcp_aliases("claude-code", &["cc"]),
        );
        assert!(mcp.report.errors.is_empty(), "{:?}", mcp.report);
        assert_eq!(
            resolved_names(&mcp),
            vec![
                "host/claude-code/Read(file_path:.env*)",
                "host/claude-code/Read",
                "mcp/cc/Read",
                "mcp/cc/Read",
            ]
        );

        for broken in [
            host_aliases("claude-code", &[]),
            host_aliases("claude-code", &["archestra", "archestra"]),
            host_aliases("claude-code", &["not a host"]),
            host_aliases("not a host", &["archestra"]),
        ] {
            assert!(
                !resolve(&authored, adapter, &ToolInventory::default(), &broken)
                    .report
                    .is_valid(),
                "{broken:?}"
            );
        }
    }

    #[test]
    fn declaration_order_and_deployment_references_are_preserved() {
        let authored: toml::Value = toml::from_str("[[tool]]\nname = 'Bash(command:git*)'\ndelta = {}\n[[tool]]\nname = 'Bash'\ndelta = {}\n[deployment]\nconfined_results = ['Bash']").unwrap();
        let resolved = resolve(
            &authored,
            appa_adapter_claude_code::adapter(),
            &ToolInventory::default(),
            &crate::config::Aliases::default(),
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
            &crate::config::Aliases::default(),
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
