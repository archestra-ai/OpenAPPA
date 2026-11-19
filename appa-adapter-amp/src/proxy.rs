//! The orb's network proxy: every outbound connection from the orb is a flow of
//! the thread's trajectory, checked by the runtime before a byte leaves.
//!
//! In an orb the filesystem is created for the thread and discarded with it, so
//! it is part of the trajectory and needs no file tracking. What leaves the
//! trajectory is network traffic, and the proxy is where it crosses. Each
//! connection is posted on the canonical hook wire as one tool call,
//! [`NETWORK_TOOL`] with string arguments `host` and `port`, under the thread's
//! trajectory. Policy governs it like any other tool: `requires` bounds what the
//! trajectory may send to a host, and `delta` labels what the host sends back.
//!
//! The outcome is admitted after the upstream connection opens and before any
//! byte is relayed, so a host's `delta` narrows the trajectory before its
//! response can reach a file, a shell, or the model. Any failure to reach the
//! runtime, or any answer other than a release, refuses the connection.
//!
//! The proxy speaks HTTP `CONNECT` (any TCP protocol, usually TLS) and plain
//! HTTP requests in absolute form. It resolves names itself, so the checked
//! host is the name the client asked for. Bytes on a connection only ever
//! reach the checked destination, so a client cannot reuse a connection to
//! reach another host.

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The raw tool spelling of one outbound connection; policy names it
/// `host/amp/appa_network`.
pub const NETWORK_TOOL: &str = "appa_network";

/// A proxy request head larger than this is refused rather than buffered.
const MAX_HEAD: usize = 64 * 1024;

/// Where the runtime answers the hook wire, and the trajectory every connection
/// belongs to.
pub struct Runtime {
    authority: String,
    root: String,
    /// When this proxy started, so call ids stay unique across restarts.
    started: u128,
    calls: AtomicU64,
}

impl Runtime {
    /// `url` is the runtime's `http://host:port`, as the plugin's
    /// `APPA_AMP_RUNTIME_URL`; `thread` is the Amp thread id the plugin posts
    /// as its `root_id`.
    pub fn new(url: &str, thread: &str) -> Result<Self, String> {
        let authority = url
            .strip_prefix("http://")
            .map(|rest| rest.trim_end_matches('/'))
            .filter(|authority| !authority.is_empty() && !authority.contains('/'))
            .ok_or_else(|| format!("{url} is not an http://host:port runtime URL"))?;
        if thread.is_empty() {
            return Err("no Amp thread id: pass --thread or set AMP_THREAD_ID".into());
        }
        Ok(Self {
            authority: authority.to_owned(),
            root: thread.to_owned(),
            started: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| since.as_nanos()),
            calls: AtomicU64::new(0),
        })
    }

    /// Ask whether the trajectory may open a connection to `destination`; when
    /// released, open it and admit the outcome before returning the stream.
    async fn open(&self, destination: &Destination) -> Result<TcpStream, String> {
        let call_id = format!(
            "{NETWORK_TOOL}-{}-{}",
            self.started,
            self.calls.fetch_add(1, Ordering::Relaxed)
        );
        let arguments = json!({ "host": destination.host, "port": destination.port.to_string() });
        let decision = self
            .post(json!({
                "event": "tool_call", "tool": NETWORK_TOOL, "arguments": arguments, "call_id": call_id,
            }))
            .await?;
        match decision["decision"].as_str() {
            Some("allow_call") => {}
            Some("deny_call") => return Err(text(&decision, "feedback")),
            Some("block") => return Err(text(&decision, "reason")),
            _ => return Err(format!("unexpected runtime decision {decision}")),
        }
        let upstream = TcpStream::connect((destination.host.as_str(), destination.port)).await;
        let outcome = match &upstream {
            // Only an admitted body carries the host's `delta` into the trajectory.
            Ok(_) => {
                json!({ "status": "success", "body": format!("connected to {}:{}", destination.host, destination.port) })
            }
            Err(error) => json!({ "status": "failure", "message": error.to_string() }),
        };
        let admitted = self
            .post(json!({
                "event": "tool_result", "tool": NETWORK_TOOL, "arguments": arguments, "call_id": call_id,
                "outcome": outcome,
            }))
            .await?;
        // Any delivery decision admits the outcome; only a withheld result stops the relay.
        if admitted["decision"] == "block" {
            return Err(text(&admitted, "reason"));
        }
        upstream.map_err(|error| format!("{}:{} is unreachable: {error}", destination.host, destination.port))
    }

    /// One hook event, posted over a fresh loopback connection. A refusal or an
    /// unreadable answer is an error, never a release.
    async fn post(&self, mut event: Value) -> Result<Value, String> {
        event["protocol"] = json!(1);
        event["adapter"] = json!("amp");
        event["root_id"] = json!(self.root);
        let body = event.to_string();
        let unreachable =
            |error: std::io::Error| format!("the APPA runtime at {} is unreachable: {error}", self.authority);
        let mut stream = TcpStream::connect(&self.authority).await.map_err(unreachable)?;
        let request = format!(
            "POST /hook HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.authority,
            body.len()
        );
        stream.write_all(request.as_bytes()).await.map_err(unreachable)?;
        let mut answer = Vec::new();
        stream.read_to_end(&mut answer).await.map_err(unreachable)?;
        let answer = String::from_utf8_lossy(&answer);
        let (_, body) = answer
            .split_once("\r\n\r\n")
            .ok_or("the APPA runtime sent an unreadable answer")?;
        let reply: Value = serde_json::from_str(body).map_err(|_| "the APPA runtime sent an unreadable answer")?;
        match reply["decision"].as_str() {
            Some("refuse") => Err(format!("the APPA runtime refused: {}", text(&reply, "detail"))),
            Some(_) if reply["protocol"] == 1 => Ok(reply),
            _ => Err("the APPA runtime sent an unreadable answer".into()),
        }
    }
}

fn text(reply: &Value, field: &str) -> String {
    reply[field].as_str().unwrap_or("no reason given").to_owned()
}

/// A host and port a client asked the proxy to reach.
#[derive(Debug, PartialEq, Eq)]
struct Destination {
    host: String,
    port: u16,
}

impl Destination {
    /// `host:port`, `[v6]:port`, or a bare host with `default_port`. The host is
    /// lowercased and loses a trailing dot, so one name has one spelling in policy.
    fn parse(authority: &str, default_port: Option<u16>) -> Option<Self> {
        let (host, port) = match authority.strip_prefix('[') {
            Some(rest) => {
                let (host, after) = rest.split_once(']')?;
                (host, after.strip_prefix(':'))
            }
            None => match authority.rsplit_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (authority, None),
            },
        };
        let port = match port {
            Some(port) => port.parse().ok()?,
            None => default_port?,
        };
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        let valid = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '_');
        (!host.is_empty() && host.chars().all(valid) && port != 0).then_some(Self { host, port })
    }
}

/// What a client asked for, read from its request head.
#[derive(Debug, PartialEq, Eq)]
enum Request {
    /// `CONNECT host:port`: a tunnel, answered before relaying.
    Tunnel(Destination),
    /// An absolute-form HTTP request, forwarded with its target in origin form.
    Forward { destination: Destination, head: Vec<u8> },
}

impl Request {
    fn parse(head: &[u8]) -> Result<Self, &'static str> {
        let text = std::str::from_utf8(head).map_err(|_| "request head is not UTF-8")?;
        let (line, rest) = text.split_once("\r\n").ok_or("request head has no line end")?;
        let mut parts = line.split(' ');
        let (Some(method), Some(target), Some(version), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err("malformed request line");
        };
        if method == "CONNECT" {
            return Destination::parse(target, None)
                .map(Request::Tunnel)
                .ok_or("CONNECT needs host:port");
        }
        let after_scheme = target
            .strip_prefix("http://")
            .ok_or("only CONNECT and absolute http:// requests are proxied")?;
        let (authority, path) = match after_scheme.find(['/', '?']) {
            Some(index) => after_scheme.split_at(index),
            None => (after_scheme, "/"),
        };
        if authority.contains('@') {
            return Err("credentials in the request target are not proxied");
        }
        let destination = Destination::parse(authority, Some(80)).ok_or("invalid host in request target")?;
        let path = if path.starts_with('/') {
            path.to_owned()
        } else {
            format!("/{path}")
        };
        let head = format!("{method} {path} {version}\r\n{rest}").into_bytes();
        Ok(Request::Forward { destination, head })
    }
}

/// Accept connections until the listener fails; each is checked and relayed on its own task.
pub async fn serve(listener: TcpListener, runtime: Runtime) -> std::io::Result<()> {
    let runtime = std::sync::Arc::new(runtime);
    loop {
        let (client, _) = listener.accept().await?;
        let runtime = runtime.clone();
        tokio::spawn(async move {
            if let Err(error) = relay(client, &runtime).await {
                tracing::debug!(%error, "proxied connection ended");
            }
        });
    }
}

async fn relay(mut client: TcpStream, runtime: &Runtime) -> std::io::Result<()> {
    let mut buffer = Vec::new();
    let head_end = loop {
        if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
        if buffer.len() > MAX_HEAD {
            return respond(
                &mut client,
                "431 Request Header Fields Too Large",
                "request head too large",
            )
            .await;
        }
        let mut chunk = [0; 4096];
        match client.read(&mut chunk).await? {
            0 => return Ok(()),
            read => buffer.extend_from_slice(&chunk[..read]),
        }
    };
    let request = match Request::parse(&buffer[..head_end]) {
        Ok(request) => request,
        Err(reason) => return respond(&mut client, "400 Bad Request", reason).await,
    };
    let destination = match &request {
        Request::Tunnel(destination) | Request::Forward { destination, .. } => destination,
    };
    let mut upstream = match runtime.open(destination).await {
        Ok(upstream) => upstream,
        Err(reason) => {
            tracing::info!(host = %destination.host, port = destination.port, %reason, "connection refused by APPA");
            let reason = format!("APPA refused {}:{}: {reason}\n", destination.host, destination.port);
            return respond(&mut client, "403 Forbidden", &reason).await;
        }
    };
    match request {
        Request::Tunnel(_) => client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await?,
        Request::Forward { head, .. } => upstream.write_all(&head).await?,
    }
    upstream.write_all(&buffer[head_end..]).await?;
    tokio::io::copy_bidirectional(&mut client, &mut upstream)
        .await
        .map(|_| ())
}

async fn respond(client: &mut TcpStream, status: &str, reason: &str) -> std::io::Result<()> {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reason}",
        reason.len()
    );
    client.write_all(response.as_bytes()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn destination(host: &str, port: u16) -> Destination {
        Destination {
            host: host.into(),
            port,
        }
    }

    #[test]
    fn destinations_have_one_spelling() {
        for (authority, default, expected) in [
            ("API.GitHub.com.:443", None, Some(destination("api.github.com", 443))),
            ("[::1]:8080", None, Some(destination("::1", 8080))),
            ("example.com", Some(80), Some(destination("example.com", 80))),
            ("example.com", None, None),
            ("example.com:0", None, None),
            ("example.com:http", None, None),
            ("exa mple.com:80", None, None),
            ("a/b:80", None, None),
            (":80", None, None),
        ] {
            assert_eq!(Destination::parse(authority, default), expected, "{authority}");
        }
    }

    #[test]
    fn requests_name_the_host_their_bytes_reach() {
        assert_eq!(
            Request::parse(b"CONNECT api.github.com:443 HTTP/1.1\r\nHost: evil.example\r\n\r\n"),
            Ok(Request::Tunnel(destination("api.github.com", 443)))
        );
        assert_eq!(
            Request::parse(b"GET http://Example.com:8080?q=1 HTTP/1.1\r\nHost: other.example\r\n\r\n"),
            Ok(Request::Forward {
                destination: destination("example.com", 8080),
                head: b"GET /?q=1 HTTP/1.1\r\nHost: other.example\r\n\r\n".to_vec(),
            })
        );
        for head in [
            &b"GET /relative HTTP/1.1\r\n\r\n"[..],
            b"GET https://example.com/ HTTP/1.1\r\n\r\n",
            b"GET http://user@example.com/ HTTP/1.1\r\n\r\n",
            b"CONNECT example.com HTTP/1.1\r\n\r\n",
            b"GET  http://example.com/ HTTP/1.1\r\n\r\n",
        ] {
            assert!(Request::parse(head).is_err(), "{}", String::from_utf8_lossy(head));
        }
    }
}
