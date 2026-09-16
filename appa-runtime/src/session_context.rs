//! The advice a protected session starts with: what a block means and what
//! to do about one. Claude Code adds a SessionStart hook's stdout to the
//! model's context, so this is printed there, by a hook entry of its own,
//! because advice is not enforcement: its failure blocks nothing. A subagent
//! starts a context of its own, without the parent's, and its SubagentStart
//! hook is heard only through `hookSpecificOutput.additionalContext`.

use std::io::Write;
use std::process::ExitCode;

use crate::hook_client::session_is_gated;

const TEXT: &str = include_str!("session_context.md");

/// How the harness reads the advice: the event's stdout as it is, or the
/// JSON shape the SubagentStart event requires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    SessionStdout,
    SubagentContext,
}

pub fn render(delivery: Delivery) -> String {
    match delivery {
        Delivery::SessionStdout => TEXT.to_owned(),
        Delivery::SubagentContext => serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "SubagentStart",
                "additionalContext": TEXT,
            }
        })
        .to_string(),
    }
}

pub fn run(delivery: Delivery) -> ExitCode {
    if !session_is_gated() {
        return ExitCode::SUCCESS;
    }
    let mut stdout = std::io::stdout();
    match stdout
        .write_all(render(delivery).as_bytes())
        .and_then(|()| stdout.flush())
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("appa session-context: cannot write the session context: {error}");
            ExitCode::FAILURE
        }
    }
}
