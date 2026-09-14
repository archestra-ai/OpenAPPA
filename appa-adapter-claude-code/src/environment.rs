//! What Claude Code's process environment says. Claude Code marks every
//! process under a session with `CLAUDECODE`, and a harness that runs a session
//! on its own model endpoint hands the session its credential in `ANTHROPIC_*`,
//! good for that session alone. A process that must outlive the session — the
//! runtime a session starts — or must not look like one — a nested CLI — reads
//! its inherited environment through these two questions.

use std::ffi::{OsStr, OsString};

/// The variable Claude Code sets on every process of a session's tree.
pub const SESSION_MARKER: &str = "CLAUDECODE";

/// Whether an environment with these variable names is inside a Claude Code session.
pub fn inside_session<'a>(names: impl IntoIterator<Item = &'a OsStr>) -> bool {
    names.into_iter().any(|name| name == SESSION_MARKER)
}

/// The variables of an environment that belong to the Claude Code session it is
/// inside — the marker, the session's `CLAUDE_CODE_*` settings and the
/// `ANTHROPIC_*` credential a harness set for it — and none where the
/// environment is not inside a session: a shell's own `ANTHROPIC_*` is a
/// deliberate choice that stands.
pub fn session_scoped(names: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    let names: Vec<OsString> = names.into_iter().collect();
    if !inside_session(names.iter().map(OsString::as_os_str)) {
        return Vec::new();
    }
    names
        .into_iter()
        .filter(|name| {
            let name = name.to_string_lossy();
            name == SESSION_MARKER || name.starts_with("CLAUDE_CODE_") || name.starts_with("ANTHROPIC_")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    /// Inside a session the session's credential and markers are its own; a
    /// shell's environment belongs to nobody but the shell.
    #[test]
    fn only_a_sessions_environment_has_session_scoped_variables() {
        assert!(session_scoped(names(&["PATH", "HOME", "ANTHROPIC_BASE_URL", "ANTHROPIC_AUTH_TOKEN"])).is_empty());
        assert_eq!(
            session_scoped(names(&[
                "PATH",
                "HOME",
                "CLAUDECODE",
                "CLAUDE_CODE_SESSION_ID",
                "CLAUDE_CODE_ENTRYPOINT",
                "ANTHROPIC_BASE_URL",
                "ANTHROPIC_AUTH_TOKEN",
                "APPA_GATE",
            ])),
            names(&[
                "CLAUDECODE",
                "CLAUDE_CODE_SESSION_ID",
                "CLAUDE_CODE_ENTRYPOINT",
                "ANTHROPIC_BASE_URL",
                "ANTHROPIC_AUTH_TOKEN",
            ])
        );
    }
}
