//! The status line of a protected Claude Code session: the label the session's
//! trajectory currently carries, read from the runtime.
//!
//! Claude Code runs this on every redraw with its status JSON on stdin, so the
//! round trip is bounded tightly: a runtime that does not answer at once
//! leaves the mark without chips rather than a status line that lags.

use std::io::Read;
use std::process::ExitCode;
use std::time::Duration;

use crate::hook_client::session_is_gated;
use crate::loopback_http::{Deadline, Endpoint, get};
use crate::runtime_url::RuntimeTarget;

const MARK_TOP: &str = "▄█▄▄▄█▄";
const MARK_BOTTOM: &str = "██▄█▄██";

pub fn run(target: &RuntimeTarget) -> ExitCode {
    if !session_is_gated() {
        return ExitCode::SUCCESS;
    }
    let mut input = Vec::new();
    // A status line that cannot read its input still shows the mark.
    let _ = std::io::stdin().read_to_end(&mut input);
    print!("{}", render(chips(&target.url, &input).as_deref()));
    ExitCode::SUCCESS
}

fn render(chips: Option<&str>) -> String {
    match chips {
        Some(chips) => format!("{MARK_TOP}  {chips}\n{MARK_BOTTOM}\n"),
        None => format!("{MARK_TOP}\n{MARK_BOTTOM}\n"),
    }
}

/// The session's trust and audience, as the runtime reports them for the
/// trajectory Claude Code's session id names; `None` whenever anything on the
/// way is missing or slow.
fn chips(url: &str, status_input: &[u8]) -> Option<String> {
    let input: serde_json::Value = serde_json::from_slice(status_input).ok()?;
    let session_id = input.get("session_id")?.as_str()?;
    let endpoint = Endpoint::parse(url).ok()?;
    let trajectory: String = url::form_urlencoded::byte_serialize(format!("cc:{session_id}").as_bytes()).collect();
    let answer = get(
        &endpoint,
        &format!("/status?trajectory={trajectory}"),
        &Deadline::spanning(Duration::from_millis(300)),
    )
    .ok()?;
    if !answer.is_success() {
        return None;
    }
    let status: serde_json::Value = serde_json::from_slice(&answer.body).ok()?;
    let trust = status.get("trust")?.as_str()?;
    let audience = status.get("audience")?.as_str()?;
    Some(format!("trust:{trust}  audience:{audience}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mark_carries_chips_only_when_the_runtime_reported_a_label() {
        assert_eq!(render(None), "▄█▄▄▄█▄\n██▄█▄██\n");
        assert_eq!(
            render(Some("trust:trusted  audience:internal")),
            "▄█▄▄▄█▄  trust:trusted  audience:internal\n██▄█▄██\n"
        );
    }

    #[test]
    fn an_input_naming_no_session_or_a_runtime_that_does_not_answer_yields_no_chips() {
        let vacated = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port binds");
        let dead = format!("http://{}", vacated.local_addr().expect("the bound address"));
        drop(vacated);
        assert_eq!(chips(&dead, br#"{"session_id":"s1"}"#), None);
        assert_eq!(chips(&dead, b"not json"), None);
        assert_eq!(chips(&dead, br#"{"model":"x"}"#), None);
    }
}
