//! `[aliases.tool]`: a rule written for a canonical tool, `tool/<namespace>/<tool>`, stands for
//! each client tool a map binds to it. Lowering runs once on the composed document: each such
//! rule is replaced in place by one copy per bound client tool, with every argument it reads
//! renamed to the client's spelling. Calls are never rewritten, so the evaluated tool and
//! arguments stay exactly what the client sent.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use appa_engine::contract::SelectorPlaceholder;
use appa_engine::registry::{RespellError, respell_declaration};
use appa_runtime_api::CanonicalTool;
use serde::Deserialize;

use super::aliases::{AliasFamily, Aliases};

const FAMILY: &str = "tool";

/// One client tool a canonical tool stands for: its canonical id, and for each canonical
/// argument the name the client spells it with.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ToolTarget {
    name: String,
    #[serde(default)]
    args: BTreeMap<String, String>,
    /// The client tool's results stay confined for sanitization, as `confined_results` does.
    #[serde(default)]
    confined: bool,
}

/// `[aliases.tool]`: per canonical namespace, per canonical tool, the client tools it binds.
pub(crate) type ToolMaps = BTreeMap<String, BTreeMap<String, Vec<ToolTarget>>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MapOrigin {
    Root,
    Battery(String),
}

impl fmt::Display for MapOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MapOrigin::Root => f.write_str("the root config"),
            MapOrigin::Battery(name) => write!(f, "battery {name:?}"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolAliasError {
    #[error("{origin} maps {canonical} onto {target:?}, which is not a canonical mcp, host or agent tool")]
    InvalidTarget {
        origin: String,
        canonical: String,
        target: String,
    },
    #[error(
        "{origin} maps an argument of {canonical} as {argument:?}, which is not an argument name a selector can read"
    )]
    InvalidArgument {
        origin: String,
        canonical: String,
        argument: String,
    },
    #[error("{origin} maps {canonical} onto no client tool")]
    BindsNothing { origin: String, canonical: String },
    #[error("{origin} maps {canonical} onto {target}, reading two arguments as {argument:?}")]
    CollidingArguments {
        origin: String,
        canonical: String,
        target: String,
        argument: String,
    },
    #[error("{first} and {second} both map {client} (bound from {target}), so a call to it has two canonical readings")]
    ClaimedTwice {
        first: String,
        second: String,
        target: String,
        client: String,
    },
    #[error("[aliases.tool] does not parse: {source}")]
    Unparsable { source: toml::de::Error },
    #[error("{origin} maps tools of {namespace:?}, which is not included")]
    UnnamedNamespace { origin: String, namespace: String },
    #[error("rule {rule:?} reads argument {argument:?}, which the map onto {target} does not bind")]
    UnmappedArgument {
        rule: String,
        target: String,
        argument: String,
    },
    #[error("rule {rule:?} is not a well-formed canonical tool rule")]
    MalformedRule { rule: String },
    #[error("rule {rule:?} names a canonical tool, which takes no server selector")]
    ServerSelector { rule: String },
    #[error("deployment.{field} names {tool:?}; confine a client tool with `confined = true` on its map entry")]
    DeploymentName { field: String, tool: String },
}

/// The canonical tool a rule name spells.
struct CanonicalRule<'a> {
    namespace: &'a str,
    tool: &'a str,
}

impl<'a> CanonicalRule<'a> {
    fn of(name: &'a str) -> Option<Result<CanonicalRule<'a>, ToolAliasError>> {
        let bare = name.split('(').next().unwrap_or(name);
        let rest = bare.strip_prefix(FAMILY)?.strip_prefix('/')?;
        let parsed = rest
            .split_once('/')
            .filter(|(namespace, tool)| CanonicalTool::of(FAMILY_PROBE, namespace, tool).is_ok())
            .map(|(namespace, tool)| CanonicalRule { namespace, tool })
            .ok_or_else(|| ToolAliasError::MalformedRule { rule: name.to_owned() });
        Some(parsed)
    }
}

/// A canonical tool's bound targets with the map that bound each.
struct Binding<'a> {
    origin: &'a MapOrigin,
    target: &'a ToolTarget,
}

/// Replace every `tool/…` rule of the composed `document` by its bound client copies, and
/// drop `aliases.tool`. `batteries` are the included batteries' maps, in include order.
pub(crate) fn lower(
    document: &mut toml::Value,
    batteries: Vec<(MapOrigin, ToolMaps)>,
    aliases: &Aliases,
) -> Result<(), ToolAliasError> {
    let root = take_root_maps(document)?;
    let maps: Vec<(MapOrigin, ToolMaps)> = std::iter::once((MapOrigin::Root, root)).chain(batteries).collect();
    let mut bindings: BTreeMap<(&str, &str), Vec<Binding<'_>>> = BTreeMap::new();
    for (origin, map) in &maps {
        for (namespace, tools) in map {
            let binds_nothing = |canonical: String| ToolAliasError::BindsNothing {
                origin: origin.to_string(),
                canonical,
            };
            if tools.is_empty() {
                return Err(binds_nothing(format!("{FAMILY}/{namespace}")));
            }
            for (tool, targets) in tools {
                if targets.is_empty() {
                    return Err(binds_nothing(format!("{FAMILY}/{namespace}/{tool}")));
                }
                for target in targets {
                    check_target(origin, namespace, tool, target)?;
                    bindings
                        .entry((namespace.as_str(), tool.as_str()))
                        .or_default()
                        .push(Binding { origin, target });
                }
            }
        }
    }
    refuse_double_claims(&bindings, aliases)?;

    let policy = document
        .get_mut("policy")
        .and_then(toml::Value::as_table_mut)
        .expect("a composed config has a policy table");
    refuse_canonical_deployment_names(policy)?;
    let mut named: BTreeSet<String> = BTreeSet::new();
    if let Some(rules) = policy.get_mut("tool").and_then(toml::Value::as_array_mut) {
        for rule in std::mem::take(rules) {
            let Some(name) = rule.get("name").and_then(toml::Value::as_str).map(str::to_owned) else {
                rules.push(rule);
                continue;
            };
            let Some(canonical) = CanonicalRule::of(&name) else {
                rules.push(rule);
                continue;
            };
            let canonical = canonical?;
            if rule.get("server").is_some() {
                return Err(ToolAliasError::ServerSelector { rule: name });
            }
            named.insert(canonical.namespace.to_owned());
            let Some(bound) = bindings.get(&(canonical.namespace, canonical.tool)) else {
                tracing::warn!(rule = %name, "no [aliases.tool] map binds this canonical tool; the rule is inactive");
                continue;
            };
            for binding in bound {
                rules.push(lowered(&rule, &name, binding.target)?);
            }
        }
    }
    // A map for a namespace no rule names depends on a battery the deployment left out.
    if let Some(((namespace, _), bound)) = bindings.iter().find(|((namespace, _), _)| !named.contains(*namespace)) {
        return Err(ToolAliasError::UnnamedNamespace {
            origin: bound[0].origin.to_string(),
            namespace: (*namespace).to_owned(),
        });
    }
    confine(policy, bindings.values().flatten().map(|binding| binding.target));
    Ok(())
}

fn take_root_maps(document: &mut toml::Value) -> Result<ToolMaps, ToolAliasError> {
    let table = document.as_table_mut().expect("a TOML document is always a table");
    let Some(aliases) = table.get_mut("aliases").and_then(toml::Value::as_table_mut) else {
        return Ok(ToolMaps::new());
    };
    let maps = aliases.remove(FAMILY).map(parse).transpose()?.unwrap_or_default();
    if aliases.is_empty() {
        table.remove("aliases");
    }
    Ok(maps)
}

/// A document's `[aliases.tool]` table.
pub(crate) fn parse(maps: toml::Value) -> Result<ToolMaps, ToolAliasError> {
    maps.try_into().map_err(|source| ToolAliasError::Unparsable { source })
}

fn check_target(origin: &MapOrigin, namespace: &str, tool: &str, target: &ToolTarget) -> Result<(), ToolAliasError> {
    let canonical = || format!("{FAMILY}/{namespace}/{tool}");
    let valid = CanonicalTool::of(FAMILY_PROBE, namespace, tool).is_ok()
        && CanonicalTool::parse(&target.name).is_ok_and(|id| !id.is_control());
    if !valid {
        return Err(ToolAliasError::InvalidTarget {
            origin: origin.to_string(),
            canonical: canonical(),
            target: target.name.clone(),
        });
    }
    // An argument name is spliced into selectors and placeholders, so it must be one their
    // grammars read back as a name: no delimiter, separator, or placeholder mark.
    let unspliceable = |argument: &String| {
        argument.is_empty()
            || argument.starts_with('$')
            || argument
                .chars()
                .any(|c| matches!(c, '(' | ')' | ',' | ':' | '\\' | '/') || c.is_control())
    };
    if let Some(argument) = target.args.keys().chain(target.args.values()).find(|a| unspliceable(a)) {
        return Err(ToolAliasError::InvalidArgument {
            origin: origin.to_string(),
            canonical: canonical(),
            argument: argument.clone(),
        });
    }
    let mut spelled = BTreeSet::new();
    if let Some(argument) = target.args.values().find(|argument| !spelled.insert(*argument)) {
        return Err(ToolAliasError::CollidingArguments {
            origin: origin.to_string(),
            canonical: canonical(),
            target: target.name.clone(),
            argument: argument.clone(),
        });
    }
    Ok(())
}

/// The family a canonical namespace and tool are checked under: segments obey the same
/// grammar as any canonical tool's.
const FAMILY_PROBE: &str = "host";

/// Each client tool a call can reach — a target after the root's mcp and host aliases
/// rebind it — has at most one canonical reading.
fn refuse_double_claims(
    bindings: &BTreeMap<(&str, &str), Vec<Binding<'_>>>,
    aliases: &Aliases,
) -> Result<(), ToolAliasError> {
    let mut claims: BTreeMap<String, String> = BTreeMap::new();
    for ((namespace, tool), bound) in bindings {
        for binding in bound {
            let claimant = format!("{} ({FAMILY}/{namespace}/{tool})", binding.origin);
            for client in rebound(&binding.target.name, aliases) {
                if let Some(first) = claims.get(&client) {
                    return Err(ToolAliasError::ClaimedTwice {
                        first: first.clone(),
                        second: claimant,
                        target: binding.target.name.clone(),
                        client,
                    });
                }
                claims.insert(client, claimant.clone());
            }
        }
    }
    Ok(())
}

/// The client ids `target` stands for once the root's mcp and host aliases rebind it.
fn rebound(target: &str, aliases: &Aliases) -> Vec<String> {
    let parts: Vec<&str> = target.split('/').collect();
    let [kind, namespace, tool] = parts.as_slice() else {
        return vec![target.to_owned()];
    };
    match AliasFamily::of_kind(kind).and_then(|family| aliases.of(family).get(*namespace)) {
        Some(bound) if !bound.is_empty() => bound.iter().map(|bound| format!("{kind}/{bound}/{tool}")).collect(),
        _ => vec![target.to_owned()],
    }
}

fn refuse_canonical_deployment_names(policy: &toml::Table) -> Result<(), ToolAliasError> {
    let Some(deployment) = policy.get("deployment").and_then(toml::Value::as_table) else {
        return Ok(());
    };
    for field in ["confined_results", "provider_run_tools", "assumed_tools"] {
        let names = deployment
            .get(field)
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten();
        if let Some(tool) = names
            .filter_map(toml::Value::as_str)
            .find(|name| CanonicalRule::of(name).is_some())
        {
            return Err(ToolAliasError::DeploymentName {
                field: field.to_owned(),
                tool: tool.to_owned(),
            });
        }
    }
    Ok(())
}

/// `rule` for `target`: its name and every argument it reads respelled for the client.
fn lowered(rule: &toml::Value, name: &str, target: &ToolTarget) -> Result<toml::Value, ToolAliasError> {
    let unmapped = |error: RespellError| match error {
        RespellError::Unmapped(argument) => ToolAliasError::UnmappedArgument {
            rule: name.to_owned(),
            target: target.name.clone(),
            argument,
        },
        RespellError::Malformed(_) | RespellError::Collides(_) => {
            ToolAliasError::MalformedRule { rule: name.to_owned() }
        }
    };
    let mut rule = rule.clone();
    let table = rule.as_table_mut().expect("a rule with a name is a table");
    let respelled = respell_declaration(name, &target.name, &target.args).map_err(unmapped)?;
    table.insert("name".into(), toml::Value::String(respelled));
    if let Some(list) = table
        .get_mut("requires")
        .and_then(|requires| requires.get_mut("audience"))
        .and_then(|audience| audience.get_mut("contains"))
    {
        respell_audience(list, &target.args, true).map_err(unmapped)?;
    }
    if let Some(list) = table.get_mut("delta").and_then(|delta| delta.get_mut("audience")) {
        respell_audience(list, &target.args, false).map_err(unmapped)?;
    }
    if let Some(parameters) = table.get_mut("parameters") {
        respell_schema(parameters, &target.args).map_err(unmapped)?;
    }
    Ok(rule)
}

/// An audience list's argument reads: a sole `$argument` recipient (where `recipients`
/// admits one) and a sole `@provider:…/$argument` placeholder.
fn respell_audience(
    list: &mut toml::Value,
    arguments: &BTreeMap<String, String>,
    recipients: bool,
) -> Result<(), RespellError> {
    let Some([entry]) = list.as_array_mut().map(Vec::as_mut_slice) else {
        return Ok(());
    };
    let Some(spelled) = entry.as_str() else {
        return Ok(());
    };
    let respelled = match (spelled.strip_prefix('$'), spelled.strip_prefix('@')) {
        (Some(argument), _) if recipients => format!(
            "${}",
            arguments
                .get(argument)
                .ok_or_else(|| RespellError::Unmapped(argument.to_owned()))?
        ),
        (_, Some(after_at)) => match SelectorPlaceholder::parse(after_at) {
            Some(placeholder) => placeholder.respelled(arguments)?.to_string(),
            None => return Ok(()),
        },
        _ => return Ok(()),
    };
    *entry = toml::Value::String(respelled);
    Ok(())
}

/// A JSON-schema `parameters` table's argument names: its `properties` keys and `required`.
fn respell_schema(parameters: &mut toml::Value, arguments: &BTreeMap<String, String>) -> Result<(), RespellError> {
    let rename = |argument: &str| {
        arguments
            .get(argument)
            .cloned()
            .ok_or_else(|| RespellError::Unmapped(argument.to_owned()))
    };
    if let Some(properties) = parameters.get_mut("properties").and_then(toml::Value::as_table_mut) {
        *properties = std::mem::take(properties)
            .into_iter()
            .map(|(argument, schema)| Ok((rename(&argument)?, schema)))
            .collect::<Result<_, RespellError>>()?;
    }
    if let Some(required) = parameters.get_mut("required").and_then(toml::Value::as_array_mut) {
        for argument in required.iter_mut() {
            if let Some(spelled) = argument.as_str() {
                *argument = toml::Value::String(rename(spelled)?);
            }
        }
    }
    Ok(())
}

/// `confined = true` adds the client tool to `deployment.confined_results`.
fn confine<'a>(policy: &mut toml::Table, targets: impl Iterator<Item = &'a ToolTarget>) {
    let confined: Vec<&str> = targets
        .filter(|target| target.confined)
        .map(|target| target.name.as_str())
        .collect();
    if confined.is_empty() {
        return;
    }
    // A deployment that is not a table, or a list that is not an array, is the composed
    // config's to refuse when it parses.
    let Some(names) = policy
        .entry("deployment")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .and_then(|deployment| {
            deployment
                .entry("confined_results")
                .or_insert_with(|| toml::Value::Array(Vec::new()))
                .as_array_mut()
        })
    else {
        return;
    };
    for name in confined {
        if !names.iter().any(|entry| entry.as_str() == Some(name)) {
            names.push(toml::Value::String(name.to_owned()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAP: &str = r#"
[aliases.tool.coding]
shell = [
  { name = "host/claude-code/Bash", args = { command = "command", to = "to" } },
  { name = "host/codex/exec_command", args = { command = "cmd", to = "recipient" }, confined = true },
]
"#;

    fn lowered(text: &str, batteries: Vec<(MapOrigin, ToolMaps)>) -> Result<toml::Value, ToolAliasError> {
        let mut document: toml::Value = toml::from_str(text).unwrap();
        let aliases = super::super::aliases::of_document(&document).unwrap();
        lower(&mut document, batteries, &aliases)?;
        Ok(document)
    }

    fn names(document: &toml::Value) -> Vec<&str> {
        document["policy"]["tool"]
            .as_array()
            .unwrap()
            .iter()
            .map(|rule| rule["name"].as_str().unwrap())
            .collect()
    }

    fn battery(name: &str, text: &str) -> (MapOrigin, ToolMaps) {
        let document: toml::Value = toml::from_str(text).unwrap();
        (
            MapOrigin::Battery(name.to_owned()),
            parse(document["aliases"]["tool"].clone()).unwrap(),
        )
    }

    #[test]
    fn each_client_copy_sits_at_the_canonical_rules_place_with_its_reads_renamed() {
        let document = lowered(
            &format!(
                r#"{MAP}
[policy]
version = 2
[[policy.tool]]
name = "host/claude-code/Read"
delta = {{}}
[[policy.tool]]
name = "tool/coding/shell(command:git push*)"
requires = {{ audience = {{ contains = ["$to"] }} }}
parameters = {{ type = "object", properties = {{ command = {{ type = "string" }}, to = {{ type = "string" }} }}, required = ["command", "to"] }}
[[policy.tool]]
name = "tool/coding/shell"
delta = {{ audience = ["@slack:channel/$to"] }}
[[policy.tool]]
name = "*"
delta = {{}}
"#
            ),
            Vec::new(),
        )
        .unwrap();
        assert_eq!(
            names(&document),
            [
                "host/claude-code/Read",
                "host/claude-code/Bash(command:git push*)",
                "host/codex/exec_command(cmd:git push*)",
                "host/claude-code/Bash",
                "host/codex/exec_command",
                "*",
            ]
        );
        let codex = &document["policy"]["tool"][2];
        assert_eq!(
            codex["requires"]["audience"]["contains"][0].as_str(),
            Some("$recipient")
        );
        let properties: BTreeSet<&String> = codex["parameters"]["properties"].as_table().unwrap().keys().collect();
        assert_eq!(properties, BTreeSet::from([&"cmd".to_owned(), &"recipient".to_owned()]));
        assert_eq!(
            codex["parameters"]["required"],
            toml::Value::Array(vec!["cmd".into(), "recipient".into()])
        );
        assert_eq!(
            document["policy"]["tool"][4]["delta"]["audience"][0].as_str(),
            Some("@slack:channel/$recipient")
        );
        assert_eq!(
            document["policy"]["deployment"]["confined_results"],
            toml::Value::Array(vec!["host/codex/exec_command".into()]),
            "only the target that opts in is confined"
        );
        assert!(document.get("aliases").is_none(), "the map is lowered away");
    }

    #[test]
    fn a_canonical_rule_no_map_binds_is_inactive() {
        let document = lowered(
            "[policy]\nversion = 2\n[[policy.tool]]\nname = 'tool/coding/web_search'\ndelta = {}\n",
            Vec::new(),
        )
        .unwrap();
        assert!(names(&document).is_empty());
    }

    #[test]
    fn a_rule_reading_an_argument_some_target_does_not_bind_is_refused() {
        let refused = lowered(
            &format!(
                "{MAP}[policy]\nversion = 2\n[[policy.tool]]\nname = 'tool/coding/shell(cwd:/tmp*)'\ndelta = {{}}\n"
            ),
            Vec::new(),
        );
        assert!(
            matches!(&refused, Err(ToolAliasError::UnmappedArgument { argument, .. }) if argument == "cwd"),
            "{refused:?}"
        );
    }

    #[test]
    fn a_map_must_be_well_formed_and_name_what_rules_name() {
        let rule = "[policy]\nversion = 2\n[[policy.tool]]\nname = 'tool/coding/shell'\ndelta = {}\n";
        let colliding = battery(
            "codex",
            "[aliases.tool.coding]\nshell = [{ name = 'host/codex/exec', args = { command = 'cmd', cwd = 'cmd' } }]\n",
        );
        assert!(matches!(
            lowered(rule, vec![colliding]),
            Err(ToolAliasError::CollidingArguments { argument, .. }) if argument == "cmd"
        ));
        let selector = battery(
            "codex",
            "[aliases.tool.coding]\nshell = [{ name = 'host/codex/exec(cmd:x)' }]\n",
        );
        assert!(matches!(
            lowered(rule, vec![selector]),
            Err(ToolAliasError::InvalidTarget { .. })
        ));
        let foreign = battery(
            "claude-code",
            "[aliases.tool.coding]\nshell = [{ name = 'host/claude-code/Bash' }]\n",
        );
        let refused = lowered("[policy]\nversion = 2\n", vec![foreign]).unwrap_err();
        assert!(
            matches!(&refused, ToolAliasError::UnnamedNamespace { origin, namespace }
                if origin == r#"battery "claude-code""# && namespace == "coding"),
            "{refused:?}"
        );
        let unruled = battery(
            "claude-code",
            "[aliases.tool.coding]\nread_file = [{ name = 'host/claude-code/Read' }]\n",
        );
        assert!(
            lowered(rule, vec![unruled]).is_ok(),
            "a canonical tool the included rules leave out maps to nothing"
        );
        for empty in ["[aliases.tool.coding]\nshell = []\n", "[aliases.tool]\ncoding = {}\n"] {
            assert!(matches!(
                lowered(rule, vec![battery("codex", empty)]),
                Err(ToolAliasError::BindsNothing { .. })
            ));
        }
        for argument in ["a,b", "a)", "a:b", "$a", "a/b", ""] {
            let spliced = battery(
                "codex",
                &format!(
                    "[aliases.tool.coding]\nshell = [{{ name = 'host/codex/exec', args = {{ command = {argument:?} }} }}]\n"
                ),
            );
            assert!(
                matches!(
                    lowered(rule, vec![spliced]),
                    Err(ToolAliasError::InvalidArgument { .. })
                ),
                "{argument:?}"
            );
        }
    }

    #[test]
    fn one_client_tool_has_one_canonical_reading_after_host_rebinding() {
        let rules = "[policy]\nversion = 2\n[[policy.tool]]\nname = 'tool/coding/shell'\ndelta = {}\n[[policy.tool]]\nname = 'tool/coding/read_file'\ndelta = {}\n";
        let claude = battery(
            "claude-code",
            "[aliases.tool.coding]\nshell = [{ name = 'host/claude-code/Bash' }]\n",
        );
        let opencode = battery(
            "opencode",
            "[aliases.tool.coding]\nshell = [{ name = 'host/opencode/bash' }]\n",
        );
        assert!(
            lowered(rules, vec![claude.clone(), opencode]).is_ok(),
            "distinct client tools may share a canonical tool"
        );
        let twice = battery(
            "other",
            "[aliases.tool.coding]\nread_file = [{ name = 'host/claude-code/Bash' }]\n",
        );
        assert!(matches!(
            lowered(rules, vec![claude.clone(), twice]),
            Err(ToolAliasError::ClaimedTwice { client, .. }) if client == "host/claude-code/Bash"
        ));
        let rebound = battery(
            "archestra",
            "[aliases.tool.coding]\nread_file = [{ name = 'host/archestra/Bash' }]\n",
        );
        let hosted = format!("[aliases]\nhost = {{ claude-code = ['archestra'] }}\n{rules}");
        assert!(matches!(
            lowered(&hosted, vec![claude, rebound]),
            Err(ToolAliasError::ClaimedTwice { client, .. }) if client == "host/archestra/Bash"
        ));
    }

    #[test]
    fn canonical_tools_are_confined_and_served_only_through_their_clients() {
        let confined = lowered(
            &format!(
                "{MAP}[policy]\nversion = 2\n[[policy.tool]]\nname = 'tool/coding/shell'\ndelta = {{}}\n[policy.deployment]\nconfined_results = ['tool/coding/shell']\n"
            ),
            Vec::new(),
        );
        assert!(matches!(confined, Err(ToolAliasError::DeploymentName { .. })));
        let server = lowered(
            &format!(
                "{MAP}[policy]\nversion = 2\n[[policy.tool]]\nname = 'tool/coding/shell'\nserver = 'x'\ndelta = {{}}\n"
            ),
            Vec::new(),
        );
        assert!(matches!(server, Err(ToolAliasError::ServerSelector { .. })));
    }
}
