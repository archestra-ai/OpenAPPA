//! The hook client: this same binary, invoked as `appa hook`, translating one
//! host hook event read on stdin onto the canonical wire, posting it to the
//! running runtime, and printing the runtime's answer in the host's own shape.
//!
//! The harness spawns a process per hook, so the cost of reaching the runtime is
//! paid on every tool call. Doing it here rather than through `curl` spends one
//! process where a shell and a curl spent two, and spends it on the binary the
//! install already puts on disk, so the install path grows nothing. The host's
//! shape translation (the adapter's [`Codec`]) runs here, on the client side;
//! the wire carries the host's raw tool spelling and the runtime derives the
//! rest itself, so nothing this client says about a call is trusted.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use appa_runtime_api::{AdapterName, Codec, HookDecision, HookEvent, ParseRefusal, WireDecision, WireEvent};

/// Where the runtime answers hooks: an authority to connect to and the prefix the
/// runtime's routes hang under. Only `http` is spoken: the runtime refuses to
/// listen anywhere but loopback, so there is no transport to secure.
struct Endpoint {
    authority: String,
    prefix: String,
}

impl Endpoint {
    fn parse(url: &str) -> Result<Self, String> {
        let rest = url
            .strip_prefix("http://")
            .ok_or_else(|| format!("{url} is not an http:// URL; the runtime serves plain HTTP on loopback"))?;
        let (authority, prefix) = match rest.find('/') {
            Some(slash) => (&rest[..slash], rest[slash..].trim_end_matches('/')),
            None => (rest, ""),
        };
        if authority.is_empty() {
            return Err(format!("{url} names no host to post to"));
        }
        Ok(Self {
            authority: authority.to_owned(),
            prefix: prefix.to_owned(),
        })
    }

    fn address(&self) -> Result<SocketAddr, String> {
        self.authority
            .to_socket_addrs()
            .map_err(|error| format!("{} does not resolve: {error}", self.authority))?
            .next()
            .ok_or_else(|| format!("{} resolves to no address", self.authority))
    }

    fn request_head(&self, length: usize) -> String {
        format!(
            "POST {}/hook HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\n\
             Content-Length: {length}\r\nConnection: close\r\n\r\n",
            self.prefix, self.authority
        )
    }
}

/// What the answer to this hook decides. A turn end reports a turn the actor has
/// already finished, so it decides nothing: it discards the answer and never
/// blocks, because every blocking outcome there would hold the actor in that turn.
/// Every other hook authorizes something, so a runtime that does not answer blocks
/// rather than letting the action through.
///
/// The caller says which, rather than this client reading the event name: the hook
/// map already registers each hook for one event and is where that choice is
/// reviewed, and nothing in the posted event can move a hook off it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Decides {
    Authorization,
    Nothing,
}

impl Decides {
    fn of_a_turn_end(turn_end: bool) -> Self {
        match turn_end {
            true => Self::Nothing,
            false => Self::Authorization,
        }
    }

    /// The budget for the whole round trip. Authorization outlasts the runtime's
    /// own evidence round trips; a turn end waits on none, so it takes the shorter
    /// one and costs a runtime that is down a call left open, never a stuck turn.
    /// Both stay under the timeout the harness declares, which is the deadline that
    /// matters: a hook the harness kills has its exit code ignored and fails open.
    fn budget(self) -> Duration {
        match self {
            Self::Authorization => Duration::from_secs(120),
            Self::Nothing => Duration::from_secs(30),
        }
    }
}

/// The wall clock the whole round trip runs against. Every socket operation takes
/// what is left of it, so a trickle of bytes cannot outlast the budget the way a
/// per-operation timeout allows.
struct Deadline(Instant);

impl Deadline {
    fn spanning(budget: Duration) -> Self {
        Self(Instant::now() + budget)
    }

    /// A socket reads a zero timeout as "no timeout", so a spent budget must fail
    /// here rather than reach one.
    fn left(&self) -> Result<Duration, String> {
        self.0
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .ok_or_else(|| "the runtime did not answer in time".to_owned())
    }
}

struct Answer {
    status: u16,
    body: Vec<u8>,
}

impl Answer {
    fn is_success(&self) -> bool {
        (200..=299).contains(&self.status)
    }
}

fn post(endpoint: &Endpoint, event: &[u8], deadline: &Deadline) -> Result<Answer, String> {
    let address = endpoint.address()?;
    let mut socket = TcpStream::connect_timeout(&address, deadline.left()?)
        .map_err(|error| format!("cannot reach {address}: {error}"))?;
    socket.set_nodelay(true).ok();

    for part in [endpoint.request_head(event.len()).as_bytes(), event] {
        socket
            .set_write_timeout(Some(deadline.left()?))
            .map_err(|error| format!("cannot bound the write to {address}: {error}"))?;
        socket
            .write_all(part)
            .map_err(|error| format!("cannot post to {address}: {error}"))?;
    }

    let mut answer = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        socket
            .set_read_timeout(Some(deadline.left()?))
            .map_err(|error| format!("cannot bound the read from {address}: {error}"))?;
        match socket.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => answer.extend_from_slice(&chunk[..read]),
            Err(error) => return Err(format!("cannot read the answer from {address}: {error}")),
        }
    }
    parse(&answer)
}

fn parse(answer: &[u8]) -> Result<Answer, String> {
    let end_of_head = answer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| "the answer ended before its headers did".to_owned())?;
    let head =
        std::str::from_utf8(&answer[..end_of_head]).map_err(|_| "the answer's headers are not text".to_owned())?;
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| format!("the answer carries no status code: {head}"))?;
    Ok(Answer {
        status,
        body: answer[end_of_head + 4..].to_vec(),
    })
}

/// The blocking hook outcome. Claude Code reads stderr as the reason it blocked.
fn block(failure: &str) -> ExitCode {
    eprintln!("OpenAPPA hook blocked: {failure}");
    ExitCode::from(2)
}

/// Preserve the status while surfacing the runtime's own diagnostic. Claude Code displays
/// stderr for a blocking hook, not the response body this client forwards on stdout. A
/// refusal arrives either as a wire decision carrying its `detail` or, before any event
/// exists, as `{"error": …}`.
fn refusal(answer: &Answer) -> String {
    let json_detail = serde_json::from_slice::<serde_json::Value>(&answer.body)
        .ok()
        .and_then(|body| {
            ["error", "detail"]
                .into_iter()
                .find_map(|field| body.get(field).and_then(serde_json::Value::as_str).map(str::to_owned))
        });
    let plain_error = std::str::from_utf8(&answer.body)
        .ok()
        .map(str::trim)
        .filter(|body| !body.is_empty())
        .map(str::to_owned);
    match json_detail.or(plain_error) {
        Some(detail) => format!("status={} {detail}", answer.status),
        None => format!("status={}", answer.status),
    }
}

/// The host whose hook bytes this client translates. Only Claude Code posts through
/// `appa hook`: the kagent plugin speaks the canonical wire itself.
fn codec_for(adapter: AdapterName) -> Result<Codec, String> {
    match adapter {
        AdapterName::ClaudeCode => Ok(appa_adapter_claude_code::codec()),
        AdapterName::Kagent => Err(format!(
            "`appa hook` translates no {adapter} events: the kagent plugin posts the canonical wire itself"
        )),
    }
}

/// The runtime's answer, read off the wire. A body that is not a wire decision is
/// not guessed at: the hook fails closed on it.
fn decision_of(body: &[u8]) -> Result<HookDecision, String> {
    let wire: WireDecision = serde_json::from_slice(body)
        .map_err(|error| format!("the runtime's answer is not a wire decision: {error}"))?;
    wire.into_decision().map_err(|refusal| match refusal {
        ParseRefusal::Unreadable { detail } | ParseRefusal::Malformed { detail } => detail,
    })
}

fn print(answer: &serde_json::Value) {
    let mut stdout = std::io::stdout();
    stdout.write_all(answer.to_string().as_bytes()).ok();
    stdout.flush().ok();
}

/// The host event translated onto the wire: `None` for a hook the adapter does not
/// gate, whose answer is the empty opinion without a round trip.
fn translate(codec: &Codec, adapter: AdapterName, host_event: &[u8]) -> Result<Option<(HookEvent, Vec<u8>)>, String> {
    let event = match (codec.parse)(host_event) {
        Ok(Some(event)) => event,
        Ok(None) => return Ok(None),
        Err(ParseRefusal::Unreadable { detail } | ParseRefusal::Malformed { detail }) => return Err(detail),
    };
    let wire = WireEvent::from_event(adapter, &event).map_err(|refusal| match refusal {
        ParseRefusal::Unreadable { detail } | ParseRefusal::Malformed { detail } => detail,
    })?;
    let body = serde_json::to_vec(&wire).map_err(|error| format!("the wire event does not serialize: {error}"))?;
    Ok(Some((event, body)))
}

pub fn run(url: &str, adapter: AdapterName, turn_end: bool) -> ExitCode {
    let decides = Decides::of_a_turn_end(turn_end);
    let codec = match codec_for(adapter) {
        Ok(codec) => codec,
        Err(failure) => return block(&failure),
    };
    let mut host_event = Vec::new();
    if let Err(error) = std::io::stdin().read_to_end(&mut host_event) {
        return block(&format!("the hook event could not be read: {error}"));
    }
    // The event outlives the round trip: a hook the runtime never answers still
    // needs the withholding rendered for the result its event reports.
    let (event, answered) = match translate(&codec, adapter, &host_event) {
        Err(failure) => (None, Err(failure)),
        Ok(None) => (None, Ok(None)),
        Ok(Some((event, body))) => {
            let posted =
                Endpoint::parse(url).and_then(|endpoint| post(&endpoint, &body, &Deadline::spanning(decides.budget())));
            (Some(event), posted.map(Some))
        }
    };
    if decides == Decides::Nothing {
        if let Err(failure) = answered {
            eprintln!("OpenAPPA runtime did not answer the turn end: {failure}");
        }
        return ExitCode::SUCCESS;
    }
    match (answered, event) {
        (Ok(None), _) => {
            print(&serde_json::json!({}));
            ExitCode::SUCCESS
        }
        (Ok(Some(answer)), Some(event)) => match (decision_of(&answer.body), answer.is_success()) {
            (Ok(decision), true) => {
                print(&(codec.render)(&event, &decision));
                ExitCode::SUCCESS
            }
            // A refusal is rendered too: Claude Code honours a withheld tool result
            // on a non-2xx answer, so the refused result is replaced as well as blocked.
            (Ok(decision), false) => {
                print(&(codec.render)(&event, &decision));
                block(&refusal(&answer))
            }
            (Err(_), false) => block(&refusal(&answer)),
            (Err(failure), true) => withhold(&codec, Some(&event), &failure),
        },
        // An answer arrives only for an event, so this pairing never occurs.
        (Ok(Some(_)), None) => block("the runtime answered a hook that named no event"),
        (Err(failure), event) => withhold(&codec, event.as_ref(), &failure),
    }
}

/// Fail closed on an unanswered hook. Exiting non-zero stops a call the harness
/// has not run yet, but a result the tool already produced stays in front of the
/// model unless the withholding is rendered for it, so an event that reports one
/// gets the blocking replacement before the client exits.
fn withhold(codec: &Codec, event: Option<&HookEvent>, failure: &str) -> ExitCode {
    let reports_a_result = matches!(
        event,
        Some(HookEvent::ToolResult { .. } | HookEvent::SpawnResult { .. } | HookEvent::ChildEnd { .. })
    );
    if let (true, Some(event)) = (reports_a_result, event) {
        let reason = format!("the runtime did not answer this hook: {failure}");
        print(&(codec.render)(event, &HookDecision::Block { reason }));
    }
    block(failure)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_endpoint_is_an_authority_and_the_prefix_its_routes_hang_under() {
        let plain = Endpoint::parse("http://127.0.0.1:8787").expect("a bare authority parses");
        assert_eq!(plain.authority, "127.0.0.1:8787");
        assert_eq!(
            plain.request_head(3),
            "POST /hook HTTP/1.1\r\nHost: 127.0.0.1:8787\r\nContent-Type: application/json\r\n\
             Content-Length: 3\r\nConnection: close\r\n\r\n"
        );

        let nested = Endpoint::parse("http://127.0.0.1:8787/appa/").expect("a prefix parses");
        assert!(nested.request_head(0).starts_with("POST /appa/hook HTTP/1.1\r\n"));

        assert!(Endpoint::parse("https://127.0.0.1:8787").is_err());
        assert!(Endpoint::parse("127.0.0.1:8787").is_err());
        assert!(Endpoint::parse("http:///hook").is_err());
    }

    #[test]
    fn a_turn_end_waits_on_less_than_an_authorization_does() {
        assert!(Decides::of_a_turn_end(true) == Decides::Nothing);
        assert!(Decides::of_a_turn_end(false) == Decides::Authorization);
        assert!(Decides::Nothing.budget() < Decides::Authorization.budget());
    }

    #[test]
    fn an_answer_is_its_status_and_the_body_after_the_headers() {
        let answered = parse(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}").expect("a well formed answer parses");
        assert_eq!(answered.status, 200);
        assert_eq!(answered.body, b"{}");
        assert!(answered.is_success());

        let refused = parse(b"HTTP/1.1 422 Unprocessable Entity\r\n\r\nwhy").expect("a refusal parses");
        assert_eq!(refused.status, 422);
        assert_eq!(refused.body, b"why");
        assert!(!refused.is_success());

        assert!(parse(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n").is_err());
        assert!(parse(b"garbage\r\n\r\n").is_err());
    }

    #[test]
    fn a_refusal_surfaces_the_runtimes_exact_error() {
        let answer = Answer {
            status: 409,
            body: serde_json::to_vec(&serde_json::json!({
                "error": "annotator=claude-code error=malformed field=delta.audience value=\"secret\" allowed=declaration.audiences"
            }))
            .expect("the fixture serializes"),
        };
        assert_eq!(
            refusal(&answer),
            "status=409 annotator=claude-code error=malformed field=delta.audience value=\"secret\" allowed=declaration.audiences"
        );

        let refused = Answer {
            status: 409,
            body: serde_json::to_vec(&WireDecision::of(&HookDecision::Refuse {
                detail: "storage failure: disk full".to_string(),
            }))
            .expect("the fixture serializes"),
        };
        assert_eq!(refusal(&refused), "status=409 storage failure: disk full");

        assert_eq!(
            refusal(&Answer {
                status: 503,
                body: b"temporarily unavailable".to_vec(),
            }),
            "status=503 temporarily unavailable"
        );
        assert_eq!(
            refusal(&Answer {
                status: 500,
                body: Vec::new(),
            }),
            "status=500"
        );
    }

    #[test]
    fn a_spent_deadline_refuses_rather_than_bounding_a_socket_by_zero() {
        assert!(Deadline::spanning(Duration::from_secs(5)).left().expect("time is left") > Duration::ZERO);
        assert!(Deadline::spanning(Duration::ZERO).left().is_err());
    }

    #[test]
    fn only_claude_code_translates_through_this_client() {
        assert!(codec_for(AdapterName::ClaudeCode).is_ok());
        assert!(codec_for(AdapterName::Kagent).is_err());
    }

    #[test]
    fn a_host_event_crosses_as_a_wire_event_with_its_raw_spelling() {
        let codec = codec_for(AdapterName::ClaudeCode).expect("claude code translates");
        let host =
            br#"{"hook_event_name":"PreToolUse","session_id":"s1","tool_name":"Agent","tool_input":{"prompt":"go"}}"#;
        let (event, body) = translate(&codec, AdapterName::ClaudeCode, host)
            .expect("the event translates")
            .expect("PreToolUse is gated");
        assert!(matches!(event, HookEvent::ToolCall { .. }));
        let wire: serde_json::Value = serde_json::from_slice(&body).expect("the wire is JSON");
        assert_eq!(wire["protocol"], appa_runtime_api::PROTOCOL);
        assert_eq!(wire["adapter"], "claude-code");
        assert_eq!(wire["event"], "tool_call");
        assert_eq!(wire["root_id"], "s1");
        assert_eq!(wire["tool"], "Agent", "the wire carries the host's raw spelling");
        assert_eq!(wire["arguments"], serde_json::json!({"prompt": "go"}));
        assert!(
            wire.get("spawn").is_none(),
            "the client claims nothing the runtime derives: {wire}"
        );

        let ungated = br#"{"hook_event_name":"Notification","session_id":"s1"}"#;
        assert!(
            translate(&codec, AdapterName::ClaudeCode, ungated)
                .expect("an ungated hook translates")
                .is_none()
        );
        assert!(translate(&codec, AdapterName::ClaudeCode, b"not json").is_err());
        assert!(
            translate(
                &codec,
                AdapterName::ClaudeCode,
                br#"{"hook_event_name":"PreToolUse","session_id":"s1"}"#
            )
            .is_err(),
            "a malformed host event blocks before any round trip"
        );
    }

    #[test]
    fn an_answer_is_read_as_a_wire_decision_or_fails_closed() {
        let allow =
            serde_json::to_vec(&WireDecision::of(&HookDecision::AllowCall { spawn: None })).expect("serializes");
        assert_eq!(
            decision_of(&allow).expect("a wire decision reads"),
            HookDecision::AllowCall { spawn: None }
        );
        assert!(decision_of(b"{}").is_err(), "an empty object is no decision");
        assert!(decision_of(br#"{"protocol":9,"decision":"ack"}"#).is_err());
        assert!(decision_of(br#"{"protocol":1,"decision":"block"}"#).is_err());
    }
}
