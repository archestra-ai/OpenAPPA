//! `corp-agent`: the corporate assistant, mediated in-process by the embedded `appa-sdk`.
//!
//! The binary owns inference (OpenRouter's OpenAI-compatible endpoint) and tool execution (it
//! spawns `corp-systems-mcp` over stdio) — but every model completion passes through the SDK's
//! `mediate` before any tool runs, and every tool result passes through `report_outcome` before it
//! enters model context. The system preamble is pinned by the policy file, not by this binary.
//!
//! ```sh
//! corp-agent "Summarise Alice Chen's HR record"          # guarded appa-policy.toml
//! corp-agent --policy appa-policy-open.toml "..."        # the unmediated contrast
//! corp-agent --chat
//! ```
//!
//! Needs an OpenRouter key: `--api-key`, `OPENROUTER_API_KEY`, or a `.env` file
//! (crate-local `.env`, then the repository root — see `.env.example`).

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use appa_runtime::inference::Inference;
use appa_runtime::tool::HttpClient;
use appa_sdk::{AppaSession, Config, SdkOptions};
use clap::Parser;
use corporate_agent_demo::appa_loop::{self, mcp_tool_schema, resolve_policy, resolve_server_bin, spawn_corp_systems};
use corporate_agent_demo::{clean_key, load_dotenv, resolve_data_root};

const OPENROUTER_BASE: &str = "https://openrouter.ai/api/v1";

#[derive(Parser)]
#[command(about = "The corporate assistant over the mock corporate systems (MCP), mediated by the embedded appa-sdk")]
struct Args {
    /// The task for the agent. Omit to fall back to an interactive prompt (or use --chat).
    prompt: Option<String>,

    /// Interactive REPL instead of a single one-shot task.
    #[arg(long)]
    chat: bool,

    /// OpenRouter model id.
    #[arg(long, env = "APPA_DEMO_MODEL", default_value = "anthropic/claude-sonnet-5")]
    model: String,

    /// OpenRouter API key. Falls back to $OPENROUTER_API_KEY or a `.env` file.
    #[arg(long, env = "OPENROUTER_API_KEY")]
    api_key: Option<String>,

    /// Maximum inference rounds per turn.
    #[arg(long, default_value_t = 12)]
    max_rounds: u32,

    /// Path to the `corp-systems-mcp` binary. Defaults to a sibling of this executable.
    #[arg(long)]
    server_bin: Option<PathBuf>,

    /// Data root passed through to the spawned server.
    #[arg(long)]
    data_root: Option<PathBuf>,

    /// The APPA policy file. Defaults to $APPA_DEMO_POLICY, else the crate's appa-policy.toml.
    #[arg(long)]
    policy: Option<PathBuf>,

    /// Suppress the run log; print only the final answer.
    #[arg(long)]
    quiet: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load `.env` before parsing so the `env = "…"` clap fields pick up its values.
    let dotenv = load_dotenv();
    let args = Args::parse();
    if !args.quiet
        && let Some(path) = &dotenv
    {
        eprintln!("loaded env from {}", path.display());
    }

    let api_key = args
        .api_key
        .as_deref()
        .map(clean_key)
        .filter(|k| !k.is_empty())
        .context(
            "no OpenRouter API key: pass --api-key, set OPENROUTER_API_KEY, or add it to a .env file \
             (see .env.example)",
        )?;

    let policy_path = resolve_policy(args.policy);
    let policy_text = std::fs::read_to_string(&policy_path)
        .with_context(|| format!("reading the policy file {}", policy_path.display()))?;
    let config =
        Config::from_toml_str(&policy_text).with_context(|| format!("loading the policy {}", policy_path.display()))?;
    let mut session =
        AppaSession::open(config, SdkOptions::default()).context("opening the APPA session on this policy")?;

    let server_bin = resolve_server_bin(args.server_bin)?;
    let data_root = resolve_data_root(args.data_root);
    let server = spawn_corp_systems(&server_bin, &data_root).await?;

    let mcp_tools = server.peer().list_all_tools().await.context("listing MCP tools")?;
    let schemas: Vec<_> = mcp_tools.iter().map(mcp_tool_schema).collect();
    let tools = session
        .bind_tools(schemas)
        .context("binding the MCP tool surface against the policy registry")?
        .to_vec();
    if !args.quiet {
        let names: Vec<&str> = tools.iter().map(|t| t.function.name.as_str()).collect();
        eprintln!(
            "connected to corp-systems-mcp — policy {} — {} tools: {}",
            policy_path.display(),
            names.len(),
            names.join(", ")
        );
    }

    let inference = Inference::new(
        OPENROUTER_BASE,
        api_key,
        args.model.clone(),
        Duration::from_secs(120),
        HttpClient::new(),
    );

    let result = if args.chat {
        run_chat(&mut session, &inference, &server, &tools, args.max_rounds, args.quiet).await
    } else {
        run_once(
            &mut session,
            &inference,
            &server,
            &tools,
            args.max_rounds,
            args.quiet,
            args.prompt,
        )
        .await
    };

    // Reap the child regardless of how the run went.
    let _ = server.cancel().await;
    result
}

#[allow(clippy::too_many_arguments)]
async fn run_once(
    session: &mut AppaSession,
    inference: &Inference,
    server: &appa_loop::CorpSystemsClient,
    tools: &[appa_sdk::WireTool],
    max_rounds: u32,
    quiet: bool,
    prompt: Option<String>,
) -> anyhow::Result<()> {
    let prompt = match prompt {
        Some(p) => p,
        None => prompt_line("task> ")?.unwrap_or_default(),
    };
    if prompt.trim().is_empty() {
        anyhow::bail!("no task given: pass a prompt argument or use --chat");
    }
    let answer = appa_loop::run_turn(session, inference, server, tools, max_rounds, quiet, &prompt).await?;
    println!("\n=== answer ===\n{answer}");
    Ok(())
}

async fn run_chat(
    session: &mut AppaSession,
    inference: &Inference,
    server: &appa_loop::CorpSystemsClient,
    tools: &[appa_sdk::WireTool],
    max_rounds: u32,
    quiet: bool,
) -> anyhow::Result<()> {
    eprintln!("chat mode — type a message, or 'exit' to quit.");
    loop {
        let Some(line) = prompt_line("\nyou> ")? else {
            break;
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if matches!(line, "exit" | "quit") {
            break;
        }
        // History lives in the SDK's log: each turn extends the same session.
        let answer = appa_loop::run_turn(session, inference, server, tools, max_rounds, quiet, line).await?;
        println!("\n=== answer ===\n{answer}");
    }
    Ok(())
}

/// Read one line from stdin with a prompt; `None` on EOF.
fn prompt_line(label: &str) -> anyhow::Result<Option<String>> {
    let mut out = std::io::stdout();
    write!(out, "{label}")?;
    out.flush()?;
    let mut line = String::new();
    let n = std::io::stdin().read_line(&mut line)?;
    if n == 0 {
        return Ok(None);
    }
    Ok(Some(line.trim_end_matches(['\n', '\r']).to_string()))
}
