//! Configuration: one immutable `appa.toml` with the policy and the
//! external endpoints. The harness adapter is a CLI flag on the
//! binary, not configuration.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct Config {
    /// The policy value as authored, preserved verbatim; compiled
    /// into the engine's registry at `Runtime::open`.
    pub policy: toml::Value,
    pub externals: Externals,
}

/// The external-service endpoints: authorities, sanitizers,
/// and the dynamic resolver. Every call carries an explicit timeout
/// and byte cap and fails closed.
#[derive(Debug, Clone)]
pub struct Externals {
    pub timeout: Duration,
    pub max_body_bytes: usize,
    pub authorities: BTreeMap<String, Endpoint>,
    pub sanitizers: BTreeMap<String, Endpoint>,
    pub dynamic: Option<Endpoint>,
}

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
    #[error("externals.max_body_bytes must be greater than zero")]
    ZeroByteCap,
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
    max_body_bytes: usize,
    #[serde(default)]
    authorities: BTreeMap<String, RawEndpoint>,
    #[serde(default)]
    sanitizers: BTreeMap<String, RawEndpoint>,
    dynamic: Option<RawEndpoint>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEndpoint {
    url: String,
    token_env: Option<String>,
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
        Config::validate(raw, |var| std::env::var(var).ok())
    }

    fn validate(raw: RawConfig, lookup: impl Fn(&str) -> Option<String>) -> Result<Config, ConfigError> {
        if raw.externals.timeout_ms == 0 {
            return Err(ConfigError::ZeroTimeout);
        }
        if raw.externals.max_body_bytes == 0 {
            return Err(ConfigError::ZeroByteCap);
        }
        Ok(Config {
            policy: raw.policy,
            externals: Externals {
                timeout: Duration::from_millis(raw.externals.timeout_ms),
                max_body_bytes: raw.externals.max_body_bytes,
                authorities: resolve_endpoints("authorities", raw.externals.authorities, &lookup)?,
                sanitizers: resolve_endpoints("sanitizers", raw.externals.sanitizers, &lookup)?,
                dynamic: raw
                    .externals
                    .dynamic
                    .map(|endpoint| resolve_endpoint("dynamic", "dynamic".to_string(), endpoint, &lookup))
                    .transpose()?,
            },
        })
    }
}

fn resolve_endpoints(
    section: &'static str,
    raw: BTreeMap<String, RawEndpoint>,
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<BTreeMap<String, Endpoint>, ConfigError> {
    raw.into_iter()
        .map(|(name, endpoint)| {
            let resolved = resolve_endpoint(section, name.clone(), endpoint, lookup)?;
            Ok((name, resolved))
        })
        .collect()
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
        Config::validate(raw, lookup)
    }

    #[test]
    fn a_minimal_file_loads_and_keeps_the_policy_opaque() {
        let config = parse(MINIMAL).expect("the minimal fixture validates");
        assert_eq!(config.externals.timeout, Duration::from_millis(5000));
        assert_eq!(config.externals.max_body_bytes, 65536);
        assert!(config.externals.dynamic.is_none());
        assert_eq!(
            config.policy.get("anything").and_then(|v| v.as_str()),
            Some("the runtime does not interpret this"),
        );
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
        let token = dynamic.token.expect("the token resolved");
        assert_eq!(token.reveal(), "sekret");
        assert_eq!(format!("{token:?}"), "Token(<redacted>)");
    }

    #[test]
    fn an_unreadable_path_is_a_named_refusal() {
        let missing = Path::new("/nonexistent/appa.toml");
        assert!(matches!(Config::load(missing), Err(ConfigError::Unreadable { .. }),));
    }
}
