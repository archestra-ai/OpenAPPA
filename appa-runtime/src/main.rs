//! Internal `appa runtime` command: an HTTP listener for hooks. Policy decisions live behind the runtime API; this file
//! parses flags, opens the runtime over an existing deployment config, picks the
//! adapter codec, and serves.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use appa_runtime_api::AdapterName;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use clap::Parser;
use sha2::{Digest, Sha256};

use crate::api::{Reloaded, Runtime};
use crate::config::{Config, InitialFileAudience};
use crate::{hooks, mcp};

#[derive(Parser)]
#[command(name = "appa runtime", version)]
struct Args {
    #[command(subcommand)]
    command: Option<RuntimeCommand>,

    #[arg(long, env = "APPA_CONFIG", global = true)]
    config: Option<PathBuf>,

    #[arg(long, env = "APPA_DB", default_value = "appa.db")]
    db: PathBuf,

    /// Host-installed agentsh backend directory for isolated declared-input processing.
    #[arg(long, env = "APPA_FILE_PROCESS_BACKEND")]
    file_process_backend: Option<PathBuf>,

    #[arg(long, env = "APPA_MODULES_DIR")]
    modules_dir: Option<PathBuf>,

    /// Directories of bundled batteries, in lookup order. First directory
    /// that contains `batteries/<name>/appa.toml`'s `<name>` wins. Colon
    /// separated when set through `APPA_BATTERIES_DIR`. None named: the
    /// deployment's store, `batteries/` beside the config.
    #[arg(
        long = "batteries-dir",
        env = "APPA_BATTERIES_DIR",
        value_delimiter = ':',
        action = clap::ArgAction::Append
    )]
    batteries_dir: Vec<PathBuf>,

    /// The address to serve. Port 0 takes a free port; the runtime prints the URL it
    /// serves as the one line on stdout once it listens.
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

#[derive(clap::Subcommand)]
enum RuntimeCommand {
    /// Bring the deployed runtime up when nothing healthy answers its endpoint.
    #[command(hide = true)]
    Ensure {
        #[command(flatten)]
        target: crate::runtime_url::RuntimeUrl,

        /// Where the started runtime keeps its database and logs; the installed data directory when absent.
        #[arg(long)]
        data_dir: Option<PathBuf>,
    },
    /// Stop the deployed runtime, when the process answering its endpoint is this user's own appa.
    Stop {
        #[command(flatten)]
        target: crate::runtime_url::RuntimeUrl,
    },
    /// Ask the policy's Annotators about the calls on standard input, without a session.
    #[command(hide = true)]
    Annotate {
        /// How many times each call is asked.
        #[arg(long, default_value_t = 1)]
        repeat: u32,

        /// How many calls are in flight at once.
        #[arg(long, default_value_t = 4)]
        concurrency: usize,
    },
}

/// `appa runtime ensure`: the start every protected SessionStart performs, run
/// on its own by the install as its last step, from the deployed binary.
fn ensure(target: &crate::runtime_url::RuntimeUrl, config: Option<PathBuf>, data_dir: Option<PathBuf>) -> ExitCode {
    let started = crate::runtime_start::Deployment::installed(config, data_dir).and_then(|deployment| {
        let executable = std::env::current_exe().map_err(|error| {
            crate::runtime_start::StartError::Paths(format!("this executable has no path to start from: {error}"))
        })?;
        // An install run from inside a Claude Code session must not hand that
        // session's credential to a runtime that outlives it.
        let withheld = appa_adapter_claude_code::environment::session_scoped(std::env::vars_os().map(|(name, _)| name));
        crate::runtime_start::ensure(&target.resolve(), &deployment, &executable, &withheld)
    });
    match started {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("appa runtime ensure: {error}");
            ExitCode::FAILURE
        }
    }
}

/// `appa runtime stop`: the deployed runtime goes, whichever install started it.
fn stop(target: &crate::runtime_url::RuntimeUrl) -> ExitCode {
    let target = target.resolve();
    match crate::runtime_start::stop(&target) {
        Ok(crate::runtime_start::Stopped::Nothing) => {
            println!("nothing answers {}", target.url);
            ExitCode::SUCCESS
        }
        Ok(crate::runtime_start::Stopped::Runtime { pid }) => {
            println!("stopped the runtime (pid {pid}) at {}", target.url);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("appa runtime stop: {error}");
            ExitCode::FAILURE
        }
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
    Ok(crate::engine::hex(&Sha256::digest(bytes)))
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

async fn file_tools(State(state): State<AppState>) -> Result<axum::Json<crate::api::files::Deployment>, StatusCode> {
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

/// What `appa ui` reads while this runtime serves: its configuration, the batteries it
/// includes, and which tokens and programs this process itself can reach. Loopback
/// management only.
async fn prerequisites(State(state): State<AppState>) -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "config": state.config,
        "included": state.runtime.included_batteries(),
        "prerequisites": crate::ui::runtime_prerequisites(&state.config, &state.battery_dirs).unwrap_or_default(),
    }))
}

/// Battery checks run in this process's environment, which is the one enforcement uses.
async fn battery_check(
    State(state): State<AppState>,
    axum::Json(names): axum::Json<Vec<String>>,
) -> Result<axum::Json<serde_json::Value>, StatusCode> {
    crate::ui::runtime_check(&state.config, &state.battery_dirs, &names)
        .await
        .map(axum::Json)
        .map_err(|_| StatusCode::BAD_REQUEST)
}

async fn reload(State(state): State<AppState>) -> Result<axum::Json<Reloaded>, (axum::http::StatusCode, String)> {
    let _reload = state.reload_gate.lock().await;
    let config = Config::load_local(&state.config, &state.battery_dirs)
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
        .prepare_deployment(config)
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
    /// A family root the caller identifies from its harness session. The runtime accepts it
    /// only when that root is in the same recent set used by an unnamed request.
    trajectory: Option<String>,
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
        // A caller gets the only recently active trajectory, or can narrow that same recent
        // set by root. It cannot ask for an older trajectory by name. This is still not a
        // per-caller boundary: a recent trajectory may belong to another session on this
        // machine, and loopback is the only thing between them.
        selection: crate::yell::Selection::Recent(body.trajectory.map(crate::api::TrajectoryId)),
        harness: crate::yell::report::Harness::served(state.adapter.name),
        hostname: None,
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
    let annotating = match args.command {
        Some(RuntimeCommand::Ensure { target, data_dir }) => return ensure(&target, args.config, data_dir),
        Some(RuntimeCommand::Stop { target }) => return stop(&target),
        Some(RuntimeCommand::Annotate { repeat, concurrency }) => Some((repeat, concurrency)),
        None => None,
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("appa runtime: cannot create async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    match annotating {
        Some((repeat, concurrency)) => runtime.block_on(annotate(args, repeat, concurrency)),
        None => runtime.block_on(serve(args)),
    }
}

fn load_config(config_path: &Path, batteries_dir: &[PathBuf]) -> Result<(Config, Vec<PathBuf>), String> {
    let battery_dirs = if batteries_dir.is_empty() {
        crate::batteries::default_search_path(config_path)
    } else {
        crate::batteries::prepare(batteries_dir)?
    };
    let config = Config::load_local(config_path, &battery_dirs).map_err(|error| error.to_string())?;
    Ok((config, battery_dirs))
}

/// `appa runtime annotate`: see [`crate::annotate`].
async fn annotate(args: Args, repeat: u32, concurrency: usize) -> ExitCode {
    let config_path = args.config.unwrap_or_else(|| PathBuf::from("appa.toml"));
    match load_config(&config_path, &args.batteries_dir) {
        Ok((config, _)) => crate::annotate::run(config, args.modules_dir, repeat, concurrency).await,
        Err(error) => {
            eprintln!("appa runtime annotate: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn serve(args: Args) -> ExitCode {
    let telemetry = crate::telemetry::Telemetry::init(log_level(args.verbose));
    let result = serve_inner(args, telemetry.enabled()).await;
    let _ = tokio::task::spawn_blocking(move || telemetry.shutdown()).await;
    result
}

async fn serve_inner(args: Args, telemetry_enabled: bool) -> ExitCode {
    let config_path = args.config.unwrap_or_else(|| PathBuf::from("appa.toml"));

    if let Err(error) = crate::runtime_start::require_policy(&config_path) {
        eprintln!("appa runtime: {error}");
        return ExitCode::FAILURE;
    }
    let (config, battery_dirs) = match load_config(&config_path, &args.batteries_dir) {
        Ok(loaded) => loaded,
        Err(error) => {
            eprintln!("appa runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    let file_tracking = config.file_tracking.clone();
    // A served deployment answers one host, and the adapter is that host: it identifies the
    // canonical identity the policy must name, its inverse spells a recorded name back for
    // the model, and its rule settles which contracts release a spawn.
    let adapter = crate::describe::served(args.adapter);
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
    let runtime = if let Some(file_tracking) = file_tracking {
        let configure = || -> Result<Runtime, String> {
            use appa_engine::label::{ChainAudience, Clause, DeclaredAudience};
            let audience = match file_tracking.initial_audience {
                InitialFileAudience::Public => DeclaredAudience::Public,
                InitialFileAudience::Internal => {
                    DeclaredAudience::Union(Clause::new([ChainAudience::Internal], [], []).map_err(|e| e.to_string())?)
                }
                InitialFileAudience::Self_ => {
                    DeclaredAudience::Union(Clause::new([ChainAudience::Self_], [], []).map_err(|e| e.to_string())?)
                }
            };
            let initial = runtime
                .file_initial_label(&file_tracking.initial_trust, audience)
                .map_err(|error| error.to_string())?;
            let runtime = runtime
                .with_file_tracking(initial, config_path.clone())
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
        if args.file_process_backend.is_some() {
            eprintln!("appa runtime: --file-process-backend requires [file_tracking] in the configuration");
            return ExitCode::FAILURE;
        }
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
        .route("/prerequisites", get(prerequisites))
        .route("/battery-check", post(battery_check))
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
            mcp::service_with_allowed_hosts(
                Arc::clone(&runtime),
                &args.mcp_allowed_hosts,
                crate::yell::Harness::served(args.adapter),
            ),
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
    let listen = match listener.local_addr() {
        Ok(listen) => listen,
        Err(error) => {
            eprintln!(
                "appa runtime: cannot read the bound address of {}: {error}",
                args.listen
            );
            return ExitCode::FAILURE;
        }
    };
    // The one line the runtime writes to stdout: the address it serves, so a caller that
    // asked for port 0 learns the port it got.
    println!("http://{listen}");
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
        listen = %listen,
        guide_listen = ?args.guide_listen,
        "appa-runtime serving /hook, /mcp, /health, and /batteries; management routes require loopback"
    );
    let serving = async move {
        if let Some((address, guide_listener, guide_app)) = guide {
            tracing::info!(listen = %address, "appa-runtime serving the vouched appa-guide MCP surface");
            tokio::select! {
                result = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()) => result,
                result = axum::serve(guide_listener, guide_app.into_make_service()) => result,
            }
        } else {
            axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await
        }
    };
    let result = if telemetry_enabled {
        tokio::select! {
            result = serving => result,
            _ = crate::telemetry::shutdown_signal() => Ok(()),
        }
    } else {
        serving.await
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
    if !management_peer_is_allowed(peer) || request.headers().contains_key("origin") {
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
    fn the_runtime_defaults_to_loopback_and_accepts_an_explicit_non_loopback_address() {
        let default = Args::try_parse_from(["appa runtime"]).expect("the default runtime command parses");
        assert_eq!(default.listen, "127.0.0.1:8787".parse().expect("the default parses"));
        assert_eq!(
            default.guide_listen, None,
            "Claude Code exposes no guide management listener"
        );
        assert!(
            Args::try_parse_from(["appa runtime", "--initial-file-trust", "suspicious"]).is_err(),
            "the retired initial Label flags are refused"
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
