//! What a host has connected, read from the host's own configuration, and
//! the batteries of a version that cover it. An install suggests those; it
//! never includes a battery on a person's behalf.
//!
//! Discovery is the one host-specific step: each host keeps its MCP servers
//! in its own files. Matching a server to a battery is not: a battery declares
//! the namespaces its contracts name, and a server key is the namespace the
//! harness reports for that server's tools.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use appa_package::{Battery, Host, Marketplace, Namespace, PackageKind, PackageName};

use super::{InstallError, battery_at};

/// The MCP servers `host` has configured, as the namespaces their tool keys
/// spell. Discovery reads configuration and never runs a server. A host with
/// no readable configuration has no servers.
pub(crate) fn servers(host: Host, cwd: &Path) -> BTreeSet<Namespace> {
    match host {
        Host::ClaudeCode => {
            let config = crate::init::paths::claude_config_file().unwrap_or_else(|error| {
                eprintln!("appa: warning: Claude Code's configuration was not read for its MCP servers: {error}");
                None
            });
            claude_code_servers(config.as_deref(), &project_root(cwd))
        }
        Host::Kagent => BTreeSet::new(),
    }
}

/// Claude Code keys a project's servers by its repository root, or by the
/// directory itself outside a repository.
fn project_root(cwd: &Path) -> PathBuf {
    cwd.ancestors()
        .find(|directory| directory.join(".git").exists())
        .unwrap_or(cwd)
        .to_path_buf()
}

/// The keys of every `mcpServers` map Claude Code reads for `project`: the
/// user scope and the project's local scope in `config`, and the project
/// scope in the project's own `.mcp.json`.
fn claude_code_servers(config: Option<&Path>, project: &Path) -> BTreeSet<Namespace> {
    let mut servers = BTreeSet::new();
    if let Some(document) = config.and_then(read_json) {
        servers.extend(server_keys(&document));
        for spelling in project_spellings(project) {
            if let Some(local) = document["projects"].get(spelling.as_str()) {
                servers.extend(server_keys(local));
            }
        }
    }
    if let Some(document) = read_json(&project.join(".mcp.json")) {
        servers.extend(server_keys(&document));
    }
    servers
}

/// The project path as `claude mcp add` may have keyed it: as given, and
/// with its links resolved.
fn project_spellings(project: &Path) -> BTreeSet<String> {
    [Some(project.to_path_buf()), project.canonicalize().ok()]
        .into_iter()
        .flatten()
        .map(|path| path.to_string_lossy().into_owned())
        .collect()
}

/// A file that is absent has no servers. One the installer would not read
/// as state (a link, a FIFO, a file past the state byte limit) or that is
/// not JSON is named on stderr and has none either: a suggestion is never
/// worth failing an install over, and a project's `.mcp.json` is a
/// stranger's file.
fn read_json(path: &Path) -> Option<serde_json::Value> {
    let skip = |reason: &dyn std::fmt::Display| {
        eprintln!(
            "appa: warning: {} was not read for its MCP servers: {reason}",
            path.display()
        );
        None
    };
    let bytes = match super::optional_bytes(path) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return None,
        Err(error) => return skip(&error),
    };
    match serde_json::from_slice(&bytes) {
        Ok(document) => Some(document),
        Err(error) => skip(&error),
    }
}

/// A key that is not a namespace names nothing the runtime could gate, so it
/// is not a server here.
fn server_keys(document: &serde_json::Value) -> impl Iterator<Item = Namespace> + '_ {
    document["mcpServers"]
        .as_object()
        .into_iter()
        .flat_map(|servers| servers.keys())
        .filter_map(|key| Namespace::parse(key).ok())
}

/// The batteries of `catalog`, the version at `marketplace`, written for `host`.
pub(crate) fn batteries(
    marketplace: &Path,
    catalog: &Marketplace,
    host: Host,
) -> Result<Vec<(PackageName, Battery)>, InstallError> {
    let mut batteries = Vec::new();
    for entry in catalog
        .packages
        .iter()
        .filter(|entry| entry.kind == PackageKind::Battery)
    {
        let battery = battery_at(marketplace, entry)?;
        if battery.hosts.contains(&host) {
            batteries.push((entry.name.clone(), battery));
        }
    }
    Ok(batteries)
}

/// One battery an install would suggest for the servers it covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Suggestion {
    /// Included as it is, the battery covers the servers its namespaces name
    /// or are bound to.
    Plain(PackageName),
    /// The battery's contracts name another namespace than the server's key,
    /// so including it takes a `--server` binding of that key.
    Bound { battery: PackageName, server: Namespace },
}

/// How a version's batteries cover the discovered servers.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Coverage {
    pub(crate) suggestions: Vec<Suggestion>,
    /// Servers no battery of the version covers; the annotator gates their
    /// tools until the guide writes rules for them.
    pub(crate) uncovered: Vec<Namespace>,
}

/// How one server key matched a battery.
#[derive(Clone, Copy)]
enum Match<'a> {
    /// The key is a namespace the battery declares.
    Native,
    /// A namespace of the battery is already bound to the key.
    Bound,
    /// The key is the battery's name; its one namespace needs a binding to it.
    Named(&'a Namespace),
}

/// Matching is exact, never by substring: a server key equal to a namespace a
/// battery declares is that battery's, as is one a namespace is already
/// bound to in `bindings` (namespace to server key), and one equal to the
/// name of a battery over one namespace, bound to it. A battery already
/// included covers the servers it matches natively or through a binding;
/// those are not suggested again. A binding moves the battery's rules to the
/// bound key, so a bound battery serves one server: a battery is suggested
/// once, with the binding when a server needs it, and a server its own
/// namespace names is then uncovered.
pub(crate) fn coverage(
    servers: &BTreeSet<Namespace>,
    batteries: &[(PackageName, Battery)],
    included: &BTreeSet<String>,
    bindings: &BTreeMap<String, String>,
) -> Coverage {
    let bound_to = |namespace: &Namespace| bindings.get(namespace.as_str()).map(String::as_str);
    // A binding in the config already sends the battery's rules to another
    // key, included or not; one to the key itself changes nothing.
    let redirected = |server: &Namespace, matched: Match<'_>| match matched {
        Match::Native => bound_to(server).is_some_and(|bound| bound != server.as_str()),
        Match::Named(namespace) => bound_to(namespace).is_some(),
        Match::Bound => false,
    };
    let covers: Vec<(&Namespace, Option<(&PackageName, Match<'_>)>)> = servers
        .iter()
        .map(|server| {
            let by_namespace = batteries
                .iter()
                .find(|(_, battery)| battery.namespaces.contains(server))
                .map(|(name, _)| (name, Match::Native));
            let by_binding = batteries
                .iter()
                .find(|(_, battery)| {
                    battery
                        .namespaces
                        .iter()
                        .any(|namespace| bound_to(namespace) == Some(server.as_str()))
                })
                .map(|(name, _)| (name, Match::Bound));
            let by_name = batteries
                .iter()
                .find(|(name, battery)| name.as_str() == server.as_str() && battery.namespaces.len() == 1)
                .map(|(name, battery)| (name, Match::Named(&battery.namespaces[0])));
            // A battery already included and covering the key is its cover,
            // ahead of one that would only be suggested for it.
            let mut candidates = [by_namespace, by_binding, by_name].into_iter().flatten();
            let already = candidates.clone().find(|(name, _)| included.contains(name.as_str()));
            (server, already.or_else(|| candidates.next()))
        })
        .collect();
    // The included batteries gating a server as they stand: a binding
    // suggested for one of them would take that server away from it.
    let gating: BTreeSet<&PackageName> = covers
        .iter()
        .filter_map(|(server, cover)| match cover {
            Some((name, matched))
                if included.contains(name.as_str())
                    && !matches!(matched, Match::Named(_))
                    && !redirected(server, *matched) =>
            {
                Some(*name)
            }
            _ => None,
        })
        .collect();
    // The batteries a binding is suggested for: a battery named by a server
    // key, its one namespace unbound and not gating a server as it stands. A
    // key is one battery's name, so a battery gets at most one binding.
    let binding: BTreeSet<&PackageName> = covers
        .iter()
        .filter_map(|(server, cover)| match cover {
            Some((name, matched @ Match::Named(_))) if !redirected(server, *matched) && !gating.contains(name) => {
                Some(*name)
            }
            _ => None,
        })
        .collect();
    let mut suggestions: BTreeMap<PackageName, Suggestion> = BTreeMap::new();
    let mut uncovered = Vec::new();
    for (server, cover) in covers {
        let Some((name, matched)) = cover else {
            uncovered.push(server.clone());
            continue;
        };
        if redirected(server, matched) {
            uncovered.push(server.clone());
            continue;
        }
        match matched {
            Match::Named(_) if binding.contains(name) => {
                suggestions.insert(
                    name.clone(),
                    Suggestion::Bound {
                        battery: name.clone(),
                        server: server.clone(),
                    },
                );
            }
            // Every namespace a battery declares, or is bound to, is served
            // by the one plain include; a suggested binding takes the battery
            // for one server, and its other key goes uncovered.
            Match::Named(_) => uncovered.push(server.clone()),
            Match::Native | Match::Bound if binding.contains(name) => uncovered.push(server.clone()),
            Match::Native | Match::Bound => {
                if !included.contains(name.as_str()) {
                    suggestions
                        .entry(name.clone())
                        .or_insert_with(|| Suggestion::Plain(name.clone()));
                }
            }
        }
    }
    uncovered.sort();
    Coverage {
        suggestions: suggestions.into_values().collect(),
        uncovered,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use appa_package::RelativePath;

    fn namespace(text: &str) -> Namespace {
        Namespace::parse(text).unwrap()
    }

    fn battery(name: &str, namespaces: &[&str]) -> (PackageName, Battery) {
        (
            PackageName::parse(name).unwrap(),
            Battery {
                policy: RelativePath::parse("appa.toml").unwrap(),
                hosts: vec![Host::ClaudeCode],
                namespaces: namespaces.iter().map(|text| namespace(text)).collect(),
                helpers: vec![],
                audiences: vec![],
                credentials: vec![],
                setup: None,
            },
        )
    }

    fn suggested(battery: &str) -> Suggestion {
        Suggestion::Plain(PackageName::parse(battery).unwrap())
    }

    fn suggested_binding(battery: &str, server: &str) -> Suggestion {
        Suggestion::Bound {
            battery: PackageName::parse(battery).unwrap(),
            server: namespace(server),
        }
    }

    fn included(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn bindings(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(namespace, server)| ((*namespace).to_owned(), (*server).to_owned()))
            .collect()
    }

    /// The three places Claude Code reads servers from are read together, the
    /// project's local scope under either spelling of its path; keys that are
    /// not namespaces are left out.
    #[test]
    fn claude_code_servers_come_from_the_user_local_and_project_scopes() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let canonical = project.canonicalize().unwrap().to_string_lossy().into_owned();
        let config = root.path().join(".claude.json");
        std::fs::write(
            &config,
            serde_json::json!({
                "mcpServers": {"appa": {}, "github": {}, "bad__key": {}},
                "projects": {canonical: {"mcpServers": {"linear": {}}}}
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(project.join(".mcp.json"), r#"{"mcpServers": {"sentry": {}}}"#).unwrap();

        let servers = claude_code_servers(Some(&config), &project);

        assert_eq!(
            servers,
            BTreeSet::from([
                namespace("appa"),
                namespace("github"),
                namespace("linear"),
                namespace("sentry")
            ])
        );
    }

    /// A missing file is no servers, and so is one that is not JSON: an
    /// install is never refused over a suggestion.
    #[test]
    fn unreadable_claude_code_configuration_is_no_servers() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        assert!(claude_code_servers(Some(&root.path().join("absent.json")), &project).is_empty());
        assert!(claude_code_servers(None, &project).is_empty());

        std::fs::write(project.join(".mcp.json"), "{not json").unwrap();
        assert!(claude_code_servers(None, &project).is_empty());
    }

    #[test]
    fn a_project_is_keyed_by_its_repository_root() {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("repo");
        let nested = repository.join("crates/one");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(repository.join(".git")).unwrap();
        assert_eq!(project_root(&nested), repository);

        let loose = root.path().join("loose");
        std::fs::create_dir_all(&loose).unwrap();
        assert_eq!(project_root(&loose), loose);
    }

    /// A server is matched by the namespace a battery declares, or by the
    /// battery's name with a binding of the server's key; a substring is no
    /// match, and an included battery is not suggested again.
    #[test]
    fn coverage_matches_servers_exactly_and_skips_included_batteries() {
        let batteries = vec![
            battery("github", &["github"]),
            battery("slack", &["claude_ai_Slack"]),
            battery("linear", &["linear"]),
        ];
        let servers = BTreeSet::from([
            namespace("github"),
            namespace("slack"),
            namespace("linear"),
            namespace("linear-server"),
            namespace("github-status-bot"),
        ]);

        let coverage = coverage(&servers, &batteries, &included(&["linear"]), &BTreeMap::new());

        assert_eq!(
            coverage.suggestions,
            vec![suggested("github"), suggested_binding("slack", "slack")]
        );
        assert_eq!(
            coverage.uncovered,
            vec![namespace("github-status-bot"), namespace("linear-server")]
        );
    }

    /// An included battery covers a server its namespace names, but a server
    /// matched by the battery's name is covered only once bound; until then
    /// the binding is still suggested. Bound elsewhere, the battery's rules
    /// no longer name the connector's own key, so that server is uncovered.
    #[test]
    fn an_included_battery_is_suggested_again_only_for_a_binding_it_lacks() {
        let batteries = vec![battery("slack", &["claude_ai_Slack"])];
        let servers = BTreeSet::from([namespace("slack")]);

        let unbound = coverage(&servers, &batteries, &included(&["slack"]), &BTreeMap::new());
        assert_eq!(unbound.suggestions, vec![suggested_binding("slack", "slack")]);
        assert_eq!(unbound.uncovered, vec![]);

        let bound = bindings(&[("claude_ai_Slack", "slack")]);
        assert_eq!(
            coverage(&servers, &batteries, &included(&["slack"]), &bound),
            Coverage::default()
        );

        let native = BTreeSet::from([namespace("claude_ai_Slack")]);
        let redirected = coverage(&native, &batteries, &included(&["slack"]), &bound);
        assert_eq!(redirected.suggestions, vec![]);
        assert_eq!(redirected.uncovered, vec![namespace("claude_ai_Slack")]);

        // Unbound and gating the connector's own key, the battery is not
        // offered a binding that would take that key away from it: the alias
        // is uncovered instead.
        let both = BTreeSet::from([namespace("claude_ai_Slack"), namespace("slack")]);
        let kept = coverage(&both, &batteries, &included(&["slack"]), &BTreeMap::new());
        assert_eq!(kept.suggestions, vec![]);
        assert_eq!(kept.uncovered, vec![namespace("slack")]);
    }

    /// One battery serves one server: with both the connector's own key and
    /// a name alias connected, the binding is suggested and the native key
    /// is reported uncovered rather than hidden behind the one suggestion.
    #[test]
    fn a_battery_needed_under_two_keys_is_suggested_once_and_the_other_key_is_uncovered() {
        let batteries = vec![battery("slack", &["claude_ai_Slack"])];
        let servers = BTreeSet::from([namespace("claude_ai_Slack"), namespace("slack")]);

        let once = coverage(&servers, &batteries, &BTreeSet::new(), &BTreeMap::new());

        assert_eq!(once.suggestions, vec![suggested_binding("slack", "slack")]);
        assert_eq!(once.uncovered, vec![namespace("claude_ai_Slack")]);
    }

    /// A battery over several namespaces covers every server they name with
    /// its one include: one suggestion, nothing uncovered. Bound, it is not
    /// suggested with `--server`, which takes a battery over one namespace.
    #[test]
    fn a_battery_over_several_namespaces_is_one_plain_suggestion() {
        let batteries = vec![battery("acme", &["acme-docs", "acme-api"])];
        let servers = BTreeSet::from([namespace("acme-docs"), namespace("acme-api")]);

        let one = coverage(&servers, &batteries, &BTreeSet::new(), &BTreeMap::new());
        assert_eq!(one.suggestions, vec![suggested("acme")]);
        assert_eq!(one.uncovered, vec![]);

        let bound = bindings(&[("acme-api", "work-acme")]);
        let through_binding = coverage(
            &BTreeSet::from([namespace("work-acme")]),
            &batteries,
            &BTreeSet::new(),
            &bound,
        );
        assert_eq!(through_binding.suggestions, vec![suggested("acme")]);

        // Each namespace bound to its own server: one include covers them all.
        let each = bindings(&[("acme-docs", "w1"), ("acme-api", "w2")]);
        let servers = BTreeSet::from([namespace("w1"), namespace("w2")]);
        let all = coverage(&servers, &batteries, &BTreeSet::new(), &each);
        assert_eq!(all.suggestions, vec![suggested("acme")]);
        assert_eq!(all.uncovered, vec![]);
        assert_eq!(
            coverage(&servers, &batteries, &included(&["acme"]), &each),
            Coverage::default()
        );
    }

    /// A key another battery declares natively is still the included
    /// battery's when a binding sends that battery to it: nothing is
    /// suggested for a server an included battery already gates.
    #[test]
    fn an_included_battery_bound_to_a_key_covers_it_ahead_of_a_native_match() {
        let batteries = vec![battery("work", &["work-github"]), battery("github", &["github"])];
        let servers = BTreeSet::from([namespace("work-github")]);
        let bound = bindings(&[("github", "work-github")]);

        assert_eq!(
            coverage(&servers, &batteries, &included(&["github"]), &bound),
            Coverage::default()
        );
        let neither = coverage(&servers, &batteries, &BTreeSet::new(), &bound);
        assert_eq!(neither.suggestions, vec![suggested("work")]);
    }

    /// A server a battery's namespace is already bound to is that battery's:
    /// covered when the battery is included, and a plain suggestion when it
    /// is not, since the binding is already in the config.
    #[test]
    fn a_server_a_namespace_is_bound_to_matches_its_battery() {
        let batteries = vec![battery("github", &["github"])];
        let servers = BTreeSet::from([namespace("work-github")]);
        let bound = bindings(&[("github", "work-github")]);

        assert_eq!(
            coverage(&servers, &batteries, &included(&["github"]), &bound),
            Coverage::default()
        );

        let absent = coverage(&servers, &batteries, &BTreeSet::new(), &bound);
        assert_eq!(absent.suggestions, vec![suggested("github")]);
        assert_eq!(absent.uncovered, vec![]);

        // A binding to the key itself changes nothing.
        let identity = bindings(&[("github", "github")]);
        let own = BTreeSet::from([namespace("github")]);
        assert_eq!(
            coverage(&own, &batteries, &included(&["github"]), &identity),
            Coverage::default()
        );

        // The binding in the config already sends the battery's rules to
        // `work-github`, so its own key is uncovered whether the battery is
        // included or only suggested.
        let both = BTreeSet::from([namespace("github"), namespace("work-github")]);
        for included in [BTreeSet::new(), included(&["github"])] {
            let redirected = coverage(&both, &batteries, &included, &bound);
            assert_eq!(redirected.uncovered, vec![namespace("github")]);
            assert_eq!(redirected.suggestions.len(), included.is_empty() as usize);
        }
    }
}
