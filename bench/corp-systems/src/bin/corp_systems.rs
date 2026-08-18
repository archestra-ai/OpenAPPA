//! `corp-systems-mcp`: the mock-corporate-systems MCP server over stdio.
//!
//! Runs standalone or (usually) spawned as a subprocess by one of the sibling
//! demo agents (`corp-agent`, `corp-agent-fides`).
//!
//! ```sh
//! corp-systems-mcp                       # data root: ./data next to the crate
//! corp-systems-mcp --data-root /tmp/corp # override the data root
//! corp-systems-mcp --sink-root /tmp/out  # send_email writes under here instead
//! ```
//!
//! stdout is the JSON-RPC channel, so **all logging goes to stderr** — a stray
//! `println!` on stdout would corrupt the protocol framing.

use anyhow::Context;
use clap::Parser;
use corp_systems::systems::System;
use corp_systems::{resolve_data_root, server::CorpSystems};
use rmcp::ServiceExt;
use rmcp::transport::stdio;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    about = "MCP server exposing mock corporate systems (hr, finance, task_tracker, public_forum, vendor) as folders"
)]
struct Args {
    /// Root directory holding the per-system folders. Defaults to `CORP_DATA_ROOT`
    /// or the crate's `data/` directory.
    #[arg(long, env = "CORP_DATA_ROOT")]
    data_root: Option<PathBuf>,

    /// Root directory `send_email` writes its `email/` folder under. Defaults to
    /// `CORP_SINK_ROOT`, else the data root — split it when the corpus is shared
    /// and the observable sink should stay local to one demo.
    #[arg(long, env = "CORP_SINK_ROOT")]
    sink_root: Option<PathBuf>,

    /// Comma-separated systems to enable, e.g. `hr,public_forum,email`.
    /// Defaults to `CORP_ENABLED_SYSTEMS`, else all six. A disabled system's
    /// tools are absent from `list_tools` and refused when called.
    #[arg(long, env = "CORP_ENABLED_SYSTEMS")]
    systems: Option<String>,
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
    let corpus_root = resolve_data_root(args.data_root);
    let sink_root = args.sink_root.unwrap_or_else(|| corpus_root.clone());
    let enabled = match args.systems.as_deref() {
        Some(list) => System::parse_list(list).context("parsing --systems / CORP_ENABLED_SYSTEMS")?,
        None => System::ALL.into_iter().collect(),
    };
    tracing::info!(
        corpus_root = %corpus_root.display(),
        sink_root = %sink_root.display(),
        systems = %enabled.iter().map(|s| s.dir_name()).collect::<Vec<_>>().join(","),
        "corp-systems-mcp starting"
    );

    let service = CorpSystems::new(corpus_root, sink_root, enabled).serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
