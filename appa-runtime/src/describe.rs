//! A small, read-only description of the facts a configuration actor can rely on.
//!
//! This deliberately does not pretend that a standalone process can see Claude's
//! session tool catalogue or connector accounts. Those are session facts and must
//! be merged by the configuring actor.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};

use crate::config::{Config, ConfigError, Externals, Implementation, Section};

#[derive(Clone, Debug, PartialEq, Eq)]
struct ConfigDescription {
    path: PathBuf,
    state: ConfigState,
    diagnostic: Option<String>,
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
    /// whether `[externals.audience.<provider>]` binds it, and where its lookups go.
    sources: Vec<SourceDescription>,
    self_from: Vec<String>,
    internal_from: Vec<String>,
    /// One entry per `[audience.group.<name>]`: `@name`, its `within` target, and its selectors.
    groups: Vec<GroupDescription>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SourceDescription {
    provider: String,
    templates: Vec<String>,
    binding_configured: bool,
    /// The entry the provider's member lookups are sent to, when not the provider's own.
    lookup: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GroupDescription {
    name: String,
    within: Option<&'static str>,
    from: Vec<String>,
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
                Section::Context => externals.context.contains_key(name),
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
    let chain = compiled.engine().registry().trust_chain();
    let mut authorities = compiled
        .engine()
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
            attention: authority.mandate.attends.spellings(),
        })
        .collect::<Vec<_>>();
    authorities.sort_by(|left, right| left.name.cmp(&right.name));
    authorities
}

impl Bindings<'_> {
    /// The lookup routing the bindings declare, from the loaded configuration or the raw table.
    fn lookup_targets(self) -> BTreeMap<String, String> {
        match self {
            Bindings::Loaded(externals) => externals.lookup_targets(),
            Bindings::Raw(root) => crate::config::lookup_targets_of(root),
        }
    }

    /// The audience sources the bindings declare, from the loaded configuration or the raw table.
    fn source_registrations(self) -> Result<Vec<appa_engine::audience::SourceRegistration>, String> {
        match self {
            Bindings::Loaded(externals) => Ok(externals.source_registrations()),
            Bindings::Raw(root) => crate::config::source_registrations_of(root).map_err(|error| error.to_string()),
        }
    }
}

/// The declared audience configuration, with each source's binding status.
fn audience_description(compiled: &appa_policy::Config, bindings: Bindings<'_>) -> AudienceDescription {
    let audience = compiled.engine().registry().audience();
    let spelled = |spec: &appa_engine::audience::SelectorSpec| spec.to_string();
    AudienceDescription {
        sources: audience
            .providers()
            .iter()
            .map(|provider| SourceDescription {
                provider: provider.clone(),
                templates: crate::engine::selector_templates(audience, provider).unwrap_or_default(),
                binding_configured: bindings.bound(Section::Audience, provider),
                lookup: audience.lookup_target(provider).map(str::to_string),
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
    }
}

/// Where a TOML error is and what it says, without the source line the error's
/// own rendering quotes: a configuration file may hold a credential.
fn toml_diagnostic(error: &toml::de::Error, text: &str) -> String {
    let message = one_line(error.message());
    match error.span() {
        Some(span) => {
            let before = &text[..span.start.min(text.len())];
            let line = before.matches('\n').count() + 1;
            let column = before.rsplit('\n').next().map_or(0, |last| last.chars().count()) + 1;
            format!("line {line}, column {column}: {message}")
        }
        None => message,
    }
}

fn one_line(message: &str) -> String {
    message.lines().collect::<Vec<_>>().join(", ")
}

/// A load error as the description prints it. The two parse variants render
/// their TOML error's message only, for the reason `toml_diagnostic` gives;
/// every other variant names fields and paths, never file contents.
fn load_diagnostic(error: &ConfigError) -> String {
    match error {
        ConfigError::Unparsable { path, source } => {
            format!("cannot parse {path}: {}", one_line(source.message()))
        }
        ConfigError::UnparsablePolicy { source } => {
            format!("cannot parse the composed policy: {}", one_line(source.message()))
        }
        other => other.to_string(),
    }
}

fn inspect(path: &Path, battery_dirs: &[PathBuf]) -> (ConfigDescription, PolicyDescription, Option<Config>) {
    let mut config = ConfigDescription {
        path: path.to_path_buf(),
        state: ConfigState::Missing,
        diagnostic: Some("configuration file does not exist".to_owned()),
        batteries: Vec::new(),
    };
    let mut policy = PolicyDescription::default();
    let mut loaded_config = None;

    match std::fs::read_to_string(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            config.state = ConfigState::Unreadable;
            config.diagnostic = Some(format!("configuration file cannot be read: {error}"));
        }
        Ok(text) => match toml::from_str::<toml::Value>(&text) {
            Err(error) => {
                config.state = ConfigState::Unparsable;
                config.diagnostic = Some(format!(
                    "configuration is not valid TOML: {}",
                    toml_diagnostic(&error, &text)
                ));
            }
            Ok(root) => {
                config.state = ConfigState::Invalid;
                config.diagnostic = Some("configuration is incomplete or does not validate".to_owned());
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
                    .filter_map(|include| crate::batteries::name_from_include(Path::new(include)))
                    .collect();
                let mut seen_batteries = BTreeSet::new();
                config
                    .batteries
                    .retain(|battery| seen_batteries.insert(battery.clone()));

                if let Some(root_policy) = root.get("policy") {
                    let _ = describe_policy_value(root_policy, Bindings::Raw(&root), &mut policy);
                }

                match Config::load_from(path, battery_dirs) {
                    Ok(loaded) => {
                        config.state = ConfigState::Loadable;
                        config.diagnostic = None;
                        let _ = describe_policy_value(
                            loaded.policy_file().value(),
                            Bindings::Loaded(&loaded.externals),
                            &mut policy,
                        );
                        loaded_config = Some(loaded);
                    }
                    Err(error) => {
                        config.diagnostic = Some(format!("configuration does not load: {}", load_diagnostic(&error)));
                    }
                }
            }
        },
    }

    (config, policy, loaded_config)
}

fn describe_policy_value(
    policy_value: &toml::Value,
    bindings: Bindings<'_>,
    out: &mut PolicyDescription,
) -> Option<appa_policy::Config> {
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
        .and_then(|source| {
            let sources = bindings.source_registrations()?;
            appa_policy::Config::from_toml_str_routed(&source, bindings.lookup_targets(), sources)
                .map_err(|error| error.to_string())
        });
    match compiled {
        Ok(compiled) => {
            out.authorities = authority_descriptions(&compiled, bindings);
            out.audience = AudienceSide::Declared(audience_description(&compiled, bindings));
            Some(compiled)
        }
        Err(error) => {
            out.audience = AudienceSide::Uncompiled(error);
            None
        }
    }
}

pub struct Description {
    pub text: String,
    pub valid: bool,
}

/// `session_tools` are the tool names the configuring agent's own session sees, in the host's
/// spelling or as canonical ids. No standalone process can list them, so the agent hands them
/// over; without them the description says they are unavailable.
pub fn render(path: &Path, battery_dirs: &[PathBuf], adapter: &'static str, session_tools: &[String]) -> Description {
    let (mut config, mut policy, loaded) = inspect(path, battery_dirs);
    let mut served_policy = None;
    let served = match adapter {
        "amp" => Some(appa_adapter_amp::adapter()),
        "claude-code" => Some(appa_adapter_claude_code::adapter()),
        "kagent" => Some(appa_adapter_kagent::adapter()),
        _ => None,
    };
    if let Some(loaded) = &loaded
        && let Some(served) = served
    {
        let resolved = crate::tool_validation::resolve(
            loaded.policy_file().value(),
            served,
            &loaded.inventory,
            &loaded.server_aliases,
        );
        let authored_tools = policy.tools.clone();
        served_policy = describe_policy_value(&resolved.policy, Bindings::Loaded(&loaded.externals), &mut policy);
        policy.tools = authored_tools;
    }
    let validation = match (loaded, served) {
        (Some(loaded), Some(served)) => {
            crate::api::Runtime::validate_served(loaded, served).map_err(|error| error.to_string())
        }
        (_, None) => Err(format!("unsupported adapter {adapter:?}")),
        (None, _) => Err("configuration cannot be loaded; check the configuration diagnostics above".to_string()),
    };
    let valid = validation.is_ok();
    if !valid && config.state == ConfigState::Loadable {
        config.state = ConfigState::Invalid;
    }
    let mut output = String::new();
    let _ = writeln!(output, "OpenAPPA world");
    let _ = writeln!(output, "Adapter: {adapter}");
    let _ = writeln!(output, "Config: {} ({})", config.path.display(), config.state.as_str());
    if let Some(diagnostic) = &config.diagnostic {
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
                    let lookups = source
                        .lookup
                        .as_deref()
                        .map(|target| format!("; lookups via {target}"))
                        .unwrap_or_default();
                    let _ = writeln!(
                        output,
                        "  {}: {} ({binding}{lookups})",
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
    let session = served.map(|served| SessionCoverage::of(session_tools, served, served_policy.as_ref()));
    #[cfg(feature = "daemon")]
    if adapter == "claude-code" {
        let servers = std::env::current_dir()
            .map(|cwd| crate::installation::discover::servers(appa_package::Host::ClaudeCode, &cwd))
            .unwrap_or_default()
            .into_iter()
            .chain(session.iter().flat_map(|session| session.servers.iter().cloned()))
            .filter(|server| server.as_str() != crate::init::RUNTIME_SERVER)
            .collect::<BTreeSet<_>>();
        render_servers(&mut output, path, &servers);
    }
    match session.filter(|_| !session_tools.is_empty()) {
        Some(session) => session.render(&mut output),
        None => {
            let _ = writeln!(
                output,
                "Session tools: unavailable to this command; pass the names this session sees with --session-tools"
            );
        }
    }
    let _ = writeln!(output, "Connector accounts: unavailable to this command");
    match validation {
        Ok(report) => {
            let _ = writeln!(output, "Validation: {}", report.summary());
            // Without a host inventory every declared tool is unknown for the
            // same reason, so one line says so instead of one per tool.
            let mut unobserved = 0usize;
            for check in &report.tools {
                match &check.status {
                    crate::tool_validation::ToolStatus::Valid => {}
                    crate::tool_validation::ToolStatus::Invalid { reason } => {
                        let _ = writeln!(output, "  invalid {}: {reason}", check.tool);
                    }
                    crate::tool_validation::ToolStatus::Unknown { .. } if !report.inventory_complete => {
                        unobserved += 1;
                    }
                    crate::tool_validation::ToolStatus::Unknown { reason } => {
                        let _ = writeln!(output, "  unknown {}: {reason}", check.tool);
                    }
                }
            }
            if unobserved > 0 {
                let _ = writeln!(
                    output,
                    "  {unobserved} declared tools have no host observation to compare against; run inside a protected session to check them"
                );
            }
            for diagnostic in report.diagnostics {
                let _ = writeln!(output, "  {diagnostic}");
            }
        }
        Err(error) => {
            let _ = writeln!(output, "Validation failed: {error}");
        }
    }
    Description { text: output, valid }
}

/// The MCP servers this machine configures or the session reports, and the batteries of the
/// installed version that cover them.
#[cfg(feature = "daemon")]
fn render_servers(output: &mut String, config: &Path, servers: &BTreeSet<appa_package::Namespace>) {
    let names: Vec<String> = servers.iter().map(|server| server.as_str().to_owned()).collect();
    let _ = writeln!(output, "MCP servers: {}", list_or_none(&names));
    if servers.is_empty() {
        return;
    }
    let mut rendered = Vec::new();
    // `describe` composes one plain document; escapes in a nested section of it
    // would be the only ones, and would travel wherever the text is put.
    match crate::installation::cli::render_server_coverage(&mut rendered, crate::style::Style::Plain, config, servers) {
        Ok(()) => output.push_str(&String::from_utf8_lossy(&rendered)),
        Err(error) => {
            let _ = writeln!(output, "Battery matches: unavailable ({error})");
        }
    }
}

/// How the served policy covers each tool the session reported.
#[derive(Debug, Default, PartialEq, Eq)]
struct SessionCoverage {
    /// Tools a contract names, exactly or by an argument selector.
    declared: usize,
    /// Tools only the wildcard rule covers: an Annotator judges each call.
    wildcard: Vec<String>,
    /// Tools no rule covers: every call is refused.
    refused: Vec<String>,
    /// Names that spell no tool of this host.
    unrecognized: Vec<String>,
    /// The MCP servers the reported tools belong to.
    servers: BTreeSet<appa_package::Namespace>,
}

impl SessionCoverage {
    fn of(tools: &[String], adapter: appa_runtime_api::Adapter, policy: Option<&appa_policy::Config>) -> Self {
        let mut coverage = SessionCoverage::default();
        for name in tools.iter().map(|name| name.trim()).filter(|name| !name.is_empty()) {
            let Some(canonical) = crate::tool_validation::precise_name(name, adapter) else {
                coverage.unrecognized.push(name.to_owned());
                continue;
            };
            if canonical.is_control() {
                continue;
            }
            if let Some(server) = canonical
                .as_str()
                .strip_prefix("mcp/")
                .and_then(|rest| rest.split_once('/'))
                .and_then(|(server, _)| appa_package::Namespace::parse(server).ok())
            {
                coverage.servers.insert(server);
            }
            let kind = policy.and_then(|policy| {
                policy
                    .engine()
                    .registry()
                    .classify(&appa_engine::value::ToolName::new(canonical.as_str()))
            });
            match kind {
                Some(appa_engine::registry::ToolKind::Declared | appa_engine::registry::ToolKind::ProviderRun) => {
                    coverage.declared += 1
                }
                Some(appa_engine::registry::ToolKind::Wildcard) => coverage.wildcard.push(canonical.into_string()),
                None => coverage.refused.push(canonical.into_string()),
            }
        }
        coverage
    }

    fn render(&self, output: &mut String) {
        let _ = writeln!(
            output,
            "Session tools: {} with a rule, {} annotated call by call, {} refused",
            self.declared,
            self.wildcard.len(),
            self.refused.len()
        );
        if !self.wildcard.is_empty() {
            let _ = writeln!(output, "  annotated call by call: {}", self.wildcard.join(", "));
        }
        if !self.refused.is_empty() {
            let _ = writeln!(output, "  refused, no rule: {}", self.refused.join(", "));
        }
        if !self.unrecognized.is_empty() {
            let _ = writeln!(output, "  not a tool name: {}", self.unrecognized.join(", "));
        }
    }
}

#[cfg(test)]
fn validation(
    path: &Path,
    battery_dirs: &[PathBuf],
    adapter: &str,
) -> Result<crate::tool_validation::ValidationReport, String> {
    let adapter = match adapter {
        "amp" => appa_adapter_amp::adapter(),
        "claude-code" => appa_adapter_claude_code::adapter(),
        "kagent" => appa_adapter_kagent::adapter(),
        _ => return Err(format!("unsupported adapter {adapter:?}")),
    };
    // Config parse errors may quote credentials from the source document.
    let config = Config::load_from(path, battery_dirs)
        .map_err(|_| "configuration cannot be loaded; check the configuration diagnostics above".to_string())?;
    crate::api::Runtime::validate_served(config, adapter).map_err(|error| error.to_string())
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
    fn preflight_allows_unknown_tools_but_rejects_known_coverage_errors() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("appa.toml");
        let policy = "[policy]\nversion = 2\n[[policy.tool]]\nname = \"read_secret\"\n[externals]\ntimeout_ms = 1000\nmax_body_bytes = 65536\n";
        std::fs::write(&path, policy).unwrap();
        let report = validation(&path, &[], "kagent").unwrap();
        assert!(!report.inventory_complete);
        assert!(report.tools_may_change);
        assert!(matches!(
            report.tools[0].status,
            crate::tool_validation::ToolStatus::Unknown { .. }
        ));
        let description = render(&path, &[], "kagent", &[]);
        assert!(description.valid);
        assert!(
            description.text.contains("1 declared tools have no host observation"),
            "{}",
            description.text
        );
        assert!(!description.text.contains("unknown read_secret:"));

        std::fs::write(
            &path,
            format!("{policy}\n[[appa_inventory.tools]]\nname = \"write_secret\"\ntool = \"mcp:demo/write_secret\"\n"),
        )
        .unwrap();
        assert!(!render(&path, &[], "kagent", &[]).valid);
    }

    #[test]
    fn preflight_compiles_the_policy_not_only_the_configuration_envelope() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("appa.toml");
        std::fs::write(&path, "[policy]\nversion = 2\n[[policy.tool]]\nname = \"read\"\nannotator = \"missing\"\n[externals]\ntimeout_ms = 1000\nmax_body_bytes = 65536\n").unwrap();
        assert!(Config::load_from(&path, &[]).is_ok());
        assert!(!render(&path, &[], "kagent", &[]).valid);
    }

    /// Each tool the session reports lands in exactly one bucket of the served policy: a rule,
    /// the wildcard's Annotator, or refusal. The session's own MCP servers are collected for
    /// battery matching; the runtime's control tool is neither.
    #[test]
    fn session_tools_are_classified_against_the_served_policy() {
        let policy = |wildcard: &str| {
            appa_policy::Config::from_toml_str_routed(
                &format!(
                    "version = 2\n[[tool]]\nname = \"host/claude-code/Bash\"\ndelta = {{}}\n[[tool]]\nname = \"mcp/github/get_issue\"\ndelta = {{ trust = \"suspicious\" }}\n{wildcard}"
                ),
                BTreeMap::new(),
                Vec::new(),
            )
            .unwrap_or_else(|error| panic!("fixture must compile: {error}"))
        };
        let tools: Vec<String> = [
            "Bash",
            "mcp__github__get_issue",
            "mcp__notes__read",
            "mcp__appa__execute_remedy_plan",
            "not a tool",
        ]
        .map(str::to_owned)
        .to_vec();
        let adapter = appa_adapter_claude_code::adapter();

        let with_wildcard = SessionCoverage::of(
            &tools,
            adapter,
            Some(&policy(
                "[[annotator]]\nname = \"judge\"\nranks = [\"suspicious\"]\n[[tool]]\nname = \"*\"\nannotator = \"judge\"\n",
            )),
        );
        assert_eq!(
            with_wildcard,
            SessionCoverage {
                declared: 2,
                wildcard: vec!["mcp/notes/read".to_owned()],
                refused: vec![],
                unrecognized: vec!["not a tool".to_owned()],
                servers: ["github", "notes"]
                    .into_iter()
                    .map(|server| appa_package::Namespace::parse(server).unwrap())
                    .collect(),
            }
        );

        let without_wildcard = SessionCoverage::of(&tools, adapter, Some(&policy("")));
        assert_eq!(without_wildcard.refused, ["mcp/notes/read"]);
        assert!(without_wildcard.wildcard.is_empty());
    }

    #[test]
    fn codex_callable_names_use_hook_identities_for_coverage_and_proposals() {
        let tools = [
            "exec_command",
            "web__run",
            "image_gen__imagegen",
            "mcp__github__exec_command",
            "mcp__appa__execute_remedy_plan",
        ]
        .map(str::to_owned);
        let adapter = appa_adapter_codex::adapter();
        let policy = appa_policy::Config::from_toml_str_routed(
            "version = 2\n[[tool]]\nname = \"host/codex/appa_exec\"\ndelta = {}\n",
            BTreeMap::new(),
            Vec::new(),
        )
        .unwrap();
        let coverage = SessionCoverage::of(&tools, adapter, Some(&policy));
        assert_eq!(coverage.declared, 1);
        assert_eq!(
            coverage.refused,
            [
                "host/codex/webrun",
                "host/codex/image_genimagegen",
                "mcp/github/exec_command"
            ]
        );
        assert!(coverage.unrecognized.is_empty());
        assert!(coverage.wildcard.is_empty());
        assert_eq!(
            coverage.servers,
            [appa_package::Namespace::parse("github").unwrap()].into()
        );

        // A proposal copies the refused identities into explicit rules.
        let proposed = coverage
            .refused
            .iter()
            .map(|name| format!("[[tool]]\nname = {name:?}\ndelta = {{}}\n"))
            .collect::<String>();
        let policy = appa_policy::Config::from_toml_str_routed(
            &format!("version = 2\n[[tool]]\nname = \"host/codex/appa_exec\"\ndelta = {{}}\n{proposed}"),
            BTreeMap::new(),
            Vec::new(),
        )
        .unwrap();
        let covered = SessionCoverage::of(&tools, adapter, Some(&policy));
        assert_eq!(covered.declared, 4);
        assert!(covered.refused.is_empty());
    }

    #[test]
    fn missing_config_is_described_without_creating_it() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("missing.toml");

        let (config, policy, loaded) = inspect(&path, &[]);

        assert_eq!(config.state, ConfigState::Missing);
        assert!(loaded.is_none());
        assert!(!path.exists());
        assert!(policy.tools.is_empty());
    }

    #[test]
    fn loadable_config_reports_batteries_tools_authorities_and_sources() {
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
            "include = [\"batteries/mail/appa.toml\"]\n[policy]\nversion = 2\n[policy.audience]\nself = [\"slack:viewer\"]\n[policy.audience.group.finance]\nwithin = \"internal\"\nfrom = [\"slack:user-group/finance\"]\n[[policy.authority]]\nname = \"operator\"\n[policy.authority.permits]\ntrust_below = \"trusted\"\naudience_missing = [\"public\"]\neffects_containing = [\"mail.sent\"]\nattention = [\"hitl\"]\n[externals]\ntimeout_ms = 1000\nmax_body_bytes = 65536\n[externals.authorities.operator]\nbuiltin = \"hitl\"\n[externals.audience.slack]\ncommand = [\"true\"]\nlookup = \"people\"\nselectors = [{ template = \"viewer\", feeds = \"self\" }, { template = \"full-members\", feeds = \"internal\" }, { template = \"user-group/<handle>\" }]\n[externals.audience.people]\nreaders = { \"slack:U1\" = \"alice@corp.example\" }\n",
        )
        .expect("root config");

        Config::load_from(&root, std::slice::from_ref(&batteries))
            .unwrap_or_else(|error| panic!("fixture must load: {error}"));

        let (config, policy, loaded) = inspect(&root, std::slice::from_ref(&batteries));

        assert_eq!(config.state, ConfigState::Loadable);
        assert!(loaded.is_some());
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
                lookup: Some("people".to_string()),
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
        let rendered = render(&root, &[batteries], "claude-code", &[]).text;
        assert!(rendered.contains(
            "operator: builtin hitl; permits trust_below=trusted, audience_missing=public, effects_containing=[mail.sent], attention=[hitl]"
        ));
        assert!(
            rendered
                .contains("slack: viewer, full-members, user-group/<handle> (binding configured; lookups via people)"),
            "{rendered}"
        );
    }

    #[test]
    fn malformed_config_does_not_echo_its_contents() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("appa.toml");
        let secret = "super-secret-token";
        std::fs::write(&path, format!("token = \\\"{secret}")).expect("malformed config");

        let (config, _, _) = inspect(&path, &[]);
        let output = render(&path, &[], "claude-code", &[]).text;

        assert_eq!(config.state, ConfigState::Unparsable);
        assert!(!output.contains(secret));
        assert!(output.contains("line 1, column"), "{output}");
    }

    #[test]
    fn a_config_that_parses_but_does_not_load_names_the_reason() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("appa.toml");
        std::fs::write(&path, "[policy]\nversion = 2\n[externals]\nmax_body_bytes = 65536\n").expect("config");

        let (config, _, _) = inspect(&path, &[]);
        let output = render(&path, &[], "claude-code", &[]).text;

        assert_eq!(config.state, ConfigState::Invalid);
        assert!(output.contains("configuration does not load: "), "{output}");
        assert!(output.contains("timeout_ms"), "{output}");
    }

    #[test]
    fn human_output_is_small_and_explicit_about_unknown_session_facts() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let output = render(&directory.path().join("appa.toml"), &[], "claude-code", &[]).text;

        assert!(output.contains("Config:"));
        assert!(output.contains("Batteries: none"));
        assert!(output.contains("Session tools: unavailable"));
    }
}
