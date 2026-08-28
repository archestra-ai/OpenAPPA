//! The hook client: this same binary, invoked as `appa-runtime hook`, posting one
//! hook event read on stdin to the running runtime and printing its answer.
//!
//! The harness spawns a process per hook, so the cost of reaching the runtime is
//! paid on every tool call. Doing it here rather than through `curl` spends one
//! process where a shell and a curl spent two, and spends it on the binary the
//! install already puts on disk, so the install path grows nothing.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::process::ExitCode;
use std::time::{Duration, Instant};

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
pub(crate) enum Decides {
    Authorization,
    Nothing,
}

impl Decides {
    pub(crate) fn of_a_turn_end(turn_end: bool) -> Self {
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
/// stderr for a blocking hook, not the response body this client forwards on stdout.
fn refusal(answer: &Answer) -> String {
    let json_error = serde_json::from_slice::<serde_json::Value>(&answer.body)
        .ok()
        .and_then(|body| body.get("error").and_then(serde_json::Value::as_str).map(str::to_owned));
    let plain_error = std::str::from_utf8(&answer.body)
        .ok()
        .map(str::trim)
        .filter(|body| !body.is_empty())
        .map(str::to_owned);
    match json_error.or(plain_error) {
        Some(detail) => format!("status={} {detail}", answer.status),
        None => format!("status={}", answer.status),
    }
}

pub(crate) fn run(url: &str, decides: Decides) -> ExitCode {
    let mut event = Vec::new();
    if let Err(error) = std::io::stdin().read_to_end(&mut event) {
        return block(&format!("the hook event could not be read: {error}"));
    }
    let answered =
        Endpoint::parse(url).and_then(|endpoint| post(&endpoint, &event, &Deadline::spanning(decides.budget())));
    if decides == Decides::Nothing {
        if let Err(failure) = answered {
            eprintln!("OpenAPPA runtime did not answer the turn end: {failure}");
        }
        return ExitCode::SUCCESS;
    }
    match answered {
        Ok(answer) => {
            std::io::stdout().write_all(&answer.body).ok();
            std::io::stdout().flush().ok();
            match answer.status {
                200..=299 => ExitCode::SUCCESS,
                _ => block(&refusal(&answer)),
            }
        }
        Err(failure) => block(&failure),
    }
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

        let refused = parse(b"HTTP/1.1 422 Unprocessable Entity\r\n\r\nwhy").expect("a refusal parses");
        assert_eq!(refused.status, 422);
        assert_eq!(refused.body, b"why");

        assert!(parse(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n").is_err());
        assert!(parse(b"garbage\r\n\r\n").is_err());
    }

    #[test]
    fn a_refusal_surfaces_the_runtimes_exact_error() {
        let answer = Answer {
            status: 409,
            body: serde_json::to_vec(&serde_json::json!({
                "error": "resolver=claude-code error=malformed field=delta.audience value=\"secret\" allowed=declaration.audiences"
            }))
            .expect("the fixture serializes"),
        };
        assert_eq!(
            refusal(&answer),
            "status=409 resolver=claude-code error=malformed field=delta.audience value=\"secret\" allowed=declaration.audiences"
        );

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
}
