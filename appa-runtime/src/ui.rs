//! The token page, served by `appa ui` itself, never by the runtime. It asks for one
//! battery's token and exits once that battery is ready. It works while the runtime is
//! stopped; when a runtime serves this configuration, a save reloads it.
mod readiness;

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::credentials::CredentialStore;
use crate::loopback_http::{Deadline, Endpoint};

#[derive(clap::Args, Clone)]
pub struct Args {
    #[arg(long, env = "APPA_CONFIG")]
    config: Option<PathBuf>,
    #[arg(skip)]
    batteries_dir: Vec<PathBuf>,
    /// The one battery whose token the page asks for.
    #[arg(long, required = true)]
    battery: Vec<String>,
    #[arg(long, env = "APPA_RUNTIME_URL", default_value = crate::runtime_url::DEFAULT_RUNTIME_URL)]
    runtime_url: String,
    /// Print the browser URL without opening the browser.
    #[arg(long)]
    no_open: bool,
    /// Loopback port for the page; a free port when absent.
    #[arg(long)]
    port: Option<u16>,
}

#[derive(clap::Args)]
pub struct StatusArgs {
    #[arg(long, env = "APPA_CONFIG")]
    config: Option<PathBuf>,
    #[arg(long, env = "APPA_BATTERIES_DIR", value_delimiter = ':')]
    batteries_dir: Vec<PathBuf>,
    #[arg(long, value_delimiter = ',')]
    battery: Vec<String>,
    #[arg(long)]
    json: bool,
    /// Run bounded read-only provider checks for all selected batteries.
    #[arg(long)]
    check: bool,
}

struct Local {
    config: PathBuf,
    dirs: Vec<PathBuf>,
    selected: BTreeSet<String>,
    store: CredentialStore,
    runtime_url: String,
    checks: Mutex<BTreeMap<String, readiness::CheckResult>>,
    gate: tokio::sync::Mutex<()>,
}
#[derive(Clone)]
struct BatteryEntry {
    name: String,
    dir: PathBuf,
    package: appa_package::Package,
}
impl BatteryEntry {
    fn battery(&self) -> Option<&appa_package::Battery> {
        match &self.package.role {
            appa_package::Role::Battery(battery) => Some(battery),
            _ => None,
        }
    }
}
impl Local {
    fn new(args: &Args) -> Result<Self, String> {
        let config = args.config.clone().unwrap_or_else(crate::init::installed_config_path);
        let config = std::path::absolute(config).map_err(|_| "Cannot resolve configuration path")?;
        // Setup is also usable before a root policy exists.
        let parent = config.parent().ok_or("Configuration has no parent directory")?;
        std::fs::create_dir_all(parent).map_err(|_| "Cannot create configuration directory")?;
        let dirs = if args.batteries_dir.is_empty() {
            crate::batteries::default_search_path(&config)
        } else {
            args.batteries_dir.clone()
        };
        let store = CredentialStore::for_config(&config)?;
        Ok(Self {
            config,
            dirs,
            selected: args.battery.iter().cloned().collect(),
            store,
            runtime_url: args.runtime_url.clone(),
            checks: Mutex::new(BTreeMap::new()),
            gate: tokio::sync::Mutex::new(()),
        })
    }
    fn catalog(&self) -> (Vec<BatteryEntry>, Vec<String>) {
        let catalog = crate::batteries::snapshot(&self.dirs);
        let mut entries = Vec::new();
        let mut errors = Vec::new();
        for battery in catalog.batteries {
            let Some(dir) = self
                .dirs
                .iter()
                .map(|root| root.join(&battery.name))
                .find(|path| path.join("appa.toml").is_file())
            else {
                continue;
            };
            // Name the cause and the one command that restores the store.
            let fix = "Run `appa plugin install claude-code` to restore the installed batteries.";
            match appa_package::validate_package(&dir) {
                Ok(package)
                    if package.name.as_str() == battery.name
                        && matches!(&package.role, appa_package::Role::Battery(_)) =>
                {
                    entries.push(BatteryEntry {
                        name: battery.name,
                        dir,
                        package,
                    });
                }
                Ok(_) => errors.push(format!(
                    "{}: the installed manifest does not describe this battery. {fix}",
                    battery.name
                )),
                Err(error) => errors.push(format!(
                    "{}: APPA cannot read the installed battery: {error}. {fix}",
                    battery.name
                )),
            }
        }
        for name in &self.selected {
            if !entries.iter().any(|entry| &entry.name == name) {
                errors.push(format!(
                    "{name}: requested battery is not available in this installation"
                ));
            }
        }
        (entries, errors)
    }
    fn configured(&self) -> BTreeSet<String> {
        std::fs::read_to_string(&self.config)
            .ok()
            .and_then(|text| toml::from_str::<toml::Value>(&text).ok())
            .and_then(|doc| doc.get("include").and_then(toml::Value::as_array).cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(toml::Value::as_str)
            .filter_map(|name| crate::batteries::name_from_include(Path::new(name)))
            .collect()
    }
    fn selected(&self, name: &str) -> bool {
        self.selected.contains(name) || self.configured().contains(name)
    }

    /// The `token_env` a connect flow stores an OrcaRouter key under: the profile's own,
    /// or the default. A login writes the one variable a reload will read.
    fn orcarouter_variable(&self) -> String {
        std::fs::read_to_string(&self.config)
            .ok()
            .and_then(|text| crate::orcarouter_login::profile_token_env(&text))
            .unwrap_or_else(|| crate::orcarouter::KEY_VARIABLE.to_string())
    }

    /// What the page shows about OrcaRouter: whether a profile names it, whether a key is
    /// already stored (never the key), and where the console lists and revokes keys.
    fn orcarouter_status(&self) -> Value {
        let configured = std::fs::read_to_string(&self.config)
            .ok()
            .and_then(|text| toml::from_str::<toml::Value>(&text).ok())
            .and_then(|document| {
                document
                    .get("externals")?
                    .get("llm")?
                    .get("provider")?
                    .as_str()
                    .map(|provider| provider == "orcarouter")
            })
            .unwrap_or(false);
        let variable = self.orcarouter_variable();
        let saved = self.store.values().unwrap_or_default().contains_key(&variable);
        let from_env = std::env::var_os(&variable).is_some_and(|value| !value.is_empty());
        json!({
            "configured": configured,
            "variable": variable,
            "stored": saved || from_env,
            "source": if from_env { "environment" } else if saved { "database" } else { "missing" },
            "key_url": "https://www.orcarouter.ai/console/api-keys",
            "authorized_apps_url": "https://www.orcarouter.ai/console/authorized-apps",
        })
    }

    async fn runtime(&self) -> Option<Value> {
        let url = self.runtime_url.clone();
        let data = tokio::task::spawn_blocking(move || {
            let endpoint = Endpoint::parse(&url).ok()?;
            let answer =
                crate::loopback_http::get(&endpoint, "/prerequisites", &Deadline::spanning(Duration::from_secs(2)))
                    .ok()?;
            if !answer.is_success() {
                return None;
            }
            serde_json::from_slice::<Value>(&answer.body).ok()
        })
        .await
        .ok()
        .flatten()?;
        let path = data.get("config")?.as_str()?;
        if !same_config(Path::new(path), &self.config) {
            return None;
        }
        Some(data)
    }

    async fn snapshot(&self) -> Result<Value, String> {
        let (entries, mut errors) = self.catalog();
        let saved = self.store.values()?;
        let configured = self.configured();
        let runtime_info = self.runtime().await;
        let active: BTreeSet<String> = runtime_info
            .as_ref()
            .and_then(|r| r["included"].as_array())
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        let batteries: Vec<_> = {
            let checks = self.checks.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            entries.iter().map(|entry| {
            let battery = entry.battery().expect("catalog contains batteries");
            let credentials: Vec<_> = battery.credentials.iter().map(|var| {
                let env = std::env::var_os(var);
                if let Some(status) = runtime_info.as_ref().and_then(|r| r["prerequisites"]["credentials"].get(var)) {
                    let mut status = status.clone(); status["variable"] = json!(var);
                    status["saved"] = json!(saved.contains_key(var));
                    return status;
                }
                json!({"variable": var, "source": if env.is_some() { "environment" } else if saved.contains_key(var) { "database" } else { "missing" },
                    "available": env.as_ref().is_some_and(|v| !v.is_empty()) || (env.is_none() && saved.contains_key(var)),
                    "saved": saved.contains_key(var)})
            }).collect();
            let dependencies: Vec<_> = battery.readiness.as_ref().into_iter().flat_map(|r| &r.required_executables)
                .map(|exe| json!({"executable": exe, "installed": runtime_info.as_ref().and_then(|r| r["prerequisites"]["executables"][exe].as_bool()).unwrap_or_else(|| readiness::executable_available(exe))})).collect();
            let alternatives: Vec<_> = battery.readiness.as_ref().into_iter().flat_map(|r| &r.cli_alternatives)
                .map(|alt| json!({"executable": alt.executable, "credential": alt.credential,
                    "login_hint": alt.login_hint, "installed": runtime_info.as_ref().and_then(|r| r["prerequisites"]["executables"][&alt.executable].as_bool()).unwrap_or_else(|| readiness::executable_available(&alt.executable))})).collect();
            json!({"name": entry.name, "description": entry.package.description,
                "benefit": battery.benefit, "setup": battery.setup,
                "included": active.contains(&entry.name), "configured": configured.contains(&entry.name),
                "selected": configured.contains(&entry.name) || self.selected.contains(&entry.name),
                "credentials": credentials, "dependencies": dependencies, "alternatives": alternatives,
                "check": checks.get(&entry.name), "has_check": battery.readiness.is_some()})
        }).collect()
        };
        for name in &configured {
            // An unreadable installed battery already has its own message.
            let reported = errors.iter().any(|error| error.starts_with(&format!("{name}: ")));
            if !reported && !entries.iter().any(|entry| &entry.name == name) {
                errors.push(format!(
                    "{name}: the configuration includes this battery, but it is not installed. Run `appa battery install {name}`, or remove it from `include`."
                ));
            }
        }
        Ok(
            json!({"batteries": batteries, "errors": errors, "runtime": runtime_info, "orcarouter": self.orcarouter_status()}),
        )
    }

    async fn check(&self, names: &[String]) -> Result<(), String> {
        let (entries, _) = self.catalog();
        if names.iter().any(|name| !entries.iter().any(|e| &e.name == name)) {
            return Err("Unknown battery".into());
        }
        let selected: Vec<_> = entries
            .iter()
            .filter(|entry| {
                if names.is_empty() {
                    self.selected(&entry.name)
                } else {
                    names.contains(&entry.name)
                }
            })
            .collect();
        let results = if self.runtime().await.is_some() {
            let names: Vec<_> = selected.iter().map(|e| e.name.clone()).collect();
            let url = self.runtime_url.clone();
            tokio::task::spawn_blocking(move || {
                let endpoint = Endpoint::parse(&url)?;
                let body = serde_json::to_vec(&names).map_err(|_| "Cannot encode checks")?;
                let answer = crate::loopback_http::request(
                    &endpoint,
                    "POST",
                    "/battery-check",
                    &body,
                    &Deadline::spanning(Duration::from_secs(90)),
                )?;
                if !answer.is_success() {
                    return Err("Runtime readiness checks failed".to_string());
                }
                serde_json::from_slice::<BTreeMap<String, readiness::CheckResult>>(&answer.body)
                    .map_err(|_| "Invalid readiness response".to_string())
            })
            .await
            .map_err(|_| "Readiness task failed")??
        } else {
            check_entries(selected.into_iter().cloned().collect(), self.store.clone()).await
        };
        self.checks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(results);
        Ok(())
    }
}
async fn check_entries(entries: Vec<BatteryEntry>, store: CredentialStore) -> BTreeMap<String, readiness::CheckResult> {
    use futures_util::{StreamExt, stream};
    stream::iter(entries)
        .map(move |entry| {
            let store = store.clone();
            async move {
                let result =
                    readiness::check(&entry.dir, entry.battery().expect("catalog contains batteries"), &store).await;
                (entry.name, result)
            }
        })
        .buffer_unordered(4)
        .collect()
        .await
}
fn runtime_local(config: &Path, dirs: &[PathBuf]) -> Result<Local, String> {
    Local::new(&Args {
        config: Some(config.to_path_buf()),
        batteries_dir: dirs.to_vec(),
        battery: vec![],
        runtime_url: String::new(),
        no_open: true,
        port: None,
    })
}
/// Inspect the daemon's actual environment, never the browser launcher's approximation.
pub(crate) fn runtime_prerequisites(config: &Path, dirs: &[PathBuf]) -> Result<Value, String> {
    let local = runtime_local(config, dirs)?;
    let saved = local.store.values()?;
    let (entries, _) = local.catalog();
    let mut credentials = BTreeMap::new();
    let mut executables = BTreeMap::new();
    for entry in entries {
        let battery = entry.battery().expect("catalog contains batteries");
        for var in &battery.credentials {
            let env = std::env::var_os(var);
            credentials.insert(var.clone(), json!({"source": if env.is_some() { "environment" } else if saved.contains_key(var) { "database" } else { "missing" },
                "available": env.as_ref().is_some_and(|v| !v.is_empty()) || (env.is_none() && saved.contains_key(var)), "saved": saved.contains_key(var)}));
        }
        if let Some(check) = &battery.readiness {
            for exe in check
                .required_executables
                .iter()
                .chain(check.cli_alternatives.iter().map(|a| &a.executable))
            {
                executables.insert(exe.clone(), readiness::executable_available(exe));
            }
        }
    }
    Ok(json!({"credentials": credentials, "executables": executables}))
}
pub(crate) async fn runtime_check(config: &Path, dirs: &[PathBuf], names: &[String]) -> Result<Value, String> {
    let local = runtime_local(config, dirs)?;
    let (entries, _) = local.catalog();
    if names
        .iter()
        .any(|name| !entries.iter().any(|entry| &entry.name == name))
    {
        return Err("Unknown battery".into());
    }
    let results = check_entries(
        entries
            .into_iter()
            .filter(|entry| names.contains(&entry.name))
            .collect(),
        local.store,
    )
    .await;
    serde_json::to_value(results).map_err(|_| "Cannot encode check results".into())
}

fn same_config(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(l), Ok(r)) => l == r,
        _ => left == right,
    }
}

#[derive(Clone)]
struct Web {
    local: Arc<Local>,
    authority: String,
    origin: String,
    setup: Arc<Setup>,
    /// The OrcaRouter connect state and model catalog. Present whenever the page serves.
    orca: Option<Arc<OrcaWeb>>,
}
/// The one battery the page asks a token for, and the signal that ends the command once a
/// save or a check finds it ready.
struct Setup {
    battery: String,
    ready: tokio::sync::Notify,
}
/// How long `appa ui` waits for the battery to become ready.
const SETUP_TIMEOUT: Duration = Duration::from_secs(15 * 60);
enum Ending {
    Ready,
    TimedOut,
    Interrupted,
}
/// Ends the command when the latest check of its battery is ready.
fn settle(web: &Web) {
    let setup = &web.setup;
    let ready = web
        .local
        .checks
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&setup.battery)
        .is_some_and(|check| check.status == readiness::Status::Ready);
    if ready {
        setup.ready.notify_one();
    }
}
type ApiResult = Result<axum::Json<Value>, (StatusCode, &'static str)>;

async fn guard(
    State(web): State<Web>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    let headers = request.headers();
    let host_ok = headers.get(header::HOST).and_then(|h| h.to_str().ok()) == Some(web.authority.as_str());
    let origin_ok = headers
        .get(header::ORIGIN)
        .is_none_or(|h| h.to_str().ok() == Some(web.origin.as_str()));
    let fetch_ok = headers.get("sec-fetch-site").is_none_or(|h| h != "cross-site");
    if !peer.ip().is_loopback() || !host_ok || !origin_ok || !fetch_ok {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert("referrer-policy", "no-referrer".parse().unwrap());
    response
        .headers_mut()
        .insert("x-content-type-options", "nosniff".parse().unwrap());
    // `connect-src` admits the OrcaRouter origins so the OrcaRouter form can offer a
    // one-click connect and model discovery; every other request stays on this origin.
    // The origins are not hardcoded: a self-hosted deployment's own are read from the
    // same environment the provider reads.
    let (connect_src, img_src) = match orcarouter_connect_src() {
        // The official OrcaRouter mark is served from the auth origin; nothing else
        // external is admitted.
        Some(origins) => (
            format!("connect-src 'self' {origins}"),
            format!("img-src 'self' {origins}"),
        ),
        None => ("connect-src 'self'".to_string(), "img-src 'self'".to_string()),
    };
    response.headers_mut().insert(
        "content-security-policy",
        format!(
            "default-src 'none'; script-src 'self'; style-src 'self'; {connect_src}; {img_src}; font-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'"
        )
        .parse()
        .unwrap(),
    );
    response
}

/// The origins the OrcaRouter form may reach: the auth origin (the connect flow) and the
/// inference origin (model discovery). The browser only ever opens the auth origin; the
/// inference origin is listed so the form's discovery call is allowed, and the key stays
/// on this server either way.
fn orcarouter_connect_src() -> Option<String> {
    let origins = crate::orcarouter::Origins::resolve(None).ok()?;
    Some(format!("{} {}", origins.auth.as_str(), origins.api.as_str()))
}
async fn snapshot(State(web): State<Web>) -> ApiResult {
    web.local
        .snapshot()
        .await
        .map(axum::Json)
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Cannot inspect local setup"))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Changes {
    credentials: BTreeMap<String, Option<String>>,
    #[serde(default)]
    batteries: Vec<String>,
}
async fn save(State(web): State<Web>, axum::Json(changes): axum::Json<Changes>) -> ApiResult {
    let _gate = web.local.gate.lock().await;
    let (entries, _) = web.local.catalog();
    let allowed: BTreeSet<_> = entries
        .iter()
        .flat_map(|entry| &entry.battery().expect("battery").credentials)
        .collect();
    if changes.credentials.keys().any(|key| !allowed.contains(key))
        || changes
            .batteries
            .iter()
            .any(|name| !entries.iter().any(|e| &e.name == name))
    {
        return Err((StatusCode::BAD_REQUEST, "Unknown battery or undeclared credential"));
    }
    web.local
        .store
        .update(&changes.credentials)
        .map_err(|_| (StatusCode::BAD_REQUEST, "Credentials could not be saved"))?;
    // A saved credential can change the readiness of every battery that declares it; other
    // batteries keep their last result.
    let affected: Vec<String> = entries
        .iter()
        .filter(|entry| {
            changes.batteries.contains(&entry.name)
                || entry
                    .battery()
                    .expect("battery")
                    .credentials
                    .iter()
                    .any(|variable| changes.credentials.contains_key(variable))
        })
        .map(|entry| entry.name.clone())
        .collect();
    {
        let mut checks = web
            .local
            .checks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        checks.retain(|name, _| !affected.contains(name));
    }
    web.local
        .check(&affected)
        .await
        .map_err(|_| (StatusCode::BAD_REQUEST, "Cannot check batteries"))?;
    // A running runtime reloads now. A stopped one reads the saved tokens when a new
    // session starts it; its startup refuses if they still do not work.
    let applied = if web.local.runtime().await.is_some() {
        reload_runtime(web.local.runtime_url.clone()).await
    } else {
        Applied::NotRunning
    };
    let mut snapshot = web
        .local
        .snapshot()
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Saved; cannot refresh setup"))?;
    snapshot["applied"] = json!(match &applied {
        Applied::Reloaded => "reloaded",
        Applied::NotRunning => "not_running",
        Applied::Refused(_) => "refused",
    });
    if let Applied::Refused(error) = applied {
        snapshot["errors"]
            .as_array_mut()
            .expect("errors array")
            .push(json!(format!("Saved. The runtime kept its previous policy: {error}")));
    }
    settle(&web);
    Ok(axum::Json(snapshot))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Check {
    #[serde(default)]
    batteries: Vec<String>,
}
async fn check(State(web): State<Web>, axum::Json(check): axum::Json<Check>) -> ApiResult {
    let _gate = web.local.gate.lock().await;
    web.local
        .check(&check.batteries)
        .await
        .map_err(|_| (StatusCode::BAD_REQUEST, "Unknown battery"))?;
    let snapshot = web
        .local
        .snapshot()
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Cannot inspect local setup"))?;
    settle(&web);
    Ok(axum::Json(snapshot))
}
// ---- OrcaRouter connect and model catalog ----------------------------------

/// The OrcaRouter side of the page: one entry holding the connect attempt and the model
/// catalog. The attempt is guarded by a generation, so a response from a superseded
/// login can never overwrite a newer one, and every terminal path clears the busy flag.
struct OrcaWeb {
    origins: crate::orcarouter::Origins,
    provider: crate::orcarouter::CatalogProvider,
    attempt: tokio::sync::Mutex<Option<Attempt>>,
    /// Monotonic. Every async answer checks it still belongs to the current attempt, so
    /// a superseded login can never install its credential or its UI state.
    generation: std::sync::atomic::AtomicU64,
}

struct Attempt {
    generation: u64,
    /// The verifier stays here until the exchange: never in the URL, never logged.
    verifier: String,
}

impl OrcaWeb {
    fn new() -> Result<OrcaWeb, crate::orcarouter::OriginError> {
        let origins = crate::orcarouter::Origins::resolve(None)?;
        Ok(OrcaWeb {
            provider: crate::orcarouter::CatalogProvider::new(origins.clone()),
            origins,
            attempt: tokio::sync::Mutex::new(None),
            generation: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// The key a catalog request uses: a stored one, never the browser's.
    fn key(&self, store: &CredentialStore, variable: &str) -> Option<String> {
        let from_env = std::env::var(variable).ok().filter(|value| !value.is_empty());
        from_env.or_else(|| store.values().ok().and_then(|values| values.get(variable).cloned()))
    }
}

/// Start a login. Out-of-band by default: this is a localhost page on a machine that may
/// not be reachable from the browser, so the code is shown and pasted back. `loopback`
/// asks for the redirect flow instead, and the callback is on this page's own port.
async fn orca_begin(State(web): State<Web>, axum::Json(request): axum::Json<OrcaBegin>) -> ApiResult {
    let orca = web
        .orca
        .as_ref()
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "OrcaRouter is unavailable"))?;
    let generation = orca.generation.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    // Any attempt already in flight is superseded: its response must not land.
    {
        let mut attempt = orca.attempt.lock().await;
        *attempt = None;
    }
    let pkce = crate::orcarouter_login::Pkce::fresh();
    let mut callback = "oob".to_string();
    if request.loopback {
        // The page's own origin is the loopback callback; the port is already bound.
        callback = format!("{}/orca/callback", web.origin);
    }
    let url = crate::orcarouter_login::authorize_url(&orca.origins, &pkce, &callback);
    *orca.attempt.lock().await = Some(Attempt {
        generation,
        verifier: pkce.verifier().to_string(),
    });
    Ok(axum::Json(json!({
        "generation": generation,
        "url": url,
        "out_of_band": !request.loopback,
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OrcaBegin {
    #[serde(default)]
    loopback: bool,
}

/// Submit a code. Only the attempt whose generation is current may exchange it, so a
/// code typed after a restart or a cancelled attempt cannot mint a key for a stale state.
async fn orca_complete(State(web): State<Web>, axum::Json(request): axum::Json<OrcaComplete>) -> ApiResult {
    let orca = web
        .orca
        .as_ref()
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "OrcaRouter is unavailable"))?;
    let attempt = {
        let attempt = orca.attempt.lock().await;
        attempt
            .as_ref()
            .map(|attempt| (attempt.generation, attempt.verifier.clone()))
    };
    let Some((generation, verifier)) = attempt else {
        return Err((StatusCode::CONFLICT, "no OrcaRouter login is in progress"));
    };
    if request.generation != generation {
        return Err((StatusCode::CONFLICT, "this login was superseded; start again"));
    }
    let response = crate::orcarouter_login::exchange(&orca.origins, request.code.trim(), &verifier)
        .await
        .map_err(|error| (StatusCode::BAD_REQUEST, orca_error(&error)))?;
    let scope = crate::orcarouter_login::check_scope(response.scope.as_deref())
        .map_err(|error| (StatusCode::BAD_REQUEST, orca_error(&error)))?;
    let variable = {
        let local = web.local.clone();
        let variable = local.orcarouter_variable();
        crate::orcarouter_login::store_key(&web.local.config, &variable, &response.key)
            .map_err(|error| (StatusCode::BAD_REQUEST, orca_error(&error)))?;
        variable
    };
    // The attempt is consumed; a second submit of the same code cannot re-exchange it.
    *orca.attempt.lock().await = None;
    let snapshot = refresh_orca_snapshot(&web, &variable).await;
    Ok(axum::Json(json!({
        "scope": scope,
        "variable": variable,
        "snapshot": snapshot,
    })))
}

/// Store a pasted OrcaRouter key. This is the API-key path: it starts no authorization
/// and reaches the same credential store the connect flow writes to, under the same
/// variable, so inference cannot tell the two apart. An empty key clears the stored one.
async fn orca_key(State(web): State<Web>, axum::Json(request): axum::Json<OrcaKey>) -> ApiResult {
    let variable = web.local.orcarouter_variable();
    let value = match request.key.as_deref().map(str::trim) {
        Some("") | None => None,
        Some(key) => Some(key.to_string()),
    };
    web.local
        .store
        .update(&std::collections::BTreeMap::from([(variable.clone(), value)]))
        .map_err(|_| (StatusCode::BAD_REQUEST, "the key could not be stored"))?;
    let snapshot = web
        .local
        .snapshot()
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "saved; cannot refresh"))?;
    // A saved key is the whole answer; the response carries the state, never the key.
    Ok(axum::Json(json!({"variable": variable, "snapshot": snapshot})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OrcaKey {
    /// The pasted `sk-orca-…` key. `null` or empty clears the stored credential.
    #[serde(default)]
    key: Option<String>,
}

/// Cancel an in-flight login. Explicit cancel and every page-hide path reach this; it
/// bumps the generation first, so a response already on its way is discarded.
async fn orca_cancel(State(web): State<Web>) -> ApiResult {
    let Some(orca) = web.orca.as_ref() else {
        return Ok(axum::Json(json!({"cancelled": false})));
    };
    orca.generation.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    *orca.attempt.lock().await = None;
    Ok(axum::Json(json!({"cancelled": true})))
}

/// The model catalog the page offers for OrcaRouter, filtered to the requested
/// capability. The key stays here; the browser sees only ids and metadata.
async fn orca_models(
    State(web): State<Web>,
    axum::extract::Query(query): axum::extract::Query<OrcaModels>,
) -> ApiResult {
    let orca = web
        .orca
        .as_ref()
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "OrcaRouter is unavailable"))?;
    let capability = query.capability.unwrap_or(crate::orcarouter::Capability::Chat);
    let variable = web.local.orcarouter_variable();
    let key = orca.key(&web.local.store, &variable);
    let catalog = orca.provider.refresh(key.as_deref(), Some(capability)).await;
    let modalities = query.modalities();
    let items: Vec<Value> = catalog
        .select(capability, &modalities)
        .into_iter()
        .map(|item| {
            json!({
                "id": item.id,
                "label": item.label(),
                "context_length": item.context_length,
                "input_modalities": item.input_modalities,
                "reasoning_efforts": item.reasoning.as_ref().map(|reasoning| reasoning.efforts.clone()),
            })
        })
        .collect();
    Ok(axum::Json(json!({
        "source": catalog.source,
        "degraded": catalog.source.is_degraded(),
        "capability": capability,
        "modalities": modalities,
        "models": items,
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OrcaComplete {
    code: String,
    /// The generation the browser is answering, from `/orca/begin`. A mismatch means a
    /// newer attempt started and this code must not be exchanged.
    generation: u64,
}

#[derive(Deserialize)]
struct OrcaModels {
    capability: Option<crate::orcarouter::Capability>,
    /// Comma-separated modalities the entry point actually uploads.
    modalities: Option<String>,
}

impl OrcaModels {
    fn modalities(&self) -> Vec<crate::orcarouter::Modality> {
        self.modalities
            .as_deref()
            .unwrap_or("text")
            .split(',')
            .filter_map(|name| match name.trim() {
                "text" => Some(crate::orcarouter::Modality::Text),
                "image" => Some(crate::orcarouter::Modality::Image),
                "audio" => Some(crate::orcarouter::Modality::Audio),
                "video" => Some(crate::orcarouter::Modality::Video),
                _ => None,
            })
            .collect()
    }
}

async fn refresh_orca_snapshot(web: &Web, _variable: &str) -> Value {
    web.local
        .snapshot()
        .await
        .unwrap_or_else(|_| json!({"orcarouter": web.local.orcarouter_status()}))
}

/// One sentence a user can act on. The provider's own error text never reaches here, and
/// neither does the key, the verifier, or the code.
fn orca_error(error: &crate::orcarouter_login::LoginError) -> &'static str {
    use crate::orcarouter_login::LoginError as E;
    match error {
        E::Denied => "The authorization was denied. Nothing was saved.",
        E::StateMismatch => "The answer did not match this login attempt. Start again.",
        E::CodeRejected => "That code was already used or has expired. Start again.",
        E::Refused { status: 429 } => "Too many authorizations. Try again later, or paste a key.",
        E::Refused { .. } => "OrcaRouter refused the exchange. Try again, or paste a key.",
        E::ScopeDowngrade { .. } => {
            "The granted access is not enough for this deployment. Ask the account owner, or paste a key."
        }
        E::Transport(_) | E::Origin(_) => "OrcaRouter could not be reached. Check the network, then try again.",
        E::Timeout(_) => "The authorization timed out. Start again.",
        E::EmptyKey => "The key was empty.",
        E::Store(_) | E::Config(_) => "The key could not be stored. Check the configuration directory.",
    }
}

pub fn run(args: Args) -> ExitCode {
    match launch(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("appa ui: {error}");
            ExitCode::FAILURE
        }
    }
}
fn launch(args: Args) -> Result<(), String> {
    let [name] = args.battery.as_slice() else {
        return Err("--battery takes exactly one battery".into());
    };
    let local = Arc::new(Local::new(&args)?);
    let setup = Arc::new(Setup {
        battery: name.clone(),
        ready: tokio::sync::Notify::new(),
    });
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    runtime.block_on(async {
        // A free port by default, so a page left open never blocks the next one.
        let port = args.port.unwrap_or(0);
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(|e| format!("cannot listen on 127.0.0.1:{port}: {e}"))?;
        let authority = listener.local_addr().map_err(|e| e.to_string())?.to_string();
        let origin = format!("http://{authority}");
        let mut url = url::Url::parse(&origin).expect("loopback origin");
        url.set_path("/connect");
        url.query_pairs_mut().append_pair("battery", name);
        eprintln!("Waiting for the token. The command exits when the battery is ready.");
        println!("{url}");
        if !args.no_open {
            open_browser(url.as_str());
        }
        let web = Web {
            local: local.clone(),
            authority,
            origin,
            setup: setup.clone(),
            orca: OrcaWeb::new().ok().map(Arc::new),
        };
        let (ended, ending) = tokio::sync::oneshot::channel();
        axum::serve(
            listener,
            router(web).into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            let reason = tokio::select! {
                () = setup.ready.notified() => Ending::Ready,
                () = tokio::time::sleep(SETUP_TIMEOUT) => Ending::TimedOut,
                _ = tokio::signal::ctrl_c() => Ending::Interrupted,
            };
            let _ = ended.send(reason);
        })
        .await
        .map_err(|e| e.to_string())?;
        // The same sanitized shape as `appa battery status --json`, for this battery only.
        let snapshot = local.snapshot().await?;
        let batteries: Vec<_> = snapshot["batteries"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|battery| battery["name"] == name.as_str())
            .collect();
        let status = json!({"batteries": batteries, "errors": snapshot["errors"]});
        println!("{}", serde_json::to_string_pretty(&status).expect("status JSON"));
        match ending.await {
            Ok(Ending::Ready) => Ok(()),
            Ok(Ending::TimedOut) => Err(format!("{name} is not ready after 15 minutes")),
            Ok(Ending::Interrupted) | Err(_) => Err(format!("stopped before {name} was ready")),
        }
    })
}
enum Applied {
    Reloaded,
    NotRunning,
    Refused(String),
}
async fn reload_runtime(url: String) -> Applied {
    tokio::task::spawn_blocking(move || {
        let endpoint = match Endpoint::parse(&url) {
            Ok(endpoint) => endpoint,
            Err(error) => return Applied::Refused(error),
        };
        match crate::loopback_http::request(
            &endpoint,
            "POST",
            "/reload",
            b"",
            &Deadline::spanning(Duration::from_secs(90)),
        ) {
            Ok(answer) if answer.is_success() => Applied::Reloaded,
            Ok(answer) => Applied::Refused(String::from_utf8_lossy(&answer.body).into_owned()),
            Err(_) => Applied::NotRunning,
        }
    })
    .await
    .unwrap_or_else(|_| Applied::Refused("reload task failed".into()))
}
pub(crate) fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(url).status();
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("rundll32.exe")
        .args(["url.dll,FileProtocolHandler", url])
        .status();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let result = std::process::Command::new("xdg-open").arg(url).status();
    if !result.is_ok_and(|status| status.success()) {
        eprintln!("Open the printed URL in your browser.");
    }
}
fn router(web: Web) -> axum::Router {
    axum::Router::new()
        .route("/connect", get(|| async { Html(include_str!("ui/index.html")) }))
        .route(
            "/app.js",
            get(|| async { ([(header::CONTENT_TYPE, "text/javascript")], include_str!("ui/app.js")) }),
        )
        .route(
            "/style.css",
            get(|| async { ([(header::CONTENT_TYPE, "text/css")], include_str!("ui/style.css")) }),
        )
        .route(
            "/logo.svg",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "image/svg+xml")],
                    include_str!("../../website/public/brand/openappa-lockup-light.svg"),
                )
            }),
        )
        .route(
            "/logo-dark.svg",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "image/svg+xml")],
                    include_str!("../../website/public/brand/openappa-lockup-dark.svg"),
                )
            }),
        )
        .route(
            "/fonts/plex-sans.woff2",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "font/woff2")],
                    include_bytes!("ui/fonts/plex-sans.woff2").as_slice(),
                )
            }),
        )
        .route(
            "/fonts/plex-mono.woff2",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "font/woff2")],
                    include_bytes!("ui/fonts/plex-mono.woff2").as_slice(),
                )
            }),
        )
        .route("/api/state", get(snapshot))
        .route("/api/credentials", post(save))
        .route("/api/check", post(check))
        // OrcaRouter: the two authenticate paths and the model catalog. A PKCE code that
        // the consent screen shows out-of-band is pasted into `/orca/complete`.
        .route("/api/orcarouter/begin", post(orca_begin))
        .route("/api/orcarouter/complete", post(orca_complete))
        .route("/api/orcarouter/cancel", post(orca_cancel))
        .route("/api/orcarouter/key", post(orca_key))
        .route("/api/orcarouter/models", get(orca_models))
        .layer(axum::extract::DefaultBodyLimit::max(256 * 1024))
        .layer(axum::middleware::from_fn_with_state(web.clone(), guard))
        .with_state(web)
}

/// CLI description uses the same manifest checks as the web UI. When the
/// deployment is running, checks execute in its environment, not the CLI's.
pub fn describe_readiness(config: &Path, dirs: &[PathBuf]) -> Result<String, String> {
    if !config.is_file() {
        return Ok(String::new());
    }
    let options = Args {
        config: Some(config.to_path_buf()),
        batteries_dir: dirs.to_vec(),
        battery: vec![],
        no_open: true,
        port: None,
        runtime_url: std::env::var("APPA_RUNTIME_URL")
            .unwrap_or_else(|_| crate::runtime_url::DEFAULT_RUNTIME_URL.into()),
    };
    let local = Local::new(&options)?;
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    runtime.block_on(async {
        local.check(&[]).await?;
        let checks = local.checks.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if checks.is_empty() {
            return Ok(String::new());
        }
        let mut text = String::from("Battery readiness:\n");
        for (name, check) in checks.iter() {
            let value = serde_json::to_value(check).map_err(|e| e.to_string())?;
            text.push_str(&format!(
                "  {name}: {} ({})\n",
                value["status"].as_str().unwrap_or("unknown"),
                value["reason"].as_str().unwrap_or("unknown")
            ));
        }
        Ok(text)
    })
}

pub fn status(args: StatusArgs) -> ExitCode {
    let options = Args {
        config: args.config,
        batteries_dir: args.batteries_dir,
        battery: args.battery,
        runtime_url: std::env::var("APPA_RUNTIME_URL")
            .unwrap_or_else(|_| crate::runtime_url::DEFAULT_RUNTIME_URL.into()),
        no_open: true,
        port: None,
    };
    let result = (|| {
        let local = Local::new(&options)?;
        let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
        runtime.block_on(async {
            if args.check {
                local.check(&[]).await?;
            }
            let snapshot = local.snapshot().await?;
            Ok::<_, String>(json!({"batteries": snapshot["batteries"], "errors": snapshot["errors"]}))
        })
    })();
    match result {
        Ok(value) => {
            if args.json {
                println!("{}", serde_json::to_string_pretty(&value).unwrap());
            } else {
                for battery in value["batteries"].as_array().into_iter().flatten() {
                    println!(
                        "{}: {}",
                        battery["name"].as_str().unwrap_or("battery"),
                        battery["check"]["status"].as_str().unwrap_or("not_checked")
                    );
                }
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("appa battery status: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(dir: &Path) -> Args {
        std::fs::write(dir.join("appa.toml"), "[policy]\nversion = 2\n").unwrap();
        let battery = dir.join("batteries/demo");
        std::fs::create_dir_all(&battery).unwrap();
        std::fs::write(battery.join("appa-package.toml"), "schema = 1\nname = \"demo\"\ndescription = \"Demo\"\n[battery]\npolicy = \"appa.toml\"\nhosts = [\"claude-code\"]\nhelpers = [\"check.py\"]\n[battery.readiness]\ncommand = [\"python3\", \"check.py\"]\nrequired_executables = [\"python3\"]\n").unwrap();
        std::fs::write(battery.join("appa.toml"), "[policy]\nversion = 2\n[externals.audience.demo]\ncommand = [\"python3\", \"check.py\"]\ntoken_env = \"APPA_PROVIDER_DEMO_TOKEN\"\n").unwrap();
        std::fs::write(battery.join("check.py"), "import json,os\nassert os.environ.get('APPA_PROVIDER_DEMO_TOKEN') == 'fixture-only-secret'\nassert 'APPA_PROVIDER_OTHER_TOKEN' not in os.environ\nprint(json.dumps(dict(status='ready',authentication='token',reason='verified')))\n").unwrap();
        Args {
            config: Some(dir.join("appa.toml")),
            batteries_dir: vec![],
            battery: vec!["demo".into()],
            runtime_url: "http://127.0.0.1:1".into(),
            no_open: true,
            port: None,
        }
    }

    #[tokio::test]
    async fn setup_without_enforcement_saves_checks_and_never_returns_secrets() {
        crate::tls::install_crypto_provider();
        let dir = tempfile::tempdir().unwrap();
        let args = fixture(dir.path());
        let local = Arc::new(Local::new(&args).unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let authority = listener.local_addr().unwrap().to_string();
        let origin = format!("http://{authority}");
        let web = Web {
            local: local.clone(),
            authority,
            origin: origin.clone(),
            setup: Arc::new(Setup {
                battery: "demo".into(),
                ready: tokio::sync::Notify::new(),
            }),
            orca: OrcaWeb::new().ok().map(Arc::new),
        };
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                router(web).into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        assert_eq!(
            client.get(format!("{origin}/api/state")).send().await.unwrap().status(),
            StatusCode::OK
        );
        assert_eq!(
            client
                .get(format!("{origin}/api/state"))
                .header("Host", "attacker.example")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        let save_url = format!("{origin}/api/credentials");
        let body = json!({"credentials": {"APPA_PROVIDER_DEMO_TOKEN": "fixture-only-secret"}, "batteries": ["demo"]});
        assert_eq!(
            client
                .post(&save_url)
                .header("Origin", "https://attacker.example")
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert!(local.store.values().unwrap().is_empty());
        let response = client
            .post(&save_url)
            .header("Origin", &origin)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        assert_eq!(response.headers()["cache-control"], "no-store");
        let text = response.text().await.unwrap();
        assert!(!text.contains("fixture-only-secret"));
        let data: Value = serde_json::from_str(&text).unwrap();
        assert!(data["runtime"].is_null());
        assert_eq!(data["batteries"][0]["check"]["status"], "ready");
        assert_eq!(data["batteries"][0]["credentials"][0]["source"], "database");
        let invalid = json!({"credentials": {"APPA_PROVIDER_FOREIGN_TOKEN": "not-allowed"}});
        assert_eq!(
            client.post(&save_url).json(&invalid).send().await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );
        assert!(
            !local
                .store
                .values()
                .unwrap()
                .contains_key("APPA_PROVIDER_FOREIGN_TOKEN")
        );
        let delete = json!({"credentials": {"APPA_PROVIDER_DEMO_TOKEN": null}, "batteries": ["demo"]});
        assert!(
            client
                .post(&save_url)
                .json(&delete)
                .send()
                .await
                .unwrap()
                .status()
                .is_success()
        );
        assert!(local.store.values().unwrap().is_empty());
        task.abort();
    }

    #[tokio::test]
    async fn prerequisites_only_battery_is_ready_when_its_declared_executables_exist() {
        let dir = tempfile::tempdir().unwrap();
        let args = fixture(dir.path());
        let local = Local::new(&args).unwrap();
        let (entries, _) = local.catalog();
        let entry = &entries[0];
        let mut battery = entry.battery().unwrap().clone();
        battery.credentials.clear();
        battery.readiness.as_mut().unwrap().command.clear();
        let result = readiness::check(&entry.dir, &battery, &local.store).await;
        assert_eq!(result.status, readiness::Status::Ready);
        battery
            .readiness
            .as_mut()
            .unwrap()
            .required_executables
            .push("appa-missing-fixture-cli".into());
        let result = readiness::check(&entry.dir, &battery, &local.store).await;
        assert_eq!(result.status, readiness::Status::NeedsConfiguration);
    }

    #[tokio::test]
    async fn a_battery_without_a_check_is_ready_when_its_bound_variables_are_set() {
        let dir = tempfile::tempdir().unwrap();
        let args = fixture(dir.path());
        let local = Local::new(&args).unwrap();
        let (entries, _) = local.catalog();
        let entry = &entries[0];
        let mut battery = entry.battery().unwrap().clone();
        battery.readiness = None;
        let result = readiness::check(&entry.dir, &battery, &local.store).await;
        assert_eq!(result.status, readiness::Status::NeedsConfiguration);
        assert!(matches!(result.reason, readiness::Reason::MissingCredential));
        local
            .store
            .update(&BTreeMap::from([(
                "APPA_PROVIDER_DEMO_TOKEN".into(),
                Some("fixture-only-secret".into()),
            )]))
            .unwrap();
        let result = readiness::check(&entry.dir, &battery, &local.store).await;
        assert_eq!(result.status, readiness::Status::Ready);
        assert!(matches!(result.reason, readiness::Reason::Configured));
        battery.credentials.clear();
        local
            .store
            .update(&BTreeMap::from([("APPA_PROVIDER_DEMO_TOKEN".into(), None)]))
            .unwrap();
        let result = readiness::check(&entry.dir, &battery, &local.store).await;
        assert_eq!(result.status, readiness::Status::Ready);
    }

    #[tokio::test]
    async fn readiness_output_cannot_echo_a_token_and_missing_executables_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let args = fixture(dir.path());
        let local = Local::new(&args).unwrap();
        let (mut entries, errors) = local.catalog();
        assert!(errors.is_empty());
        let entry = entries.pop().unwrap();
        std::fs::write(entry.dir.join("check.py"), "print('{\"status\":\"ready\",\"authentication\":\"token\",\"reason\":\"verified\",\"message\":\"secret\"}')").unwrap();
        let result = readiness::check(&entry.dir, entry.battery().unwrap(), &local.store).await;
        assert!(matches!(result.reason, readiness::Reason::CheckFailed));
        let mut battery = entry.battery().unwrap().clone();
        battery
            .readiness
            .as_mut()
            .unwrap()
            .required_executables
            .push("appa-no-such-fixture-executable".into());
        let result = readiness::check(&entry.dir, &battery, &local.store).await;
        assert!(matches!(result.reason, readiness::Reason::MissingExecutable));
    }
}
