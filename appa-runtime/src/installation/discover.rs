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

use super::{InstallError, battery_package};

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
        let (_, battery) = battery_package(marketplace, entry.name.as_str())?;
        if battery.hosts.contains(&host) {
            batteries.push((entry.name.clone(), battery));
        }
    }
    Ok(batteries)
}

/// One battery an install would suggest for one discovered server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Suggestion {
    pub(crate) battery: PackageName,
    pub(crate) server: Namespace,
    /// The battery's contracts name another namespace than the server's key,
    /// so including it takes a `--server` binding of that key.
    pub(crate) bind_server: bool,
}

/// How a version's batteries cover the discovered servers.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Coverage {
    pub(crate) suggestions: Vec<Suggestion>,
    /// Servers no battery of the version covers; the annotator gates their
    /// tools until the guide writes rules for them.
    pub(crate) uncovered: Vec<Namespace>,
}

impl Coverage {
    /// The commands that include the suggested batteries: every battery
    /// without a binding in one `appa battery install`, and one command per
    /// binding, since a binding names one battery.
    pub(crate) fn commands(&self, config: Option<&Path>) -> Vec<String> {
        let target = config
            .map(|config| format!(" --config {}", shell_word(&config.to_string_lossy())))
            .unwrap_or_default();
        let plain: Vec<&str> = self
            .suggestions
            .iter()
            .filter(|suggestion| !suggestion.bind_server)
            .map(|suggestion| suggestion.battery.as_str())
            .collect();
        let mut commands = Vec::new();
        if !plain.is_empty() {
            commands.push(format!("appa battery install {}{target}", plain.join(" ")));
        }
        for suggestion in self.suggestions.iter().filter(|suggestion| suggestion.bind_server) {
            commands.push(format!(
                "appa battery install {} --server {}{target}",
                suggestion.battery,
                suggestion.server.as_str()
            ));
        }
        commands
    }
}

/// `text` as one word of a POSIX shell command line: as it is when every
/// character is one a shell passes through, single-quoted otherwise.
fn shell_word(text: &str) -> String {
    let plain = !text.is_empty()
        && !text.starts_with('-')
        && text
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "/._~+:@,-".contains(character));
    match plain {
        true => text.to_owned(),
        false => format!("'{}'", text.replace('\'', "'\\''")),
    }
}

/// Matching is exact, never by substring: a server key equal to a namespace a
/// battery declares is that battery's, and one equal to a battery's name is
/// too, bound as that battery's one namespace, as is one a namespace is
/// already bound to in `bindings` (namespace to server key). A battery
/// already included covers the servers its namespaces name or are bound to;
/// those are not suggested again. A binding moves the battery's rules to the
/// bound key, so one battery serves one server: it is suggested once, with
/// the binding when a server needs it, and a server its own namespace names
/// is then uncovered.
pub(crate) fn coverage(
    servers: &BTreeSet<Namespace>,
    batteries: &[(PackageName, Battery)],
    included: &BTreeSet<String>,
    bindings: &BTreeMap<String, String>,
) -> Coverage {
    let mut suggestions: BTreeMap<PackageName, Suggestion> = BTreeMap::new();
    let mut uncovered = Vec::new();
    for server in servers {
        let by_namespace = batteries
            .iter()
            .find(|(_, battery)| battery.namespaces.contains(server))
            .map(|(name, _)| (name, None));
        let by_binding = batteries.iter().find_map(|(name, battery)| {
            battery
                .namespaces
                .iter()
                .find(|namespace| bindings.get(namespace.as_str()) == Some(&server.as_str().to_owned()))
                .map(|namespace| (name, Some(namespace)))
        });
        let by_name = batteries
            .iter()
            .find(|(name, battery)| name.as_str() == server.as_str() && battery.namespaces.len() == 1)
            .map(|(name, battery)| (name, Some(&battery.namespaces[0])));
        let Some((name, binds)) = by_namespace.or(by_binding).or(by_name) else {
            uncovered.push(server.clone());
            continue;
        };
        if included.contains(name.as_str()) {
            // A binding moves the battery's rules to the bound server key, so
            // a battery serves one server: the one its namespace is bound to,
            // or the namespace's own server while unbound.
            let namespace = binds.unwrap_or(server);
            match bindings.get(namespace.as_str()).map(String::as_str) {
                None if binds.is_none() => continue,
                Some(bound) if bound == server.as_str() => continue,
                None => {}
                Some(_) => {
                    uncovered.push(server.clone());
                    continue;
                }
            }
        }
        let suggestion = Suggestion {
            battery: name.clone(),
            server: server.clone(),
            bind_server: binds.is_some(),
        };
        match suggestions.get(name) {
            None => {
                suggestions.insert(name.clone(), suggestion);
            }
            Some(existing) if existing.bind_server => uncovered.push(server.clone()),
            Some(existing) if suggestion.bind_server => {
                uncovered.push(existing.server.clone());
                suggestions.insert(name.clone(), suggestion);
            }
            Some(_) => uncovered.push(server.clone()),
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
            },
        )
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
        let included = BTreeSet::from(["linear".to_owned()]);

        let coverage = coverage(&servers, &batteries, &included, &BTreeMap::new());

        assert_eq!(
            coverage.suggestions,
            vec![
                Suggestion {
                    battery: PackageName::parse("github").unwrap(),
                    server: namespace("github"),
                    bind_server: false,
                },
                Suggestion {
                    battery: PackageName::parse("slack").unwrap(),
                    server: namespace("slack"),
                    bind_server: true,
                },
            ]
        );
        assert_eq!(
            coverage.uncovered,
            vec![namespace("github-status-bot"), namespace("linear-server")]
        );
        assert_eq!(
            coverage.commands(None),
            vec![
                "appa battery install github",
                "appa battery install slack --server slack"
            ]
        );
    }

    /// An included battery covers a server its namespace names, but a server
    /// matched by the battery's name is covered only once bound; until then
    /// the binding is still suggested. One battery is suggested once, with
    /// the binding when any of its servers needs it.
    #[test]
    fn an_included_battery_is_suggested_again_only_for_a_binding_it_lacks() {
        let batteries = vec![battery("slack", &["claude_ai_Slack"])];
        let included = BTreeSet::from(["slack".to_owned()]);
        let servers = BTreeSet::from([namespace("slack")]);

        let unbound = coverage(&servers, &batteries, &included, &BTreeMap::new());
        assert_eq!(
            unbound.commands(None),
            vec!["appa battery install slack --server slack"]
        );

        let bindings = BTreeMap::from([("claude_ai_Slack".to_owned(), "slack".to_owned())]);
        let bound = coverage(&servers, &batteries, &included, &bindings);
        assert_eq!(bound, Coverage::default());

        // Bound to `slack`, the battery's rules no longer name the connector's
        // own key, so that server is uncovered, not silently covered.
        let native = BTreeSet::from([namespace("claude_ai_Slack")]);
        let redirected = coverage(&native, &batteries, &included, &bindings);
        assert_eq!(redirected.suggestions, vec![]);
        assert_eq!(redirected.uncovered, vec![namespace("claude_ai_Slack")]);

        // One battery serves one server: the binding is suggested and the
        // connector's own key, which the bound battery will not name, is
        // reported uncovered rather than hidden behind the one suggestion.
        let both = BTreeSet::from([namespace("claude_ai_Slack"), namespace("slack")]);
        let once = coverage(&both, &batteries, &BTreeSet::new(), &BTreeMap::new());
        assert_eq!(once.commands(None), vec!["appa battery install slack --server slack"]);
        assert_eq!(once.uncovered, vec![namespace("claude_ai_Slack")]);
    }

    /// A server a battery's namespace is already bound to is that battery's:
    /// covered when the battery is included, and suggested with the same
    /// binding when it is not.
    #[test]
    fn a_server_a_namespace_is_bound_to_matches_its_battery() {
        let batteries = vec![battery("github", &["github"])];
        let servers = BTreeSet::from([namespace("work-github")]);
        let bindings = BTreeMap::from([("github".to_owned(), "work-github".to_owned())]);

        let included = coverage(&servers, &batteries, &BTreeSet::from(["github".to_owned()]), &bindings);
        assert_eq!(included, Coverage::default());

        let absent = coverage(&servers, &batteries, &BTreeSet::new(), &bindings);
        assert_eq!(
            absent.commands(None),
            vec!["appa battery install github --server work-github"]
        );
        assert_eq!(absent.uncovered, vec![]);
    }

    /// Every battery without a binding goes in one command, so the person
    /// runs one line; each binding is its own, as the command takes one.
    #[test]
    fn commands_batch_plain_batteries_and_separate_bindings() {
        let batteries = vec![
            battery("github", &["github"]),
            battery("linear", &["linear"]),
            battery("slack", &["claude_ai_Slack"]),
        ];
        let servers = BTreeSet::from([namespace("github"), namespace("linear"), namespace("slack")]);

        let commands = coverage(&servers, &batteries, &BTreeSet::new(), &BTreeMap::new())
            .commands(Some(Path::new("./deployment/appa.toml")));

        assert_eq!(
            commands,
            vec![
                "appa battery install github linear --config ./deployment/appa.toml",
                "appa battery install slack --server slack --config ./deployment/appa.toml"
            ]
        );

        let spaced = coverage(&servers, &batteries, &BTreeSet::new(), &BTreeMap::new())
            .commands(Some(Path::new("/Users/me/my deployment/it's.toml")));
        assert_eq!(
            spaced[0],
            "appa battery install github linear --config '/Users/me/my deployment/it'\\''s.toml'"
        );
        assert_eq!(shell_word("-deploy/appa.toml"), "'-deploy/appa.toml'");
    }
}
