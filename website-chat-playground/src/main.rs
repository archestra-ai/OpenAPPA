//! `appa-demo`: serve the chat playground.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use appa_example_agent::Endpoint;
use clap::Parser;
use website_chat_playground::api::{AppState, router};
use website_chat_playground::session::Sessions;

#[derive(Parser)]
#[command(
    about = "The openappa.com chat-playground service: sessions over the appa-example-agent loop, tools in-process"
)]
struct Args {
    #[arg(long, default_value = "127.0.0.1:8787")]
    listen: SocketAddr,

    #[arg(long, env = "APPA_DEMO_WORLD")]
    data_root: Option<PathBuf>,

    #[arg(long)]
    worlds_root: Option<PathBuf>,

    #[arg(long = "cors-origin")]
    cors_origins: Vec<String>,

    #[arg(long, default_value_t = 1800)]
    session_ttl_secs: u64,

    #[arg(long, env = "APPA_DEMO_OPENROUTER_API_KEY", hide_env_values = true)]
    openrouter_key: Option<String>,

    #[arg(
        long,
        env = "APPA_DEMO_INFERENCE_BASE_URL",
        default_value = "https://openrouter.ai/api/v1"
    )]
    inference_base_url: String,

    #[arg(long, default_value_t = 30)]
    max_turns: u32,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let seed = args
        .data_root
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("world"));
    anyhow::ensure!(seed.is_dir(), "seed world {} is not a directory", seed.display());
    let worlds = args
        .worlds_root
        .unwrap_or_else(|| std::env::temp_dir().join("appa-demo-worlds"));
    std::fs::create_dir_all(&worlds).with_context(|| format!("creating worlds root {}", worlds.display()))?;

    let mut origins = vec!["http://localhost:4321".to_string(), "http://127.0.0.1:4321".to_string()];
    origins.extend(args.cors_origins);

    let sessions = Arc::new(Sessions::new(
        seed.clone(),
        worlds.clone(),
        Duration::from_secs(args.session_ttl_secs),
        Endpoint::new(args.inference_base_url),
    ));
    sessions.spawn_expiry();

    if args.openrouter_key.is_none() {
        eprintln!("appa-demo: no OpenRouter key (APPA_DEMO_OPENROUTER_API_KEY); turns will be refused");
    }
    let app = router(AppState {
        sessions,
        origins: Arc::new(origins),
        openrouter_key: args.openrouter_key.map(Arc::new),
        max_turns: args.max_turns,
    });

    let listener = tokio::net::TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("binding {}", args.listen))?;
    eprintln!(
        "appa-demo: listening on {} — seed world {}, worlds under {}",
        args.listen,
        seed.display(),
        worlds.display()
    );
    axum::serve(listener, app).await.context("serving")?;
    Ok(())
}
