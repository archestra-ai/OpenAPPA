//! Mock corporate systems shared by the corporate-agent demos.
//!
//! One binary, `corp-systems-mcp` ([`server`]) — a stdio MCP server exposing
//! mock systems (`hr`, `finance`, `task_tracker`, `public_forum`, `vendor`) as
//! folders, with generic file tools plus `send_email` and `share_legal_packet`.
//! The semantics live once, in [`systems`]; the crate's `data/` directory is the
//! canonical corpus, including the planted prompt-injection thread.
//!
//! Two sibling demos act on the same corpus and differ only in the defense
//! mediating the agent loop:
//! - `../corp-agent` — a Rust agent on the full `appa-example-agent` loop, defended
//!   by OpenAPPA; it links this crate as a library and runs [`systems`]
//!   in-process rather than spawning the server;
//! - `../corp-agent-fides` — a Python Agent Framework agent defended by
//!   FIDES; it spawns `corp-systems-mcp`.
//!
//! Each demo passes its own sink root, so the shared corpus stays read-only
//! and the observable `email/` side-effect lands per demo.

pub mod server;
pub mod systems;

use std::path::PathBuf;

/// Resolve the corpus root: an explicit override, else `CORP_DATA_ROOT`, else
/// the `data/` folder next to this crate's manifest.
pub fn resolve_data_root(explicit: Option<PathBuf>) -> PathBuf {
    if let Some(path) = explicit {
        return path;
    }
    if let Ok(env) = std::env::var("CORP_DATA_ROOT")
        && !env.trim().is_empty()
    {
        return PathBuf::from(env);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data")
}
