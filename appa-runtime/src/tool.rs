//! South tool execution: the runtime *invokes* a tool and classifies the result into a
//! [`ToolOutcome`] (RP3) the turn-drive admits.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use appa_engine::value::{ResolvedCall, ToolName};

/// The maximum tool-result body the runtime will admit as a value. A larger 2xx body still commits
/// effects but is sealed, never admitted.
pub const DEFAULT_BODY_CAP_BYTES: usize = 256 * 1024;

/// A runtime-owned HTTP client with **redirects disabled** — a newtype so the safe policy is the
/// only way to obtain one. Redirect-following would let a backend hide a 3xx behind a final 2xx, or
/// resend a tool/authority payload to a `Location` it chose (307/308).
#[derive(Clone, Debug)]
pub struct HttpClient(reqwest::Client);

impl HttpClient {
    pub fn new() -> Self {
        HttpClient(
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("a default rustls reqwest client builds"),
        )
    }

    pub(crate) fn inner(&self) -> &reqwest::Client {
        &self.0
    }
}

impl Default for HttpClient {
    fn default() -> Self {
        HttpClient::new()
    }
}

/// A call rendered for south execution: the tool and its concrete arguments. Derived from the
/// engine's [`ResolvedCall`]; the argument-reference bookkeeping stays engine-side.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderedCall {
    pub tool: ToolName,
    pub arguments: serde_json::Value,
}

impl RenderedCall {
    pub fn from_call(call: &ResolvedCall) -> Self {
        RenderedCall {
            tool: call.tool().clone(),
            arguments: call.arguments().clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BodyDisposition {
    Available(String),
    RejectedTooLarge,
}

/// The real outcome of a tool invocation (RP3). Note that [`ToolOutcome::Failure`] is payload-free
/// by construction: a backend's error bytes never reach the model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolOutcome {
    Success { body: BodyDisposition },
    Failure,
    Indeterminate,
}

/// Classify a completed HTTP response into an outcome: 2xx → success (body admitted, or sealed if
/// over the cap); any other status → failure (bytes discarded).
pub fn classify_http(status: u16, body: String, cap: usize) -> ToolOutcome {
    if (200..300).contains(&status) {
        let disposition = if body.len() > cap {
            BodyDisposition::RejectedTooLarge
        } else {
            BodyDisposition::Available(body)
        };
        ToolOutcome::Success { body: disposition }
    } else {
        ToolOutcome::Failure
    }
}

/// A builtin tool — a fixture with a fixed outcome, for tests and self-contained deployments. Real
/// tools are `http` (MCP later).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BuiltinTool {
    Echo(String),
    Oversized(usize),
    Fail,
    Indeterminate,
}

impl BuiltinTool {
    fn invoke(&self, cap: usize) -> ToolOutcome {
        match self {
            BuiltinTool::Echo(body) => classify_http(200, body.clone(), cap),
            BuiltinTool::Oversized(len) => classify_http(200, "x".repeat(*len), cap),
            BuiltinTool::Fail => ToolOutcome::Failure,
            BuiltinTool::Indeterminate => ToolOutcome::Indeterminate,
        }
    }
}

#[derive(Clone, Debug)]
pub struct HttpTool {
    url: String,
    timeout: Duration,
    client: HttpClient,
}

impl HttpTool {
    pub fn new(url: impl Into<String>, timeout: Duration, client: HttpClient) -> Self {
        HttpTool {
            url: url.into(),
            timeout,
            client,
        }
    }

    async fn invoke(&self, call: &RenderedCall, cap: usize) -> ToolOutcome {
        let response = match self
            .client
            .inner()
            .post(&self.url)
            .timeout(self.timeout)
            .json(call)
            .send()
            .await
        {
            Ok(response) => response,
            // Timeout or connection fault: the request may have reached the backend.
            Err(_) => return ToolOutcome::Indeterminate,
        };
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return ToolOutcome::Failure;
        }
        read_capped(response, cap).await
    }
}

async fn read_capped(mut response: reqwest::Response, cap: usize) -> ToolOutcome {
    match read_body_capped(&mut response, cap).await {
        Some(body) if body.len() <= cap => ToolOutcome::Success {
            body: BodyDisposition::Available(String::from_utf8_lossy(&body).into_owned()),
        },
        Some(_) => ToolOutcome::Success {
            body: BodyDisposition::RejectedTooLarge,
        },
        None => ToolOutcome::Indeterminate,
    }
}

/// Read a response body up to `cap + 1` bytes (the extra byte only flags "over cap"), returning the
/// buffer or `None` on a transport fault. The buffer's length never exceeds `cap + 1`, so a backend
/// cannot drive it to grow with the response — total allocation stays `O(cap)`, independent of how
/// much the backend sends. `limit` uses saturating arithmetic, so an absurd `cap` near `usize::MAX`
/// cannot overflow.
pub(crate) async fn read_body_capped(response: &mut reqwest::Response, cap: usize) -> Option<Vec<u8>> {
    let limit = cap.saturating_add(1);
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                // Invariant at the top of the loop: body.len() <= cap < limit, so room >= 1.
                let room = limit - body.len();
                let take = room.min(chunk.len());
                body.extend_from_slice(&chunk[..take]);
                if body.len() > cap {
                    return Some(body);
                }
            }
            Ok(None) => return Some(body),
            Err(_) => return None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum ToolBackend {
    Builtin(BuiltinTool),
    Http(HttpTool),
}

impl ToolBackend {
    pub async fn invoke(&self, call: &RenderedCall, cap: usize) -> ToolOutcome {
        match self {
            ToolBackend::Builtin(builtin) => builtin.invoke(cap),
            ToolBackend::Http(http) => http.invoke(call, cap).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call() -> RenderedCall {
        RenderedCall {
            tool: ToolName::new("t"),
            arguments: json!({ "k": "v" }),
        }
    }

    #[tokio::test]
    async fn builtin_outcomes_cover_success_failure_indeterminate() {
        let cap = DEFAULT_BODY_CAP_BYTES;
        assert_eq!(
            ToolBackend::Builtin(BuiltinTool::Echo("ok".into()))
                .invoke(&call(), cap)
                .await,
            ToolOutcome::Success {
                body: BodyDisposition::Available("ok".into())
            }
        );
        assert_eq!(
            ToolBackend::Builtin(BuiltinTool::Fail).invoke(&call(), cap).await,
            ToolOutcome::Failure
        );
        assert_eq!(
            ToolBackend::Builtin(BuiltinTool::Indeterminate)
                .invoke(&call(), cap)
                .await,
            ToolOutcome::Indeterminate
        );
    }

    #[test]
    fn oversized_2xx_is_success_but_sealed() {
        let out = classify_http(200, "x".repeat(50), 10);
        assert_eq!(
            out,
            ToolOutcome::Success {
                body: BodyDisposition::RejectedTooLarge
            }
        );
    }

    #[test]
    fn non_2xx_is_failure_and_discards_the_body() {
        assert_eq!(
            classify_http(500, "internal secret leak".into(), 1024),
            ToolOutcome::Failure
        );
        assert_eq!(classify_http(404, "not found".into(), 1024), ToolOutcome::Failure);
    }

    #[test]
    fn body_exactly_at_cap_is_admitted_one_over_is_sealed() {
        assert_eq!(
            classify_http(200, "x".repeat(10), 10),
            ToolOutcome::Success {
                body: BodyDisposition::Available("x".repeat(10))
            }
        );
        assert_eq!(
            classify_http(200, "x".repeat(11), 10),
            ToolOutcome::Success {
                body: BodyDisposition::RejectedTooLarge
            }
        );
    }

    #[tokio::test]
    async fn http_tool_admits_a_body_exactly_at_cap_over_the_wire() {
        let (addr, server) = spawn_capture_server("200 OK", &"z".repeat(16)).await;
        let tool = HttpTool::new(
            format!("http://{addr}/exact"),
            Duration::from_secs(5),
            HttpClient::new(),
        );
        assert_eq!(
            tool.invoke(&call(), 16).await,
            ToolOutcome::Success {
                body: BodyDisposition::Available("z".repeat(16))
            }
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn http_tool_seals_at_one_byte_over_cap_over_the_wire() {
        let (addr, server) = spawn_capture_server("200 OK", &"z".repeat(17)).await;
        let tool = HttpTool::new(format!("http://{addr}/over"), Duration::from_secs(5), HttpClient::new());
        assert_eq!(
            tool.invoke(&call(), 16).await,
            ToolOutcome::Success {
                body: BodyDisposition::RejectedTooLarge
            }
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn http_tool_posts_rendered_call_across_a_real_socket() {
        let (addr, server) = spawn_capture_server("200 OK", "done").await;
        let tool = HttpTool::new(
            format!("http://{addr}/invoke"),
            Duration::from_secs(5),
            HttpClient::new(),
        );
        let outcome = tool.invoke(&call(), DEFAULT_BODY_CAP_BYTES).await;
        assert_eq!(
            outcome,
            ToolOutcome::Success {
                body: BodyDisposition::Available("done".into())
            }
        );

        let request = server.await.unwrap();
        assert!(request.starts_with("POST /invoke "), "request line: {request:?}");
        let body = request.split("\r\n\r\n").nth(1).expect("request has a body");
        let parsed: RenderedCall = serde_json::from_str(body).expect("body is a RenderedCall");
        assert_eq!(parsed, call());
    }

    #[tokio::test]
    async fn http_tool_oversized_2xx_seals_across_the_wire() {
        let (addr, server) = spawn_capture_server("200 OK", &"y".repeat(64)).await;
        let tool = HttpTool::new(format!("http://{addr}/big"), Duration::from_secs(5), HttpClient::new());
        assert_eq!(
            tool.invoke(&call(), 8).await,
            ToolOutcome::Success {
                body: BodyDisposition::RejectedTooLarge
            }
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn http_tool_non_2xx_is_failure_across_the_wire() {
        let (addr, server) = spawn_capture_server("500 Internal Server Error", "leaked secret bytes").await;
        let tool = HttpTool::new(format!("http://{addr}/fail"), Duration::from_secs(5), HttpClient::new());
        assert_eq!(tool.invoke(&call(), DEFAULT_BODY_CAP_BYTES).await, ToolOutcome::Failure);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn http_tool_treats_connection_refused_as_indeterminate() {
        let tool = HttpTool::new("http://127.0.0.1:1/x", Duration::from_millis(200), HttpClient::new());
        assert_eq!(
            tool.invoke(&call(), DEFAULT_BODY_CAP_BYTES).await,
            ToolOutcome::Indeterminate
        );
    }

    async fn spawn_capture_server(
        status_line: &'static str,
        body: &str,
    ) -> (std::net::SocketAddr, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let body = body.to_string();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let mut received = Vec::new();
            loop {
                let n = socket.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                received.extend_from_slice(&buf[..n]);
                if let Some(pos) = received.windows(4).position(|w| w == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&received[..pos]).to_lowercase();
                    let len: usize = header
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(0);
                    if received.len() >= pos + 4 + len {
                        break;
                    }
                }
            }
            let response = format!("HTTP/1.1 {status_line}\r\nContent-Length: {}\r\n\r\n{body}", body.len());
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.flush().await.unwrap();
            String::from_utf8_lossy(&received).to_string()
        });
        (addr, handle)
    }
}
