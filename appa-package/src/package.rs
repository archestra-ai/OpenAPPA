//! A package manifest: `appa-package.toml` beside the package's own files.
//!
//! A package is either a battery or a plugin. A plugin carries the fields
//! needed to install APPA support for its host.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

use crate::manifest::{ManifestError, SCHEMA};
use crate::names::{ExecutableName, Host, NameError, Namespace, PackageName, RelativePath, lower_kebab};

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

/// The deployment shape a battery add-on is written for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BatteryTarget {
    SingleUser,
    MultiUser,
}

impl BatteryTarget {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "single-user" => Some(Self::SingleUser),
            "multi-user" => Some(Self::MultiUser),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::SingleUser => "single-user",
            Self::MultiUser => "multi-user",
        }
    }

    /// The manifest field that names this target's add-on policy.
    pub(crate) fn policy_field(self) -> &'static str {
        match self {
            Self::SingleUser => "battery.targets.single-user.policy",
            Self::MultiUser => "battery.targets.multi-user.policy",
        }
    }
}

impl fmt::Display for BatteryTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A policy composed over a battery's base for one target. Its audiences and
/// credentials are read from its own policy by `validate_package`, as the
/// base's are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatteryAddon {
    pub policy: RelativePath,
    pub audiences: Vec<String>,
    pub credentials: Vec<String>,
}

/// A battery that declares targets has no add-on for the one requested.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("the battery declares no add-on for the `{0}` target")]
pub struct MissingTarget(pub BatteryTarget);

/// A battery: a base policy file with optional per-target add-ons, the hosts
/// it is written for, the namespaces its contracts may name, and the helper
/// programs its externals may run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Battery {
    pub base: RelativePath,
    pub targets: BTreeMap<BatteryTarget, BatteryAddon>,
    pub hosts: Vec<Host>,
    /// The tool namespaces this battery covers. A battery usually covers the
    /// one namespace its own name spells, which is why the field defaults to
    /// it. It cannot always: the namespace is the tool key the harness
    /// reports, and a connector reports keys such as `claude_ai_Slack` that no
    /// package name can spell. A battery covering several endpoints of one
    /// product names each of them.
    pub namespaces: Vec<Namespace>,
    pub helpers: Vec<RelativePath>,
    /// The audience source providers the policy binds under
    /// `[externals.audience.<provider>]`, read from the policy by
    /// `validate_package` and empty from the manifest alone. A marketplace
    /// gives each provider one owner, as it does each namespace.
    pub audiences: Vec<String>,
    /// The environment variables the policy's bindings name as `token_env`,
    /// read from the policy by `validate_package` like `audiences`, so the
    /// manifest cannot disagree with the policy about what its helpers read.
    /// An install names each one to the person who then has to set it.
    pub credentials: Vec<String>,
    /// One line that tells the person what the battery's helpers ask the
    /// provider, and what APPA does better with the answer.
    pub benefit: Option<String>,
    /// The steps the person follows after an install, one line each, that the
    /// variable names cannot say: which token to make, where, with which
    /// scopes, or a login the helpers fall back to.
    pub setup: Vec<String>,
    /// The programs whose presence on `PATH` means the battery is relevant
    /// on this machine: a Claude Code install includes it when one is found.
    pub detect: Vec<ExecutableName>,
    pub readiness: Option<Readiness>,
}

impl Battery {
    /// The policies a deployment of `target` includes, add-on first. A battery
    /// without targets, or a deployment that names none, includes the base alone.
    pub fn compose_order(&self, target: Option<BatteryTarget>) -> Result<Vec<&RelativePath>, MissingTarget> {
        match target {
            Some(target) if !self.targets.is_empty() => self
                .targets
                .get(&target)
                .map(|addon| vec![&addon.policy, &self.base])
                .ok_or(MissingTarget(target)),
            _ => Ok(vec![&self.base]),
        }
    }
}

/// Optional, bounded, read-only battery authentication check.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Readiness {
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub required_executables: Vec<String>,
    #[serde(default)]
    pub cli_alternatives: Vec<CliAlternative>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct CliAlternative {
    pub executable: String,
    pub credential: String,
    pub login_hint: String,
}

/// A plugin package, with installation fields specific to its host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plugin {
    ClaudeCode {
        default_policy: RelativePath,
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

    /// The batteries the plugin's host cannot be gated without: a first
    /// install includes each of them. Every other battery is the person's
    /// choice.
    pub fn batteries(&self) -> &[PackageName] {
        match self {
            Self::ClaudeCode { batteries, .. } | Self::Kagent { batteries, .. } => batteries,
        }
    }
}

/// What a package is. A manifest declares exactly one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    Battery(Box<Battery>),
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
            (Some(battery), None) => Role::Battery(Box::new(battery.validate(&name, path)?)),
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
    policy: Option<String>,
    base: Option<String>,
    #[serde(default)]
    targets: BTreeMap<String, RawAddon>,
    hosts: Vec<String>,
    #[serde(default)]
    namespaces: Vec<String>,
    #[serde(default)]
    helpers: Vec<String>,
    benefit: Option<String>,
    #[serde(default)]
    setup: Vec<String>,
    readiness: Option<Readiness>,
    #[serde(default)]
    detect: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAddon {
    policy: String,
}

impl RawBattery {
    fn validate(self, name: &PackageName, path: &Path) -> Result<Battery, ManifestError> {
        let base = match (&self.policy, &self.base) {
            (Some(policy), None) => relative(policy, "battery.policy", path)?,
            (None, Some(base)) => relative(base, "battery.base", path)?,
            _ => {
                return Err(ManifestError::BatteryBase {
                    path: path.to_path_buf(),
                });
            }
        };
        let mut targets = BTreeMap::new();
        for (target, addon) in self.targets {
            let parsed = BatteryTarget::parse(&target).ok_or_else(|| ManifestError::Target {
                path: path.to_path_buf(),
                target,
            })?;
            let addon = BatteryAddon {
                policy: relative(&addon.policy, parsed.policy_field(), path)?,
                audiences: Vec::new(),
                credentials: Vec::new(),
            };
            targets.insert(parsed, addon);
        }
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
        // Each is a line the install and the UI print verbatim: a blank or
        // multi-line one is a manifest mistake, refused where it is read
        // rather than tidied.
        let benefit = match self.benefit.as_deref().map(one_line) {
            None => None,
            Some(Some(line)) => Some(line),
            Some(None) => {
                return Err(ManifestError::Benefit {
                    path: path.to_path_buf(),
                });
            }
        };
        let mut setup = Vec::new();
        for (index, step) in self.setup.iter().enumerate() {
            setup.push(one_line(step).ok_or_else(|| ManifestError::Setup {
                path: path.to_path_buf(),
                step: index + 1,
            })?);
        }
        let mut detect = Vec::new();
        for program in self.detect {
            let parsed = ExecutableName::parse(&program).ok_or_else(|| ManifestError::Detect {
                path: path.to_path_buf(),
                name: program.clone(),
            })?;
            if detect.contains(&parsed) {
                return Err(ManifestError::RepeatedDetect {
                    path: path.to_path_buf(),
                    name: program,
                });
            }
            detect.push(parsed);
        }
        Ok(Battery {
            base,
            targets,
            hosts,
            namespaces,
            helpers,
            audiences: Vec::new(),
            credentials: Vec::new(),
            benefit,
            setup,
            readiness: self.readiness,
            detect,
        })
    }
}

/// `text` trimmed, when it is one non-empty line.
fn one_line(text: &str) -> Option<String> {
    let line = text.trim();
    (!line.is_empty() && !line.contains(['\n', '\r'])).then(|| line.to_owned())
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
        let mut batteries: Vec<PackageName> = Vec::new();
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
            Host::Embedded => unreachable!("Host::parse names only served hosts"),
            // These integrations are source-distributed, not marketplace installers.
            Host::Amp | Host::OpenCode => Err(ManifestError::Host {
                path: path.to_path_buf(),
                host: self.host,
            }),
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
                base: RelativePath::parse("appa.toml").unwrap(),
                targets: BTreeMap::new(),
                hosts: vec![Host::ClaudeCode],
                namespaces: vec![Namespace::parse("github").unwrap()],
                audiences: vec![],
                credentials: vec![],
                benefit: None,
                setup: vec![],
                readiness: None,
                detect: vec![],
                helpers: vec![RelativePath::parse("audience-source.py").unwrap()],
            }
        );
    }

    const TARGETED: &str = "schema = 1\nname = \"github\"\ndescription = \"GitHub MCP server\"\n\n\
         [battery]\nbase = \"appa.toml\"\nhosts = [\"claude-code\"]\n\n\
         [battery.targets.multi-user]\npolicy = \"multi-user.toml\"\n";

    #[test]
    fn a_battery_parses_its_base_and_target_add_ons() {
        let package = manifest(TARGETED).unwrap();
        let battery = package.battery().unwrap();

        assert_eq!(battery.base.as_str(), "appa.toml");
        assert_eq!(
            battery.targets.keys().copied().collect::<Vec<_>>(),
            vec![BatteryTarget::MultiUser]
        );
        assert_eq!(
            battery.targets[&BatteryTarget::MultiUser].policy.as_str(),
            "multi-user.toml"
        );
    }

    #[test]
    fn a_legacy_policy_reads_as_the_base_with_no_targets() {
        let battery = manifest(BATTERY).unwrap().battery().unwrap().clone();

        assert_eq!(battery.base.as_str(), "appa.toml");
        assert!(battery.targets.is_empty());
    }

    #[test]
    fn a_battery_declares_exactly_one_base_and_only_known_targets() {
        assert!(matches!(
            manifest(&TARGETED.replace("base = \"appa.toml\"", "base = \"appa.toml\"\npolicy = \"appa.toml\"")),
            Err(ManifestError::BatteryBase { .. })
        ));
        assert!(matches!(
            manifest(&TARGETED.replace("base = \"appa.toml\"\n", "")),
            Err(ManifestError::BatteryBase { .. })
        ));
        assert!(matches!(
            manifest(&TARGETED.replace("targets.multi-user", "targets.team")),
            Err(ManifestError::Target { target, .. }) if target == "team"
        ));
        assert!(matches!(
            manifest(&TARGETED.replace("\"multi-user.toml\"", "\"../multi-user.toml\"")),
            Err(ManifestError::Path { .. })
        ));
    }

    #[test]
    fn compose_order_puts_the_target_add_on_before_the_base() {
        let targeted = manifest(TARGETED).unwrap();
        let targeted = targeted.battery().unwrap();
        fn names(order: Vec<&RelativePath>) -> Vec<&str> {
            order.into_iter().map(RelativePath::as_str).collect()
        }

        assert_eq!(
            names(targeted.compose_order(Some(BatteryTarget::MultiUser)).unwrap()),
            vec!["multi-user.toml", "appa.toml"]
        );
        assert_eq!(
            targeted.compose_order(Some(BatteryTarget::SingleUser)),
            Err(MissingTarget(BatteryTarget::SingleUser))
        );
        assert_eq!(names(targeted.compose_order(None).unwrap()), vec!["appa.toml"]);

        let plain = manifest(BATTERY).unwrap();
        assert_eq!(
            names(
                plain
                    .battery()
                    .unwrap()
                    .compose_order(Some(BatteryTarget::SingleUser))
                    .unwrap()
            ),
            vec!["appa.toml"]
        );
    }

    #[test]
    fn a_battery_target_round_trips_its_kebab_name() {
        for target in [BatteryTarget::SingleUser, BatteryTarget::MultiUser] {
            assert_eq!(BatteryTarget::parse(&target.to_string()), Some(target));
        }
        assert_eq!(BatteryTarget::parse("single_user"), None);
    }

    /// `detect` names programs a shell finds on `PATH`: bare names, each once.
    #[test]
    fn a_battery_detects_bare_program_names_each_once() {
        let with = |list: &str| manifest(&BATTERY.replace("helpers", &format!("detect = {list}\nhelpers")));
        let detected = with(r#"["gh", "git-lfs"]"#).unwrap();
        assert_eq!(
            detected.battery().unwrap().detect,
            vec![
                ExecutableName::parse("gh").unwrap(),
                ExecutableName::parse("git-lfs").unwrap()
            ]
        );
        for refused in [
            r#"[""]"#,
            r#"["/usr/bin/gh"]"#,
            r#"["bin\\gh"]"#,
            r#"["g h"]"#,
            r#"["c:gh"]"#,
            r#"[".."]"#,
        ] {
            assert!(
                matches!(with(refused), Err(ManifestError::Detect { .. })),
                "{refused} is refused"
            );
        }
        assert!(matches!(
            with(r#"["gh", "gh"]"#),
            Err(ManifestError::RepeatedDetect { .. })
        ));
    }

    /// `benefit` is one line the install and the UI print verbatim, so a
    /// blank or multi-line one is refused where it is read.
    #[test]
    fn a_battery_benefit_is_one_non_empty_line() {
        let with = |line: &str| manifest(&BATTERY.replace("helpers", &format!("benefit = {line:?}\nhelpers")));
        assert_eq!(
            with(" APPA asks GitHub who can see a repository. ")
                .unwrap()
                .battery()
                .unwrap()
                .benefit
                .as_deref(),
            Some("APPA asks GitHub who can see a repository.")
        );
        assert!(matches!(with("  "), Err(ManifestError::Benefit { .. })));
        assert!(matches!(with("one\ntwo"), Err(ManifestError::Benefit { .. })));
        assert!(matches!(with("one\rtwo"), Err(ManifestError::Benefit { .. })));
    }

    /// Each `setup` step is one line the install numbers and prints verbatim,
    /// so a blank or multi-line step is refused where it is read, by number.
    #[test]
    fn each_battery_setup_step_is_one_non_empty_line() {
        let with = |steps: &[&str]| manifest(&BATTERY.replace("helpers", &format!("setup = {steps:?}\nhelpers")));
        assert_eq!(
            with(&["Run `gh auth login`.", " Or make a token. "])
                .unwrap()
                .battery()
                .unwrap()
                .setup,
            vec!["Run `gh auth login`.".to_owned(), "Or make a token.".to_owned()]
        );
        assert!(with(&[]).unwrap().battery().unwrap().setup.is_empty());
        assert!(matches!(with(&["ok", "  "]), Err(ManifestError::Setup { step: 2, .. })));
        assert!(matches!(with(&["one\ntwo"]), Err(ManifestError::Setup { step: 1, .. })));
        assert!(matches!(with(&["one\rtwo"]), Err(ManifestError::Setup { step: 1, .. })));
        assert!(matches!(
            manifest(&BATTERY.replace("helpers", "setup = \"one line\"\nhelpers")),
            Err(ManifestError::Syntax { .. })
        ));
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
                batteries: vec![],
            }
        );
    }

    /// A plugin names the batteries its host cannot be gated without; a
    /// plugin that names none has no required battery.
    #[test]
    fn a_plugin_parses_the_batteries_it_requires() {
        let declared = manifest(&format!("{CLAUDE_CODE}batteries = [\"claude-code\", \"github\"]\n")).unwrap();
        assert_eq!(
            declared.plugin().unwrap().batteries(),
            &[
                PackageName::parse("claude-code").unwrap(),
                PackageName::parse("github").unwrap()
            ]
        );
        assert!(manifest(KAGENT).unwrap().plugin().unwrap().batteries().is_empty());

        let refused = manifest(&format!("{CLAUDE_CODE}batteries = [\"Claude Code\"]\n"));
        assert!(matches!(refused, Err(ManifestError::Name { .. })));

        let repeated = manifest(&format!("{CLAUDE_CODE}batteries = [\"github\", \"github\"]\n"));
        assert!(matches!(repeated, Err(ManifestError::RepeatedBattery { .. })));
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
    fn source_plugins_are_battery_hosts_but_have_no_marketplace_installer() {
        for host in ["amp", "opencode"] {
            assert!(manifest(&BATTERY.replace("[\"claude-code\"]", &format!("[\"{host}\"]"))).is_ok());
            assert!(matches!(
                manifest(&CLAUDE_CODE.replace("host = \"claude-code\"", &format!("host = \"{host}\""))),
                Err(ManifestError::Host { .. })
            ));
        }
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
