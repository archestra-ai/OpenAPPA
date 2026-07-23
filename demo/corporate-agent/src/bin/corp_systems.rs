//! `corp-systems-mcp`: the mock-corporate-systems MCP server over stdio.
//!
//! Runs standalone or (usually) spawned as a subprocess by `corp-agent`.
//!
//! ```sh
//! corp-systems-mcp                       # data root: ./data next to the crate
//! corp-systems-mcp --data-root /tmp/corp # override the data root
//! ```
//!
//! stdout is the JSON-RPC channel, so **all logging goes to stderr** — a stray
//! `println!` on stdout would corrupt the protocol framing.

use clap::Parser;
use corporate_agent_demo::{resolve_data_root, server::CorpSystems};
use rmcp::ServiceExt;
use rmcp::transport::stdio;
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "MCP server exposing mock corporate systems (hr, finance, task_tracker, internet) as folders")]
struct Args {
    /// Root directory holding the per-system folders. Defaults to `CORP_DATA_ROOT`
    /// or the crate's `data/` directory.
    #[arg(long, env = "CORP_DATA_ROOT")]
    data_root: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // stderr only: stdout carries the MCP protocol.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()))
        .init();

    let args = Args::parse();
    let root = resolve_data_root(args.data_root);
    tracing::info!(data_root = %root.display(), "corp-systems-mcp starting");

    let service = CorpSystems::new(root).serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
