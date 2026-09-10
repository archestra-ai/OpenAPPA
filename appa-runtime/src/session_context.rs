//! The advice a protected session starts with: what a block means and what
//! to do about one. Claude Code adds a SessionStart hook's stdout to the
//! model's context, so this is printed there, by a hook entry of its own,
//! because advice is not enforcement: its failure blocks nothing.

use std::io::Write;
use std::process::ExitCode;

use crate::hook_client::session_is_gated;

const TEXT: &str = include_str!("session_context.md");

pub fn run() -> ExitCode {
    if !session_is_gated() {
        return ExitCode::SUCCESS;
    }
    let mut stdout = std::io::stdout();
    match stdout.write_all(TEXT.as_bytes()).and_then(|()| stdout.flush()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("appa session-context: cannot write the session context: {error}");
            ExitCode::FAILURE
        }
    }
}
