//! OrcaRouter as a first-class provider: its two origins, its key variable, the model
//! catalog it serves, and the credential seam both connect paths write to.
//!
//! [OrcaRouter](https://www.orcarouter.ai) is an OpenAI-compatible AI gateway that
//! routes many providers behind one endpoint. Inference and model discovery use
//! `https://api.orcarouter.ai/v1`; authentication and code exchange use
//! `https://www.orcarouter.ai`. The two are different origins and neither is derived
//! from the other: `https://api.orcarouter.ai/v1/auth/keys` is a 404, which is the
//! mistake this module exists to prevent.
//!
//! Two credential paths produce the same ordinary `sk-orca-…` key. A pasted key is a
//! `token_env` like any other provider's; a PKCE connect flow (helper process
//! `appa-orca-connect`) exchanges a browser authorization for one and writes it to the
//! runtime's own credential store. The inference path reads through [`Credential`] and
//! never learns which path filled it.

use serde::{Deserialize, Serialize};

pub use crate::orcarouter_catalog::{
    Capability, Catalog, CatalogError, CatalogItem, CatalogProvider, CatalogSource, Modality, SeedModel, VerifiedSeed,
};

/// The variable an OrcaRouter key is stored under, whether it arrived pasted or through a
/// connect flow. It is the profile's `token_env` in every shipped configuration, and the
/// default a connect flow writes when the loaded profile names none.
///
/// It is a runtime variable (`APPA_`), not a battery credential (`APPA_PROVIDER_`):
/// the runtime reads this key and sends it itself, which is exactly what the
/// `APPA_PROVIDER_` namespace forbids.
pub const KEY_VARIABLE: &str = "APPA_ORCAROUTER_API_KEY";

/// Where inference and model discovery reach OrcaRouter. The `/v1` prefix is the OpenAI
/// wire surface.
pub const DEFAULT_API_BASE: &str = "https://api.orcarouter.ai/v1";

/// Where authentication and code exchange reach OrcaRouter. Never `/api/orcarouter.ai/v1`.
pub const DEFAULT_AUTH_BASE: &str = "https://www.orcarouter.ai";

/// The one authorize path, fixed by the consent service.
pub const AUTHORIZE_PATH: &str = "/auth";

/// The one exchange path, fixed by the consent service. Deliberately not `/v1/auth/keys`,
/// which is the inference origin's spelling and a 404 there.
pub const EXCHANGE_PATH: &str = "/api/v1/auth/keys";

/// The shared self-hosted base, when a deployment runs both origins on one host.
pub const BASE_URL_VARIABLE: &str = "APPA_ORCAROUTER_BASE_URL";
/// The explicit authentication origin. It takes precedence over [`BASE_URL_VARIABLE`].
pub const AUTH_URL_VARIABLE: &str = "APPA_ORCAROUTER_AUTH_URL";
/// The explicit inference origin. A profile `url` takes precedence over it, which takes
/// precedence over [`BASE_URL_VARIABLE`].
pub const API_URL_VARIABLE: &str = "APPA_ORCAROUTER_API_URL";

/// How much of a catalog response the runtime will read, and how many items it will keep.
pub const MAX_CATALOG_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_CATALOG_ITEMS: usize = 4096;
/// The catalog request's own budget, independent of a consult's.
pub const CATALOG_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// One origin and the path prefix a request to it hangs under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    base: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OriginError {
    #[error("{value:?} is not a URL")]
    Unparsable { value: String },
    #[error("{value:?} uses {scheme}, and only https or loopback http is allowed")]
    Cleartext { value: String, scheme: String },
    #[error("{value:?} carries credentials in the URL")]
    Credentials { value: String },
}

impl Origin {
    /// One origin from an explicit base URL. HTTPS is required everywhere except
    /// loopback: the shared `http://127.0.0.1:8080` self-hosted deployment is the one
    /// cleartext case, and it must not become a way to send a key in the open.
    pub fn parse(value: &str) -> Result<Origin, OriginError> {
        let parsed = reqwest::Url::parse(value).map_err(|_| OriginError::Unparsable {
            value: value.to_string(),
        })?;
        match parsed.scheme() {
            "https" => {}
            "http" if is_loopback(&parsed) => {}
            scheme => {
                return Err(OriginError::Cleartext {
                    value: value.to_string(),
                    scheme: scheme.to_string(),
                });
            }
        }
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(OriginError::Credentials {
                value: value.to_string(),
            });
        }
        Ok(Origin {
            base: value.trim_end_matches('/').to_string(),
        })
    }

    pub fn as_str(&self) -> &str {
        &self.base
    }

    /// This origin with `path` appended, an absolute path or a bare one.
    pub fn join(&self, path: &str) -> String {
        match path.starts_with('/') {
            true => format!("{}{path}", self.base),
            false => format!("{}/{path}", self.base),
        }
    }
}

fn is_loopback(url: &reqwest::Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(domain)) => domain == "localhost",
        None => false,
    }
}

/// The two origins one deployment reaches, resolved from its environment. Explicit
/// overrides win, then the shared self-hosted base, then the public defaults. The
/// environment is read first and the deployment's own configuration second, so a
/// profile `url` stays the strongest statement of where inference goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origins {
    pub auth: Origin,
    pub api: Origin,
}

impl Origins {
    /// The origins this process's environment names, over an optional profile URL.
    pub fn resolve(profile_url: Option<&str>) -> Result<Origins, OriginError> {
        let shared = nonempty(BASE_URL_VARIABLE);
        let auth = nonempty(AUTH_URL_VARIABLE);
        let api = nonempty(API_URL_VARIABLE);
        Origins::from_overrides(shared.as_deref(), auth.as_deref(), api.as_deref(), profile_url)
    }

    /// The precedence, as a pure function so it is testable without touching the process
    /// environment: an explicit auth override wins for authentication, and for inference
    /// a profile `url` wins over an explicit API override, which wins over the shared
    /// self-hosted base, which wins over the public default. Neither origin is ever
    /// derived from the other.
    pub fn from_overrides(
        shared: Option<&str>,
        auth: Option<&str>,
        api: Option<&str>,
        profile_url: Option<&str>,
    ) -> Result<Origins, OriginError> {
        let nonempty = |value: Option<&str>| value.filter(|value| !value.is_empty()).map(str::to_owned);
        let auth = nonempty(auth)
            .or_else(|| nonempty(shared))
            .unwrap_or_else(|| DEFAULT_AUTH_BASE.to_string());
        let api = nonempty(profile_url)
            .or_else(|| nonempty(api))
            .or_else(|| nonempty(shared))
            .unwrap_or_else(|| DEFAULT_API_BASE.to_string());
        Ok(Origins {
            auth: Origin::parse(&auth)?,
            api: Origin::parse(&api)?,
        })
    }

    /// The authorize URL a connect flow opens, with no query.
    pub fn authorize_url(&self) -> String {
        self.auth.join(AUTHORIZE_PATH)
    }

    /// The exchange URL a connect flow posts the code and verifier to.
    pub fn exchange_url(&self) -> String {
        self.auth.join(EXCHANGE_PATH)
    }

    /// `GET /models` on the inference origin: the model catalog.
    pub fn models_url(&self) -> String {
        self.api.join("models")
    }

    /// A chat-completions URL on the inference origin, for a host that speaks the wire
    /// itself rather than through the `llm` builtin.
    pub fn chat_completions_url(&self) -> String {
        self.api.join("chat/completions")
    }
}

fn nonempty(variable: &str) -> Option<String> {
    std::env::var(variable).ok().filter(|value| !value.is_empty())
}

/// How the runtime came to hold an OrcaRouter credential. Recorded so status and error
/// text can say which path is in play, and never carried to the inference request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialSource {
    /// A key the deployment or the user pasted, from the process environment.
    Environment,
    /// A key the user pasted into `appa ui`, held in the credential store.
    Database,
    /// A key a PKCE connect flow exchanged, held in the credential store.
    Connect,
}

/// The seam both credential paths fill: one place that answers "what key serves this
/// deployment, and is it still good". A pasted key and a PKCE login are adapters over
/// it; the provider, the catalog, and every entry point read through this and never
/// touch either path directly.
///
/// Deliberately without a derived `Debug`: it owns the key, and a derived form would
/// print it. [`Debug`] here prints the non-secret fingerprint instead.
#[derive(Clone)]
pub struct Credential {
    /// The key itself. Never logged, never serialized, never in an error.
    key: String,
    pub source: CredentialSource,
    /// Which stored credential this is. A re-login increments it, so a late `401` from
    /// the previous generation cannot mark the new one broken.
    pub generation: u64,
}

impl Credential {
    pub fn connect(key: String, generation: u64) -> Credential {
        Credential {
            key,
            source: CredentialSource::Connect,
            generation,
        }
    }

    pub fn pasted(key: String, source: CredentialSource) -> Credential {
        Credential {
            key,
            source,
            generation: 1,
        }
    }

    /// The key to put in an `Authorization: Bearer` header. Kept behind a call so a
    /// `Debug` or a log line can never reach it by accident.
    pub fn bearer(&self) -> &str {
        &self.key
    }

    /// A short non-secret fingerprint, safe for a status line or a log: the key's
    /// variable and generation, never any part of the key itself.
    pub fn fingerprint(&self) -> String {
        format!(
            "{} (generation {})",
            match self.source {
                CredentialSource::Environment => "pasted key from the environment",
                CredentialSource::Database => "pasted key from the credential store",
                CredentialSource::Connect => "connected account",
            },
            self.generation
        )
    }

    /// The credential after the runtime observed `status` on a request it served. Only
    /// a `401`/`403` is terminal: the key is refused, so reauthentication is required,
    /// and nothing is refreshed — OrcaRouter issues no refresh grant.
    pub fn observe(&self, status: u16) -> CredentialHealth {
        match status {
            401 | 403 => CredentialHealth::ReauthenticationRequired,
            _ => CredentialHealth::Usable,
        }
    }
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Credential({})", self.fingerprint())
    }
}

/// What the runtime knows about one credential's usability after a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialHealth {
    Usable,
    /// The credential was refused. The account must reconnect; the runtime does not
    /// retry, refresh, or delete the stored key before a new login succeeds.
    ReauthenticationRequired,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_origins_never_derive_from_each_other() {
        let origins = Origins {
            auth: Origin::parse(DEFAULT_AUTH_BASE).unwrap(),
            api: Origin::parse(DEFAULT_API_BASE).unwrap(),
        };
        assert_eq!(origins.authorize_url(), "https://www.orcarouter.ai/auth");
        assert_eq!(
            origins.exchange_url(),
            "https://www.orcarouter.ai/api/v1/auth/keys",
            "the exchange lives on the auth origin, never under the relay's /v1"
        );
        assert_eq!(origins.models_url(), "https://api.orcarouter.ai/v1/models");
        assert_eq!(
            origins.chat_completions_url(),
            "https://api.orcarouter.ai/v1/chat/completions"
        );
        assert!(!origins.exchange_url().contains("api.orcarouter.ai"));
        assert!(
            !origins.models_url().starts_with("https://www.orcarouter.ai"),
            "inference never follows the auth origin"
        );
    }

    #[test]
    fn the_explicit_overrides_beat_the_shared_base_and_the_defaults() {
        let defaults = Origins::from_overrides(None, None, None, None).unwrap();
        assert_eq!(defaults.auth.as_str(), DEFAULT_AUTH_BASE);
        assert_eq!(defaults.api.as_str(), DEFAULT_API_BASE);

        // The shared self-hosted base moves both origins to one host.
        let shared = Origins::from_overrides(Some("https://appa.internal"), None, None, None).unwrap();
        assert_eq!(shared.auth.as_str(), "https://appa.internal");
        assert_eq!(shared.api.as_str(), "https://appa.internal");
        assert_eq!(shared.exchange_url(), "https://appa.internal/api/v1/auth/keys");
        assert_eq!(shared.models_url(), "https://appa.internal/models");

        // Explicit overrides beat the shared base, and a profile url beats the API override.
        let split = Origins::from_overrides(
            Some("https://shared.internal"),
            Some("https://auth.internal"),
            Some("https://api.internal/v1"),
            None,
        )
        .unwrap();
        assert_eq!(split.auth.as_str(), "https://auth.internal");
        assert_eq!(split.api.as_str(), "https://api.internal/v1");
        let profiled = Origins::from_overrides(
            None,
            None,
            Some("https://ignored.internal/v1"),
            Some("https://profile.internal/v1"),
        )
        .unwrap();
        assert_eq!(profiled.api.as_str(), "https://profile.internal/v1");
        assert_eq!(
            profiled.auth.as_str(),
            DEFAULT_AUTH_BASE,
            "a profile url never moves authentication"
        );
    }

    #[test]
    fn a_cleartext_origin_is_refused_unless_it_is_loopback() {
        assert!(Origin::parse("http://127.0.0.1:8080").is_ok());
        assert!(Origin::parse("http://localhost:8080").is_ok());
        assert!(matches!(
            Origin::parse("http://api.example.com"),
            Err(OriginError::Cleartext { .. })
        ));
        assert!(matches!(
            Origin::parse("http://user:pass@127.0.0.1:1"),
            Err(OriginError::Credentials { .. })
        ));
    }

    #[test]
    fn a_fingerprint_names_the_source_and_generation_and_never_the_key() {
        let credential = Credential::connect("sk-orca-abcdef0123456789".to_string(), 3);
        let fingerprint = credential.fingerprint();
        assert!(fingerprint.contains("generation 3"));
        assert!(!fingerprint.contains("sk-orca-"));
        assert!(!format!("{credential:?}").contains("sk-orca-abcdef"), "{credential:?}");
    }

    #[test]
    fn only_a_refusal_is_terminal_and_never_a_refresh() {
        let credential = Credential::connect("sk-orca-1".to_string(), 1);
        assert_eq!(credential.observe(200), CredentialHealth::Usable);
        assert_eq!(credential.observe(429), CredentialHealth::Usable);
        assert_eq!(credential.observe(500), CredentialHealth::Usable);
        assert_eq!(credential.observe(401), CredentialHealth::ReauthenticationRequired);
        assert_eq!(credential.observe(403), CredentialHealth::ReauthenticationRequired);
    }
}
