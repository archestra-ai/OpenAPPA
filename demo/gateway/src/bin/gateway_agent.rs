//! `baton-gateway-agent`: a rig-core agent that plays the external harness. It talks
//! **directly** to the LLM (OpenRouter) — the policy lives entirely at the
//! tool layer — and takes every tool from the gateway over MCP, discovered
//! dynamically via `tools/list` (nothing is hardcoded: whatever `gateway.toml`
//! declares, plus `baton__escalate`, is what the model gets).

use clap::Parser;
use rig_core::client::CompletionClient;
use rig_core::completion::Prompt;
use rig_core::providers::openai;
use rmcp::ServiceExt;
use rmcp::transport::StreamableHttpClientTransport;

#[path = "../demo_support.rs"]
mod demo_support;
use demo_support::{ElicitingClient, clean_key, key_from_env_file};

const DEFAULT_TASK: &str = "Summarize this quarter's invoices and email the summary to our external \
     auditor at alex@finance-audit.com.";

const PREAMBLE: &str = "You are a finance assistant. Use the available tools to complete the task. Some tools are \
     policy-gated: a result may say a call was blocked and explain how to proceed — follow those instructions. \
     When you are done, summarize what happened for the user.";

#[derive(Parser)]
#[command(about = "Demo agent that drives baton-gateway through the soft-block/escalation flow")]
struct Args {
    #[arg(long, env = "BATON_GATEWAY_URL", default_value = "http://127.0.0.1:8732/mcp")]
    gateway_url: String,
    #[arg(long, env = "BATON_UPSTREAM_URL", default_value = "https://openrouter.ai/api/v1")]
    upstream_url: String,
    #[arg(long, env = "BATON_DEMO_MODEL", default_value = "anthropic/claude-sonnet-5")]
    model: String,
    #[arg(long, env = "OPENROUTER_API_KEY")]
    api_key: Option<String>,
    #[arg(long, default_value = DEFAULT_TASK)]
    task: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let api_key = args
        .api_key
        .map(|k| clean_key(&k))
        .filter(|k| !k.is_empty())
        .or_else(key_from_env_file)
        .ok_or("no OpenRouter API key: pass --api-key, set OPENROUTER_API_KEY, or add it to the repo-root .env")?;

    let transport = StreamableHttpClientTransport::from_uri(args.gateway_url.clone());
    let gateway = ElicitingClient::new("baton-gateway-agent")
        .serve(transport)
        .await
        .map_err(|e| {
            format!(
                "connecting to baton-gateway at {}: {e} (is it running?)",
                args.gateway_url
            )
        })?;
    let peer = gateway.peer().clone();

    // Dynamic discovery: every tool the gateway serves becomes an agent tool.
    let tools = peer.list_all_tools().await?;
    println!(
        "gateway tools: {}",
        tools.iter().map(|t| t.name.as_ref()).collect::<Vec<_>>().join(", ")
    );

    let client = openai::CompletionsClient::builder()
        .api_key(api_key)
        .base_url(&args.upstream_url)
        .build()?;
    let agent = client
        .agent(&args.model)
        .preamble(PREAMBLE)
        .rmcp_tools(tools, peer)
        .build();

    println!("task: {}\n", args.task);
    let answer = agent.prompt(args.task.as_str()).max_turns(12).await?;
    println!("\nagent: {answer}");

    gateway.cancel().await?;
    Ok(())
}
