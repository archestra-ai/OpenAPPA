//! The mediated agent loop: the host side of the `appa-sdk` contract, shared by the `corp-agent`
//! binary and the e2e test so the test exercises the exact loop the demo runs.
//!
//! The host owns inference (OpenRouter via `appa-runtime`'s client) and tool execution (MCP to
//! `corp-systems-mcp`); the SDK owns the trajectory, the transcript, and every policy decision.
//! The loop honors the three host commitments: model context comes only from `transcript()`, only
//! the one surfaced call is executed and reported before anything else, and the advertised tools
//! are exactly the bound surface.

use std::path::PathBuf;

use anyhow::Context;
use appa_runtime::inference::Inference;
use appa_runtime::wire::ChatCompletionRequest;
use appa_sdk::{AdmittedResult, AppaSession, BodyDisposition, RenderedCall, Step, ToolOutcome, WireTool};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use tokio::process::Command;

/// The MCP connection to the spawned `corp-systems-mcp`.
pub type CorpSystemsClient = RunningService<RoleClient, ()>;

/// The largest tool-result body admitted as a value, mirroring the runtime's default cap.
pub const BODY_CAP_BYTES: usize = appa_runtime::tool::DEFAULT_BODY_CAP_BYTES;

/// Spawn `corp-systems-mcp` as a stdio child and complete the MCP handshake. Its stderr is
/// inherited, so the server's own logging shows in the terminal alongside ours.
pub async fn spawn_corp_systems(server_bin: &PathBuf, data_root: &PathBuf) -> anyhow::Result<CorpSystemsClient> {
    let transport = TokioChildProcess::new(Command::new(server_bin).configure(|cmd| {
        cmd.arg("--data-root").arg(data_root);
    }))
    .with_context(|| format!("spawning MCP server at {}", server_bin.display()))?;
    ().serve(transport)
        .await
        .context("MCP handshake with corp-systems-mcp failed")
}

/// Default the server binary to a sibling of the current executable.
pub fn resolve_server_bin(explicit: Option<PathBuf>) -> anyhow::Result<PathBuf> {
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

/// Resolve the policy file: an explicit override, else `APPA_DEMO_POLICY`, else the guarded
/// `appa-policy.toml` next to this crate's manifest.
pub fn resolve_policy(explicit: Option<PathBuf>) -> PathBuf {
    if let Some(path) = explicit {
        return path;
    }
    if let Ok(env) = std::env::var("APPA_DEMO_POLICY")
        && !env.trim().is_empty()
    {
        return PathBuf::from(env);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("appa-policy.toml")
}

/// Render an MCP tool listing entry as the wire schema the model sees. The registry stays the
/// policy authority (`bind_tools` validates names); the MCP server contributes presentation —
/// description and parameter schema.
pub fn mcp_tool_schema(tool: &rmcp::model::Tool) -> WireTool {
    WireTool {
        kind: "function".to_string(),
        function: appa_sdk::WireToolSchema {
            name: tool.name.to_string(),
            description: tool.description.as_ref().map(|d| d.to_string()),
            parameters: serde_json::to_value(&tool.input_schema).ok(),
        },
    }
}

/// Classify an MCP tool result into the outcome the SDK admits. `is_error` is a payload-free
/// failure — the error bytes never travel to the model; non-text content is likewise refused
/// rather than silently repaired; a body over the cap seals (effects stand, no value).
pub fn classify_mcp(result: &CallToolResult, cap: usize) -> ToolOutcome {
    if result.is_error == Some(true) {
        return ToolOutcome::Failure;
    }
    let mut body = String::new();
    for content in &result.content {
        match content.as_text() {
            Some(text) => body.push_str(&text.text),
            None => return ToolOutcome::Failure,
        }
    }
    if body.len() > cap {
        return ToolOutcome::Success {
            body: BodyDisposition::RejectedTooLarge,
        };
    }
    ToolOutcome::Success {
        body: BodyDisposition::Available(body),
    }
}

/// Execute one surfaced call against the MCP server. Non-object arguments are a payload-free
/// failure (never repaired); a transport fault is indeterminate — the server may have acted.
pub async fn execute_call(server: &CorpSystemsClient, call: &RenderedCall, cap: usize) -> ToolOutcome {
    let Some(arguments) = call.arguments.as_object().cloned() else {
        return ToolOutcome::Failure;
    };
    let mut params = CallToolRequestParams::new(call.tool.as_str().to_string());
    params.arguments = Some(arguments);
    match server.peer().call_tool(params).await {
        Ok(result) => classify_mcp(&result, cap),
        Err(_) => ToolOutcome::Indeterminate,
    }
}

/// Drive one full user turn through the SDK: admit, then rounds of transcript → inference →
/// mediate, executing each surfaced call in order, until the model yields its final answer.
pub async fn run_turn(
    session: &mut AppaSession,
    inference: &Inference,
    server: &CorpSystemsClient,
    tools: &[WireTool],
    max_rounds: u32,
    quiet: bool,
    user_text: &str,
) -> anyhow::Result<String> {
    session.admit_user_turn(user_text).context("admitting the user turn")?;
    // Transcript positions already narrated, so each quiescent point prints only what is new.
    let mut narrated = print_new_messages(session, 0, quiet)?;

    for _ in 0..max_rounds {
        let messages = session.transcript().context("rendering the transcript")?;
        let request = ChatCompletionRequest {
            model: String::new(),
            messages,
            tools: Some(tools.to_vec()),
            stream: None,
        };
        let completion = match inference.complete(request).await {
            Ok(completion) => completion,
            Err(e) => {
                session.stop_turn("This turn could not continue: upstream inference was unavailable.")?;
                anyhow::bail!("inference failed: {e}");
            }
        };

        let mut step = session.mediate(completion).await.context("mediating the completion")?;
        loop {
            match step {
                Step::Final { text } => {
                    print_new_messages(session, narrated, quiet)?;
                    return Ok(text);
                }
                Step::Continue => break,
                Step::Execute { handle, call } => {
                    if !quiet {
                        eprintln!(
                            "appa: allowed — executing {} (occurrence {}) {}",
                            call.tool.as_str(),
                            handle.occurrence(),
                            call.arguments
                        );
                    }
                    let outcome = execute_call(server, &call, BODY_CAP_BYTES).await;
                    let reported = session
                        .report_outcome(handle, outcome)
                        .await
                        .context("reporting the tool outcome")?;
                    if !quiet {
                        match &reported.result {
                            AdmittedResult::Admitted { label, .. } => {
                                eprintln!("appa: result admitted at label {label:?}");
                            }
                            AdmittedResult::Sealed { token } => eprintln!("appa: result sealed — {token}"),
                        }
                    }
                    step = reported.next;
                }
            }
        }
        narrated = print_new_messages(session, narrated, quiet)?;
    }

    session.stop_turn("This turn reached its resource budget and was stopped.")?;
    anyhow::bail!("the turn exceeded {max_rounds} inference rounds")
}

/// Narrate transcript messages not yet printed (quiescent points only) and return the new frontier.
/// Blocked calls become visible here: their policy feedback is a `tool`-role message.
fn print_new_messages(session: &AppaSession, from: usize, quiet: bool) -> anyhow::Result<usize> {
    let transcript = session.transcript().context("rendering the transcript")?;
    if !quiet {
        for message in &transcript[from.min(transcript.len())..] {
            match message.role.as_str() {
                "user" => eprintln!("you> {}", message.content.as_deref().unwrap_or_default()),
                "assistant" => {
                    if let Some(content) = message.content.as_deref().filter(|c| !c.is_empty()) {
                        eprintln!("assistant> {content}");
                    }
                    for call in message.tool_calls.iter().flatten() {
                        eprintln!(
                            "assistant> [proposes {}({})]",
                            call.function.name, call.function.arguments
                        );
                    }
                }
                "tool" => eprintln!("  [tool response] {}", message.content.as_deref().unwrap_or_default()),
                _ => {}
            }
        }
    }
    Ok(transcript.len())
}
