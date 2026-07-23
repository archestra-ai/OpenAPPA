//! `corp-agent`: a [`rig`](https://docs.rs/rig-core) agent, on OpenRouter's
//! OpenAI-compatible endpoint, that drives the mock corporate systems over MCP.
//!
//! It spawns `corp-systems-mcp` as a subprocess (stdio), registers every tool
//! the server advertises, and runs the model's tool loop — one-shot by default,
//! or an interactive `--chat` REPL. A [`PrettyLog`](corporate_agent_demo::logview::PrettyLog)
//! prints the system prompt, each tool call and its arguments, each tool result,
//! and the assistant's text as the run unfolds.
//!
//! ```sh
//! corp-agent "Summarise Alice Chen's HR record"
//! corp-agent --chat
//! ```
//!
//! Needs an OpenRouter key: `--api-key`, `OPENROUTER_API_KEY`, or a `.env` file
//! (crate-local `.env`, then the repository root — see `.env.example`).

use std::io::Write;
use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use corporate_agent_demo::logview::PrettyLog;
use corporate_agent_demo::{clean_key, load_dotenv, resolve_data_root};
use rig::client::CompletionClient;
use rig::completion::Prompt;
use rig::message::Message;
use rig::providers::openrouter;
use rmcp::ServiceExt;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use tokio::process::Command;

const PREAMBLE: &str = "You are a corporate assistant with access to the company's internal systems — HR, finance, \
     the task tracker — and a public forum, plus the ability to send email. Use the tools to complete the \
     user's request. Read what you need, then act. When you are done, briefly summarise what you did.";

#[derive(Parser)]
#[command(about = "A rig/OpenRouter agent over the mock corporate systems (MCP), for exercising OpenAPPA")]
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

    /// Maximum model turns per request (the tool loop bound).
    #[arg(long, default_value_t = 12)]
    max_turns: usize,

    /// Path to the `corp-systems-mcp` binary. Defaults to a sibling of this executable.
    #[arg(long)]
    server_bin: Option<PathBuf>,

    /// Data root passed through to the spawned server.
    #[arg(long)]
    data_root: Option<PathBuf>,

    /// Suppress the pretty agent log; print only the final answer.
    #[arg(long)]
    quiet: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load `.env` before parsing so the `env = "…"` clap fields pick up its
    // values (crate-local `.env`, then the repository-root one).
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

    let server_bin = resolve_server_bin(args.server_bin)?;
    let data_root = resolve_data_root(args.data_root);

    // Spawn the MCP server as a child over stdio. Its stderr is inherited (the
    // default), so its own logging shows in the terminal alongside ours.
    let transport = TokioChildProcess::new(Command::new(&server_bin).configure(|cmd| {
        cmd.arg("--data-root").arg(&data_root);
    }))
    .with_context(|| format!("spawning MCP server at {}", server_bin.display()))?;
    let server = ().serve(transport).await.context("MCP handshake with corp-systems-mcp failed")?;

    let tools = server.peer().list_all_tools().await.context("listing MCP tools")?;
    let sink = server.peer().clone();
    if !args.quiet {
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        eprintln!(
            "connected to corp-systems-mcp — {} tools: {}",
            names.len(),
            names.join(", ")
        );
    }

    let client = openrouter::Client::new(&api_key).context("building OpenRouter client")?;
    let agent = client
        .agent(args.model.clone())
        .preamble(PREAMBLE)
        .default_max_turns(args.max_turns)
        .add_hook(PrettyLog::new(PREAMBLE, !args.quiet))
        .rmcp_tools(tools, sink)
        .build();

    let result = if args.chat {
        run_chat(&agent).await
    } else {
        run_once(&agent, args.prompt).await
    };

    // Reap the child regardless of how the run went.
    let _ = server.cancel().await;
    result
}

async fn run_once(
    agent: &rig::agent::Agent<impl rig::completion::CompletionModel + 'static>,
    prompt: Option<String>,
) -> anyhow::Result<()> {
    let prompt = match prompt {
        Some(p) => p,
        None => prompt_line("task> ")?.unwrap_or_default(),
    };
    if prompt.trim().is_empty() {
        anyhow::bail!("no task given: pass a prompt argument or use --chat");
    }
    let answer = agent.prompt(prompt.as_str()).await.context("agent run failed")?;
    println!("\n=== answer ===\n{answer}");
    Ok(())
}

async fn run_chat(agent: &rig::agent::Agent<impl rig::completion::CompletionModel + 'static>) -> anyhow::Result<()> {
    eprintln!("chat mode — type a message, or 'exit' to quit.");
    let mut history: Vec<Message> = Vec::new();
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
        let resp = agent
            .prompt(line)
            .history(history.clone())
            .extended_details()
            .await
            .context("agent run failed")?;
        println!("\n=== answer ===\n{}", resp.output);
        if let Some(messages) = resp.messages {
            history = messages;
        }
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

/// Default the server binary to a sibling of the current executable.
fn resolve_server_bin(explicit: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path);
    }
    let exe = std::env::current_exe().context("locating the current executable")?;
    let dir = exe.parent().context("current executable has no parent directory")?;
    let name = if cfg!(windows) {
        "corp-systems-mcp.exe"
    } else {
        "corp-systems-mcp"
    };
    Ok(dir.join(name))
}
