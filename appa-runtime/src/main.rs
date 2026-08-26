//! Process entry: an HTTP listener for hooks. Policy decisions live behind the runtime API; this file
//! parses flags, initializes a missing deployment config, opens the
//! runtime, picks the adapter codec, and serves.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use axum::extract::State;
use axum::routing::{get, post};
use clap::Parser;

use appa_runtime::api::{Reloaded, Runtime};
use appa_runtime::config::Config;
use appa_runtime::{hooks, mcp};
use appa_runtime_api::Codec;

const DEFAULT_CONFIG: &str = include_str!("../../integrations/claude-code/examples/claude-code.appa.toml");
const SOURCE_BATTERY_INCLUDE: &str = "../../../batteries/claude-code/appa.toml";
const INSTALLED_BATTERY_INCLUDE: &str = "batteries/claude-code/appa.toml";

struct DefaultAsset {
    path: &'static str,
    contents: &'static str,
}

const DEFAULT_ASSETS: &[DefaultAsset] = &[DefaultAsset {
    path: "batteries/claude-code/appa.toml",
    contents: include_str!("../../batteries/claude-code/appa.toml"),
}];

fn installed_default_config() -> String {
    DEFAULT_CONFIG.replacen(SOURCE_BATTERY_INCLUDE, INSTALLED_BATTERY_INCLUDE, 1)
}

fn ensure_default_config(path: &Path) -> io::Result<bool> {
    let mut file = match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => return Err(error),
    };

    let root = path.parent().unwrap_or_else(|| Path::new("."));
    let config = installed_default_config();
    let mut created_assets = Vec::new();
    let write = (|| {
        for asset in DEFAULT_ASSETS {
            let asset_path = root.join(asset.path);
            if let Some(parent) = asset_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut output = match OpenOptions::new().write(true).create_new(true).open(&asset_path) {
                Ok(output) => output,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            };
            created_assets.push(asset_path);
            output
                .write_all(asset.contents.as_bytes())
                .and_then(|()| output.sync_all())?;
        }
        file.write_all(config.as_bytes()).and_then(|()| file.sync_all())
    })();
    if let Err(error) = write {
        drop(file);
        let _ = fs::remove_file(path);
        for asset in created_assets {
            let _ = fs::remove_file(asset);
        }
        return Err(error);
    }
    Ok(true)
}

#[derive(Parser)]
#[command(
    name = "appa-runtime",
    version = include_str!("../../version.txt").trim()
)]
struct Args {
    #[arg(long, env = "APPA_CONFIG", default_value = "appa.toml")]
    config: PathBuf,

    #[arg(long, env = "APPA_DB", default_value = "appa.db")]
    db: PathBuf,

    #[arg(long, env = "APPA_MODULES_DIR")]
    modules_dir: Option<PathBuf>,

    #[arg(long, default_value = "127.0.0.1:8787")]
    listen: SocketAddr,

    #[arg(long, value_enum, default_value_t = Adapter::ClaudeCode)]
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
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum Adapter {
    ClaudeCode,
}

impl Adapter {
    fn codec(self) -> Codec {
        match self {
            Adapter::ClaudeCode => appa_adapter_claude_code::codec(),
        }
    }
}

fn refuse_unobservable_returns(adapter: Adapter, policy: &toml::Value) -> Result<(), String> {
    match adapter {
        Adapter::ClaudeCode => {
            let controls_context = policy
                .get("deployment")
                .and_then(|deployment| deployment.get("context_control"))
                .and_then(toml::Value::as_bool)
                .unwrap_or(false);
            if !controls_context {
                return Ok(());
            }
            let tools = policy
                .get("tool")
                .and_then(toml::Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default();
            for tool in tools {
                let Some(name) = tool.get("name").and_then(toml::Value::as_str) else {
                    continue;
                };
                if (name == "Agent" || name == "Task") && !pins_foreground(tool) {
                    return Err(format!(
                        "this deployment controls the subagent's context, and its `{name}` tool does not pin                          `run_in_background` to `false` (`parameters.properties.run_in_background.const = false`):                          a background subagent returns where no hook can check it. Pin the argument, as the                          shipped examples do."
                    ));
                }
            }
            Ok(())
        }
    }
}

fn pins_foreground(tool: &toml::Value) -> bool {
    tool.get("parameters")
        .and_then(|parameters| parameters.get("properties"))
        .and_then(|properties| properties.get("run_in_background"))
        .and_then(|argument| argument.get("const"))
        .and_then(toml::Value::as_bool)
        == Some(false)
}

fn log_level(verbose: u8) -> &'static str {
    match verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    }
}

#[derive(Clone)]
struct AppState {
    runtime: Arc<Runtime>,
    codec: Codec,
    config: PathBuf,
    adapter: Adapter,
}

async fn hook(
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> (axum::http::StatusCode, axum::Json<serde_json::Value>) {
    let (status, body) = hooks::answer(&state.runtime, &state.codec, &body).await;
    let status = axum::http::StatusCode::from_u16(status).expect("hook answers carry valid status codes");
    (status, axum::Json(body))
}

async fn health() -> &'static str {
    "ok"
}

async fn reload(State(state): State<AppState>) -> Result<axum::Json<Reloaded>, (axum::http::StatusCode, String)> {
    let config = Config::load(&state.config)
        .map_err(|error| (axum::http::StatusCode::UNPROCESSABLE_ENTITY, error.to_string()))?;
    refuse_unobservable_returns(state.adapter, config.policy_file().value())
        .map_err(|refusal| (axum::http::StatusCode::UNPROCESSABLE_ENTITY, refusal))?;
    match state.runtime.reload(config) {
        Ok(reloaded) => Ok(axum::Json(reloaded)),
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
) -> Result<axum::Json<appa_runtime::api::TrajectoryStatus>, axum::http::StatusCode> {
    let query = query.map_err(|_| axum::http::StatusCode::BAD_REQUEST)?;
    let id = appa_runtime::api::TrajectoryId(query.0.trajectory);
    match state.runtime.status(&id) {
        Some(status) => Ok(axum::Json(status)),
        None => Err(axum::http::StatusCode::NOT_FOUND),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(log_level(args.verbose))),
        )
        .init();

    if let Err(refusal) = require_loopback(&args.listen) {
        eprintln!("appa-runtime: {refusal}");
        return ExitCode::FAILURE;
    }
    match ensure_default_config(&args.config) {
        Ok(true) => tracing::info!(path = %args.config.display(), "created default configuration"),
        Ok(false) => {}
        Err(error) => {
            eprintln!("appa-runtime: cannot create {}: {error}", args.config.display());
            return ExitCode::FAILURE;
        }
    }
    let config = match Config::load(&args.config) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("appa-runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(refusal) = refuse_unobservable_returns(args.adapter, config.policy_file().value()) {
        eprintln!("appa-runtime: {refusal}");
        return ExitCode::FAILURE;
    }
    let runtime = match Runtime::open(config, args.db, args.modules_dir) {
        Ok(runtime) => Arc::new(runtime),
        Err(error) => {
            eprintln!("appa-runtime: {error}");
            return ExitCode::FAILURE;
        }
    };

    let state = AppState {
        runtime: Arc::clone(&runtime),
        codec: args.adapter.codec(),
        config: args.config,
        adapter: args.adapter,
    };
    let app = axum::Router::new()
        .route("/health", get(health))
        .route("/status", get(status))
        .route("/hook", post(hook))
        .route("/reload", post(reload))
        .nest_service("/mcp", mcp::service(runtime))
        .with_state(state);

    let listener = match tokio::net::TcpListener::bind(args.listen).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("appa-runtime: cannot bind {}: {error}", args.listen);
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(listen = %args.listen, "appa-runtime serving /hook, /mcp, /status, /reload, and /health");
    match axum::serve(listener, app).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("appa-runtime: server failed: {error}");
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
            installed_default_config()
        );
        assert_eq!(
            fs::read_to_string(directory.path().join(DEFAULT_ASSETS[0].path)).expect("default battery is readable"),
            DEFAULT_ASSETS[0].contents
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
    fn verbosity_selects_the_level() {
        assert_eq!(log_level(0), "info");
        assert_eq!(log_level(1), "debug");
        assert_eq!(log_level(2), "trace");
    }
}
