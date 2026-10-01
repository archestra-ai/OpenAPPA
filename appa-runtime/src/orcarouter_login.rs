//! The OrcaRouter connect command: `appa login orcarouter`.
//!
//! One command, two ways in, both producing the same ordinary `sk-orca-…` key:
//!
//! - **Connect** (default): OAuth 2.0 authorization code with PKCE, out-of-band. The
//!   consent screen shows a one-time code that the user pastes back here. This is the
//!   default because `appa` runs as a sidecar on servers, containers, and CI runners
//!   where a loopback listener is often unreachable from the user's browser and the
//!   install address differs on every deployment; out-of-band needs no predictable
//!   address and no redirect registration.
//! - **API key** (`--api-key`, or prompting in a terminal): the user pastes an existing
//!   `sk-orca-…` key. No browser, no authorization.
//!
//! Either way the key is written to the runtime's own credential store
//! ([`crate::credentials::CredentialStore`]) under the profile's `token_env`, and is
//! redacted everywhere it could be printed. The verifier never leaves this process, is
//! never logged, and is never put in the authorize URL.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::credentials::CredentialStore;
use crate::orcarouter::{self, CredentialSource, Origins};

/// The application name the consent screen shows.
pub const APP_NAME: &str = "OpenAPPA";

/// The one scope a runtime key needs.
pub const SCOPE: &str = "api";

#[derive(clap::Args)]
pub struct Args {
    /// The configuration whose deployment stores the key.
    #[arg(long, env = "APPA_CONFIG")]
    config: Option<PathBuf>,

    /// The `token_env` the loaded `[externals.llm]` profile names. A connect flow stores
    /// under it; the pasted-key path reports it as the variable to export. Defaults to
    /// `APPA_ORCAROUTER_API_KEY`.
    #[arg(long)]
    token_env: Option<String>,

    /// The OrcaRouter API key to paste instead of connecting. Prefer the environment
    /// variable or the prompt: an argument lands in the shell history and the process
    /// list.
    #[arg(long, value_name = "KEY", conflicts_with = "loopback")]
    api_key: Option<String>,

    /// Use the loopback redirect flow instead of the out-of-band code. Only for a
    /// machine whose browser reaches `127.0.0.1`; the callback is never registered.
    #[arg(long)]
    loopback: bool,

    /// Print the authorize URL without opening a browser.
    #[arg(long)]
    no_open: bool,

    /// Where authentication goes. Defaults to the environment, then
    /// `https://www.orcarouter.ai`.
    #[arg(long)]
    auth_url: Option<String>,

    /// Where inference goes. Defaults to the environment, then
    /// `https://api.orcarouter.ai/v1`.
    #[arg(long)]
    api_url: Option<String>,
}

/// Why a connect attempt ended. Every variant is terminal and releases whatever the
/// attempt held; none of them retries or refreshes.
#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error("the key cannot be stored: {0}")]
    Store(String),
    #[error("the configuration cannot be read: {0}")]
    Config(String),
    #[error("the authorization was denied")]
    Denied,
    #[error("the authorization answer did not match the one this command sent (state mismatch)")]
    StateMismatch,
    #[error("the authorization code was already used, expired, or does not match the verifier")]
    CodeRejected,
    #[error("the service refused the exchange with {status}")]
    Refused { status: u16 },
    #[error("the authorization service could not be reached: {0}")]
    Transport(String),
    #[error("the authorization timed out after {0} seconds")]
    Timeout(u64),
    #[error("the granted scope {granted:?} does not include what this runtime needs ({needed:?})")]
    ScopeDowngrade { granted: String, needed: String },
    #[error("the auth origin is not usable: {0}")]
    Origin(String),
    #[error("the key was empty")]
    EmptyKey,
}

/// The outcome of one login: where the key went, and the non-secret facts worth printing.
#[derive(Debug, Clone)]
pub struct SignedIn {
    pub variable: String,
    pub source: CredentialSource,
    /// The scope the service actually granted, read back from the exchange.
    pub granted_scope: String,
}

/// The PKCE verifier and challenge for one attempt, plus the CSRF state. Fresh
/// cryptographic randomness every attempt; the verifier is never logged or placed in a URL.
pub struct Pkce {
    verifier: String,
    pub challenge: String,
    pub state: String,
}

impl Pkce {
    /// A fresh verifier, its S256 challenge, and an independent state, all from the OS
    /// cryptographic RNG. The verifier is 32 random bytes, base64url, no padding.
    pub fn fresh() -> Pkce {
        let verifier = random_token(32);
        let challenge = base64url(&Sha256::digest(verifier.as_bytes()));
        let state = random_token(16);
        Pkce {
            verifier,
            challenge,
            state,
        }
    }

    /// The verifier, only to be handed to the exchange request.
    pub fn verifier(&self) -> &str {
        &self.verifier
    }

    /// Compare an echoed state in constant time. A state that does not match means the
    /// code was not delivered by the authorization this process started.
    pub fn state_matches(&self, candidate: &str) -> bool {
        let a = self.state.as_bytes();
        let b = candidate.as_bytes();
        if a.len() != b.len() {
            return false;
        }
        let mut difference = 0u8;
        for (left, right) in a.iter().zip(b) {
            difference |= left ^ right;
        }
        difference == 0
    }
}

fn random_token(bytes: usize) -> String {
    // The OS cryptographic RNG, as elsewhere in this crate. `rand`'s thread RNG is
    // seeded from it and is the process's one entropy source.
    let raw: Vec<u8> = (0..bytes).map(|_| rand::random::<u8>()).collect();
    base64url(&raw)
}

fn base64url(raw: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
}

/// The authorize URL for one attempt. `callback_url` is the loopback callback for Flow A
/// or the literal `oob` for Flow B.
pub fn authorize_url(origins: &Origins, pkce: &Pkce, callback_url: &str) -> String {
    let mut url = reqwest::Url::parse(&origins.authorize_url()).expect("the authorize URL is absolute");
    url.query_pairs_mut()
        .append_pair("callback_url", callback_url)
        .append_pair("code_challenge", &pkce.challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &pkce.state)
        .append_pair("app_name", APP_NAME)
        .append_pair("scope", SCOPE);
    url.to_string()
}

/// One exchange result, as the service answered it.
#[derive(Debug, serde::Deserialize)]
pub struct ExchangeResponse {
    pub key: String,
    #[serde(default)]
    pub scope: Option<String>,
}

/// The key one code and verifier exchange for. Bounded by a timeout; a non-success is
/// classified and never retried, because an auth code is single-use.
pub async fn exchange(origins: &Origins, code: &str, verifier: &str) -> Result<ExchangeResponse, LoginError> {
    crate::tls::install_crypto_provider();
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|error| LoginError::Transport(error.to_string()))?;
    let body = serde_json::json!({
        "code": code,
        "code_verifier": verifier,
        "code_challenge_method": "S256",
    });
    let response = client
        .post(origins.exchange_url())
        // The exchange lives on the auth origin. Never the inference origin's `/v1`.
        .json(&body)
        .send()
        .await
        .map_err(|error| LoginError::Transport(error.to_string()))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| LoginError::Transport(error.to_string()))?;
    match status.as_u16() {
        200 => serde_json::from_slice(&bytes).map_err(|error| LoginError::Transport(error.to_string())),
        400 => Err(LoginError::CodeRejected),
        403 => Err(LoginError::CodeRejected),
        429 => Err(LoginError::Refused { status: 429 }),
        other => Err(LoginError::Refused { status: other }),
    }
}

/// Read the granted scope back and refuse a downgrade. What the response says was
/// granted is the truth; what the request asked for is not.
pub fn check_scope(granted: Option<&str>) -> Result<String, LoginError> {
    let granted = granted.unwrap_or(SCOPE).to_string();
    // `api` is what this runtime needs; a wider `connector` grant includes it.
    if granted == SCOPE || granted == "connector" {
        Ok(granted)
    } else {
        Err(LoginError::ScopeDowngrade {
            granted,
            needed: SCOPE.to_string(),
        })
    }
}

/// Store a key under `variable` in the deployment's credential store.
pub fn store_key(config: &Path, variable: &str, key: &str) -> Result<(), LoginError> {
    if key.trim().is_empty() {
        return Err(LoginError::EmptyKey);
    }
    let store = CredentialStore::for_config(config).map_err(LoginError::Store)?;
    store
        .update(&std::collections::BTreeMap::from([(
            variable.to_string(),
            Some(key.to_string()),
        )]))
        .map_err(LoginError::Store)
}

/// The `token_env` the deployment's profile names, or the default.
pub fn token_variable(config: &Path, explicit: Option<&str>) -> Result<String, LoginError> {
    if let Some(explicit) = explicit {
        return Ok(explicit.to_string());
    }
    let text = std::fs::read_to_string(config).map_err(|error| LoginError::Config(error.to_string()))?;
    Ok(profile_token_env(&text).unwrap_or_else(|| orcarouter::KEY_VARIABLE.to_string()))
}

/// The `token_env` of the loaded `[externals.llm]` table, read without resolving the
/// whole document: a connect flow needs the variable name before it has a key to store.
pub fn profile_token_env(text: &str) -> Option<String> {
    let document: toml::Value = toml::from_str(text).ok()?;
    document
        .get("externals")?
        .get("llm")?
        .get("token_env")?
        .as_str()
        .map(str::to_owned)
}

/// Resolve the two origins for a run, from the environment or explicit overrides.
pub fn origins(args: &Args) -> Result<Origins, LoginError> {
    if args.auth_url.is_none() && args.api_url.is_none() {
        // The environment supplies both; the profile URL is the caller's own.
        return Origins::resolve(None).map_err(|error| LoginError::Origin(error.to_string()));
    }
    // Explicit overrides win over the environment, as the shared base does.
    let origins = Origins::resolve(None).map_err(|error| LoginError::Origin(error.to_string()))?;
    let auth = args
        .auth_url
        .as_deref()
        .map(orcarouter::Origin::parse)
        .transpose()
        .map_err(|error| LoginError::Origin(error.to_string()))?
        .unwrap_or(origins.auth);
    let api = args
        .api_url
        .as_deref()
        .map(orcarouter::Origin::parse)
        .transpose()
        .map_err(|error| LoginError::Origin(error.to_string()))?
        .unwrap_or(origins.api);
    Ok(Origins { auth, api })
}

/// Run one login. `code_reader` supplies the out-of-band code for Flow B (stdin in the
/// real command, a scripted answer in a test); `browser_opener` opens the authorize URL.
pub async fn run_with(
    args: &Args,
    origins: &Origins,
    code_reader: impl FnOnce(&str) -> Result<String, LoginError>,
    browser_opener: impl FnOnce(&str),
) -> Result<SignedIn, LoginError> {
    let variable = token_variable(&config_path(args), args.token_env.as_deref())?;
    let config = config_path(args);

    // The API-key path is independent of the connect flow and starts no authorization.
    if let Some(key) = args.api_key.as_deref() {
        return store_pasted(&config, &variable, key);
    }

    let pkce = Pkce::fresh();
    if args.loopback {
        return connect_loopback(args, origins, &config, &variable, pkce, browser_opener, code_reader).await;
    }
    let url = authorize_url(origins, &pkce, "oob");
    // The verifier is never in this line, and neither is the code.
    eprintln!("Open this URL, approve access, then paste the code it shows:\n{url}");
    if !args.no_open {
        browser_opener(&url);
    }
    let code = code_reader(&url)?;
    let code = code.trim();
    let response = exchange(origins, code, pkce.verifier()).await?;
    let scope = check_scope(response.scope.as_deref())?;
    store_key(&config, &variable, &response.key)?;
    Ok(SignedIn {
        variable,
        source: CredentialSource::Connect,
        granted_scope: scope,
    })
}

async fn connect_loopback(
    args: &Args,
    origins: &Origins,
    config: &Path,
    variable: &str,
    pkce: Pkce,
    browser_opener: impl FnOnce(&str),
    _code_reader: impl FnOnce(&str) -> Result<String, LoginError>,
) -> Result<SignedIn, LoginError> {
    // Listen first, so the port is known before the browser opens.
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|error| LoginError::Transport(error.to_string()))?;
    let port = listener
        .local_addr()
        .map_err(|error| LoginError::Transport(error.to_string()))?
        .port();
    let callback = format!("http://127.0.0.1:{port}/cb");
    let url = authorize_url(origins, &pkce, &callback);
    eprintln!("Open this URL and approve access:\n{url}");
    if !args.no_open {
        browser_opener(&url);
    }
    let code =
        crate::orcarouter_login::loopback::await_code(listener, &pkce, std::time::Duration::from_secs(300)).await?;
    let response = exchange(origins, &code, pkce.verifier()).await?;
    let scope = check_scope(response.scope.as_deref())?;
    store_key(config, variable, &response.key)?;
    Ok(SignedIn {
        variable: variable.to_string(),
        source: CredentialSource::Connect,
        granted_scope: scope,
    })
}

fn store_pasted(config: &Path, variable: &str, key: &str) -> Result<SignedIn, LoginError> {
    store_key(config, variable, key)?;
    Ok(SignedIn {
        variable: variable.to_string(),
        source: CredentialSource::Database,
        granted_scope: SCOPE.to_string(),
    })
}

fn config_path(args: &Args) -> PathBuf {
    args.config.clone().unwrap_or_else(crate::init::installed_config_path)
}

/// Run the command as the CLI entry point runs it.
pub fn run(args: Args) -> std::process::ExitCode {
    let origins = match origins(&args) {
        Ok(origins) => origins,
        Err(error) => {
            eprintln!("appa login: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("appa login: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(run_with(
        &args,
        &origins,
        |_url| {
            eprint!("Code: ");
            let mut line = String::new();
            std::io::stdin()
                .read_line(&mut line)
                .map_err(|error| LoginError::Transport(error.to_string()))?;
            Ok(line)
        },
        crate::ui::open_browser,
    ));
    match result {
        Ok(signed_in) => {
            println!(
                "Connected. The key is stored as {} ({}). Scope: {}.",
                signed_in.variable,
                match signed_in.source {
                    CredentialSource::Connect => "connected account",
                    _ => "pasted key",
                },
                signed_in.granted_scope
            );
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("appa login: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// The loopback listener Flow A returns to. The state is compared before anything else.
pub mod loopback {
    use super::{LoginError, Pkce};
    use std::time::Duration;

    /// Wait for the one callback, serving a short page, and return the code. A denial or
    /// a mismatched state ends the wait with the matching error; the timeout releases
    /// the listener.
    pub async fn await_code(
        listener: tokio::net::TcpListener,
        pkce: &Pkce,
        timeout: Duration,
    ) -> Result<String, LoginError> {
        let accept = async {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return Err(LoginError::Transport("the callback listener closed".into()));
                };
                let mut buffer = [0u8; 4096];
                let read = tokio::io::AsyncReadExt::read(&mut stream, &mut buffer)
                    .await
                    .unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]);
                let target = request.split_whitespace().nth(1).unwrap_or("/").to_string();
                let url = reqwest::Url::parse(&format!("http://127.0.0.1{target}"));
                let Ok(url) = url else { continue };
                if url.path() != "/cb" {
                    let _ = respond(&mut stream, 404, "Not found").await;
                    continue;
                }
                let _ = respond(&mut stream, 200, "Connected. You can close this tab.").await;
                let state = url
                    .query_pairs()
                    .find(|(k, _)| k == "state")
                    .map(|(_, v)| v.into_owned());
                // Compare the state before reading anything else out of the callback.
                match state.as_deref() {
                    Some(state) if pkce.state_matches(state) => {}
                    _ => return Err(LoginError::StateMismatch),
                }
                if let Some((_, error)) = url.query_pairs().find(|(k, _)| k == "error") {
                    return Err(match error.as_ref() {
                        "access_denied" => LoginError::Denied,
                        _ => LoginError::StateMismatch,
                    });
                }
                match url.query_pairs().find(|(k, _)| k == "code") {
                    Some((_, code)) => return Ok(code.into_owned()),
                    None => return Err(LoginError::CodeRejected),
                }
            }
        };
        tokio::time::timeout(timeout, accept)
            .await
            .unwrap_or(Err(LoginError::Timeout(timeout.as_secs())))
    }

    async fn respond(stream: &mut tokio::net::TcpStream, status: u16, body: &str) -> std::io::Result<()> {
        use tokio::io::AsyncWriteExt;
        let page = format!(
            "HTTP/1.1 {status} OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(page.as_bytes()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn cli_args(config: PathBuf) -> Args {
        Args {
            config: Some(config),
            token_env: None,
            api_key: None,
            loopback: false,
            no_open: true,
            auth_url: None,
            api_url: None,
        }
    }

    fn fixture(dir: &Path) {
        std::fs::write(
            dir.join("appa.toml"),
            "[policy]\nversion = 2\n[externals.llm]\nprovider = \"orcarouter\"\nmodel = \"orcarouter/auto\"\ntoken_env = \"APPA_ORCAROUTER_API_KEY\"\n",
        )
        .unwrap();
    }

    #[test]
    fn the_token_env_comes_from_the_profile_and_defaults_only_when_absent() {
        let text = "[externals.llm]\nprovider = \"orcarouter\"\nmodel = \"m\"\n";
        assert_eq!(profile_token_env(text), None);
        assert_eq!(
            profile_token_env("[externals.llm]\ntoken_env = \"APPA_ORCAROUTER_API_KEY\"\n").as_deref(),
            Some("APPA_ORCAROUTER_API_KEY")
        );
    }

    #[test]
    fn each_attempt_gets_a_fresh_verifier_and_state_and_sends_only_the_challenge() {
        let first = Pkce::fresh();
        let second = Pkce::fresh();
        assert_ne!(first.verifier(), second.verifier());
        assert_ne!(first.state, second.state);
        assert_ne!(first.challenge, second.challenge);
        // The challenge is base64url(sha256(verifier)) with no padding.
        let expected = base64url(&Sha256::digest(first.verifier().as_bytes()));
        assert_eq!(first.challenge, expected);
        assert!(!first.challenge.contains('='));
        assert!(!first.verifier().contains('='));
    }

    #[test]
    fn a_mismatched_state_is_refused() {
        let pkce = Pkce::fresh();
        assert!(pkce.state_matches(&pkce.state));
        assert!(!pkce.state_matches("somebody-elses-state"));
        assert!(!pkce.state_matches(""));
    }

    #[test]
    fn the_authorize_url_sends_s256_the_state_and_the_callback_and_never_the_verifier() {
        let origins = Origins::resolve(None).unwrap();
        let pkce = Pkce::fresh();
        let url = authorize_url(&origins, &pkce, "oob");
        assert!(url.starts_with("https://www.orcarouter.ai/auth?"));
        assert!(url.contains("callback_url=oob"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains(&format!("state={}", pkce.state)));
        assert!(url.contains(&format!("code_challenge={}", pkce.challenge)));
        assert!(!url.contains(pkce.verifier()), "the verifier must never be in the URL");
        assert!(!url.contains("api.orcarouter.ai"));
    }

    /// A fake authorization service: the authorize origin and the exchange origin are the
    /// same loopback host, so a test can watch exactly which path is called and with what.
    async fn fake_auth() -> (Arc<FakeAuth>, Origins) {
        use axum::routing::post;
        let fake = Arc::new(FakeAuth::default());
        let router = axum::Router::new()
            .route(
                "/api/v1/auth/keys",
                post(
                    |axum::extract::State(fake): axum::extract::State<Arc<FakeAuth>>,
                     axum::Json(body): axum::Json<serde_json::Value>| async move {
                        *fake.exchange_body.lock().unwrap() = Some(body);
                        fake.exchange_answer.lock().unwrap().clone()
                    },
                ),
            )
            .route("/auth", axum::routing::get(|| async { "consent" }))
            .with_state(fake.clone());
        let addr = crate::test_support::serve(router).await;
        let origins = Origins {
            auth: orcarouter::Origin::parse(&format!("http://{addr}")).unwrap(),
            api: orcarouter::Origin::parse(&format!("http://{addr}/v1")).unwrap(),
        };
        (fake, origins)
    }

    #[derive(Default)]
    struct FakeAuth {
        exchange_body: std::sync::Mutex<Option<serde_json::Value>>,
        exchange_answer: std::sync::Mutex<(axum::http::StatusCode, String)>,
    }

    impl FakeAuth {
        fn answering(&self, status: u16, body: serde_json::Value) {
            *self.exchange_answer.lock().unwrap() =
                (axum::http::StatusCode::from_u16(status).unwrap(), body.to_string());
        }
    }

    #[tokio::test]
    async fn a_connect_writes_the_same_key_a_pasted_one_does() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let (fake, origins) = fake_auth().await;
        fake.answering(
            200,
            serde_json::json!({"key": "sk-orca-connectFixture", "user_id": "1", "scope": "api"}),
        );
        let args = cli_args(dir.path().join("appa.toml"));
        let signed = run_with(&args, &origins, |_| Ok("the-code".to_string()), |_| {})
            .await
            .expect("the connect succeeds");
        assert_eq!(signed.variable, "APPA_ORCAROUTER_API_KEY");
        assert_eq!(signed.source, CredentialSource::Connect);
        assert_eq!(signed.granted_scope, "api");

        let body = fake.exchange_body.lock().unwrap().clone().expect("the exchange posted");
        assert_eq!(body["code_challenge_method"], "S256");
        assert_eq!(body["code"], "the-code");
        assert!(body.get("code_verifier").is_some());
        assert!(body.get("client_secret").is_none(), "PKCE needs no client secret");

        // The key landed in the store under the profile's variable.
        let store = CredentialStore::for_config(&dir.path().join("appa.toml")).unwrap();
        assert_eq!(
            store
                .values()
                .unwrap()
                .get("APPA_ORCAROUTER_API_KEY")
                .map(String::as_str),
            Some("sk-orca-connectFixture")
        );
        // A pasted key writes the same shape.
        let pasted = Args {
            api_key: Some("sk-orca-pastedFixture".to_string()),
            ..cli_args(dir.path().join("appa.toml"))
        };
        let signed = run_with(&pasted, &origins, |_| unreachable!(), |_| {})
            .await
            .expect("the pasted key is stored");
        assert_eq!(signed.source, CredentialSource::Database);
        assert_eq!(
            store
                .values()
                .unwrap()
                .get("APPA_ORCAROUTER_API_KEY")
                .map(String::as_str),
            Some("sk-orca-pastedFixture")
        );
    }

    #[tokio::test]
    async fn a_denied_or_expired_code_ends_the_attempt_and_stores_nothing() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let (fake, origins) = fake_auth().await;
        fake.answering(403, serde_json::json!({"error": "invalid_grant"}));
        let args = cli_args(dir.path().join("appa.toml"));
        let error = run_with(&args, &origins, |_| Ok("used-code".to_string()), |_| {})
            .await
            .expect_err("a 403 ends the attempt");
        assert!(matches!(error, LoginError::CodeRejected), "{error:?}");
        #[cfg(feature = "daemon")]
        assert!(
            !format!("{error:?}{error}").contains("used-code"),
            "an error never echoes the code"
        );
        let store = CredentialStore::for_config(&dir.path().join("appa.toml")).unwrap();
        assert!(store.values().unwrap().is_empty(), "a failed exchange stores no key");
    }

    #[tokio::test]
    async fn a_granted_scope_that_does_not_fit_is_refused_rather_than_assumed() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let (fake, origins) = fake_auth().await;
        fake.answering(
            200,
            serde_json::json!({"key": "sk-orca-x", "user_id": "1", "scope": "readonly"}),
        );
        let args = cli_args(dir.path().join("appa.toml"));
        let error = run_with(&args, &origins, |_| Ok("code".to_string()), |_| {})
            .await
            .expect_err("a downgraded scope is refused");
        assert!(matches!(error, LoginError::ScopeDowngrade { .. }), "{error:?}");
        let store = CredentialStore::for_config(&dir.path().join("appa.toml")).unwrap();
        assert!(store.values().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_transport_failure_is_reported_and_never_retried() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = closed.local_addr().unwrap();
        drop(closed);
        let origins = Origins {
            auth: orcarouter::Origin::parse(&format!("http://{addr}")).unwrap(),
            api: orcarouter::Origin::parse(&format!("http://{addr}/v1")).unwrap(),
        };
        let args = cli_args(dir.path().join("appa.toml"));
        let error = run_with(&args, &origins, |_| Ok("code".to_string()), |_| {})
            .await
            .expect_err("a dead origin is a transport error");
        assert!(matches!(error, LoginError::Transport(_)), "{error:?}");
    }
}
