//! Configuration: one immutable `appa.toml` with the policy and the
//! external endpoints. The harness adapter is a CLI flag on the
//! binary, not configuration.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct Config {
    policy: PolicyFile,
    pub externals: Externals,
}

/// The policy file as supplied at startup: the exact bytes read from
/// disk, the policy value parsed from those bytes, and the key derived
/// from them. The only constructor parses the bytes it is
/// given, so the three can never disagree.
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
    /// The shared dynamic-resolver implementation. HTTP supports both resolver
    /// contracts; the `claude-code` builtin is deliberately tool-level only.
    pub dynamic: Option<Implementation>,
    /// The membership resolver the policy's `[membership]` registers.
    pub membership: Option<Endpoint>,
}

/// How a registered authority, sanitizer, or dynamic resolver is implemented —
/// `builtin` or `resolver`, a closed choice per entry. Dynamic resolution has
/// one stock builtin; membership remains HTTP-only.
#[derive(Debug, Clone)]
pub enum Implementation {
    Resolver(Endpoint),
    Builtin(String),
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
    #[error("the {section} entry {name:?} must name exactly one of url or builtin, and only url takes token_env")]
    ImplementationChoice { section: &'static str, name: String },
    #[error("the {section} entry {name:?} cannot be builtin")]
    BuiltinNotAllowed { section: &'static str, name: String },
    #[error("the {section} entry {name:?} names the builtin {builtin:?}, which is not a valid implementation name")]
    InvalidBuiltinName {
        section: &'static str,
        name: String,
        builtin: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
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
    dynamic: Option<RawImplementation>,
    membership: Option<RawImplementation>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawImplementation {
    url: Option<String>,
    token_env: Option<String>,
    builtin: Option<String>,
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
        let raw: RawConfig = toml::from_str(&text).map_err(|source| ConfigError::Unparsable {
            path: path.display().to_string(),
            source,
        })?;
        Config::validate(text, raw, |var| std::env::var(var).ok())
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

    fn validate(text: String, raw: RawConfig, lookup: impl Fn(&str) -> Option<String>) -> Result<Config, ConfigError> {
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
                authorities: resolve_implementations("authorities", raw.externals.authorities, &lookup)?,
                sanitizers: resolve_implementations("sanitizers", raw.externals.sanitizers, &lookup)?,
                casts: resolve_endpoints("casts", raw.externals.casts, &lookup)?,
                dynamic: raw
                    .externals
                    .dynamic
                    .map(|implementation| resolve_dynamic(implementation, &lookup))
                    .transpose()?,
                membership: raw
                    .externals
                    .membership
                    .map(|endpoint| {
                        let raw = endpoint_only("membership", "membership", endpoint)?;
                        resolve_endpoint("membership", "membership".to_string(), raw, &lookup)
                    })
                    .transpose()?,
            },
        })
    }
}

fn resolve_dynamic(
    entry: RawImplementation,
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<Implementation, ConfigError> {
    // `claude-code` is the one dynamic builtin; modules cannot serve this section.
    resolve_implementation("dynamic", "dynamic", entry, lookup, |builtin| {
        builtin == CLAUDE_CODE_BUILTIN
    })
}

fn resolve_implementations(
    section: &'static str,
    raw: BTreeMap<String, RawImplementation>,
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<BTreeMap<String, Implementation>, ConfigError> {
    raw.into_iter()
        .map(|(name, entry)| {
            let implementation =
                resolve_implementation(section, &name, entry, lookup, crate::builtins::valid_implementation_name)?;
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
        Config::validate(text.to_string(), raw, lookup)
    }

    #[test]
    fn a_minimal_file_loads_and_keeps_the_policy_opaque() {
        let config = parse(MINIMAL).expect("the minimal fixture validates");
        assert_eq!(config.externals.timeout, Duration::from_millis(5000));
        assert_eq!(config.externals.max_body_bytes, 65536);
        assert!(config.externals.dynamic.is_none());
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
            "{MINIMAL}\n[externals.dynamic]\nurl = \"https://resolver.internal\"\ntoken_env = \"APPA_RESOLVER_TOKEN\"\n"
        );
        let config = parse_with(&text, |var| {
            (var == "APPA_RESOLVER_TOKEN").then(|| "sekret".to_string())
        })
        .expect("the fixture with a set secret validates");
        assert!(!format!("{:?}", config.externals).contains("sekret"));
        let dynamic = config.externals.dynamic.expect("the dynamic endpoint is set");
        let Implementation::Resolver(dynamic) = dynamic else {
            panic!("the URL config selects an endpoint");
        };
        let token = dynamic.token.expect("the token resolved");
        assert_eq!(token.reveal(), "sekret");
        assert_eq!(format!("{token:?}"), "Token(<redacted>)");
    }

    #[test]
    fn an_unreadable_path_is_a_named_refusal() {
        let missing = Path::new("/nonexistent/appa.toml");
        assert!(matches!(Config::load(missing), Err(ConfigError::Unreadable { .. }),));
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

        let text = format!("{MINIMAL}\n[externals.dynamic]\nbuiltin = \"claude-code\"\n");
        let config = parse(&text).expect("the Claude Code dynamic builtin validates");
        assert!(matches!(
            config.externals.dynamic,
            Some(Implementation::Builtin(name)) if name == CLAUDE_CODE_BUILTIN
        ));
        let text = format!("{MINIMAL}\n[externals.dynamic]\nbuiltin = \"approve\"\n");
        assert!(matches!(parse(&text), Err(ConfigError::InvalidBuiltinName { .. })));
        let text = format!("{MINIMAL}\n[externals.dynamic]\nbuiltin = \"claude-code\"\ntoken_env = \"APPA_TOKEN\"\n");
        assert!(matches!(parse(&text), Err(ConfigError::ImplementationChoice { .. })));
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
}
