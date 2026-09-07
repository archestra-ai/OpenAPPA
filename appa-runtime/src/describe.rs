//! A small, read-only description of the facts a configuration actor can rely on.
//!
//! This deliberately does not pretend that a standalone process can see Claude's
//! session tool catalogue or connector accounts. Those are session facts and must
//! be merged by the configuring actor.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};

use crate::config::{Config, Externals, Implementation, Section};

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
    authorities: Vec<AuthorityDescription>,
    audience: AudienceSide,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AuthorityDescription {
    name: String,
    implementation: AuthorityImplementation,
    trust_below: Option<String>,
    audience_missing: Option<String>,
    effects_containing: Vec<String>,
    attention: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum AuthorityImplementation {
    Builtin(String),
    Url,
    Command,
    Unbound,
    Invalid,
}

impl AuthorityImplementation {
    fn as_text(&self) -> String {
        match self {
            AuthorityImplementation::Builtin(name) => format!("builtin {name}"),
            AuthorityImplementation::Url => "url".to_string(),
            AuthorityImplementation::Command => "command".to_string(),
            AuthorityImplementation::Unbound => "unbound".to_string(),
            AuthorityImplementation::Invalid => "invalid binding".to_string(),
        }
    }
}

/// The audience side of the policy, as far as this command can describe it offline.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum AudienceSide {
    /// No policy to compile: the configuration is missing, unreadable, or not TOML.
    #[default]
    Absent,
    /// The policy does not compile; the error names why.
    Uncompiled(String),
    Declared(AudienceDescription),
}

/// What the configuration itself declares about audiences. Live provider group catalogues
/// are session facts this command does not reach.
#[derive(Clone, Debug, PartialEq, Eq)]
struct AudienceDescription {
    /// One entry per registered source: the provider, its advertised selector templates,
    /// and whether `[externals.audience.<provider>]` binds it.
    sources: Vec<SourceDescription>,
    self_from: Vec<String>,
    internal_from: Vec<String>,
    /// One entry per `[[audience.group]]`: `@name`, its `within` target, and its selectors.
    groups: Vec<GroupDescription>,
    identity: IdentityDescription,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SourceDescription {
    provider: String,
    templates: Vec<String>,
    binding_configured: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GroupDescription {
    name: String,
    within: Option<&'static str>,
    from: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum IdentityDescription {
    VerifiedEmail,
    Custom { name: String, binding_configured: bool },
}

/// Where the `[externals]` bindings are read from: the loaded configuration, or the raw TOML
/// of one that does not load.
#[derive(Clone, Copy)]
enum Bindings<'a> {
    Loaded(&'a Externals),
    Raw(&'a toml::Value),
}

impl Bindings<'_> {
    fn bound(self, section: Section, name: &str) -> bool {
        match self {
            Bindings::Loaded(externals) => match section {
                Section::Authorities => externals.authorities.contains_key(name),
                Section::Sanitizers => externals.sanitizers.contains_key(name),
                Section::Annotators => externals.annotators.contains_key(name),
                Section::Audience => externals.audience.contains_key(name),
                Section::Identity => externals.identity.contains_key(name),
            },
            Bindings::Raw(root) => root
                .get("externals")
                .and_then(|externals| externals.get(section.name()))
                .and_then(|table| table.get(name))
                .is_some(),
        }
    }

    fn authority_implementation(self, name: &str) -> AuthorityImplementation {
        match self {
            Bindings::Loaded(externals) => match externals.authorities.get(name) {
                Some(Implementation::Resolver(_)) => AuthorityImplementation::Url,
                Some(Implementation::Command(_)) => AuthorityImplementation::Command,
                Some(Implementation::Builtin(builtin)) => AuthorityImplementation::Builtin(builtin.clone()),
                None => AuthorityImplementation::Unbound,
            },
            Bindings::Raw(root) => {
                let Some(binding) = root
                    .get("externals")
                    .and_then(|externals| externals.get(Section::Authorities.name()))
                    .and_then(|table| table.get(name))
                else {
                    return AuthorityImplementation::Unbound;
                };
                let Some(binding) = binding.as_table() else {
                    return AuthorityImplementation::Invalid;
                };
                match (
                    binding.get("url"),
                    binding.get("command"),
                    binding.get("builtin").and_then(toml::Value::as_str),
                ) {
                    (Some(_), None, None) => AuthorityImplementation::Url,
                    (None, Some(_), None) => AuthorityImplementation::Command,
                    (None, None, Some(builtin)) => AuthorityImplementation::Builtin(builtin.to_string()),
                    _ => AuthorityImplementation::Invalid,
                }
            }
        }
    }
}

fn authority_descriptions(compiled: &appa_policy::Config, bindings: Bindings<'_>) -> Vec<AuthorityDescription> {
    let chain = compiled.registry().trust_chain();
    let mut authorities = compiled
        .registry()
        .authorities()
        .iter()
        .map(|authority| AuthorityDescription {
            name: authority.name.as_str().to_string(),
            implementation: bindings.authority_implementation(authority.name.as_str()),
            trust_below: authority.mandate.trust_ceiling.map(|ceiling| {
                chain
                    .name_of(ceiling)
                    .expect("a compiled authority names a rank in its trust chain")
                    .to_string()
            }),
            audience_missing: authority
                .mandate
                .reader_ceiling
                .as_ref()
                .map(|audience| match audience {
                    appa_engine::label::DeclaredAudience::Public => "public".to_string(),
                    appa_engine::label::DeclaredAudience::Union(clause) => {
                        format!("[{}]", crate::consult::clause_entries(clause).join(", "))
                    }
                }),
            effects_containing: authority
                .mandate
                .waivers
                .iter()
                .map(|effect| effect.as_str().to_string())
                .collect(),
            attention: authority
                .mandate
                .attends
                .iter()
                .map(|mark| mark.as_str().to_string())
                .collect(),
        })
        .collect::<Vec<_>>();
    authorities.sort_by(|left, right| left.name.cmp(&right.name));
    authorities
}

/// The declared audience configuration, with each source's and the identity's binding status.
fn audience_description(compiled: &appa_policy::Config, bindings: Bindings<'_>) -> AudienceDescription {
    let audience = compiled.registry().audience();
    let spelled = |spec: &appa_engine::audience::SelectorSpec| spec.to_string();
    AudienceDescription {
        sources: audience
            .providers()
            .iter()
            .map(|provider| SourceDescription {
                provider: provider.clone(),
                templates: audience
                    .templates(provider)
                    .into_iter()
                    .flatten()
                    .map(|template| template.as_str().to_string())
                    .collect(),
                binding_configured: bindings.bound(Section::Audience, provider),
            })
            .collect(),
        self_from: audience
            .chain_from(appa_engine::label::ChainAudience::Self_)
            .iter()
            .map(spelled)
            .collect(),
        internal_from: audience
            .chain_from(appa_engine::label::ChainAudience::Internal)
            .iter()
            .map(spelled)
            .collect(),
        groups: audience
            .groups()
            .map(|group| GroupDescription {
                name: format!("@{}", group.name.as_str()),
                within: group.within.map(|target| target.as_str()),
                from: group.from.iter().map(spelled).collect(),
            })
            .collect(),
        identity: match audience.identity() {
            appa_engine::audience::IdentityImplementation::VerifiedEmail => IdentityDescription::VerifiedEmail,
            appa_engine::audience::IdentityImplementation::Custom(name) => IdentityDescription::Custom {
                name: name.as_str().to_string(),
                binding_configured: bindings.bound(Section::Identity, name.as_str()),
            },
        },
    }
}

fn inspect(path: &Path, battery_dirs: &[PathBuf]) -> (ConfigDescription, PolicyDescription) {
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
                    describe_policy_value(root_policy, Bindings::Raw(&root), &mut policy);
                }

                if let Ok(loaded) = Config::load_from(path, battery_dirs) {
                    config.state = ConfigState::Loadable;
                    config.diagnostic = None;
                    describe_policy_value(
                        loaded.policy_file().value(),
                        Bindings::Loaded(&loaded.externals),
                        &mut policy,
                    );
                }
            }
        },
    }

    (config, policy)
}

fn describe_policy_value(policy_value: &toml::Value, bindings: Bindings<'_>, out: &mut PolicyDescription) {
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

    let compiled = toml::to_string(policy_value)
        .map_err(|error| error.to_string())
        .and_then(|source| appa_policy::Config::from_toml_str(&source).map_err(|error| error.to_string()));
    out.audience = match compiled {
        Ok(compiled) => {
            out.authorities = authority_descriptions(&compiled, bindings);
            AudienceSide::Declared(audience_description(&compiled, bindings))
        }
        Err(error) => AudienceSide::Uncompiled(error),
    };
}

fn battery_name(path: &Path) -> Option<String> {
    crate::batteries::name_from_include(path)
}

pub fn render(path: &Path, battery_dirs: &[PathBuf], adapter: &'static str) -> String {
    let (config, policy) = inspect(path, battery_dirs);
    let mut output = String::new();
    let _ = writeln!(output, "OpenAPPA world");
    let _ = writeln!(output, "Adapter: {adapter}");
    let _ = writeln!(output, "Config: {} ({})", config.path.display(), config.state.as_str());
    if let Some(diagnostic) = config.diagnostic {
        let _ = writeln!(output, "  {diagnostic}");
    }
    let _ = writeln!(output, "Batteries: {}", list_or_none(&config.batteries));
    let _ = writeln!(output, "Policy tools: {}", list_or_none(&policy.tools));
    if policy.authorities.is_empty() {
        let _ = writeln!(output, "Authorities: none");
    } else {
        let _ = writeln!(output, "Authorities:");
        for authority in &policy.authorities {
            let mut permits = Vec::new();
            if let Some(trust_below) = &authority.trust_below {
                permits.push(format!("trust_below={trust_below}"));
            }
            if let Some(audience_missing) = &authority.audience_missing {
                permits.push(format!("audience_missing={audience_missing}"));
            }
            if !authority.effects_containing.is_empty() {
                permits.push(format!(
                    "effects_containing=[{}]",
                    authority.effects_containing.join(", ")
                ));
            }
            if !authority.attention.is_empty() {
                permits.push(format!("attention=[{}]", authority.attention.join(", ")));
            }
            let _ = writeln!(
                output,
                "  {}: {}; permits {}",
                authority.name,
                authority.implementation.as_text(),
                permits.join(", ")
            );
        }
    }
    let _ = writeln!(output, "Audience chain: self ⊆ internal ⊆ public (built-in)");
    match &policy.audience {
        AudienceSide::Declared(audience) => {
            if audience.sources.is_empty() {
                let _ = writeln!(output, "Audience sources: none");
            } else {
                let _ = writeln!(output, "Audience sources:");
                for source in &audience.sources {
                    let binding = if source.binding_configured {
                        "binding configured"
                    } else {
                        "binding missing"
                    };
                    let _ = writeln!(
                        output,
                        "  {}: {} ({binding})",
                        source.provider,
                        source.templates.join(", ")
                    );
                }
            }
            let _ = writeln!(output, "  self from: {}", list_or_none(&audience.self_from));
            let _ = writeln!(output, "  internal from: {}", list_or_none(&audience.internal_from));
            if audience.groups.is_empty() {
                let _ = writeln!(output, "Named audiences: none");
            } else {
                let _ = writeln!(output, "Named audiences:");
                for group in &audience.groups {
                    let within = group.within.map(|target| format!(" ⊆ {target}")).unwrap_or_default();
                    let _ = writeln!(output, "  {}{} from {}", group.name, within, group.from.join(", "));
                }
            }
            match &audience.identity {
                IdentityDescription::VerifiedEmail => {
                    let _ = writeln!(output, "Identity: verified-email (built-in)");
                }
                IdentityDescription::Custom {
                    name,
                    binding_configured,
                } => {
                    let binding = if *binding_configured {
                        "binding configured"
                    } else {
                        "binding missing"
                    };
                    let _ = writeln!(output, "Identity: {name} ({binding})");
                }
            }
        }
        AudienceSide::Uncompiled(error) => {
            let _ = writeln!(
                output,
                "Audience configuration: unavailable (policy does not compile: {error})"
            );
        }
        AudienceSide::Absent => {
            let _ = writeln!(output, "Audience configuration: unavailable (no policy)");
        }
    }
    let _ = writeln!(
        output,
        "Session integrations/tools/accounts: unavailable to this command"
    );
    output
}

pub fn is_loadable(path: &Path, battery_dirs: &[PathBuf]) -> bool {
    matches!(inspect(path, battery_dirs).0.state, ConfigState::Loadable)
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

        let (config, policy) = inspect(&path, &[]);

        assert_eq!(config.state, ConfigState::Missing);
        assert!(!is_loadable(&path, &[]));
        assert!(!path.exists());
        assert!(policy.tools.is_empty());
    }

    #[test]
    fn loadable_config_reports_batteries_tools_authorities_sources_and_identity() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let batteries = directory.path().join("bundled-batteries");
        let battery = batteries.join("mail");
        std::fs::create_dir_all(&battery).expect("battery directory");
        std::fs::write(
            battery.join("appa.toml"),
            "[policy]\nversion = 2\n[[policy.tool]]\nname = \"mail_read\"\ndelta = { audience = [\"self\"] }\n",
        )
        .expect("battery config");
        let root = directory.path().join("appa.toml");
        std::fs::write(
            &root,
            "include = [\"batteries/mail/appa.toml\"]\n[policy]\nversion = 2\n[policy.audience.self]\nfrom = [\"slack:viewer\"]\n[[policy.audience.group]]\nname = \"finance\"\nwithin = \"internal\"\nfrom = [\"slack:user-group/finance\"]\n[policy.identity]\nimplementation = \"corp-identity\"\n[[policy.authority]]\nname = \"operator\"\n[policy.authority.permits]\ntrust_below = \"trusted\"\naudience_missing = [\"public\"]\neffects_containing = [\"mail.sent\"]\nattention = [\"hitl\"]\n[externals]\ntimeout_ms = 1000\nmax_body_bytes = 65536\n[externals.authorities.operator]\nbuiltin = \"hitl\"\n[externals.audience.slack]\ncommand = [\"true\"]\n[externals.identity.corp-identity]\ncommand = [\"true\"]\n",
        )
        .expect("root config");

        Config::load_from(&root, std::slice::from_ref(&batteries))
            .unwrap_or_else(|error| panic!("fixture must load: {error}"));

        let (config, policy) = inspect(&root, std::slice::from_ref(&batteries));

        assert_eq!(config.state, ConfigState::Loadable);
        assert!(is_loadable(&root, std::slice::from_ref(&batteries)));
        assert_eq!(config.batteries, ["mail"]);
        assert_eq!(policy.tools, ["mail_read"]);
        assert_eq!(
            policy.authorities,
            [AuthorityDescription {
                name: "operator".to_string(),
                implementation: AuthorityImplementation::Builtin("hitl".to_string()),
                trust_below: Some("trusted".to_string()),
                audience_missing: Some("public".to_string()),
                effects_containing: vec!["mail.sent".to_string()],
                attention: vec!["hitl".to_string()],
            }]
        );
        let AudienceSide::Declared(audience) = policy.audience else {
            panic!("a loadable policy describes its audience side: {:?}", policy.audience);
        };
        assert_eq!(
            audience.sources,
            [SourceDescription {
                provider: "slack".to_string(),
                templates: vec![
                    "viewer".to_string(),
                    "full-members".to_string(),
                    "user-group/<handle>".to_string()
                ],
                binding_configured: true,
            }]
        );
        assert_eq!(audience.self_from, ["slack:viewer"]);
        assert!(audience.internal_from.is_empty());
        assert_eq!(
            audience.groups,
            [GroupDescription {
                name: "@finance".to_string(),
                within: Some("internal"),
                from: vec!["slack:user-group/finance".to_string()],
            }]
        );
        assert_eq!(
            audience.identity,
            IdentityDescription::Custom {
                name: "corp-identity".to_string(),
                binding_configured: true,
            }
        );
        assert!(render(&root, &[batteries], "claude-code").contains(
            "operator: builtin hitl; permits trust_below=trusted, audience_missing=public, effects_containing=[mail.sent], attention=[hitl]"
        ));
    }

    #[test]
    fn malformed_config_does_not_echo_its_contents() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("appa.toml");
        let secret = "super-secret-token";
        std::fs::write(&path, format!("token = \\\"{secret}")).expect("malformed config");

        let (config, _) = inspect(&path, &[]);
        let output = render(&path, &[], "claude-code");

        assert_eq!(config.state, ConfigState::Unparsable);
        assert!(!output.contains(secret));
    }

    #[test]
    fn human_output_is_small_and_explicit_about_unknown_session_facts() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let output = render(&directory.path().join("appa.toml"), &[], "claude-code");

        assert!(output.contains("Config:"));
        assert!(output.contains("Batteries: none"));
        assert!(output.contains("Session integrations/tools/accounts: unavailable"));
    }
}
