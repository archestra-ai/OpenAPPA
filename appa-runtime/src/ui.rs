//! Local management UI, served by `appa ui` itself, never by the runtime. Setup works
//! while the runtime is stopped; when a runtime serves this configuration, the page shows
//! what that process reaches and reloads it after a save.
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
    /// Loopback port for the page; the runtime's port + 1 when absent.
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

    async fn runtime(&self) -> Option<Value> {
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
        let config = match Config::inspect_local(&self.config, &self.dirs) {
            Ok(config) => Some(config),
            Err(error) => {
                errors.push(format!(
                    "APPA cannot read the configuration: {error}. The overview is empty until it is fixed; you can still add tokens."
                ));
                None
            }
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
        // The overview reads the configuration on disk: the composed policy, and each
        // rule's origin from the root file or the included battery that names it first.
        let policy = config
            .as_ref()
            .map(|c| json!(c.policy_file().value()))
            .unwrap_or(json!({}));
        let mut origins = BTreeMap::new();
        if config.is_some() {
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
        Ok(
            json!({"batteries": batteries, "errors": errors, "runtime": runtime_info, "policy": policy, "origins": origins,
                "config": self.config.display().to_string()}),
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
        setup: false,
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
    let local = Arc::new(Local::new(&args)?);
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let port = page_port(&args)?;
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::AddrInUse => {
                    format!("port {port} is in use; is appa ui already running? Use --port to choose another.")
                }
                _ => format!("cannot listen on 127.0.0.1:{port}: {e}"),
            })?;
        let authority = listener.local_addr().map_err(|e| e.to_string())?.to_string();
        let origin = format!("http://{authority}");
        let mut url = url::Url::parse(&origin).expect("loopback origin");
        url.query_pairs_mut()
            .append_pair("configure", if args.setup { "true" } else { "false" });
        if !args.battery.is_empty() {
            url.query_pairs_mut().append_pair("batteries", &args.battery.join(","));
        }
        println!("{url}");
        eprintln!("Serving the page until you press Ctrl-C.");
        if !args.no_open {
            open_browser(url.as_str());
        }
        let web = Web {
            local,
            authority,
            origin,
        };
        axum::serve(
            listener,
            router(web).into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|e| e.to_string())
    })
}
/// The page sits one port above the runtime it manages, so its address stays the same.
fn page_port(args: &Args) -> Result<u16, String> {
    if let Some(port) = args.port {
        return Ok(port);
    }
    url::Url::parse(&args.runtime_url)
        .ok()
        .and_then(|url| url.port_or_known_default())
        .and_then(|port| port.checked_add(1))
        .ok_or_else(|| format!("cannot derive a page port from {}; use --port", args.runtime_url))
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
        setup: false,
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
        setup: true,
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
            setup: true,
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

    #[test]
    fn the_page_sits_one_port_above_the_runtime_unless_a_port_is_given() {
        let mut args = fixture(tempfile::tempdir().unwrap().path());
        args.runtime_url = "http://127.0.0.1:8787".into();
        assert_eq!(page_port(&args), Ok(8788));
        args.port = Some(9000);
        assert_eq!(page_port(&args), Ok(9000));
        args.port = None;
        args.runtime_url = "http://127.0.0.1:65535".into();
        assert!(page_port(&args).is_err());
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
