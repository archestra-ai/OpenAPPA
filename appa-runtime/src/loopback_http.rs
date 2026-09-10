//! One plain HTTP request to the runtime on loopback, bounded by a deadline.
//!
//! Every client this binary runs as — the hook poster, the statusline, the
//! starter — reaches the runtime through here. Only `http` is spoken: the
//! runtime refuses to listen anywhere but loopback, so there is no transport
//! to secure, and a socket is cheaper than an HTTP client on a path the
//! harness pays for on every tool call.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

/// Where the runtime answers: an authority to connect to and the prefix its
/// routes hang under.
pub(crate) struct Endpoint {
    authority: String,
    prefix: String,
}

impl Endpoint {
    pub(crate) fn parse(url: &str) -> Result<Self, String> {
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

    /// The `host:port` the runtime listens on, as the URL spelled it.
    pub(crate) fn authority(&self) -> &str {
        &self.authority
    }

    /// Every address this authority resolves to, in the order the resolver
    /// gives them. `localhost` resolves to both loopback families and the
    /// runtime listens on one of them, so the first address is a candidate
    /// and not the answer.
    fn addresses(&self) -> Result<Vec<SocketAddr>, String> {
        let addresses: Vec<SocketAddr> = self
            .authority
            .to_socket_addrs()
            .map_err(|error| format!("{} does not resolve: {error}", self.authority))?
            .collect();
        match addresses.is_empty() {
            true => Err(format!("{} resolves to no address", self.authority)),
            false => Ok(addresses),
        }
    }

    fn request_head(&self, method: &str, path: &str, length: usize) -> String {
        format!(
            "{method} {}{path} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\n\
             Content-Length: {length}\r\nConnection: close\r\n\r\n",
            self.prefix, self.authority
        )
    }
}

/// The wall clock the whole round trip runs against. Every socket operation takes
/// what is left of it, so a trickle of bytes cannot outlast the budget the way a
/// per-operation timeout allows.
pub(crate) struct Deadline(Instant);

impl Deadline {
    pub(crate) fn spanning(budget: Duration) -> Self {
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

pub(crate) struct Answer {
    pub(crate) status: u16,
    pub(crate) body: Vec<u8>,
}

impl Answer {
    pub(crate) fn is_success(&self) -> bool {
        (200..=299).contains(&self.status)
    }
}

/// The first of these addresses that accepts, with the address it reached. Each
/// attempt takes what is left of the deadline rather than a share of it: a
/// refused address answers at once, and the budget the whole round trip runs
/// against — never the number of addresses — is what bounds the walk.
fn connect(addresses: &[SocketAddr], deadline: &Deadline) -> Result<(TcpStream, SocketAddr), String> {
    let mut refusals = Vec::new();
    for address in addresses {
        match TcpStream::connect_timeout(address, deadline.left()?) {
            Ok(socket) => return Ok((socket, *address)),
            Err(error) => refusals.push(format!("cannot reach {address}: {error}")),
        }
    }
    Err(refusals.join("; "))
}

/// One request, answered inside the deadline or refused.
pub(crate) fn request(
    endpoint: &Endpoint,
    method: &str,
    path: &str,
    body: &[u8],
    deadline: &Deadline,
) -> Result<Answer, String> {
    let (mut socket, address) = connect(&endpoint.addresses()?, deadline)?;
    socket.set_nodelay(true).ok();

    for part in [endpoint.request_head(method, path, body.len()).as_bytes(), body] {
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
            Ok(read) => {
                answer.extend_from_slice(&chunk[..read]);
                if declared_answer_len(&answer)?.is_some_and(|length| answer.len() >= length) {
                    break;
                }
            }
            Err(error) => return Err(format!("cannot read the answer from {address}: {error}")),
        }
    }
    parse(&answer)
}

pub(crate) fn get(endpoint: &Endpoint, path: &str, deadline: &Deadline) -> Result<Answer, String> {
    request(endpoint, "GET", path, b"", deadline)
}

fn declared_answer_len(answer: &[u8]) -> Result<Option<usize>, String> {
    let Some(end_of_head) = answer.windows(4).position(|window| window == b"\r\n\r\n") else {
        return Ok(None);
    };
    let head =
        std::str::from_utf8(&answer[..end_of_head]).map_err(|_| "the answer's headers are not text".to_owned())?;
    let mut declared = None;
    for line in head.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("content-length") {
            if declared.is_some() {
                return Err("the answer carries more than one content-length".to_owned());
            }
            declared = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| "the answer carries an invalid content-length".to_owned())?,
            );
        }
    }
    declared
        .map(|length| {
            end_of_head
                .checked_add(4)
                .and_then(|head_length| head_length.checked_add(length))
                .ok_or_else(|| "the answer's content-length overflows this platform".to_owned())
        })
        .transpose()
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
    if let Some(length) = declared_answer_len(answer)?
        && answer.len() != length
    {
        return Err("the answer body does not match its content-length".to_owned());
    }
    Ok(Answer {
        status,
        body: answer[end_of_head + 4..].to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_endpoint_is_an_authority_and_the_prefix_its_routes_hang_under() {
        let plain = Endpoint::parse("http://127.0.0.1:8787").expect("a bare authority parses");
        assert_eq!(plain.authority, "127.0.0.1:8787");
        assert_eq!(
            plain.request_head("POST", "/hook", 3),
            "POST /hook HTTP/1.1\r\nHost: 127.0.0.1:8787\r\nContent-Type: application/json\r\n\
             Content-Length: 3\r\nConnection: close\r\n\r\n"
        );

        let nested = Endpoint::parse("http://127.0.0.1:8787/appa/").expect("a prefix parses");
        assert!(
            nested
                .request_head("GET", "/health", 0)
                .starts_with("GET /appa/health HTTP/1.1\r\n")
        );

        assert!(Endpoint::parse("https://127.0.0.1:8787").is_err());
        assert!(Endpoint::parse("127.0.0.1:8787").is_err());
        assert!(Endpoint::parse("http:///hook").is_err());
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
        assert!(parse(b"HTTP/1.1 200 OK\r\ncontent-length: 3\r\n\r\n{}").is_err());
        assert!(parse(b"HTTP/1.1 200 OK\r\ncontent-length: 1\r\n\r\n{}").is_err());
        assert!(parse(b"garbage\r\n\r\n").is_err());
    }

    #[test]
    fn a_complete_declared_body_does_not_wait_for_the_server_to_close() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port binds");
        let address = listener.local_addr().expect("the listener has an address");
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("the client connects");
            socket
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}")
                .expect("the server answers");
            std::thread::sleep(Duration::from_secs(1));
        });
        let endpoint = Endpoint::parse(&format!("http://{address}")).expect("the endpoint parses");
        let answer = request(
            &endpoint,
            "POST",
            "/hook",
            b"{}",
            &Deadline::spanning(Duration::from_millis(250)),
        )
        .expect("the complete body answers before the connection closes");
        assert_eq!(answer.body, b"{}");
        server.join().expect("the server exits");
    }

    /// A URL naming a host that resolves to more than one address — the
    /// `localhost` of every default install, which resolves to both loopback
    /// families — reaches the runtime on whichever of them it listens on. The
    /// first address is tried first and a refusal there is not the answer.
    #[test]
    fn a_refused_address_falls_through_to_the_next_the_authority_resolves_to() {
        let listening = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port binds");
        let live = listening.local_addr().expect("the listener has an address");
        let vacated = std::net::TcpListener::bind("127.0.0.1:0").expect("a second loopback port binds");
        let closed = vacated.local_addr().expect("the listener has an address");
        drop(vacated);

        let deadline = Deadline::spanning(Duration::from_secs(5));
        let (socket, reached) = connect(&[closed, live], &deadline).expect("the second address answers");
        assert_eq!(reached, live, "the refused address is not the one it posts to");
        assert_eq!(socket.peer_addr().expect("the socket is connected"), live);

        assert!(
            connect(&[closed], &deadline).is_err(),
            "an authority whose every address refuses is unreachable"
        );
        assert!(
            connect(&[closed, live], &Deadline::spanning(Duration::ZERO)).is_err(),
            "the walk stays inside the deadline the round trip runs against"
        );

        assert_eq!(
            Endpoint::parse("http://127.0.0.1:8787")
                .expect("the authority parses")
                .addresses()
                .expect("a literal address resolves"),
            vec![SocketAddr::from(([127, 0, 0, 1], 8787))]
        );
    }

    #[test]
    fn a_spent_deadline_refuses_rather_than_bounding_a_socket_by_zero() {
        assert!(Deadline::spanning(Duration::from_secs(5)).left().expect("time is left") > Duration::ZERO);
        assert!(Deadline::spanning(Duration::ZERO).left().is_err());
    }
}
