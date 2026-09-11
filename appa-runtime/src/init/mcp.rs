//! The runtime's MCP server in Claude Code's user scope, registered through the
//! `claude` CLI, which owns the file that holds it.
//!
//! The URL is a template Claude Code expands per session: `APPA_RUNTIME_URL`
//! when the session names a runtime of its own, this deployment's endpoint
//! otherwise, the same precedence the hook entries apply. A server under APPA's
//! name whose URL is not that template was not written by an install and is
//! refused, never replaced.

use std::ffi::OsStr;
use std::process::Command;

use super::{Compensation, InitError, Undo};

pub(super) const SERVER: &str = "appa";

const TEMPLATE_PREFIX: &str = "${APPA_RUNTIME_URL:-";
const TEMPLATE_SUFFIX: &str = "}/mcp";

/// What Claude Code reports under APPA's server name.
pub(super) enum Registered {
    Absent,
    /// An install's registration, carrying the template URL it wrote.
    Ours {
        url: String,
    },
}

/// The registration Claude Code reports, whichever scope it comes from: a
/// project-scoped server of the same name shadows the user scope one, so it is
/// as much a conflict as a foreign user-scoped one.
pub(super) fn current() -> Result<Registered, InitError> {
    let output = Command::new("claude")
        .args(["mcp", "get", SERVER])
        .output()
        .map_err(InitError::ClaudeUnavailable)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        if is_absent(&stderr) || is_absent(&stdout) {
            return Ok(Registered::Absent);
        }
        return Err(InitError::ClaudeCommand {
            command: format!("mcp get {SERVER}"),
            message: message(&stdout, &stderr),
        });
    }
    let url = served_url(&stdout).ok_or_else(|| InitError::ClaudeCommand {
        command: format!("mcp get {SERVER}"),
        message: "the answer names no URL".to_owned(),
    })?;
    if !is_ours(url) {
        return Err(InitError::McpConflict { url: url.to_owned() });
    }
    Ok(Registered::Ours { url: url.to_owned() })
}

/// Register this deployment's endpoint, replacing an earlier install's
/// registration. `add-json` refuses an existing name, so the earlier one is
/// removed first, and the undo record is taken before either step so a
/// failure between them puts the earlier registration back.
pub(super) fn register(before: &Registered, url: &str, compensation: &mut Compensation) -> Result<(), InitError> {
    let wanted = template(url);
    let previous = match before {
        Registered::Ours { url } if *url == wanted => return Ok(()),
        Registered::Ours { url } => Some(url.clone()),
        Registered::Absent => None,
    };
    compensation.record(Undo::Mcp {
        previous: previous.clone(),
    });
    if previous.is_some() {
        remove()?;
    }
    add(&wanted)
}

pub(super) fn remove() -> Result<(), InitError> {
    run(["mcp", "remove", SERVER, "--scope", "user"]).map(drop)
}

/// Put the registration back the way it was before `register`: nothing, or the
/// earlier install's URL. Whatever `register` left is cleared first; an absent
/// server is not a failure to clear.
pub(super) fn restore(previous: Option<String>) -> Result<(), InitError> {
    match remove() {
        Ok(()) => {}
        Err(InitError::ClaudeCommand { message, .. }) if is_absent(&message) => {}
        Err(error) => return Err(error),
    }
    match previous {
        Some(url) => add(&url),
        None => Ok(()),
    }
}

fn add(url: &str) -> Result<(), InitError> {
    let server = serde_json::json!({"type": "http", "url": url});
    run([
        "mcp",
        "add-json",
        "--scope",
        "user",
        SERVER,
        &serde_json::to_string(&server).expect("JSON values encode"),
    ])
    .map(drop)
}

fn template(url: &str) -> String {
    format!("{TEMPLATE_PREFIX}{url}{TEMPLATE_SUFFIX}")
}

fn is_ours(url: &str) -> bool {
    url.starts_with(TEMPLATE_PREFIX) && url.ends_with(TEMPLATE_SUFFIX)
}

fn is_absent(message: &str) -> bool {
    message.contains("No MCP server named")
}

/// The `URL:` line of `claude mcp get`.
fn served_url(report: &str) -> Option<&str> {
    report
        .lines()
        .find_map(|line| line.trim_start().strip_prefix("URL:"))
        .map(str::trim)
}

fn message(stdout: &str, stderr: &str) -> String {
    let stderr = stderr.trim();
    if stderr.is_empty() {
        stdout.trim().to_owned()
    } else {
        stderr.to_owned()
    }
}

/// Run one `claude` command, answering with its stdout only when it succeeded.
fn run<A: AsRef<OsStr>, const N: usize>(arguments: [A; N]) -> Result<String, InitError> {
    let output = Command::new("claude")
        .args(&arguments)
        .output()
        .map_err(InitError::ClaudeUnavailable)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if output.status.success() {
        return Ok(stdout.into_owned());
    }
    Err(InitError::ClaudeCommand {
        command: arguments
            .iter()
            .map(|argument| argument.as_ref().to_string_lossy())
            .collect::<Vec<_>>()
            .join(" "),
        message: message(&stdout, &String::from_utf8_lossy(&output.stderr)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_report_names_the_url_and_only_the_template_is_ours() {
        let report = "appa:\n  Scope: User config (available in all your projects)\n  Status: ✔ Connected\n  Type: http\n  URL: ${APPA_RUNTIME_URL:-http://127.0.0.1:8787}/mcp\n\nTo remove this server, run: claude mcp remove appa -s user\n";
        let url = served_url(report).unwrap();
        assert_eq!(url, template("http://127.0.0.1:8787"));
        assert!(is_ours(url));
        assert!(is_ours(&template("http://127.0.0.1:1")));
        for foreign in [
            "http://127.0.0.1:8787/mcp",
            "${APPA_RUNTIME_URL:-http://127.0.0.1:8787}/other",
            "${OTHER:-http://127.0.0.1:8787}/mcp",
        ] {
            assert!(!is_ours(foreign), "{foreign}");
        }
        assert_eq!(served_url("appa:\n  Type: http\n"), None);
        assert!(is_absent("No MCP server named \"appa\" in user scope"));
    }
}
