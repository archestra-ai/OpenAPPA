//! A corporate assistant demo for exercising OpenAPPA.
//!
//! Two binaries share this crate:
//! - `corp-systems-mcp` ([`server`]) — a stdio MCP server exposing mock
//!   internal systems (`hr`, `finance`, `task_tracker`, `public_forum`) as
//!   folders, with `search_`/`read_`/`create_` tools per system plus `send_email`.
//! - `corp-agent` — a [`rig`](https://docs.rs/rig-core) agent on OpenRouter that
//!   spawns the server, registers its tools, and drives a one-shot (or `--chat`)
//!   loop, printing a [`logview::PrettyLog`] of everything it sends and calls.
//!
//! There is deliberately no policy engine in the loop: an unmediated run of the
//! injection scenario will happily read HR secrets and exfiltrate them via
//! `send_email`. Putting OpenAPPA between the agent and these tools is the point
//! of the demo.

pub mod logview;
pub mod server;
pub mod systems;

use std::path::{Path, PathBuf};

/// Resolve the data root: an explicit override, else `CORP_DATA_ROOT`, else the
/// `data/` folder next to this crate's manifest.
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

/// Strip surrounding whitespace and a single pair of matching quotes — the shape
/// a value takes in a `.env` file (`KEY="sk-…"`). Ported from the old demo.
pub fn clean_key(raw: &str) -> String {
    let t = raw.trim();
    let t = t.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(t);
    let t = t.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')).unwrap_or(t);
    t.to_string()
}

/// Read `OPENROUTER_API_KEY` from the repository-root `.env` (two levels up from
/// this crate), the same file the rest of the repo uses. Returns `None` if absent.
pub fn key_from_env_file() -> Option<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.env");
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        let line = line.trim().strip_prefix("export ").unwrap_or(line.trim());
        if let Some(value) = line.strip_prefix("OPENROUTER_API_KEY=") {
            let key = clean_key(value);
            if !key.is_empty() {
                return Some(key);
            }
        }
    }
    None
}
