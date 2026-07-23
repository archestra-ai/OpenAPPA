//! `corp-agent`: the corporate assistant as a normal [`rig`](https://docs.rs/rig-core) agent, with
//! OpenAPPA dropped in via a mediation hook.
//!
//! rig owns the loop, the model conversation, and the tool schemas; the embedded `appa-sdk`
//! [`CallSession`], driven by [`AppaHook`], mediates every proposed tool call before it runs and
//! admits or seals every result. The system prompt is agent config (this binary); the policy file
//! governs flows only — labels, contracts, authorities — never the model's instructions.
//!
//! ```sh
//! corp-agent "Summarise Alice Chen's HR record"          # guarded appa-policy.toml
//! corp-agent --policy appa-policy-open.toml "..."        # the unmediated contrast
//! corp-agent --chat
//! ```
//!
//! Needs an OpenRouter key: `--api-key`, `OPENROUTER_API_KEY`, or a `.env` file.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use appa_sdk::{CallSession, Config, SdkOptions};
use clap::Parser;
use corporate_agent_demo::appa_hook::{AppaHook, RemedyTool};
use corporate_agent_demo::mcp::{self, BODY_CAP_BYTES, mcp_tool_schema, resolve_policy, resolve_server_bin};
use corporate_agent_demo::{clean_key, load_dotenv, resolve_data_root};
use rig::client::CompletionClient;
use rig::completion::Prompt;
use rig::message::Message;
use rig::providers::openrouter;
use tokio::sync::Mutex;

/// The agent's system prompt. This is *agent* configuration, not policy — the policy file governs
/// flows (labels, contracts, authorities), never the model's instructions.
const PREAMBLE: &str = "You are a corporate assistant. Use the available tools to complete the user's \
     request. Read what you need, then act. When you are done, briefly summarise what you did.";

#[derive(Parser)]
#[command(about = "The corporate assistant (rig agent) over the mock corporate systems (MCP), mediated by appa-sdk")]
struct Args {
    /// The task for the agent. Omit to fall back to an interactive prompt (or use --chat).
    prompt: Option<String>,

    /// Interactive REPL instead of a single one-shot task.
    #[arg(long)]
    chat: bool,

    /// OpenRouter model id.
    #[arg(long, env = "APPA_DEMO_MODEL", default_value = "openai/gpt-5.6-luna")]
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

    /// The APPA policy file. Defaults to $APPA_DEMO_POLICY, else the crate's appa-policy.toml.
    #[arg(long)]
    policy: Option<PathBuf>,

    /// Suppress the mediation log; print only the final answer.
    #[arg(long)]
    quiet: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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
    let mut session = CallSession::open(config, SdkOptions::default()).context("opening the APPA session")?;

    let server_bin = resolve_server_bin(args.server_bin)?;
    let data_root = resolve_data_root(args.data_root);
    let server = mcp::spawn_corp_systems(&server_bin, &data_root).await?;

    let mcp_tools = server.peer().list_all_tools().await.context("listing MCP tools")?;
    let schemas: Vec<_> = mcp_tools.iter().map(mcp_tool_schema).collect();
    session
        .bind_tools(schemas)
        .context("binding the MCP tool surface against the policy registry")?;
    if !args.quiet {
        let names: Vec<&str> = mcp_tools.iter().map(|t| t.name.as_ref()).collect();
        eprintln!(
            "connected to corp-systems-mcp — policy {} — {} tools (+ execute_remedy_plan): {}",
            policy_path.display(),
            names.len(),
            names.join(", ")
        );
    }

    let session = Arc::new(Mutex::new(session));
    let sink = server.peer().clone();
    let hook = AppaHook::new(session.clone(), server.peer().clone(), BODY_CAP_BYTES, args.quiet);

    let client = openrouter::Client::new(&api_key).context("building OpenRouter client")?;
    // Tool concurrency stays at rig's default of 1 (sequential) — the SDK's serial
    // check-against-prior-result invariant depends on it; do not raise it.
    let agent = client
        .agent(args.model.clone())
        .preamble(PREAMBLE)
        .default_max_turns(args.max_turns)
        .add_hook(hook)
        .tool(RemedyTool)
        .rmcp_tools(mcp_tools, sink)
        .build();

    let result = if args.chat {
        run_chat(&agent, &session).await
    } else {
        run_once(&agent, &session, args.prompt).await
    };

    let _ = server.cancel().await;
    result
}

async fn run_once(
    agent: &rig::agent::Agent<impl rig::completion::CompletionModel + 'static>,
    session: &Arc<Mutex<CallSession>>,
    prompt: Option<String>,
) -> anyhow::Result<()> {
    let prompt = match prompt {
        Some(p) => p,
        None => prompt_line("task> ")?.unwrap_or_default(),
    };
    if prompt.trim().is_empty() {
        anyhow::bail!("no task given: pass a prompt argument or use --chat");
    }
    let answer = drive_turn(agent, session, &prompt, None).await?;
    println!("\n=== answer ===\n{}", answer.0);
    Ok(())
}

async fn run_chat(
    agent: &rig::agent::Agent<impl rig::completion::CompletionModel + 'static>,
    session: &Arc<Mutex<CallSession>>,
) -> anyhow::Result<()> {
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
        let (answer, messages) = drive_turn(agent, session, line, Some(history.clone())).await?;
        println!("\n=== answer ===\n{answer}");
        if let Some(messages) = messages {
            history = messages;
        }
    }
    Ok(())
}

/// Drive one turn: admit the user turn into APPA, run rig's loop (the hook mediates), close the
/// APPA turn. Returns the answer and, for chat, the updated rig history.
async fn drive_turn(
    agent: &rig::agent::Agent<impl rig::completion::CompletionModel + 'static>,
    session: &Arc<Mutex<CallSession>>,
    prompt: &str,
    history: Option<Vec<Message>>,
) -> anyhow::Result<(String, Option<Vec<Message>>)> {
    session
        .lock()
        .await
        .begin_turn(prompt)
        .context("admitting the user turn")?;
    let outcome = match history {
        Some(history) => {
            let resp = agent
                .prompt(prompt)
                .history(history)
                .extended_details()
                .await
                .context("agent run failed");
            resp.map(|r| (r.output, r.messages))
        }
        None => agent
            .prompt(prompt)
            .await
            .map(|o| (o, None))
            .context("agent run failed"),
    };
    // Close the APPA turn whether the run succeeded or not, so the trajectory is punctuated.
    session.lock().await.end_turn().context("closing the APPA turn")?;
    outcome
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
