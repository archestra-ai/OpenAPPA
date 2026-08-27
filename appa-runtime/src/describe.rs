//! A small, read-only description of the facts a configuration actor can rely on.
//!
//! This deliberately does not pretend that a standalone process can see Claude's
//! session tool catalogue or connector accounts. Those are session facts and must
//! be merged by the configuring actor.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::config::Config;

#[derive(Clone, Debug, PartialEq, Eq)]
struct ConfigDescription {
    path: PathBuf,
    state: ConfigState,
    diagnostic: Option<&'static str>,
    batteries: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConfigState {
    Missing,
    Unreadable,
    Unparsable,
    Invalid,
    Loadable,
}

impl ConfigState {
    fn as_str(self) -> &'static str {
        match self {
            ConfigState::Missing => "missing",
            ConfigState::Unreadable => "unreadable",
            ConfigState::Unparsable => "unparsable",
            ConfigState::Invalid => "invalid",
            ConfigState::Loadable => "loadable",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct PolicyDescription {
    tools: Vec<String>,
    referenced_groups: Vec<String>,
    membership: Option<MembershipDescription>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MembershipDescription {
    resolver: String,
    binding_configured: bool,
}

fn inspect(path: &Path) -> (ConfigDescription, PolicyDescription) {
    let mut config = ConfigDescription {
        path: path.to_path_buf(),
        state: ConfigState::Missing,
        diagnostic: Some("configuration file does not exist"),
        batteries: Vec::new(),
    };
    let mut policy = PolicyDescription::default();

    match std::fs::read_to_string(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => {
            config.state = ConfigState::Unreadable;
            config.diagnostic = Some("configuration file cannot be read");
        }
        Ok(text) => match toml::from_str::<toml::Value>(&text) {
            Err(_) => {
                config.state = ConfigState::Unparsable;
                config.diagnostic = Some("configuration is not valid TOML");
            }
            Ok(root) => {
                config.state = ConfigState::Invalid;
                config.diagnostic = Some("configuration is incomplete or does not validate");
                let includes: Vec<_> = root
                    .get("include")
                    .and_then(toml::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(toml::Value::as_str)
                    .map(str::to_owned)
                    .collect();
                config.batteries = includes
                    .iter()
                    .filter_map(|include| battery_name(Path::new(include)))
                    .collect();
                let mut seen_batteries = BTreeSet::new();
                config
                    .batteries
                    .retain(|battery| seen_batteries.insert(battery.clone()));

                if let Some(root_policy) = root.get("policy") {
                    describe_policy_value(root_policy, &root, &mut policy);
                }

                if let Ok(loaded) = Config::load(path) {
                    config.state = ConfigState::Loadable;
                    config.diagnostic = None;
                    describe_policy_value(loaded.policy_file().value(), &root, &mut policy);
                    if let Ok(source) = toml::to_string(loaded.policy_file().value())
                        && let Ok(compiled) = appa_policy::Config::from_toml_str(&source)
                    {
                        policy.referenced_groups = compiled
                            .registry()
                            .groups()
                            .iter()
                            .map(|group| format!("@{}", group.as_str()))
                            .collect();
                        policy.membership = compiled.registry().membership().map(|resolver| MembershipDescription {
                            resolver: resolver.as_str().to_string(),
                            binding_configured: loaded.externals.membership.contains_key(resolver.as_str()),
                        });
                    }
                }
            }
        },
    }

    (config, policy)
}

fn describe_policy_value(policy_value: &toml::Value, root: &toml::Value, out: &mut PolicyDescription) {
    out.tools = policy_value
        .get("tool")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|tool| tool.get("name"))
        .filter_map(toml::Value::as_str)
        .map(str::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    if let Ok(source) = toml::to_string(policy_value)
        && let Ok(compiled) = appa_policy::Config::from_toml_str(&source)
    {
        out.referenced_groups = compiled
            .registry()
            .groups()
            .iter()
            .map(|group| format!("@{}", group.as_str()))
            .collect();
        out.membership = compiled.registry().membership().map(|resolver| MembershipDescription {
            resolver: resolver.as_str().to_string(),
            binding_configured: root
                .get("externals")
                .and_then(|externals| externals.get("membership"))
                .and_then(|membership| membership.get(resolver.as_str()))
                .is_some(),
        });
    }
}

fn battery_name(path: &Path) -> Option<String> {
    let parts: Vec<_> = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => part.to_str(),
            _ => None,
        })
        .collect();
    parts
        .windows(3)
        .find_map(|window| (window[0] == "batteries" && window[2] == "appa.toml").then(|| window[1].to_string()))
}

pub fn render(path: &Path, adapter: &'static str) -> String {
    let (config, policy) = inspect(path);
    let mut output = String::new();
    let _ = writeln!(output, "OpenAPPA world");
    let _ = writeln!(output, "Adapter: {adapter}");
    let _ = writeln!(output, "Config: {} ({})", config.path.display(), config.state.as_str());
    if let Some(diagnostic) = config.diagnostic {
        let _ = writeln!(output, "  {diagnostic}");
    }
    let _ = writeln!(output, "Batteries: {}", list_or_none(&config.batteries));
    let _ = writeln!(output, "Policy tools: {}", list_or_none(&policy.tools));
    let _ = writeln!(output, "Referenced groups: {}", list_or_none(&policy.referenced_groups));
    match &policy.membership {
        Some(membership) => {
            let binding = if membership.binding_configured {
                "binding configured"
            } else {
                "binding missing"
            };
            let _ = writeln!(output, "Membership: {} ({binding})", membership.resolver);
        }
        None => {
            let _ = writeln!(output, "Membership: none");
        }
    }
    let _ = writeln!(
        output,
        "Session integrations/tools/accounts: unavailable to this command"
    );
    output
}

fn list_or_none(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_string()
    } else {
        items.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_config_is_described_without_creating_it() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("missing.toml");

        let (config, policy) = inspect(&path);

        assert_eq!(config.state, ConfigState::Missing);
        assert!(!path.exists());
        assert!(policy.tools.is_empty());
    }

    #[test]
    fn loadable_config_reports_batteries_tools_groups_and_membership() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let battery = directory.path().join("batteries/mail");
        std::fs::create_dir_all(&battery).expect("battery directory");
        std::fs::write(
            battery.join("appa.toml"),
            "[policy]\nversion = 1\n[[policy.tool]]\nname = \"mail_read\"\ndelta = { audience = [\"@self\"] }\n",
        )
        .expect("battery config");
        let root = directory.path().join("appa.toml");
        std::fs::write(
            &root,
            "include = [\"batteries/mail/appa.toml\"]\n[policy]\nversion = 1\n[policy.membership]\nname = \"directory\"\n[externals]\ntimeout_ms = 1000\nmax_body_bytes = 65536\n[externals.membership.directory]\ncommand = [\"true\"]\n",
        )
        .expect("root config");

        Config::load(&root).unwrap_or_else(|error| panic!("fixture must load: {error}"));

        let (config, policy) = inspect(&root);

        assert_eq!(config.state, ConfigState::Loadable);
        assert_eq!(config.batteries, ["mail"]);
        assert_eq!(policy.tools, ["mail_read"]);
        assert_eq!(policy.referenced_groups, ["@self"]);
        assert_eq!(
            policy.membership,
            Some(MembershipDescription {
                resolver: "directory".to_string(),
                binding_configured: true,
            })
        );
    }

    #[test]
    fn malformed_config_does_not_echo_its_contents() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("appa.toml");
        let secret = "super-secret-token";
        std::fs::write(&path, format!("token = \\\"{secret}")).expect("malformed config");

        let (config, _) = inspect(&path);
        let output = render(&path, "claude-code");

        assert_eq!(config.state, ConfigState::Unparsable);
        assert!(!output.contains(secret));
    }

    #[test]
    fn human_output_is_small_and_explicit_about_unknown_session_facts() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let output = render(&directory.path().join("appa.toml"), "claude-code");

        assert!(output.contains("Config:"));
        assert!(output.contains("Batteries: none"));
        assert!(output.contains("Session integrations/tools/accounts: unavailable"));
    }
}
