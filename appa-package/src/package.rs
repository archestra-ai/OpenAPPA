//! A package manifest: `appa-package.toml` beside the package's own files.
//!
//! A package is either a battery or a plugin. A plugin carries the fields
//! needed to install APPA support for its host.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::manifest::{ManifestError, SCHEMA};
use crate::names::{Host, NameError, Namespace, PackageName, RelativePath, lower_kebab};

/// The manifest file every package carries.
pub const MANIFEST_FILE: &str = "appa-package.toml";

/// One of a kagent plugin's named images.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImageName(String);

impl ImageName {
    pub fn parse(text: &str) -> Result<Self, NameError> {
        match lower_kebab(text) {
            true => Ok(Self(text.to_owned())),
            false => Err(NameError::Malformed(text.to_owned())),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ImageName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A container image reference, digest-pinned or not. This crate does not read
/// registries, so it holds the reference and refuses only an unusable one.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImageReference(String);

impl ImageReference {
    pub fn parse(text: &str) -> Option<Self> {
        let usable = !text.is_empty()
            && !text
                .chars()
                .any(|character| character.is_whitespace() || character.is_control());
        usable.then(|| Self(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ImageReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A battery: one policy file, the hosts it is written for, the namespaces its
/// contracts may name, and the helper programs its externals may run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Battery {
    pub policy: RelativePath,
    pub hosts: Vec<Host>,
    /// The tool namespaces this battery covers. A battery usually covers the
    /// one namespace its own name spells, which is why the field defaults to
    /// it. It cannot always: the namespace is the tool key the harness
    /// reports, and a connector reports keys such as `claude_ai_Slack` that no
    /// package name can spell. A battery covering several endpoints of one
    /// product names each of them.
    pub namespaces: Vec<Namespace>,
    pub helpers: Vec<RelativePath>,
}

/// A plugin package, with installation fields specific to its host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plugin {
    ClaudeCode {
        default_policy: RelativePath,
        /// Batteries a first install of this plugin includes with it.
        batteries: Vec<PackageName>,
    },
    Kagent {
        default_policy: RelativePath,
        batteries: Vec<PackageName>,
        images: BTreeMap<ImageName, ImageReference>,
    },
}

impl Plugin {
    /// The host this plugin connects, which is also the tag its manifest block
    /// is written under.
    pub fn host(&self) -> Host {
        match self {
            Self::ClaudeCode { .. } => Host::ClaudeCode,
            Self::Kagent { .. } => Host::Kagent,
        }
    }

    pub fn default_policy(&self) -> &RelativePath {
        match self {
            Self::ClaudeCode { default_policy, .. } | Self::Kagent { default_policy, .. } => default_policy,
        }
    }

    /// The batteries a first install of this plugin includes with it. Later
    /// installs leave the battery selection to the person.
    pub fn batteries(&self) -> &[PackageName] {
        match self {
            Self::ClaudeCode { batteries, .. } | Self::Kagent { batteries, .. } => batteries,
        }
    }
}

/// What a package is. A manifest declares exactly one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    Battery(Battery),
    Plugin(Plugin),
}

/// A parsed package manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub name: PackageName,
    pub description: String,
    pub role: Role,
}

impl Package {
    pub fn read(path: &Path) -> Result<Self, ManifestError> {
        let text = fs::read_to_string(path).map_err(|source| ManifestError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&text, path)
    }

    /// `path` names the file the text came from; it appears in every refusal.
    pub fn parse(text: &str, path: &Path) -> Result<Self, ManifestError> {
        let raw: RawPackage = toml::from_str(text).map_err(|source| ManifestError::Syntax {
            path: path.to_path_buf(),
            source,
        })?;
        if raw.schema != SCHEMA {
            return Err(ManifestError::Schema {
                path: path.to_path_buf(),
                found: raw.schema,
            });
        }
        let name = PackageName::parse(&raw.name).map_err(|source| ManifestError::Name {
            path: path.to_path_buf(),
            field: "name".to_owned(),
            source,
        })?;
        let role = match (raw.battery, raw.plugin) {
            (Some(_), Some(_)) => {
                return Err(ManifestError::BothRoles {
                    path: path.to_path_buf(),
                });
            }
            (None, None) => {
                return Err(ManifestError::NoRole {
                    path: path.to_path_buf(),
                });
            }
            (Some(battery), None) => Role::Battery(battery.validate(&name, path)?),
            (None, Some(plugin)) => Role::Plugin(plugin.validate(path)?),
        };
        Ok(Self {
            name,
            description: raw.description,
            role,
        })
    }

    #[cfg(test)]
    pub fn battery(&self) -> Option<&Battery> {
        match &self.role {
            Role::Battery(battery) => Some(battery),
            Role::Plugin(_) => None,
        }
    }

    #[cfg(test)]
    pub fn plugin(&self) -> Option<&Plugin> {
        match &self.role {
            Role::Plugin(plugin) => Some(plugin),
            Role::Battery(_) => None,
        }
    }
}

// ---------------------------------------------------------------------------
// The wire shape, and the one place it becomes a `Package`
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPackage {
    schema: u32,
    name: String,
    description: String,
    battery: Option<RawBattery>,
    plugin: Option<RawPlugin>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBattery {
    policy: String,
    hosts: Vec<String>,
    #[serde(default)]
    namespaces: Vec<String>,
    #[serde(default)]
    helpers: Vec<String>,
}

impl RawBattery {
    fn validate(self, name: &PackageName, path: &Path) -> Result<Battery, ManifestError> {
        let policy = relative(&self.policy, "battery.policy", path)?;
        let mut hosts = Vec::new();
        for host in self.hosts {
            hosts.push(Host::parse(&host).ok_or(ManifestError::Host {
                path: path.to_path_buf(),
                host,
            })?);
        }
        let mut namespaces = Vec::new();
        for namespace in &self.namespaces {
            let namespace = Namespace::parse(namespace).map_err(|source| ManifestError::Namespace {
                path: path.to_path_buf(),
                source,
            })?;
            // A marketplace refuses two batteries over one namespace by
            // comparing what each covers, and reads a repeat as a battery
            // conflicting with itself. It is a malformed list, so it is
            // refused where the list is read.
            if namespaces.contains(&namespace) {
                return Err(ManifestError::RepeatedNamespace {
                    path: path.to_path_buf(),
                    namespace: namespace.to_string(),
                });
            }
            namespaces.push(namespace);
        }
        if namespaces.is_empty() {
            namespaces
                .push(Namespace::parse(name.as_str()).expect("a package name is a namespace under both grammars"));
        }
        let mut helpers = Vec::new();
        for helper in &self.helpers {
            helpers.push(relative(helper, "battery.helpers", path)?);
        }
        Ok(Battery {
            policy,
            hosts,
            namespaces,
            helpers,
        })
    }
}

/// Every plugin field, so the host that does not own one can refuse it by
/// name instead of the reader guessing at a TOML error.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPlugin {
    host: String,
    protocol: u32,
    default_policy: String,
    #[serde(default)]
    batteries: Vec<String>,
    images: Option<BTreeMap<String, String>>,
}

impl RawPlugin {
    fn validate(self, path: &Path) -> Result<Plugin, ManifestError> {
        let host = Host::parse(&self.host).ok_or(ManifestError::Host {
            path: path.to_path_buf(),
            host: self.host.clone(),
        })?;
        if self.protocol != crate::PROTOCOL {
            return Err(ManifestError::Protocol {
                path: path.to_path_buf(),
                found: self.protocol,
            });
        }
        let default_policy = relative(&self.default_policy, "plugin.default_policy", path)?;
        let mut batteries = Vec::new();
        for battery in &self.batteries {
            let battery = PackageName::parse(battery).map_err(|source| ManifestError::Name {
                path: path.to_path_buf(),
                field: "plugin.batteries".to_owned(),
                source,
            })?;
            if batteries.contains(&battery) {
                return Err(ManifestError::RepeatedBattery {
                    path: path.to_path_buf(),
                    battery: battery.to_string(),
                });
            }
            batteries.push(battery);
        }
        let absent = |present: bool, field: &'static str| match present {
            true => Err(ManifestError::FieldNotForHost {
                path: path.to_path_buf(),
                host,
                field,
            }),
            false => Ok(()),
        };
        let missing = |field: &'static str| ManifestError::MissingField {
            path: path.to_path_buf(),
            host,
            field,
        };

        match host {
            Host::ClaudeCode => {
                absent(self.images.is_some(), "images")?;
                Ok(Plugin::ClaudeCode {
                    default_policy,
                    batteries,
                })
            }
            Host::Kagent => {
                let declared = self.images.ok_or_else(|| missing("images"))?;
                let mut images = BTreeMap::new();
                for (name, reference) in declared {
                    let name = ImageName::parse(&name).map_err(|source| ManifestError::Name {
                        path: path.to_path_buf(),
                        field: format!("plugin.images.{name}"),
                        source,
                    })?;
                    let reference = ImageReference::parse(&reference).ok_or(ManifestError::ImageReference {
                        path: path.to_path_buf(),
                        name: name.to_string(),
                    })?;
                    images.insert(name, reference);
                }
                Ok(Plugin::Kagent {
                    default_policy,
                    batteries,
                    images,
                })
            }
        }
    }
}

fn relative(text: &str, field: &str, path: &Path) -> Result<RelativePath, ManifestError> {
    RelativePath::parse(text).map_err(|source| ManifestError::Path {
        path: path.to_path_buf(),
        field: field.to_owned(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BATTERY: &str = "schema = 1\nname = \"github\"\ndescription = \"GitHub MCP server\"\n\n\
         [battery]\npolicy = \"appa.toml\"\nhosts = [\"claude-code\"]\nhelpers = [\"audience-source.py\"]\n";

    const CLAUDE_CODE: &str = "schema = 1\nname = \"claude-code\"\ndescription = \"Claude Code plugin\"\n\n\
         [plugin]\nhost = \"claude-code\"\nprotocol = 1\ndefault_policy = \"default.appa.toml\"\n";

    const KAGENT: &str = "schema = 1\nname = \"kagent\"\ndescription = \"kagent plugin\"\n\n\
         [plugin]\nhost = \"kagent\"\nprotocol = 1\ndefault_policy = \"default.appa.toml\"\n\
         images = { adk = \"ghcr.io/x/adk@sha256:aa\", adk-go = \"ghcr.io/x/adk-go@sha256:bb\" }\n";

    fn manifest(body: &str) -> Result<Package, ManifestError> {
        Package::parse(body, Path::new("batteries/github/appa-package.toml"))
    }

    #[test]
    fn a_battery_parses_its_policy_hosts_and_helpers() {
        let package = manifest(BATTERY).unwrap();

        assert_eq!(package.name.as_str(), "github");
        assert_eq!(
            package.battery().unwrap(),
            &Battery {
                policy: RelativePath::parse("appa.toml").unwrap(),
                hosts: vec![Host::ClaudeCode],
                namespaces: vec![Namespace::parse("github").unwrap()],
                helpers: vec![RelativePath::parse("audience-source.py").unwrap()],
            }
        );
    }

    /// A battery covers the namespace its own name spells until it says
    /// otherwise. It has to be able to say otherwise: the namespace is the tool
    /// key the harness reports, and a connector reports keys no package name
    /// can spell.
    #[test]
    fn a_battery_covers_the_namespaces_it_declares_and_its_own_name_by_default() {
        let declared = manifest(&BATTERY.replace(
            "helpers = [\"audience-source.py\"]",
            "namespaces = [\"claude_ai_Slack\", \"claude_ai_Grain\"]\nhelpers = [\"audience-source.py\"]",
        ))
        .unwrap();
        assert_eq!(
            declared.battery().unwrap().namespaces,
            vec![
                Namespace::parse("claude_ai_Slack").unwrap(),
                Namespace::parse("claude_ai_Grain").unwrap()
            ]
        );

        let refused = manifest(&BATTERY.replace(
            "helpers = [\"audience-source.py\"]",
            "namespaces = [\"mcp__github\"]\nhelpers = [\"audience-source.py\"]",
        ));
        assert!(matches!(refused, Err(ManifestError::Namespace { .. })));
    }

    /// A marketplace refuses two batteries over one namespace by comparing what
    /// each covers, so a repeat inside one list reads as a battery conflicting
    /// with itself. The list is malformed, and is refused where it is read.
    #[test]
    fn a_battery_may_not_declare_one_namespace_twice() {
        let refused = manifest(&BATTERY.replace(
            "helpers = [\"audience-source.py\"]",
            "namespaces = [\"github\", \"github\"]\nhelpers = [\"audience-source.py\"]",
        ));

        assert!(matches!(refused, Err(ManifestError::RepeatedNamespace { .. })));
    }

    #[test]
    fn a_claude_code_adapter_parses_its_plugin() {
        let package = manifest(CLAUDE_CODE).unwrap();

        assert_eq!(
            package.plugin().unwrap(),
            &Plugin::ClaudeCode {
                default_policy: RelativePath::parse("default.appa.toml").unwrap(),
                batteries: Vec::new(),
            }
        );
    }

    #[test]
    fn a_kagent_adapter_parses_its_images() {
        let package = manifest(KAGENT).unwrap();

        let Some(Plugin::Kagent { images, .. }) = package.plugin() else {
            panic!("expected a kagent plugin");
        };
        assert_eq!(
            images.keys().map(ImageName::as_str).collect::<Vec<_>>(),
            vec!["adk", "adk-go"]
        );
        assert_eq!(
            images[&ImageName::parse("adk").unwrap()].as_str(),
            "ghcr.io/x/adk@sha256:aa"
        );
    }

    #[test]
    fn a_manifest_declares_exactly_one_role() {
        let both = format!(
            "{BATTERY}\n[plugin]\nhost = \"kagent\"\nprotocol = 1\ndefault_policy = \"d.toml\"\nimages = {{ adk = \"x\" }}\n"
        );
        assert!(matches!(manifest(&both), Err(ManifestError::BothRoles { .. })));

        let neither = "schema = 1\nname = \"github\"\ndescription = \"nothing\"\n";
        assert!(matches!(manifest(neither), Err(ManifestError::NoRole { .. })));
    }

    #[test]
    fn another_schema_is_refused() {
        assert!(matches!(
            manifest(&BATTERY.replace("schema = 1", "schema = 7")),
            Err(ManifestError::Schema { found: 7, .. })
        ));
    }

    #[test]
    fn another_protocol_is_refused() {
        assert!(matches!(
            manifest(&CLAUDE_CODE.replace("protocol = 1", "protocol = 2")),
            Err(ManifestError::Protocol { found: 2, .. })
        ));
    }

    #[test]
    fn a_host_outside_the_closed_set_is_refused() {
        assert!(matches!(
            manifest(&CLAUDE_CODE.replace("host = \"claude-code\"", "host = \"codex\"")),
            Err(ManifestError::Host { .. })
        ));
        assert!(matches!(
            manifest(&BATTERY.replace("[\"claude-code\"]", "[\"codex\"]")),
            Err(ManifestError::Host { .. })
        ));
    }

    #[test]
    fn a_field_of_the_other_host_is_refused() {
        let claude_code_with_images = format!("{CLAUDE_CODE}images = {{ adk = \"ghcr.io/x/adk@sha256:aa\" }}\n");
        assert!(matches!(
            manifest(&claude_code_with_images),
            Err(ManifestError::FieldNotForHost {
                host: Host::ClaudeCode,
                field: "images",
                ..
            })
        ));
    }

    #[test]
    fn a_host_field_that_is_absent_is_refused() {
        assert!(matches!(
            manifest(
                &KAGENT
                    .lines()
                    .filter(|line| !line.starts_with("images"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
            Err(ManifestError::MissingField { field: "images", .. })
        ));
    }

    #[test]
    fn an_escaping_declared_path_is_refused() {
        assert!(matches!(
            manifest(&BATTERY.replace("\"appa.toml\"", "\"../appa.toml\"")),
            Err(ManifestError::Path { .. })
        ));
        assert!(matches!(
            manifest(&BATTERY.replace("\"audience-source.py\"", "\"/etc/passwd\"")),
            Err(ManifestError::Path { .. })
        ));
        assert!(matches!(
            manifest(&CLAUDE_CODE.replace("\"default.appa.toml\"", "\"../default.appa.toml\"")),
            Err(ManifestError::Path { .. })
        ));
    }

    #[test]
    fn an_empty_image_reference_is_refused() {
        assert!(matches!(
            manifest(&KAGENT.replace("\"ghcr.io/x/adk@sha256:aa\"", "\"\"")),
            Err(ManifestError::ImageReference { .. })
        ));
    }

    #[test]
    fn a_stray_field_is_refused() {
        assert!(matches!(
            manifest(&format!("{BATTERY}owner = \"someone\"\n")),
            Err(ManifestError::Syntax { .. })
        ));
    }
}
