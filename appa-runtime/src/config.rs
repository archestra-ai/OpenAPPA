//! Configuration: one immutable `appa.toml` with the policy and the
//! external endpoints. The harness adapter is a CLI flag on the
//! binary, not configuration.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct Config {
    policy: PolicyFile,
    pub externals: Externals,
}

/// The effective policy file: deterministic TOML bytes after includes
/// compose, and the policy value parsed from those bytes. The stored
/// value and bytes always describe the same deployment.
#[derive(Debug, Clone)]
pub struct PolicyFile {
    bytes: Vec<u8>,
    value: toml::Value,
}

impl PolicyFile {
    fn new(bytes: Vec<u8>, value: toml::Value) -> PolicyFile {
        PolicyFile { bytes, value }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn value(&self) -> &toml::Value {
        &self.value
    }
}

/// The registered externals: authorities, sanitizers, the
/// dynamic resolver, and the membership resolver. Every HTTP call
/// carries an explicit timeout and byte cap and fails closed.
#[derive(Debug, Clone)]
pub struct Externals {
    pub timeout: Duration,
    /// How long a human review may stay open before the runtime treats
    /// it as no answer. Deliberately separate from
    /// `timeout`, which bounds a machine consult: a person reads the
    /// arguments and thinks. It also bounds the MCP session's
    /// keep-alive, because a review outliving its session cannot be
    /// answered.
    pub review_timeout: Duration,
    pub max_body_bytes: usize,
    pub authorities: BTreeMap<String, Implementation>,
    pub sanitizers: BTreeMap<String, Implementation>,
    /// The classifiers a resolver-backed `[[cast]]` consults. Endpoint-only: a constant
    /// cast is answered from the policy and binds nothing here.
    pub casts: BTreeMap<String, Endpoint>,
    /// One implementation per policy-declared dynamic resolver.
    pub dynamic: BTreeMap<String, DynamicImplementation>,
    /// The membership resolver the policy's `[membership]` registers.
    pub membership: Option<Endpoint>,
    /// Deployment knobs for the stock `claude-code` dynamic resolver.
    pub claude_code: ClaudeCode,
}

/// How this deployment runs the stock `claude-code` classifier. `command` overrides the
/// executable (a service environment often strips `PATH`); `model` pins the model the
/// consult runs on; `timeout` bounds one consult on its own budget instead of the shared
/// machine-consult `timeout`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeCode {
    pub command: std::path::PathBuf,
    pub model: String,
    pub timeout: Option<Duration>,
}

impl Default for ClaudeCode {
    /// The usable defaults every construction path shares — an embedded host building
    /// `Externals` by hand gets the same `claude` on `PATH` and `sonnet` alias the file
    /// loader fills in, never an empty command.
    fn default() -> ClaudeCode {
        ClaudeCode {
            command: "claude".into(),
            model: "sonnet".to_string(),
            timeout: None,
        }
    }
}

/// How a registered authority or sanitizer is implemented — `builtin` or
/// `resolver`, a closed choice per entry.
#[derive(Debug, Clone)]
pub enum Implementation {
    Resolver(Endpoint),
    Builtin(String),
}

/// How one named dynamic resolver runs.
#[derive(Debug, Clone)]
pub enum DynamicImplementation {
    Resolver(Endpoint),
    Builtin(String),
    Command(ResolverCommand),
}

/// A command resolver's argv and the directory of the config that declared it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolverCommand {
    pub argv: Vec<String>,
    pub cwd: std::path::PathBuf,
}

pub const CLAUDE_CODE_BUILTIN: &str = "claude-code";

/// One external endpoint: a validated URL plus its bearer token, if
/// the service needs one. `https` reaches anywhere; `http` only
/// loopback — a bearer token MUST NOT cross the network in the clear.
#[derive(Debug, Clone)]
pub struct Endpoint {
    pub url: String,
    pub token: Option<Token>,
}

/// A bearer token resolved from an `APPA_*` environment variable.
/// The value never appears in logs: `Debug` redacts it.
#[derive(Clone)]
pub struct Token(String);

impl Token {
    pub(crate) fn new(value: String) -> Token {
        Token(value)
    }

    pub(crate) fn reveal(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Unreadable { path: String, source: std::io::Error },
    #[error("cannot parse {path}: {source}")]
    Unparsable { path: String, source: toml::de::Error },
    #[error("cannot parse the composed policy: {source}")]
    UnparsablePolicy { source: toml::de::Error },
    #[error("the composed policy does not render as a policy file: {source}")]
    UnrenderablePolicy { source: toml::ser::Error },
    #[error("root policy.version must be an integer")]
    InvalidPolicyVersion,
    #[error("include path {path:?} must be relative to the root config")]
    AbsoluteInclude { path: String },
    #[error("the root config includes {path:?} more than once")]
    DuplicateInclude { path: String },
    #[error("included config {path} has unsupported top-level field {field:?}")]
    IncludedTopLevel { path: String, field: String },
    #[error("included config {path} cannot set policy field {field:?}")]
    IncludedPolicyField { path: String, field: String },
    #[error("included config {path} cannot set externals field {field:?}")]
    IncludedExternalsField { path: String, field: String },
    #[error("included config {path} uses policy version {found}, but the root uses {root}")]
    IncludedVersion { path: String, root: i64, found: i64 },
    #[error("included config {path} repeats [externals.{section}] entry {name:?}")]
    DuplicateExternal {
        path: String,
        section: String,
        name: String,
    },
    #[error("the {section} endpoint {name:?} has an invalid url: {url}")]
    InvalidEndpoint {
        section: &'static str,
        name: String,
        url: String,
    },
    #[error("the {section} endpoint {name:?} uses cleartext http to a non-loopback host: {url}")]
    CleartextEndpoint {
        section: &'static str,
        name: String,
        url: String,
    },
    #[error("the {section} endpoint {name:?} embeds credentials in its url")]
    CredentialsInUrl { section: &'static str, name: String },
    #[error("the {section} endpoint {name:?} names {var}, which must start with APPA_")]
    ForeignSecretVariable {
        section: &'static str,
        name: String,
        var: String,
    },
    #[error("the {section} endpoint {name:?} names {var}, which is not set")]
    MissingSecret {
        section: &'static str,
        name: String,
        var: String,
    },
    #[error("externals.timeout_ms must be greater than zero")]
    ZeroTimeout,
    #[error("externals.review_timeout_ms must be greater than zero")]
    ZeroReviewTimeout,
    #[error("externals.max_body_bytes must be greater than zero")]
    ZeroByteCap,
    #[error("the {section} entry {name:?} must name exactly one implementation, and only url takes token_env")]
    ImplementationChoice { section: &'static str, name: String },
    #[error("the {section} entry {name:?} cannot be builtin")]
    BuiltinNotAllowed { section: &'static str, name: String },
    #[error("the {section} entry {name:?} names the builtin {builtin:?}, which is not a valid implementation name")]
    InvalidBuiltinName {
        section: &'static str,
        name: String,
        builtin: String,
    },
    #[error("the dynamic resolver {name:?} command must contain at least one non-empty argument")]
    InvalidCommand { name: String },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    include: Vec<String>,
    policy: toml::Value,
    externals: RawExternals,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawExternals {
    timeout_ms: u64,
    #[serde(default = "default_review_timeout_ms")]
    review_timeout_ms: u64,
    max_body_bytes: usize,
    #[serde(default)]
    authorities: BTreeMap<String, RawImplementation>,
    #[serde(default)]
    sanitizers: BTreeMap<String, RawImplementation>,
    #[serde(default)]
    casts: BTreeMap<String, RawImplementation>,
    #[serde(default)]
    dynamic: BTreeMap<String, RawDynamicImplementation>,
    membership: Option<RawImplementation>,
    claude_code: Option<RawClaudeCode>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawClaudeCode {
    command: Option<String>,
    model: Option<String>,
    timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawImplementation {
    url: Option<String>,
    token_env: Option<String>,
    builtin: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDynamicImplementation {
    url: Option<String>,
    token_env: Option<String>,
    builtin: Option<String>,
    command: Option<Vec<String>>,
}

fn default_review_timeout_ms() -> u64 {
    600_000
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Unreadable {
            path: path.display().to_string(),
            source,
        })?;
        let root: RawConfig = toml::from_str(&text).map_err(|source| ConfigError::Unparsable {
            path: path.display().to_string(),
            source,
        })?;
        let source_dir = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let mut document: toml::Value = toml::from_str(&text).map_err(|source| ConfigError::Unparsable {
            path: path.display().to_string(),
            source,
        })?;
        document
            .as_table_mut()
            .expect("RawConfig parsed the root as a table")
            .remove("include");

        let root_version = policy_version(&root.policy).ok_or(ConfigError::InvalidPolicyVersion)?;
        let mut origins = root
            .externals
            .dynamic
            .keys()
            .map(|name| (name.clone(), source_dir.to_path_buf()))
            .collect::<BTreeMap<_, _>>();
        let mut seen = std::collections::BTreeSet::new();
        for authored in &root.include {
            let include = Path::new(authored);
            if include.is_absolute() {
                return Err(ConfigError::AbsoluteInclude { path: authored.clone() });
            }
            if !seen.insert(authored.clone()) {
                return Err(ConfigError::DuplicateInclude { path: authored.clone() });
            }
            let include_path = source_dir.join(include);
            let included_text = std::fs::read_to_string(&include_path).map_err(|source| ConfigError::Unreadable {
                path: include_path.display().to_string(),
                source,
            })?;
            let included: toml::Value = toml::from_str(&included_text).map_err(|source| ConfigError::Unparsable {
                path: include_path.display().to_string(),
                source,
            })?;
            compose_include(&mut document, included, &include_path, root_version, &mut origins)?;
        }

        let composed = toml::to_string(&document).map_err(|source| ConfigError::UnrenderablePolicy { source })?;
        let raw: RawConfig = toml::from_str(&composed).map_err(|source| ConfigError::UnparsablePolicy { source })?;
        Config::validate_composed(composed, raw, origins, |var| std::env::var(var).ok())
    }

    /// The configuration of a host that composes its policy in memory
    /// rather than reading `appa.toml`: the policy text it composed, and
    /// the bindings it binds itself.
    pub fn embedded(policy: String, externals: Externals) -> Result<Config, ConfigError> {
        let value: toml::Value = toml::from_str(&policy).map_err(|source| ConfigError::UnparsablePolicy { source })?;
        let mut file = toml::value::Table::new();
        file.insert("policy".to_string(), value.clone());
        let text =
            toml::to_string(&toml::Value::Table(file)).map_err(|source| ConfigError::UnrenderablePolicy { source })?;
        Ok(Config {
            policy: PolicyFile::new(text.into_bytes(), value),
            externals,
        })
    }

    pub fn policy_file(&self) -> &PolicyFile {
        &self.policy
    }

    #[cfg(test)]
    fn validate(
        text: String,
        raw: RawConfig,
        source_dir: &Path,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Config, ConfigError> {
        let origins = raw
            .externals
            .dynamic
            .keys()
            .map(|name| (name.clone(), source_dir.to_path_buf()))
            .collect();
        Config::validate_composed(text, raw, origins, lookup)
    }

    fn validate_composed(
        text: String,
        raw: RawConfig,
        dynamic_origins: BTreeMap<String, PathBuf>,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Config, ConfigError> {
        debug_assert!(raw.include.is_empty(), "composed configuration has no includes");
        if raw.externals.timeout_ms == 0 {
            return Err(ConfigError::ZeroTimeout);
        }
        if raw.externals.review_timeout_ms == 0 {
            return Err(ConfigError::ZeroReviewTimeout);
        }
        if raw.externals.max_body_bytes == 0 {
            return Err(ConfigError::ZeroByteCap);
        }
        Ok(Config {
            policy: PolicyFile::new(text.into_bytes(), raw.policy),
            externals: Externals {
                timeout: Duration::from_millis(raw.externals.timeout_ms),
                review_timeout: Duration::from_millis(raw.externals.review_timeout_ms),
                max_body_bytes: raw.externals.max_body_bytes,
                authorities: resolve_implementations(
                    "authorities",
                    raw.externals.authorities,
                    &lookup,
                    crate::builtins::valid_implementation_name,
                )?,
                sanitizers: resolve_implementations(
                    "sanitizers",
                    raw.externals.sanitizers,
                    &lookup,
                    crate::builtins::valid_implementation_name,
                )?,
                casts: resolve_endpoints("casts", raw.externals.casts, &lookup)?,
                dynamic: resolve_dynamic_implementations(raw.externals.dynamic, &dynamic_origins, &lookup)?,
                membership: raw
                    .externals
                    .membership
                    .map(|endpoint| {
                        let raw = endpoint_only("membership", "membership", endpoint)?;
                        resolve_endpoint("membership", "membership".to_string(), raw, &lookup)
                    })
                    .transpose()?,
                claude_code: resolve_claude_code(raw.externals.claude_code)?,
            },
        })
    }
}

fn policy_version(policy: &toml::Value) -> Option<i64> {
    policy.as_table()?.get("version")?.as_integer()
}

fn compose_include(
    root: &mut toml::Value,
    included: toml::Value,
    include_path: &Path,
    root_version: i64,
    dynamic_origins: &mut BTreeMap<String, PathBuf>,
) -> Result<(), ConfigError> {
    let display = include_path.display().to_string();
    let mut included = included.as_table().expect("a TOML document is always a table").clone();
    for field in included.keys() {
        if !matches!(field.as_str(), "policy" | "externals") {
            return Err(ConfigError::IncludedTopLevel {
                path: display,
                field: field.clone(),
            });
        }
    }

    let included_policy = included
        .remove("policy")
        .and_then(|value| value.as_table().cloned())
        .ok_or_else(|| ConfigError::IncludedTopLevel {
            path: display.clone(),
            field: "policy".to_string(),
        })?;
    let found_version = included_policy
        .get("version")
        .and_then(toml::Value::as_integer)
        .ok_or_else(|| ConfigError::IncludedPolicyField {
            path: display.clone(),
            field: "version".to_string(),
        })?;
    if found_version != root_version {
        return Err(ConfigError::IncludedVersion {
            path: display,
            root: root_version,
            found: found_version,
        });
    }

    let root_table = root.as_table_mut().expect("a TOML document is always a table");
    let root_policy = root_table
        .get_mut("policy")
        .and_then(toml::Value::as_table_mut)
        .expect("RawConfig requires a policy table");
    for (field, value) in included_policy {
        if field == "version" {
            continue;
        }
        if !matches!(
            field.as_str(),
            "tool" | "dynamic_resolver" | "authority" | "sanitizer" | "cast"
        ) {
            return Err(ConfigError::IncludedPolicyField {
                path: include_path.display().to_string(),
                field,
            });
        }
        let mut declarations = value
            .as_array()
            .cloned()
            .ok_or_else(|| ConfigError::IncludedPolicyField {
                path: include_path.display().to_string(),
                field: field.clone(),
            })?;
        let destination = root_policy
            .entry(field.clone())
            .or_insert_with(|| toml::Value::Array(Vec::new()))
            .as_array_mut()
            .ok_or_else(|| ConfigError::IncludedPolicyField {
                path: include_path.display().to_string(),
                field,
            })?;
        destination.append(&mut declarations);
    }

    let Some(included_externals) = included.remove("externals") else {
        return Ok(());
    };
    let included_externals = included_externals
        .as_table()
        .ok_or_else(|| ConfigError::IncludedTopLevel {
            path: include_path.display().to_string(),
            field: "externals".to_string(),
        })?;
    let root_externals = root_table
        .get_mut("externals")
        .and_then(toml::Value::as_table_mut)
        .expect("RawConfig requires an externals table");
    for (section, entries) in included_externals {
        if !matches!(section.as_str(), "authorities" | "sanitizers" | "casts" | "dynamic") {
            return Err(ConfigError::IncludedExternalsField {
                path: include_path.display().to_string(),
                field: section.clone(),
            });
        }
        let entries = entries.as_table().ok_or_else(|| ConfigError::IncludedExternalsField {
            path: include_path.display().to_string(),
            field: section.clone(),
        })?;
        let destination = root_externals
            .entry(section.clone())
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
            .as_table_mut()
            .expect("RawExternals requires named external tables");
        for (name, entry) in entries {
            if destination.contains_key(name) {
                return Err(ConfigError::DuplicateExternal {
                    path: include_path.display().to_string(),
                    section: section.clone(),
                    name: name.clone(),
                });
            }
            destination.insert(name.clone(), entry.clone());
            if section == "dynamic" {
                dynamic_origins.insert(
                    name.clone(),
                    include_path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf(),
                );
            }
        }
    }
    Ok(())
}

fn resolve_dynamic_implementations(
    raw: BTreeMap<String, RawDynamicImplementation>,
    origins: &BTreeMap<String, PathBuf>,
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<BTreeMap<String, DynamicImplementation>, ConfigError> {
    raw.into_iter()
        .map(|(name, entry)| {
            let RawDynamicImplementation {
                url,
                token_env,
                builtin,
                command,
            } = entry;
            let implementation = match (url, builtin, command) {
                (Some(url), None, None) => {
                    let endpoint = resolve_endpoint("dynamic", name.clone(), RawEndpoint { url, token_env }, lookup)?;
                    DynamicImplementation::Resolver(endpoint)
                }
                (None, Some(builtin), None) if token_env.is_none() && builtin == CLAUDE_CODE_BUILTIN => {
                    DynamicImplementation::Builtin(builtin)
                }
                (None, Some(builtin), None) if token_env.is_none() => {
                    return Err(ConfigError::InvalidBuiltinName {
                        section: "dynamic",
                        name,
                        builtin,
                    });
                }
                (None, None, Some(argv)) if token_env.is_none() && argv.iter().all(|argument| !argument.is_empty()) => {
                    if argv.is_empty() {
                        return Err(ConfigError::InvalidCommand { name });
                    }
                    DynamicImplementation::Command(ResolverCommand {
                        argv,
                        cwd: origins
                            .get(&name)
                            .expect("every composed dynamic binding records its source")
                            .clone(),
                    })
                }
                (None, None, Some(_)) if token_env.is_none() => return Err(ConfigError::InvalidCommand { name }),
                _ => {
                    return Err(ConfigError::ImplementationChoice {
                        section: "dynamic",
                        name,
                    });
                }
            };
            Ok((name, implementation))
        })
        .collect()
}

fn resolve_implementations(
    section: &'static str,
    raw: BTreeMap<String, RawImplementation>,
    lookup: &impl Fn(&str) -> Option<String>,
    valid_builtin: fn(&str) -> bool,
) -> Result<BTreeMap<String, Implementation>, ConfigError> {
    raw.into_iter()
        .map(|(name, entry)| {
            let implementation = resolve_implementation(section, &name, entry, lookup, valid_builtin)?;
            Ok((name, implementation))
        })
        .collect()
}

/// One entry's implementation choice: exactly one of `url` (with an optional token) or a
/// `builtin` naming an implementation the section accepts.
fn resolve_implementation(
    section: &'static str,
    name: &str,
    entry: RawImplementation,
    lookup: &impl Fn(&str) -> Option<String>,
    valid_builtin: impl Fn(&str) -> bool,
) -> Result<Implementation, ConfigError> {
    let RawImplementation {
        url,
        token_env,
        builtin,
    } = entry;
    match (url, builtin) {
        (Some(url), None) => {
            let endpoint = RawEndpoint { url, token_env };
            Ok(Implementation::Resolver(resolve_endpoint(
                section,
                name.to_string(),
                endpoint,
                lookup,
            )?))
        }
        (None, Some(builtin)) => {
            if token_env.is_some() {
                return Err(ConfigError::ImplementationChoice {
                    section,
                    name: name.to_string(),
                });
            }
            if !valid_builtin(&builtin) {
                return Err(ConfigError::InvalidBuiltinName {
                    section,
                    name: name.to_string(),
                    builtin,
                });
            }
            Ok(Implementation::Builtin(builtin))
        }
        _ => Err(ConfigError::ImplementationChoice {
            section,
            name: name.to_string(),
        }),
    }
}

/// A section whose every entry is an endpoint. A cast classifies content over the wire or
/// resolves to a declared constant the engine reads itself, so there is no builtin to name.
/// The `[externals.claude_code]` table with its defaults filled: bare `claude` on `PATH`,
/// the `sonnet` alias, and the shared machine-consult timeout. A zero `timeout_ms` is a
/// refusal like the shared one.
fn resolve_claude_code(raw: Option<RawClaudeCode>) -> Result<ClaudeCode, ConfigError> {
    let raw = raw.unwrap_or(RawClaudeCode {
        command: None,
        model: None,
        timeout_ms: None,
    });
    if raw.timeout_ms == Some(0) {
        return Err(ConfigError::ZeroTimeout);
    }
    Ok(ClaudeCode {
        command: raw
            .command
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| "claude".into()),
        model: raw.model.unwrap_or_else(|| "sonnet".to_string()),
        timeout: raw.timeout_ms.map(Duration::from_millis),
    })
}

fn resolve_endpoints(
    section: &'static str,
    raw: BTreeMap<String, RawImplementation>,
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<BTreeMap<String, Endpoint>, ConfigError> {
    raw.into_iter()
        .map(|(name, entry)| {
            let endpoint = endpoint_only(section, &name, entry)?;
            let resolved = resolve_endpoint(section, name.clone(), endpoint, lookup)?;
            Ok((name, resolved))
        })
        .collect()
}

fn endpoint_only(section: &'static str, name: &str, entry: RawImplementation) -> Result<RawEndpoint, ConfigError> {
    if entry.builtin.is_some() {
        return Err(ConfigError::BuiltinNotAllowed {
            section,
            name: name.to_string(),
        });
    }
    match entry.url {
        Some(url) => Ok(RawEndpoint {
            url,
            token_env: entry.token_env,
        }),
        None => Err(ConfigError::ImplementationChoice {
            section,
            name: name.to_string(),
        }),
    }
}

struct RawEndpoint {
    url: String,
    token_env: Option<String>,
}

fn resolve_endpoint(
    section: &'static str,
    name: String,
    raw: RawEndpoint,
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<Endpoint, ConfigError> {
    let parsed = reqwest::Url::parse(&raw.url).map_err(|_| ConfigError::InvalidEndpoint {
        section,
        name: name.clone(),
        url: raw.url.clone(),
    })?;
    match parsed.scheme() {
        "https" => {}
        "http" => {
            if !is_loopback(&parsed) {
                return Err(ConfigError::CleartextEndpoint {
                    section,
                    name,
                    url: raw.url,
                });
            }
        }
        _ => {
            return Err(ConfigError::InvalidEndpoint {
                section,
                name,
                url: raw.url,
            });
        }
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(ConfigError::CredentialsInUrl { section, name });
    }
    let token = match raw.token_env {
        None => None,
        Some(var) => {
            if !var.starts_with("APPA_") {
                return Err(ConfigError::ForeignSecretVariable { section, name, var });
            }
            match lookup(&var) {
                Some(value) if !value.is_empty() => Some(Token::new(value)),
                _ => return Err(ConfigError::MissingSecret { section, name, var }),
            }
        }
    };
    Ok(Endpoint { url: raw.url, token })
}

fn is_loopback(url: &reqwest::Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(domain)) => domain == "localhost",
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
        [policy]
        anything = "the runtime does not interpret this"

        [externals]
        timeout_ms = 5000
        max_body_bytes = 65536
    "#;

    fn parse(text: &str) -> Result<Config, ConfigError> {
        parse_with(text, |_| None)
    }

    fn parse_with(text: &str, lookup: impl Fn(&str) -> Option<String>) -> Result<Config, ConfigError> {
        let raw: RawConfig = toml::from_str(text).expect("test fixture parses as TOML");
        Config::validate(text.to_string(), raw, Path::new("."), lookup)
    }

    #[test]
    fn a_minimal_file_loads_and_keeps_the_policy_opaque() {
        let config = parse(MINIMAL).expect("the minimal fixture validates");
        assert_eq!(config.externals.timeout, Duration::from_millis(5000));
        assert_eq!(config.externals.max_body_bytes, 65536);
        assert!(config.externals.dynamic.is_empty());
        assert_eq!(
            config.policy_file().value().get("anything").and_then(|v| v.as_str()),
            Some("the runtime does not interpret this"),
        );
        assert_eq!(config.policy_file().bytes(), MINIMAL.as_bytes());
    }

    #[test]
    fn an_unknown_key_is_refused() {
        let text = MINIMAL.replace("[policy]", "typo_key = 1\n[policy]");
        let result: Result<RawConfig, _> = toml::from_str(&text);
        assert!(result.is_err(), "a typo must not silently weaken a deployment");
    }

    #[test]
    fn a_zero_timeout_and_a_zero_byte_cap_are_refused() {
        let zero_timeout = MINIMAL.replace("timeout_ms = 5000", "timeout_ms = 0");
        assert!(matches!(parse(&zero_timeout), Err(ConfigError::ZeroTimeout)));

        let zero_cap = MINIMAL.replace("max_body_bytes = 65536", "max_body_bytes = 0");
        assert!(matches!(parse(&zero_cap), Err(ConfigError::ZeroByteCap)));
    }

    #[test]
    fn invalid_and_cleartext_and_credentialed_urls_are_refused() {
        let bad = format!("{MINIMAL}\n[externals.authorities.security]\nurl = \"ftp://authority.internal\"\n");
        assert!(matches!(parse(&bad), Err(ConfigError::InvalidEndpoint { .. })));

        let unparsable = format!("{MINIMAL}\n[externals.authorities.security]\nurl = \"https://\"\n");
        assert!(matches!(parse(&unparsable), Err(ConfigError::InvalidEndpoint { .. })));

        let cleartext = format!("{MINIMAL}\n[externals.authorities.security]\nurl = \"http://authority.internal\"\n");
        assert!(matches!(parse(&cleartext), Err(ConfigError::CleartextEndpoint { .. })));

        let loopback = format!("{MINIMAL}\n[externals.authorities.security]\nurl = \"http://127.0.0.1:9000/\"\n");
        assert!(parse(&loopback).is_ok());

        let userinfo =
            format!("{MINIMAL}\n[externals.authorities.security]\nurl = \"https://user:pw@authority.internal/\"\n");
        assert!(matches!(parse(&userinfo), Err(ConfigError::CredentialsInUrl { .. })));
    }

    #[test]
    fn secrets_come_only_from_appa_variables() {
        let foreign = format!(
            "{MINIMAL}\n[externals.authorities.security]\nurl = \"https://authority.internal\"\ntoken_env = \"HOME\"\n"
        );
        assert!(matches!(
            parse(&foreign),
            Err(ConfigError::ForeignSecretVariable { .. }),
        ));

        let unset = format!(
            "{MINIMAL}\n[externals.authorities.security]\nurl = \"https://authority.internal\"\ntoken_env = \"APPA_AUTHORITY_TOKEN\"\n"
        );
        assert!(matches!(parse(&unset), Err(ConfigError::MissingSecret { .. })));

        let empty = format!(
            "{MINIMAL}\n[externals.authorities.security]\nurl = \"https://authority.internal\"\ntoken_env = \"APPA_AUTHORITY_TOKEN\"\n"
        );
        assert!(matches!(
            parse_with(&empty, |_| Some(String::new())),
            Err(ConfigError::MissingSecret { .. }),
        ));
    }

    #[test]
    fn a_present_secret_resolves_and_debug_redacts_it() {
        let text = format!(
            "{MINIMAL}\n[externals.dynamic.classifier]\nurl = \"https://resolver.internal\"\ntoken_env = \"APPA_RESOLVER_TOKEN\"\n"
        );
        let config = parse_with(&text, |var| {
            (var == "APPA_RESOLVER_TOKEN").then(|| "sekret".to_string())
        })
        .expect("the fixture with a set secret validates");
        assert!(!format!("{:?}", config.externals).contains("sekret"));
        let Some(DynamicImplementation::Resolver(dynamic)) = config.externals.dynamic.get("classifier") else {
            panic!("the named dynamic endpoint is set")
        };
        let token = dynamic.token.as_ref().expect("the token resolved");
        assert_eq!(token.reveal(), "sekret");
        assert_eq!(format!("{token:?}"), "Token(<redacted>)");
    }

    #[test]
    fn an_unreadable_path_is_a_named_refusal() {
        let missing = Path::new("/nonexistent/appa.toml");
        assert!(matches!(Config::load(missing), Err(ConfigError::Unreadable { .. }),));
    }

    #[test]
    fn the_claude_code_table_fills_its_defaults_and_refuses_junk() {
        let config = parse(MINIMAL).expect("no claude table is the default");
        assert_eq!(config.externals.claude_code.command, std::path::PathBuf::from("claude"));
        assert_eq!(config.externals.claude_code.model, "sonnet");
        assert_eq!(config.externals.claude_code.timeout, None);

        let text = format!(
            "{MINIMAL}\n[externals.claude_code]\ncommand = \"/opt/claude/bin/claude\"\nmodel = \"pinned\"\ntimeout_ms = 60000\n"
        );
        let config = parse(&text).expect("the claude table validates");
        assert_eq!(
            config.externals.claude_code.command,
            std::path::PathBuf::from("/opt/claude/bin/claude")
        );
        assert_eq!(config.externals.claude_code.model, "pinned");
        assert_eq!(config.externals.claude_code.timeout, Some(Duration::from_secs(60)));

        let text = format!("{MINIMAL}\n[externals.claude_code]\ntimeout_ms = 0\n");
        assert!(matches!(parse(&text), Err(ConfigError::ZeroTimeout)));
        let text = format!("{MINIMAL}\n[externals.claude_code]\nurl = \"https://x.example\"\n");
        assert!(
            toml::from_str::<RawConfig>(&text).is_err(),
            "a typo cannot silently weaken the deployment"
        );
    }

    #[test]
    fn builtin_entries_load_only_where_supported() {
        let text = format!(
            "{MINIMAL}\n[externals.authorities.auto]\nbuiltin = \"approve\"\n\n[externals.sanitizers.pii]\nbuiltin = \"redact-email\"\n"
        );
        let config = parse(&text).expect("builtin entries validate");
        assert!(matches!(
            config.externals.authorities.get("auto"),
            Some(Implementation::Builtin(name)) if name == "approve",
        ));
        assert!(matches!(
            config.externals.sanitizers.get("pii"),
            Some(Implementation::Builtin(name)) if name == "redact-email",
        ));

        let text = format!("{MINIMAL}\n[externals.dynamic.classifier]\nbuiltin = \"claude-code\"\n");
        assert!(matches!(
            parse(&text).expect("the dynamic builtin validates").externals.dynamic.get("classifier"),
            Some(DynamicImplementation::Builtin(name)) if name == "claude-code"
        ));
        let text = format!("{MINIMAL}\n[externals.membership]\nbuiltin = \"approve\"\n");
        assert!(matches!(parse(&text), Err(ConfigError::BuiltinNotAllowed { .. })));
        let text = format!("{MINIMAL}\n[externals.membership]\nurl = \"https://directory.internal\"\n");
        let config = parse(&text).expect("a membership endpoint validates");
        assert_eq!(
            config.externals.membership.map(|endpoint| endpoint.url),
            Some("https://directory.internal".to_string())
        );
    }

    #[test]
    fn an_entry_names_exactly_one_implementation() {
        let both =
            format!("{MINIMAL}\n[externals.authorities.auto]\nurl = \"https://a.example\"\nbuiltin = \"approve\"\n");
        assert!(matches!(parse(&both), Err(ConfigError::ImplementationChoice { .. })));

        let neither = format!("{MINIMAL}\n[externals.authorities.auto]\n");
        assert!(matches!(parse(&neither), Err(ConfigError::ImplementationChoice { .. })));

        let token = format!("{MINIMAL}\n[externals.authorities.auto]\nbuiltin = \"approve\"\ntoken_env = \"APPA_X\"\n");
        assert!(matches!(parse(&token), Err(ConfigError::ImplementationChoice { .. })));
    }

    #[test]
    fn a_builtin_name_outside_the_grammar_is_refused() {
        for bad in ["Upper", "under_score", "-lead", ""] {
            let text = format!("{MINIMAL}\n[externals.sanitizers.pii]\nbuiltin = \"{bad}\"\n");
            assert!(
                matches!(parse(&text), Err(ConfigError::InvalidBuiltinName { .. })),
                "builtin name {bad:?} must refuse",
            );
        }
    }

    #[test]
    fn includes_append_contracts_after_root_contracts_in_authored_order() {
        let dir = tempfile::tempdir().expect("temp directory");
        std::fs::write(
            dir.path().join("appa.toml"),
            r#"
                include = ["first.toml", "second.toml"]

                [policy]
                version = 1

                [[policy.tool]]
                name = "root"
                delta = {}

                [externals]
                timeout_ms = 5000
                max_body_bytes = 65536
            "#,
        )
        .expect("write root config");
        for (file, name) in [("first.toml", "first"), ("second.toml", "second")] {
            std::fs::write(
                dir.path().join(file),
                format!(
                    r#"
                        [policy]
                        version = 1

                        [[policy.tool]]
                        name = "{name}"
                        delta = {{}}
                    "#
                ),
            )
            .expect("write included config");
        }

        let config = Config::load(&dir.path().join("appa.toml")).expect("composed config loads");
        let names = config.policy_file().value()["tool"]
            .as_array()
            .expect("tool declarations")
            .iter()
            .map(|tool| tool["name"].as_str().expect("tool name"))
            .collect::<Vec<_>>();
        assert_eq!(names, ["root", "first", "second"]);
        assert!(!String::from_utf8_lossy(config.policy_file().bytes()).contains("include"));
    }

    #[test]
    fn included_command_paths_are_relative_to_their_declaring_configs() {
        let dir = tempfile::tempdir().expect("temp directory");
        std::fs::create_dir(dir.path().join("battery")).expect("create battery directory");
        std::fs::write(
            dir.path().join("appa.toml"),
            r#"
                include = ["battery/claude.toml"]
                [policy]
                version = 1
                [externals]
                timeout_ms = 5000
                max_body_bytes = 65536
                [externals.dynamic.local]
                command = ["python3", "local.py"]
            "#,
        )
        .expect("write root config");
        std::fs::write(
            dir.path().join("battery/claude.toml"),
            r#"
                [policy]
                version = 1
                [externals.dynamic.battery]
                command = ["python3", "resolver.py"]
            "#,
        )
        .expect("write included config");

        let config = Config::load(&dir.path().join("appa.toml")).expect("composed config loads");
        let command_cwd = |name| match config.externals.dynamic.get(name).expect("dynamic binding") {
            DynamicImplementation::Command(command) => command.cwd.as_path(),
            _ => panic!("binding is a command"),
        };
        assert_eq!(command_cwd("local"), dir.path());
        assert_eq!(command_cwd("battery"), dir.path().join("battery"));
    }

    #[test]
    fn include_boundaries_are_strict() {
        let dir = tempfile::tempdir().expect("temp directory");
        let root = dir.path().join("appa.toml");
        let write_root = |include: &str| {
            std::fs::write(
                &root,
                format!(
                    "include = [{include}]\n[policy]\nversion = 1\n[externals]\ntimeout_ms = 5000\nmax_body_bytes = 65536\n"
                ),
            )
            .expect("write root config");
        };

        write_root("\"/absolute.toml\"");
        assert!(matches!(Config::load(&root), Err(ConfigError::AbsoluteInclude { .. })));

        write_root("\"battery.toml\", \"battery.toml\"");
        std::fs::write(dir.path().join("battery.toml"), "[policy]\nversion = 1\n").expect("write included config");
        assert!(matches!(Config::load(&root), Err(ConfigError::DuplicateInclude { .. })));

        write_root("\"battery.toml\"");
        std::fs::write(
            dir.path().join("battery.toml"),
            "include = [\"nested.toml\"]\n[policy]\nversion = 1\n",
        )
        .expect("write nested include");
        assert!(matches!(Config::load(&root), Err(ConfigError::IncludedTopLevel { .. })));

        std::fs::write(dir.path().join("battery.toml"), "[policy]\nversion = 2\n").expect("write version mismatch");
        assert!(matches!(Config::load(&root), Err(ConfigError::IncludedVersion { .. })));
    }

    #[test]
    fn included_files_cannot_replace_root_settings_or_named_externals() {
        let dir = tempfile::tempdir().expect("temp directory");
        let root = dir.path().join("appa.toml");
        std::fs::write(
            &root,
            r#"
                include = ["battery.toml"]
                [policy]
                version = 1
                [externals]
                timeout_ms = 5000
                max_body_bytes = 65536
                [externals.dynamic.classifier]
                builtin = "claude-code"
            "#,
        )
        .expect("write root config");

        std::fs::write(dir.path().join("battery.toml"), "[policy]\nversion = 1\nlimits = {}\n")
            .expect("write singleton override");
        assert!(matches!(
            Config::load(&root),
            Err(ConfigError::IncludedPolicyField { .. })
        ));

        std::fs::write(
            dir.path().join("battery.toml"),
            r#"
                [policy]
                version = 1
                [externals.dynamic.classifier]
                builtin = "claude-code"
            "#,
        )
        .expect("write duplicate external");
        assert!(matches!(
            Config::load(&root),
            Err(ConfigError::DuplicateExternal { .. })
        ));

        std::fs::write(
            dir.path().join("battery.toml"),
            "[policy]\nversion = 1\n[externals]\ntimeout_ms = 1\n",
        )
        .expect("write external singleton");
        assert!(matches!(
            Config::load(&root),
            Err(ConfigError::IncludedExternalsField { .. })
        ));
    }

    #[test]
    fn composed_bytes_are_stable_and_standalone() {
        let dir = tempfile::tempdir().expect("temp directory");
        let first = dir.path().join("first.toml");
        let second = dir.path().join("second.toml");
        std::fs::write(
            &first,
            "# comment\n[policy]\nversion=1\n[externals]\ntimeout_ms=5000\nmax_body_bytes=65536\n",
        )
        .expect("write first config");
        std::fs::write(
            &second,
            "[policy]\nversion = 1 # another comment\n\n[externals]\ntimeout_ms = 5000\nmax_body_bytes = 65536\n",
        )
        .expect("write second config");
        let first = Config::load(&first).expect("first config loads");
        let second = Config::load(&second).expect("second config loads");
        assert_eq!(first.policy_file().bytes(), second.policy_file().bytes());

        let standalone = dir.path().join("standalone.toml");
        std::fs::write(&standalone, first.policy_file().bytes()).expect("write composed bytes");
        Config::load(&standalone).expect("composed bytes load without source files");

        let ordered = |names: [&str; 2]| {
            let path = dir.path().join(format!("{}-{}.toml", names[0], names[1]));
            std::fs::write(
                &path,
                format!(
                    "[policy]\nversion = 1\n[[policy.tool]]\nname = {:?}\n[[policy.tool]]\nname = {:?}\n[externals]\ntimeout_ms = 5000\nmax_body_bytes = 65536\n",
                    names[0], names[1]
                ),
            )
            .expect("write ordered declarations");
            Config::load(&path).expect("ordered config loads")
        };
        assert_ne!(
            ordered(["alpha", "beta"]).policy_file().bytes(),
            ordered(["beta", "alpha"]).policy_file().bytes(),
            "declaration order is part of deployment identity"
        );
    }
}
