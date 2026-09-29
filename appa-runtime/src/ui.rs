//! Local management UI. It can serve setup while enforcement cannot start.
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

use crate::config::Config;
use crate::credentials::CredentialStore;
use crate::loopback_http::{Deadline, Endpoint};

#[derive(clap::Args, Clone)]
pub struct Args {
    #[arg(long, env = "APPA_CONFIG")]
    config: Option<PathBuf>,
    #[arg(skip)]
    batteries_dir: Vec<PathBuf>,
    /// Configure several proposed batteries together, before including them.
    #[arg(long, value_delimiter = ',')]
    battery: Vec<String>,
    /// Open the consolidated prerequisites screen.
    #[arg(long)]
    setup: bool,
    #[arg(long, env = "APPA_RUNTIME_URL", default_value = crate::runtime_url::DEFAULT_RUNTIME_URL)]
    runtime_url: String,
    /// Print the browser URL without opening the browser.
    #[arg(long)]
    no_open: bool,
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
    host: Option<Arc<crate::runtime_cli::RuntimeHost>>,
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
            host: None,
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
                _ => errors.push(format!(
                    "{}: installed manifest or helper files are missing or invalid",
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

    async fn runtime(&self) -> Option<Value> {
        if let Some(host) = &self.host {
            return host.dashboard();
        }
        let url = self.runtime_url.clone();
        let data = tokio::task::spawn_blocking(move || {
            let endpoint = Endpoint::parse(&url).ok()?;
            let answer =
                crate::loopback_http::get(&endpoint, "/dashboard", &Deadline::spanning(Duration::from_secs(2))).ok()?;
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
        let runtime = runtime_info.clone().filter(|data| data["enforcement_ready"] != false);
        let active: BTreeSet<String> = runtime
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
            json!({"name": entry.name, "description": entry.package.description, "setup": battery.setup,
                "included": active.contains(&entry.name), "configured": configured.contains(&entry.name),
                "selected": configured.contains(&entry.name) || self.selected.contains(&entry.name),
                "credentials": credentials, "dependencies": dependencies, "alternatives": alternatives,
                "check": checks.get(&entry.name), "has_check": battery.readiness.is_some()})
        }).collect()
        };
        let config = Config::inspect_local(&self.config, &self.dirs).ok();
        if config.is_none() {
            errors.push("Configuration is absent or invalid. Credential setup is still available.".into());
        }
        for name in &configured {
            if !entries.iter().any(|entry| &entry.name == name) {
                errors.push(format!("{name}: configured battery is unavailable"));
            }
        }
        let configured_policy = config.as_ref().map(|c| json!(c.policy_file().value()));
        let policy = runtime
            .as_ref()
            .map(|r| r["policy"].clone())
            .or(configured_policy)
            .unwrap_or(json!({}));
        let configured_servers: Vec<_> = std::env::current_dir()
            .ok()
            .map(|cwd| {
                crate::installation::discover::servers(appa_package::Host::ClaudeCode, &cwd)
                    .into_iter()
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();
        // Policy origins are hints only when the loaded policy matches the current disk configuration.
        let disk_matches = config
            .as_ref()
            .is_some_and(|c| json!(c.policy_file().value()) == policy);
        let mut origins = BTreeMap::new();
        if disk_matches {
            let paths = std::iter::once(("root configuration".to_string(), self.config.clone())).chain(
                entries
                    .iter()
                    .filter(|e| configured.contains(&e.name))
                    .map(|e| (e.name.clone(), e.dir.join("appa.toml"))),
            );
            for (origin, path) in paths {
                if let Some(doc) = std::fs::read_to_string(path)
                    .ok()
                    .and_then(|t| toml::from_str::<toml::Value>(&t).ok())
                {
                    for rule in doc
                        .get("policy")
                        .and_then(|p| p.get("tool"))
                        .and_then(toml::Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        if let Some(name) = rule.get("name").and_then(toml::Value::as_str) {
                            origins.entry(name.to_owned()).or_insert_with(|| origin.clone());
                        }
                    }
                }
            }
        }
        let policy_source = if runtime.is_some() { "active" } else { "configured" };
        Ok(
            json!({"batteries": batteries, "errors": errors, "runtime": runtime, "policy": policy,
            "policy_source": policy_source,
            "origins": origins, "configured_servers": configured_servers}),
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
        let results = if self.host.is_none() && self.runtime().await.is_some() {
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
        setup: false,
        runtime_url: String::new(),
        no_open: true,
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
    response.headers_mut().insert("content-security-policy", "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; font-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'".parse().unwrap());
    response
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenRequest {
    config: Option<PathBuf>,
    #[serde(default)]
    batteries: Vec<String>,
    #[serde(default)]
    setup: bool,
}
async fn open_ui(State(web): State<Web>, axum::Json(request): axum::Json<OpenRequest>) -> ApiResult {
    if request
        .config
        .as_ref()
        .is_some_and(|path| !same_config(path, &web.local.config))
    {
        return Err((StatusCode::CONFLICT, "The runtime serves a different configuration"));
    }
    let (entries, _) = web.local.catalog();
    if request
        .batteries
        .iter()
        .any(|name| !entries.iter().any(|entry| &entry.name == name))
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "Requested battery is not installed in this runtime",
        ));
    }
    let mut url = url::Url::parse(&web.origin).expect("runtime origin");
    url.query_pairs_mut()
        .append_pair("configure", if request.setup { "true" } else { "false" });
    if !request.batteries.is_empty() {
        url.query_pairs_mut()
            .append_pair("batteries", &request.batteries.join(","));
    }
    Ok(axum::Json(json!({"url": url.as_str()})))
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
    web.local
        .checks
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
    web.local
        .check(&changes.batteries)
        .await
        .map_err(|_| (StatusCode::BAD_REQUEST, "Cannot check batteries"))?;
    let activation_failed = if let Some(host) = &web.local.host {
        match host.activate().await {
            Ok(_) => false,
            Err(error) => {
                tracing::warn!(%error, "credentials saved; enforcement reload refused");
                true
            }
        }
    } else {
        false
    };
    let mut snapshot = web
        .local
        .snapshot()
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Saved; cannot refresh setup"))?;
    if activation_failed {
        snapshot["errors"].as_array_mut().expect("errors array").push(json!(
            "Credentials saved. Enforcement configuration could not be applied; inspect runtime diagnostics."
        ));
    }
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
    web.local
        .snapshot()
        .await
        .map(axum::Json)
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Cannot inspect local setup"))
}
async fn apply(State(web): State<Web>) -> ApiResult {
    let _gate = web.local.gate.lock().await;
    let host = web
        .local
        .host
        .as_ref()
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Runtime unavailable"))?;
    host.activate()
        .await
        .map(|_| axum::Json(json!({"active": true})))
        .map_err(|error| {
            tracing::warn!(%error, "enforcement reload refused");
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "Configuration could not be applied. Inspect runtime diagnostics.",
            )
        })
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
    let endpoint = Endpoint::parse(&args.runtime_url)?;
    let body = serde_json::to_vec(&json!({"config": args.config, "batteries": args.battery, "setup": args.setup}))
        .map_err(|e| e.to_string())?;
    let answer = crate::loopback_http::request(
        &endpoint,
        "POST",
        "/ui/open",
        &body,
        &Deadline::spanning(Duration::from_secs(5)),
    )
    .map_err(|_| {
        format!(
            "No runtime is reachable at {}. Start appa runtime first.",
            args.runtime_url
        )
    })?;
    if !answer.is_success() {
        return Err(String::from_utf8_lossy(&answer.body).into_owned());
    }
    let response: Value = serde_json::from_slice(&answer.body).map_err(|_| "Invalid UI response")?;
    let url = response["url"].as_str().ok_or("Runtime did not provide a UI address")?;
    println!("{url}");
    if !args.no_open {
        open_browser(url);
    }
    Ok(())
}
fn open_browser(url: &str) {
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
        .route("/", get(|| async { Html(include_str!("ui/index.html")) }))
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
        .route("/ui/open", post(open_ui))
        .route("/api/state", get(snapshot))
        .route("/api/credentials", post(save))
        .route("/api/check", post(check))
        .route("/api/apply", post(apply))
        .layer(axum::extract::DefaultBodyLimit::max(256 * 1024))
        .layer(axum::middleware::from_fn_with_state(web.clone(), guard))
        .with_state(web)
}

pub(crate) fn runtime_router(
    host: Arc<crate::runtime_cli::RuntimeHost>,
    listen: SocketAddr,
) -> Result<axum::Router, String> {
    let mut local = runtime_local(&host.config, &host.dirs)?;
    local.host = Some(host);
    // The management surface remains loopback-only even for network-facing deployments.
    let address = SocketAddr::new(
        if listen.is_ipv4() {
            std::net::Ipv4Addr::LOCALHOST.into()
        } else {
            std::net::Ipv6Addr::LOCALHOST.into()
        },
        listen.port(),
    );
    let authority = address.to_string();
    Ok(router(Web {
        local: Arc::new(local),
        origin: format!("http://{authority}"),
        authority,
    }))
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
        setup: false,
        no_open: true,
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
                value["status"].as_str().unwrap_or("unverified"),
                value["reason"].as_str().unwrap_or("no_check")
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
        setup: true,
        runtime_url: std::env::var("APPA_RUNTIME_URL")
            .unwrap_or_else(|_| crate::runtime_url::DEFAULT_RUNTIME_URL.into()),
        no_open: true,
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
                        battery["check"]["status"].as_str().unwrap_or("unverified")
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
            setup: true,
            runtime_url: "http://127.0.0.1:1".into(),
            no_open: true,
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
