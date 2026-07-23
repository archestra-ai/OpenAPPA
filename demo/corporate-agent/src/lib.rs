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

use std::path::PathBuf;

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

/// Load `KEY=VALUE` lines from a `.env` file into the process environment,
/// without overwriting variables already set — a real environment variable
/// always wins. Looks crate-local first (`<crate>/.env`), then the repository
/// root (`<crate>/../../.env`); when both exist their variables are merged, with
/// the crate-local file winning on overlap. Returns the first file found, for a
/// status line.
///
/// Call this once at the very start of `main`, before any threads spawn and
/// before parsing args (so `clap`'s `env = "…"` fields see the loaded values).
/// That ordering is what makes the `set_var` calls sound under the Rust 2024
/// rules.
pub fn load_dotenv() -> Option<PathBuf> {
    let mut first_found = None;
    for candidate in [
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".env"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.env"),
    ] {
        if let Ok(text) = std::fs::read_to_string(&candidate) {
            apply_env_file(&text);
            first_found.get_or_insert(candidate);
        }
    }
    first_found
}

fn apply_env_file(text: &str) {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        // A real environment variable (or an earlier .env) always wins.
        if key.is_empty() || std::env::var_os(key).is_some() {
            continue;
        }
        // SAFETY: `load_dotenv` is called at the start of `main`, before any
        // other thread exists, so there is no concurrent environment access.
        unsafe {
            std::env::set_var(key, clean_key(value));
        }
    }
}
