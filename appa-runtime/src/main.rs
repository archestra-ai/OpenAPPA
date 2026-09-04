//! Internal `appa runtime` command: an HTTP listener for hooks. Policy decisions live behind the runtime API; this file
//! parses flags, initializes a missing deployment config, opens the
//! runtime, picks the adapter codec, and serves.

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use axum::extract::State;
use axum::routing::{get, post};
use clap::Parser;
use sha2::{Digest, Sha256};

use crate::api::{Reloaded, Runtime};
use crate::config::Config;
use crate::default_config;
use crate::{hooks, mcp};
use appa_runtime_api::Codec;

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

    #[arg(long, value_enum, default_value_t = Adapter::ClaudeCode, global = true)]
    adapter: Adapter,

    #[arg(short, action = clap::ArgAction::Count)]
    verbose: u8,
}

fn require_loopback(addr: &SocketAddr) -> Result<(), String> {
    if addr.ip().is_loopback() {
        Ok(())
    } else {
        Err(format!("refusing to listen on non-loopback address {addr}"))
    }
}

/// The adapter surface this binary can serve. The one place harness
/// names appear in this crate: each variant maps to one codec crate.
///
/// Public because the MCP service is: the tools it serves name the harness in what they
/// build, so a caller that mounts the service chooses one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Adapter {
    ClaudeCode,
    Kagent,
}

impl Adapter {
    /// kagent's spawns are other agents called as tools: a child runs only under a contract
    /// that names the agent. Claude Code's `Task` keeps the wildcard's cover.
    fn spawn_coverage(self) -> crate::api::SpawnCoverage {
        match self {
            Adapter::ClaudeCode => crate::api::SpawnCoverage::Wildcard,
            Adapter::Kagent => crate::api::SpawnCoverage::Declared,
        }
    }

    fn codec(self) -> Codec {
        match self {
            Adapter::ClaudeCode => appa_adapter_claude_code::codec(),
            Adapter::Kagent => appa_adapter_kagent::codec(),
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
    let digest = Sha256::digest(bytes);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[derive(Clone)]
struct AppState {
    runtime: Arc<Runtime>,
    codec: Codec,
    /// Which harness this process serves. A report names it, because the same policy behaves
    /// differently under a harness that declares its spawns and one that does not.
    adapter: Adapter,
    config: PathBuf,
    battery_dirs: Vec<PathBuf>,
    batteries: Arc<RwLock<crate::batteries::BatteriesResponse>>,
    reload_gate: Arc<tokio::sync::Mutex<()>>,
    executable: Option<ExecutableAtStart>,
}

async fn hook(
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> (axum::http::StatusCode, axum::Json<serde_json::Value>) {
    let (status, body) = hooks::answer(&state.runtime, &state.codec, &body).await;
    let status = axum::http::StatusCode::from_u16(status).expect("hook answers carry valid status codes");
    (status, axum::Json(body))
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
        .batteries
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    axum::Json(catalog)
}

async fn reload(State(state): State<AppState>) -> Result<axum::Json<Reloaded>, (axum::http::StatusCode, String)> {
    let _reload = state.reload_gate.lock().await;
    let config = Config::load_from(&state.config, &state.battery_dirs)
        .map_err(|error| (axum::http::StatusCode::UNPROCESSABLE_ENTITY, error.to_string()))?;
    match state.runtime.reload(config) {
        Ok(reloaded) => {
            let catalog = crate::batteries::snapshot(&state.battery_dirs);
            *state
                .batteries
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = catalog;
            Ok(axum::Json(reloaded))
        }
        Err(refusal) => {
            tracing::warn!(%refusal, "the reload was refused; the running deployment keeps serving");
            Err((axum::http::StatusCode::UNPROCESSABLE_ENTITY, refusal.to_string()))
        }
    }
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
        harness: state.adapter,
    };
    state
        .runtime
        .report(request)
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

    if let Err(refusal) = require_loopback(&args.listen) {
        eprintln!("appa runtime: {refusal}");
        return ExitCode::FAILURE;
    }
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
    let runtime = match Runtime::open(config, args.db, args.modules_dir) {
        Ok(runtime) => Arc::new(runtime.with_spawn_coverage(args.adapter.spawn_coverage())),
        Err(error) => {
            eprintln!("appa runtime: {error}");
            return ExitCode::FAILURE;
        }
    };

    let state = AppState {
        runtime: Arc::clone(&runtime),
        codec: args.adapter.codec(),
        adapter: args.adapter,
        config: config_path,
        batteries: Arc::new(RwLock::new(crate::batteries::snapshot(&battery_dirs))),
        battery_dirs,
        reload_gate: Arc::new(tokio::sync::Mutex::new(())),
        executable: ExecutableAtStart::of_this_process(),
    };
    let app = axum::Router::new()
        .route("/health", get(health))
        .route("/binary-fingerprint", get(binary_fingerprint))
        .route("/policy-key", get(policy_key))
        .route("/batteries", get(batteries))
        .route("/status", get(status))
        .route("/report", post(report))
        .route("/hook", post(hook))
        .route("/reload", post(reload))
        .nest_service("/mcp", mcp::service(runtime, args.adapter))
        .with_state(state);

    let listener = match tokio::net::TcpListener::bind(args.listen).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("appa runtime: cannot bind {}: {error}", args.listen);
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(
        listen = %args.listen,
        "appa-runtime serving /hook, /mcp, /status, /reload, /health, /binary-fingerprint, /policy-key, and /batteries"
    );
    match axum::serve(listener, app).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("appa runtime: server failed: {error}");
            ExitCode::FAILURE
        }
    }
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
    fn a_non_loopback_listen_address_is_refused() {
        assert!(require_loopback(&"127.0.0.1:8787".parse().expect("parses")).is_ok());
        assert!(require_loopback(&"[::1]:8787".parse().expect("parses")).is_ok());
        assert!(require_loopback(&"0.0.0.0:8787".parse().expect("parses")).is_err());
        assert!(require_loopback(&"192.168.1.10:8787".parse().expect("parses")).is_err());
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
