//! A corporate assistant demo for exercising OpenAPPA.
//!
//! One binary, `appa-corp-agent`, over the mock corporate systems (the sibling
//! [`corp-systems`](../corp-systems) crate: `hr`, `finance`, `task_tracker`,
//! `public_forum`, and `vendor`, plus email tools): the assistant on
//! the runtime, where the harness owns the loop and the runtime answers one
//! question in front of every flow. Branching is live — a tainting read can be
//! confined to a child trajectory, and a child's return can cross through a
//! registered sanitizer. Its tools run in-process ([`shim`]), the same
//! `corp-systems` code the MCP server wraps, so this agent and the sibling
//! `corp-agent-fides` demo act on identical systems.
//!
//! The policy is the demo's payload, and the agent takes it explicitly: the
//! bench arms run `bench/corp/policies/{appa,open}.toml`. The guarded variant
//! carries the branch-aware story — forum taint, an egress-gated ticket, and an
//! hr-audience sanitizer for child returns; the open variant registers the same
//! tools with the neutral delta, reproducing the undefended leak.

pub mod catalogue;
pub mod shim;

use std::path::PathBuf;

/// The corpus root the shim reads: the sibling `corp-systems` crate resolves
/// it, so this demo and the FIDES one cannot drift onto different corpora.
pub use corp_systems::resolve_data_root;

/// Resolve where the spawned server's `send_email` writes its `email/` folder:
/// an explicit override, else `CORP_SINK_ROOT`, else this crate's `data/` — so
/// the shared corpus stays read-only and the observable leak lands here.
pub fn resolve_sink_root(explicit: Option<PathBuf>) -> PathBuf {
    if let Some(path) = explicit {
        return path;
    }
    if let Ok(env) = std::env::var("CORP_SINK_ROOT")
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
