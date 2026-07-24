//! MCP plumbing shared by the agent binary and its tests: spawning `corp-systems-mcp`, converting
//! its tool listing to the wire schema the SDK validates against, classifying its results, and
//! resolving the server binary and policy file.

use std::path::PathBuf;

use anyhow::Context;
use appa_sdk::{BodyDisposition, ToolOutcome, WireTool, WireToolSchema};
use rmcp::ServiceExt;
use rmcp::model::CallToolResult;
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use tokio::process::Command;

/// The MCP connection to the spawned `corp-systems-mcp`.
pub type CorpSystemsClient = RunningService<RoleClient, ()>;

/// The largest tool-result body admitted as a value, mirroring the runtime's default cap.
pub const BODY_CAP_BYTES: usize = appa_runtime::tool::DEFAULT_BODY_CAP_BYTES;

/// Spawn `corp-systems-mcp` as a stdio child and complete the MCP handshake.
/// `data_root` is the read-only corpus; `sink_root` is where `send_email` writes.
pub async fn spawn_corp_systems(
    server_bin: &PathBuf,
    data_root: &PathBuf,
    sink_root: &PathBuf,
) -> anyhow::Result<CorpSystemsClient> {
    let transport = TokioChildProcess::new(Command::new(server_bin).configure(|cmd| {
        cmd.arg("--data-root").arg(data_root);
        cmd.arg("--sink-root").arg(sink_root);
    }))
    .with_context(|| format!("spawning MCP server at {}", server_bin.display()))?;
    ().serve(transport)
        .await
        .context("MCP handshake with corp-systems-mcp failed")
}

/// Resolve the `corp-systems-mcp` binary: an explicit override, else
/// `CORP_SYSTEMS_BIN`, else the sibling `corp-systems` crate's debug build —
/// built on demand via `cargo build`, so a fresh checkout just works.
pub fn resolve_server_bin(explicit: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path);
    }
    if let Ok(env) = std::env::var("CORP_SYSTEMS_BIN")
        && !env.trim().is_empty()
    {
        return Ok(PathBuf::from(env));
    }
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../corp-systems");
    let name = if cfg!(windows) {
        "corp-systems-mcp.exe"
    } else {
        "corp-systems-mcp"
    };
    let bin = crate_dir.join("target/debug").join(name);
    let manifest = crate_dir.join("Cargo.toml");
    let status = std::process::Command::new("cargo")
        .args(["build", "-q", "--manifest-path"])
        .arg(&manifest)
        .status()
        .with_context(|| format!("running `cargo build` for {}", manifest.display()))?;
    anyhow::ensure!(
        status.success() && bin.is_file(),
        "building corp-systems-mcp failed — build it manually: cargo build --manifest-path {}",
        manifest.display()
    );
    Ok(bin)
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

/// Convert an MCP tool listing entry to the wire schema the SDK's `bind_tools` validates. The
/// registry stays the policy authority; the MCP server contributes presentation.
pub fn mcp_tool_schema(tool: &rmcp::model::Tool) -> WireTool {
    WireTool {
        kind: "function".to_string(),
        function: WireToolSchema {
            name: tool.name.to_string(),
            description: tool.description.as_ref().map(|d| d.to_string()),
            parameters: serde_json::to_value(&tool.input_schema).ok(),
        },
    }
}

/// Classify an MCP tool result into the outcome the SDK admits. `is_error` is a payload-free
/// failure — the error bytes never travel to the model; non-text content is likewise refused; a
/// body over the cap seals (effects stand, no value).
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
