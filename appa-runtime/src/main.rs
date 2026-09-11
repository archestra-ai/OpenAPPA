//! Internal `appa runtime` command: an HTTP listener for hooks. Policy decisions live behind the runtime API; this file
//! parses flags, initializes a missing deployment config, opens the
//! runtime, picks the adapter codec, and serves.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use appa_runtime_api::AdapterName;
pub use appa_runtime_api::AdapterName as Adapter;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use clap::Parser;
use sha2::{Digest, Sha256};

use crate::api::{Reloaded, Runtime};
use crate::config::Config;
use crate::default_config;
use crate::{hooks, mcp};

fn ensure_default_config(path: &Path) -> io::Result<bool> {
    let mut file = match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => return Err(error),
    };
    if let Err(error) = file
        .write_all(default_config::text().as_bytes())
        .and_then(|()| file.sync_all())
    {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(error);
    }
    Ok(true)
}

#[derive(Parser)]
#[command(name = "appa runtime", version)]
struct Args {
    #[arg(long, env = "APPA_CONFIG", global = true)]
    config: Option<PathBuf>,

    #[arg(long, env = "APPA_DB", default_value = "appa.db")]
    db: PathBuf,

    /// Workspace served by runtime-owned file tools. Use the constrained claude-files launcher.
    #[arg(long, env = "APPA_FILE_WORKSPACE", requires = "file_ledger")]
    file_workspace: Option<PathBuf>,

    /// Initialized file ledger outside the workspace. Missing ledgers fail closed.
    #[arg(long, env = "APPA_FILE_LEDGER", requires = "file_workspace")]
    file_ledger: Option<PathBuf>,

    /// Host-installed agentsh backend directory for isolated declared-input processing.
    #[arg(long, env = "APPA_FILE_PROCESS_BACKEND", requires = "file_workspace")]
    file_process_backend: Option<PathBuf>,

    /// First start only: classify all existing files with this policy trust name.
    #[arg(long, requires = "file_workspace")]
    initialize_file_trust: Option<String>,

    /// Initial audience for every existing file. Required with initialization.
    #[arg(long, requires = "initialize_file_trust", value_parser = ["self", "internal", "public"])]
    initialize_file_audience: Option<String>,

    #[arg(long, env = "APPA_MODULES_DIR")]
    modules_dir: Option<PathBuf>,

    /// Directories of bundled batteries, in lookup order. First directory
    /// that contains `batteries/<name>/appa.toml`'s `<name>` wins. Colon
    /// separated when set through `APPA_BATTERIES_DIR`.
    #[arg(
        long = "batteries-dir",
        env = "APPA_BATTERIES_DIR",
        value_delimiter = ':',
        action = clap::ArgAction::Append
    )]
    batteries_dir: Vec<PathBuf>,

    #[arg(long, default_value = "127.0.0.1:8787")]
    listen: SocketAddr,

    /// Optional dedicated listener for the kagent appa-guide MCP surface.
    #[arg(long, env = "APPA_GUIDE_LISTEN")]
    guide_listen: Option<SocketAddr>,

    /// Host headers accepted by the MCP endpoint. Empty keeps rmcp's
    /// loopback defaults. Shared deployments name each Service address.
    #[arg(
        long = "mcp-allowed-host",
        env = "APPA_MCP_ALLOWED_HOST",
        value_delimiter = ',',
        action = clap::ArgAction::Append
    )]
    mcp_allowed_hosts: Vec<String>,

    #[arg(long, default_value_t = AdapterName::ClaudeCode, global = true)]
    adapter: AdapterName,

    #[arg(short, action = clap::ArgAction::Count)]
    verbose: u8,
}

/// The derivation the runtime applies to every call of the host it serves. The one
/// place this crate names the adapter crates.
fn served(adapter: AdapterName) -> appa_runtime_api::Adapter {
    match adapter {
        AdapterName::ClaudeCode => appa_adapter_claude_code::adapter(),
        AdapterName::Kagent => appa_adapter_kagent::adapter(),
    }
}

fn log_level(verbose: u8) -> &'static str {
    match verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    }
}

/// The executable this process runs, as it stood on disk when the process started. An install
/// that replaces the file leaves this process serving the old build; `/health` reports the
/// replacement so the plugin's starter replaces the process too.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ExecutableAtStart {
    len: u64,
    modified: SystemTime,
    digest: String,
}

impl ExecutableAtStart {
    fn snapshot(path: PathBuf) -> Option<Self> {
        let metadata = fs::metadata(&path).ok()?;
        let modified = metadata.modified().ok()?;
        let digest = binary_digest(&path).ok()?;
        Some(Self {
            len: metadata.len(),
            modified,
            digest,
        })
    }

    fn of_this_process() -> Option<Self> {
        std::env::current_exe().ok().and_then(Self::snapshot)
    }

    /// Whether the executable installed at this process's path no longer matches the one it
    /// started from. A missing or unreadable path is stale too: Unix can keep an unlinked old
    /// executable running after an install removes it.
    fn is_replaced(&self) -> bool {
        self.differs_from(current_executable_metadata())
    }

    fn differs_from(&self, current: io::Result<(u64, SystemTime)>) -> bool {
        current
            .map(|(len, modified)| len != self.len || modified != self.modified)
            .unwrap_or(true)
    }
}

/// Read only the path the operating system says this process started from. Keeping this
/// filesystem lookup outside Axum's extracted state makes the trust boundary explicit: an HTTP
/// request supplies neither the executable path nor any part of it.
fn current_executable_metadata() -> io::Result<(u64, SystemTime)> {
    let path = std::env::current_exe()?;
    let metadata = fs::metadata(path)?;
    Ok((metadata.len(), metadata.modified()?))
}

/// The fingerprint a runtime serves at `/binary-fingerprint`.
///
/// `init` computes the same value for the binary it is about to deploy and compares
/// the two to decide whether the process on the endpoint is its own deployment. The
/// two sides must render the digest identically, so they share this one definition.
pub(crate) fn binary_digest(path: &Path) -> io::Result<String> {
    let bytes = fs::read(path)?;
    let digest = Sha256::digest(bytes);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[derive(Clone)]
struct AppState {
    runtime: Arc<Runtime>,
    adapter: appa_runtime_api::Adapter,
    config: PathBuf,
    battery_dirs: Vec<PathBuf>,
    battery_state: Arc<RwLock<mcp::BatteryState>>,
    reload_gate: Arc<tokio::sync::Mutex<()>>,
    executable: Option<ExecutableAtStart>,
}

async fn hook(
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> (axum::http::StatusCode, axum::Json<serde_json::Value>) {
    let (status, body) = hooks::answer(&state.runtime, &state.adapter, &body).await;
    let status = axum::http::StatusCode::from_u16(status).expect("hook answers carry valid status codes");
    (status, axum::Json(body))
}

async fn file_tools(State(state): State<AppState>) -> Result<axum::Json<crate::claude_files::Deployment>, StatusCode> {
    state
        .runtime
        .file_deployment(state.config)
        .map(axum::Json)
        .ok_or(StatusCode::NOT_FOUND)
}

async fn validate_tools(
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> (axum::http::StatusCode, axum::Json<serde_json::Value>) {
    let (status, report) = crate::tool_validation::answer(&state.runtime, state.adapter, &body);
    (
        axum::http::StatusCode::from_u16(status).expect("validation answers carry valid status codes"),
        axum::Json(report),
    )
}

/// `ok` while this process serves the executable installed on disk; `stale <pid>` once an
/// install replaced that file, naming the process to stop before starting the new build.
async fn health(State(state): State<AppState>) -> String {
    let stale = state.executable.as_ref().is_some_and(ExecutableAtStart::is_replaced);
    health_answer(stale, std::process::id())
}

/// The policy this process serves, so an install can tell whether a runtime it left
/// running still answers under the configuration on disk. Read-only: reloading is the
/// caller's separate, deliberate step.
async fn policy_key(State(state): State<AppState>) -> String {
    state.runtime.serving_policy_key()
}

/// Which deployment answers here: the build, the process, and the configuration it serves.
///
/// The build alone does not identify a deployment. Two installs of one build are
/// byte-identical, so an install that compared digests alone would take another
/// deployment's runtime for its own. The configuration path is what separates them.
async fn binary_fingerprint(State(state): State<AppState>) -> Result<String, axum::http::StatusCode> {
    state
        .executable
        .as_ref()
        .map(|executable| binary_fingerprint_answer(&executable.digest, std::process::id(), &state.config))
        .ok_or(axum::http::StatusCode::NOT_FOUND)
}

/// The first line's fields are read positionally, so the configuration follows the first
/// newline and runs to the end of the answer. A path may hold spaces and, on Unix, newlines;
/// taking the whole remainder verbatim keeps either from being mistaken for a field break.
fn binary_fingerprint_answer(digest: &str, pid: u32, config: &Path) -> String {
    format!("{digest} {pid}\n{}", config.display())
}

fn health_answer(stale: bool, pid: u32) -> String {
    if stale { format!("stale {pid}") } else { "ok".to_owned() }
}

async fn batteries(State(state): State<AppState>) -> axum::Json<crate::batteries::BatteriesResponse> {
    let catalog = state
        .battery_state
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .catalog
        .clone();
    axum::Json(catalog)
}

async fn reload(State(state): State<AppState>) -> Result<axum::Json<Reloaded>, (axum::http::StatusCode, String)> {
    let _reload = state.reload_gate.lock().await;
    let config = Config::load_from(&state.config, &state.battery_dirs)
        .map_err(|error| (axum::http::StatusCode::UNPROCESSABLE_ENTITY, error.to_string()))?;
    let battery_state = mcp::BatteryState {
        catalog: crate::batteries::snapshot(&state.battery_dirs),
        included: config.included_batteries().iter().cloned().collect(),
        serving_tools: config.tool_names().into_iter().collect(),
    };
    let refused = |refusal: String| {
        tracing::warn!(%refusal, "the reload was refused; the running deployment keeps serving");
        (axum::http::StatusCode::UNPROCESSABLE_ENTITY, refusal)
    };
    let prepared = state
        .runtime
        .prepare_reload(config)
        .map_err(|refusal| refused(refusal.to_string()))?;
    // The new sources are probed before anything swaps, and before the battery lock below
    // is taken: nothing holds a std lock across the await.
    prepared
        .probe_sources()
        .await
        .map_err(|refusal| refused(refusal.to_string()))?;
    let mut published = state
        .battery_state
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // The matcher holds this lock while reading policy metadata. Keep it
    // across the synchronous policy swap so no response can describe the
    // old policy after the new one starts serving.
    let reloaded = state.runtime.install(prepared);
    *published = battery_state;
    Ok(axum::Json(reloaded))
}

#[derive(serde::Deserialize)]
struct StatusQuery {
    trajectory: String,
}

async fn status(
    State(state): State<AppState>,
    query: Result<axum::extract::Query<StatusQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<axum::Json<crate::api::TrajectoryStatus>, axum::http::StatusCode> {
    let query = query.map_err(|_| axum::http::StatusCode::BAD_REQUEST)?;
    let id = crate::api::TrajectoryId(query.0.trajectory);
    match state.runtime.status(&id) {
        Some(status) => Ok(axum::Json(status)),
        None => Err(axum::http::StatusCode::NOT_FOUND),
    }
}

/// What `appa yell` asks this process to build. The runtime assembles and measures the whole
/// document, so the CLI never holds an unclassified byte of a session.
#[derive(serde::Deserialize)]
struct ReportRequestBody {
    message: String,
    /// Replace the names the deployment chose with report-local tokens. The classification is
    /// the same either way; only the naming differs.
    ///
    /// Required, with no default: this is the question a person was asked, and a request that
    /// does not carry their answer has no business getting either kind of report.
    pseudonymize: bool,
}

/// One finished `openappa.yell.v1` document.
///
/// The listener is loopback-only, and that is the whole of this endpoint's access control —
/// the same boundary `/reload` and `/mcp` already stand behind. Be exact about what it is
/// worth: it separates this machine from the network, not one local process from another. A
/// process that can reach this port can read the recently active trajectory. What it answers
/// still leaves the machine only when a person chooses to send it.
async fn report(
    State(state): State<AppState>,
    body: Result<axum::Json<ReportRequestBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Vec<u8>, (axum::http::StatusCode, String)> {
    let refuse = |status, message: String| (status, message);
    let body = body
        .map_err(|error| refuse(axum::http::StatusCode::BAD_REQUEST, error.body_text()))?
        .0;
    let message = crate::yell::YellMessage::new(&body.message)
        .map_err(|refusal| refuse(axum::http::StatusCode::BAD_REQUEST, refusal.to_string()))?;
    let request = crate::yell::ReportRequest {
        message,
        author: crate::yell::Author::Cli,
        mode: match body.pseudonymize {
            true => crate::yell::Mode::Pseudonymized,
            false => crate::yell::Mode::Baseline,
        },
        // A caller here names no trajectory: it gets whichever one was recently active, or
        // nothing. That narrows the endpoint — no session can be asked for by name — without
        // making it a per-caller boundary. The recently active trajectory may well belong to
        // someone else's session on this machine, and loopback is the only thing between them.
        selection: crate::yell::Selection::Recent,
        harness: state.adapter.name,
    };
    state
        .runtime
        .report_off_thread(request)
        .await
        .map(|finished| finished.plain)
        .map_err(|oversize| refuse(axum::http::StatusCode::PAYLOAD_TOO_LARGE, oversize.to_string()))
}

/// Run the internal daemon command from arguments supplied by the public CLI.
pub fn run_from<I, T>(args: I) -> ExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let args = Args::parse_from(args);
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("appa runtime: cannot create async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(serve(args))
}

async fn serve(args: Args) -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(log_level(args.verbose))),
        )
        .init();

    let config_path = args.config.unwrap_or_else(|| PathBuf::from("appa.toml"));

    match ensure_default_config(&config_path) {
        Ok(true) => tracing::info!(path = %config_path.display(), "created default configuration"),
        Ok(false) => {}
        Err(error) => {
            eprintln!("appa runtime: cannot create {}: {error}", config_path.display());
            return ExitCode::FAILURE;
        }
    }
    let battery_dirs = match crate::batteries::prepare(&args.batteries_dir) {
        Ok(dirs) => dirs,
        Err(error) => {
            eprintln!("appa runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    let config = match Config::load_from(&config_path, &battery_dirs) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("appa runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    // A served deployment answers one host, and the adapter is that host: it derives the
    // canonical identity the policy must name, its inverse spells a recorded name back for
    // the model, and its rule settles which contracts release a spawn.
    let adapter = served(args.adapter);
    let battery_state = Arc::new(RwLock::new(mcp::BatteryState {
        catalog: crate::batteries::snapshot(&battery_dirs),
        included: config.included_batteries().iter().cloned().collect::<BTreeSet<_>>(),
        serving_tools: config.tool_names().into_iter().collect(),
    }));
    let runtime = match Runtime::open_served(config, args.db, args.modules_dir, adapter) {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("appa runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    let runtime = if let Some(workspace) = args.file_workspace {
        let configure = || -> Result<Runtime, String> {
            let workspace = fs::canonicalize(workspace).map_err(|error| error.to_string())?;
            if fs::canonicalize(&config_path)
                .map_err(|error| error.to_string())?
                .starts_with(&workspace)
            {
                return Err("file tracking requires configuration outside the workspace".into());
            }
            let initial = match args.initialize_file_trust {
                Some(trust) => {
                    use appa_engine::label::{ChainAudience, Clause, DeclaredAudience};
                    let audience = match args.initialize_file_audience.as_deref() {
                        Some("public") => DeclaredAudience::Public,
                        Some("internal") => DeclaredAudience::Union(
                            Clause::new([ChainAudience::Internal], [], []).map_err(|e| e.to_string())?,
                        ),
                        Some("self") => DeclaredAudience::Union(
                            Clause::new([ChainAudience::Self_], [], []).map_err(|e| e.to_string())?,
                        ),
                        _ => return Err("initialization requires --initialize-file-audience".into()),
                    };
                    Some(
                        runtime
                            .file_initial_label(&trust, audience)
                            .map_err(|error| error.to_string())?,
                    )
                }
                None => None,
            };
            let runtime = runtime
                .with_file_tracking(workspace, args.file_ledger.expect("clap requires a ledger"), initial)
                .map_err(|error| error.to_string())?;
            match args.file_process_backend {
                Some(backend) => runtime
                    .with_file_process_backend(backend)
                    .map_err(|error| error.to_string()),
                None => Ok(runtime),
            }
        };
        match configure() {
            Ok(runtime) => runtime,
            Err(error) => {
                eprintln!("appa runtime: {error}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        runtime
    };
    let runtime = Arc::new(runtime);
    // Every audience source the policy references answers once before the runtime serves:
    // a source that is down or reports a malformed reader stops the start here.
    if let Err(error) = runtime.probe_sources().await {
        eprintln!("appa runtime: {error}");
        return ExitCode::FAILURE;
    }

    let state = AppState {
        runtime: Arc::clone(&runtime),
        adapter,
        config: config_path,
        battery_state: Arc::clone(&battery_state),
        battery_dirs,
        reload_gate: Arc::new(tokio::sync::Mutex::new(())),
        executable: ExecutableAtStart::of_this_process(),
    };
    let management = axum::Router::new()
        .route("/binary-fingerprint", get(binary_fingerprint))
        .route("/policy-key", get(policy_key))
        .route("/file-tools", get(file_tools))
        .route("/status", get(status))
        .route("/report", post(report))
        .route("/reload", post(reload))
        .route_layer(axum::middleware::from_fn(loopback_management_only));
    let app = axum::Router::new()
        .route("/health", get(health))
        .route("/batteries", get(batteries))
        .route("/hook", post(hook))
        .route(
            "/validate",
            post(validate_tools).layer(axum::extract::DefaultBodyLimit::max(
                appa_runtime_api::inventory::MAX_INVENTORY_BYTES,
            )),
        )
        .nest_service(
            "/mcp",
            mcp::service_with_allowed_hosts(Arc::clone(&runtime), &args.mcp_allowed_hosts, args.adapter),
        )
        .merge(management)
        .with_state(state);

    let listener = match tokio::net::TcpListener::bind(args.listen).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("appa runtime: cannot bind {}: {error}", args.listen);
            return ExitCode::FAILURE;
        }
    };
    let guide = if let Some(address) = args.guide_listen {
        let listener = match tokio::net::TcpListener::bind(address).await {
            Ok(listener) => listener,
            Err(error) => {
                eprintln!("appa runtime: cannot bind guide listener {address}: {error}");
                return ExitCode::FAILURE;
            }
        };
        let app = axum::Router::new().nest_service(
            "/guide-mcp",
            mcp::guide_service_with_allowed_hosts(runtime, battery_state, &args.mcp_allowed_hosts),
        );
        Some((address, listener, app))
    } else {
        None
    };
    tracing::info!(
        listen = %args.listen,
        guide_listen = ?args.guide_listen,
        "appa-runtime serving /hook, /mcp, /health, and /batteries; management routes require loopback"
    );
    let result = if let Some((address, guide_listener, guide_app)) = guide {
        tracing::info!(listen = %address, "appa-runtime serving the vouched appa-guide MCP surface");
        tokio::select! {
            result = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()) => result,
            result = axum::serve(guide_listener, guide_app.into_make_service()) => result,
        }
    } else {
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("appa runtime: server failed: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn loopback_management_only(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    if !management_peer_is_allowed(peer) {
        return (StatusCode::FORBIDDEN, "management routes require a loopback peer").into_response();
    }
    next.run(request).await
}

fn management_peer_is_allowed(peer: SocketAddr) -> bool {
    peer.ip().is_loopback()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_config_is_created_without_replacing_an_existing_file() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("appa.toml");

        assert!(ensure_default_config(&path).expect("default config is created"));
        assert_eq!(
            fs::read_to_string(&path).expect("default config is readable"),
            default_config::text()
        );
        Config::load(&path).expect("the embedded default config validates");

        fs::write(&path, "existing deployment").expect("existing config is replaced by the test");
        assert!(!ensure_default_config(&path).expect("existing config is preserved"));
        assert_eq!(
            fs::read_to_string(path).expect("existing config is readable"),
            "existing deployment"
        );
    }

    #[test]
    fn the_runtime_defaults_to_loopback_and_accepts_an_explicit_non_loopback_address() {
        let default = Args::try_parse_from(["appa runtime"]).expect("the default runtime command parses");
        assert_eq!(default.listen, "127.0.0.1:8787".parse().expect("the default parses"));
        assert_eq!(
            default.guide_listen, None,
            "Claude Code exposes no guide management listener"
        );

        let shared = Args::try_parse_from(["appa runtime", "--listen", "0.0.0.0:18787"])
            .expect("an explicit shared-runtime address parses");
        assert_eq!(
            shared.listen,
            "0.0.0.0:18787".parse().expect("the shared address parses")
        );
        assert!(shared.mcp_allowed_hosts.is_empty());

        let hosted = Args::try_parse_from([
            "appa runtime",
            "--guide-listen",
            "0.0.0.0:18788",
            "--mcp-allowed-host",
            "appa-runtime.appa.svc.cluster.local:18787",
        ])
        .expect("an MCP Service host parses");
        assert_eq!(
            hosted.guide_listen,
            Some("0.0.0.0:18788".parse().expect("guide address"))
        );
        assert_eq!(hosted.mcp_allowed_hosts, ["appa-runtime.appa.svc.cluster.local:18787"]);
    }

    #[test]
    fn management_routes_accept_only_loopback_peers() {
        assert!(management_peer_is_allowed("127.0.0.1:1234".parse().unwrap()));
        assert!(management_peer_is_allowed("[::1]:1234".parse().unwrap()));
        assert!(!management_peer_is_allowed("10.0.0.8:1234".parse().unwrap()));
    }

    #[test]
    fn health_reports_a_replaced_executable_by_the_pid_to_stop() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("appa");
        fs::write(&path, "build one").expect("the executable is written");
        let started = ExecutableAtStart::snapshot(path.clone()).expect("the executable is readable");

        let replaced = |path: &Path| {
            started.differs_from(fs::metadata(path).and_then(|metadata| Ok((metadata.len(), metadata.modified()?))))
        };

        assert!(!replaced(&path));
        assert_eq!(health_answer(false, 41), "ok");

        let later = started.modified + std::time::Duration::from_secs(2);
        fs::File::open(&path)
            .and_then(|file| file.set_modified(later))
            .expect("the executable's timestamp is moved");
        assert!(replaced(&path));
        assert_eq!(health_answer(true, 41), "stale 41");

        fs::write(&path, "build two, longer").expect("the executable is replaced");
        assert!(replaced(&path));

        fs::remove_file(&path).expect("the executable is removed");
        assert!(replaced(&path));
    }

    #[test]
    fn the_binary_fingerprint_names_the_deployment_that_serves_it() {
        // The configuration is on its own line so a path holding spaces stays one value.
        assert_eq!(
            binary_fingerprint_answer("abc123", 41, Path::new("/home/user/Application Support/appa.toml")),
            "abc123 41\n/home/user/Application Support/appa.toml"
        );
    }

    #[test]
    fn verbosity_selects_the_level() {
        assert_eq!(log_level(0), "info");
        assert_eq!(log_level(1), "debug");
        assert_eq!(log_level(2), "trace");
    }
}
