//! Process entry: an HTTP listener for hooks. No policy, no state.
//! Policy lives behind the runtime
//! API; this file only parses flags, opens the runtime, picks the
//! adapter codec, and serves.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use axum::extract::State;
use axum::routing::{get, post};
use clap::Parser;

use appa_runtime_api::Codec;
use appa_runtime_v2::api::Runtime;
use appa_runtime_v2::config::Config;
use appa_runtime_v2::{hooks, mcp};

#[derive(Parser)]
#[command(name = "appa-runtime-v2")]
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

    fn substitutes_child_returns(self) -> bool {
        match self {
            Adapter::ClaudeCode => false,
        }
    }
}

fn refuse_unenforceable(adapter: Adapter, policy: &toml::Value) -> Result<(), String> {
    if binds_return_sanitizer(policy) && !adapter.substitutes_child_returns() {
        return Err(
            "this policy binds a child return_sanitizer, and the selected harness cannot substitute \
             a finished child's return — it would cross as the child spelled it. Serve a harness \
             that can, or drop the binding."
                .to_string(),
        );
    }
    Ok(())
}

fn binds_return_sanitizer(policy: &toml::Value) -> bool {
    policy
        .get("child")
        .and_then(|child| child.get("return_sanitizer"))
        .is_some()
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

#[derive(serde::Deserialize)]
struct StatusQuery {
    trajectory: String,
}

async fn status(
    State(state): State<AppState>,
    query: Result<axum::extract::Query<StatusQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<axum::Json<appa_runtime_v2::api::TrajectoryStatus>, axum::http::StatusCode> {
    let query = query.map_err(|_| axum::http::StatusCode::BAD_REQUEST)?;
    let id = appa_runtime_v2::api::TrajectoryId(query.0.trajectory);
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
        eprintln!("appa-runtime-v2: {refusal}");
        return ExitCode::FAILURE;
    }
    let config = match Config::load(&args.config) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("appa-runtime-v2: {error}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(refusal) = refuse_unenforceable(args.adapter, config.policy_file().value()) {
        eprintln!("appa-runtime-v2: {refusal}");
        return ExitCode::FAILURE;
    }
    let runtime = match Runtime::open(config, args.db, args.modules_dir) {
        Ok(runtime) => Arc::new(runtime),
        Err(error) => {
            eprintln!("appa-runtime-v2: {error}");
            return ExitCode::FAILURE;
        }
    };

    let state = AppState {
        runtime: Arc::clone(&runtime),
        codec: args.adapter.codec(),
    };
    let app = axum::Router::new()
        .route("/health", get(health))
        .route("/status", get(status))
        .route("/hook", post(hook))
        .nest_service("/mcp", mcp::service(runtime))
        .with_state(state);

    let listener = match tokio::net::TcpListener::bind(args.listen).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("appa-runtime-v2: cannot bind {}: {error}", args.listen);
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(listen = %args.listen, "appa-runtime-v2 serving /hook, /mcp, /status, and /health");
    match axum::serve(listener, app).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("appa-runtime-v2: server failed: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn a_bound_return_sanitizer_refuses_a_harness_that_cannot_substitute() {
        let bound = r#"
version = 1

[[tool]]
name = "read_hr"
delta = { audience = { exactly = ["hr"] } }

[[sanitizer]]
name = "redact-email"
on = ["tool_output"]
mandate = { audience = { from = { includes = ["hr"] }, to = { exactly = ["public"] } } }

[child]
return_sanitizer = "redact-email"

[deployment]
context_control = true
confined_child_return = true
"#;
        appa_policy::Config::from_toml_str(bound).expect("the fixture is a policy the dialect accepts");
        let bound: toml::Value = toml::from_str(bound).expect("the fixture parses");
        assert!(binds_return_sanitizer(&bound));
        assert!(refuse_unenforceable(Adapter::ClaudeCode, &bound).is_err());

        let unbound: toml::Value = toml::from_str("version = 1\n").expect("the fixture parses");
        assert!(!binds_return_sanitizer(&unbound));
        assert!(refuse_unenforceable(Adapter::ClaudeCode, &unbound).is_ok());
    }
}
