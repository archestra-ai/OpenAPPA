//! Calls to the externals: authorities, sanitizers, cast resolvers,
//! membership resolvers, and dynamic resolvers.

use serde::{Deserialize, Serialize};

use crate::config::{Endpoint, Externals};

/// Which registered external a consult addresses. Closed: the wire
/// format is per kind, not per deployment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsultKind {
    Authority,
    Sanitizer,
}

impl ConsultKind {
    fn wire_name(self) -> &'static str {
        match self {
            ConsultKind::Authority => "authority",
            ConsultKind::Sanitizer => "sanitizer",
        }
    }
}

/// Why a consult produced no answer. Diagnostic only: every reason has
/// the same no-answer effect, and none is a denial.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoAnswerReason {
    Unregistered,
    NonSuccess {
        status: u16,
    },
    Timeout,
    Transport,
    Malformed,
    Oversized,
    UnsupportedVersion,
}

/// The outcome of one consult: a typed answer for the engine to
/// validate, or no answer.
#[derive(Debug, Clone, PartialEq)]
pub enum ConsultOutcome {
    Answer(serde_json::Value),
    NoAnswer(NoAnswerReason),
}

/// The outcome of one dynamic resolution: the literal
/// readers, or an unresolved recipient.
#[derive(Debug, Clone, PartialEq)]
pub enum DynamicResolution {
    Resolved { readers: Vec<String> },
    Unresolved(NoAnswerReason),
}

#[derive(Debug, Serialize)]
struct ConsultRequest<'a> {
    version: u32,
    kind: &'static str,
    name: &'a str,
    payload: &'a serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct ConsultResponse {
    version: u32,
    answer: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct DynamicRequest<'a> {
    version: u32,
    resolver: &'a str,
    tool: &'a str,
    argument: &'a str,
    value: &'a str,
}

#[derive(Debug, Deserialize)]
struct DynamicResponse {
    version: u32,
    readers: Vec<String>,
}

/// The HTTP client over the configured endpoints. Async and lock-free;
/// the store's mutex is never in scope here.
pub struct ExternalServices {
    http: reqwest::Client,
    config: Externals,
}

impl ExternalServices {
    pub fn new(config: Externals) -> ExternalServices {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(config.timeout)
            .build()
            .expect("the reqwest client builds: no TLS or resolver overrides are set");
        ExternalServices { http, config }
    }

    /// One consult of a registered authority, sanitizer, cast
    /// resolver, or membership resolver. One POST, no retries.
    pub async fn consult(&self, kind: ConsultKind, name: &str, payload: &serde_json::Value) -> ConsultOutcome {
        let endpoint = match self.endpoint_for(kind, name) {
            Some(endpoint) => endpoint,
            None => {
                tracing::debug!(kind = kind.wire_name(), name, "consult of an unregistered external");
                return ConsultOutcome::NoAnswer(NoAnswerReason::Unregistered);
            }
        };
        let request = ConsultRequest {
            version: 1,
            kind: kind.wire_name(),
            name,
            payload,
        };
        let body = match self.post(endpoint, &request).await {
            Ok(body) => body,
            Err(reason) => return ConsultOutcome::NoAnswer(reason),
        };
        let response: ConsultResponse = match serde_json::from_slice(&body) {
            Ok(response) => response,
            Err(_) => return ConsultOutcome::NoAnswer(NoAnswerReason::Malformed),
        };
        if response.version != 1 {
            return ConsultOutcome::NoAnswer(NoAnswerReason::UnsupportedVersion);
        }
        ConsultOutcome::Answer(response.answer)
    }

    /// One dynamic resolution: the named string argument's
    /// value in, literal readers out.
    pub async fn resolve_dynamic(&self, resolver: &str, tool: &str, argument: &str, value: &str) -> DynamicResolution {
        let endpoint = match &self.config.dynamic {
            Some(endpoint) => endpoint,
            None => {
                tracing::debug!(resolver, "dynamic resolution without a configured endpoint");
                return DynamicResolution::Unresolved(NoAnswerReason::Unregistered);
            }
        };
        let request = DynamicRequest {
            version: 1,
            resolver,
            tool,
            argument,
            value,
        };
        let body = match self.post(endpoint, &request).await {
            Ok(body) => body,
            Err(reason) => return DynamicResolution::Unresolved(reason),
        };
        let response: DynamicResponse = match serde_json::from_slice(&body) {
            Ok(response) => response,
            Err(_) => return DynamicResolution::Unresolved(NoAnswerReason::Malformed),
        };
        if response.version != 1 {
            return DynamicResolution::Unresolved(NoAnswerReason::UnsupportedVersion);
        }
        if response
            .readers
            .iter()
            .any(|reader| reader == "public" || reader.starts_with('@'))
        {
            return DynamicResolution::Unresolved(NoAnswerReason::Malformed);
        }
        DynamicResolution::Resolved {
            readers: response.readers,
        }
    }

    fn endpoint_for(&self, kind: ConsultKind, name: &str) -> Option<&Endpoint> {
        let map = match kind {
            ConsultKind::Authority => &self.config.authorities,
            ConsultKind::Sanitizer => &self.config.sanitizers,
        };
        map.get(name)
    }

    async fn post<T: Serialize>(&self, endpoint: &Endpoint, request: &T) -> Result<Vec<u8>, NoAnswerReason> {
        let mut builder = self.http.post(&endpoint.url).json(request);
        if let Some(token) = &endpoint.token {
            builder = builder.bearer_auth(token.reveal());
        }
        let response = builder.send().await.map_err(classify_transport)?;
        let status = response.status();
        if !status.is_success() {
            return Err(NoAnswerReason::NonSuccess {
                status: status.as_u16(),
            });
        }
        let cap = self.config.max_body_bytes as u64;
        if response.content_length().is_some_and(|len| len > cap) {
            return Err(NoAnswerReason::Oversized);
        }
        let mut response = response;
        let mut body: Vec<u8> = Vec::new();
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    if body.len() as u64 + chunk.len() as u64 > cap {
                        return Err(NoAnswerReason::Oversized);
                    }
                    body.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(error) => return Err(classify_transport(error)),
            }
        }
        Ok(body)
    }
}

fn classify_transport(error: reqwest::Error) -> NoAnswerReason {
    if error.is_timeout() {
        NoAnswerReason::Timeout
    } else {
        NoAnswerReason::Transport
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use axum::Router;
    use axum::routing::post;

    use super::*;
    use crate::config::Token;

    async fn raw_stub(response: &'static [u8], hold_open: bool) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("an ephemeral loopback port binds");
        let addr = listener.local_addr().expect("the bound address is readable");
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("the stub accepts");
            let mut request = [0u8; 4096];
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let _ = socket.read(&mut request).await;
            socket.write_all(response).await.expect("the stub writes");
            if hold_open {
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        });
        format!("http://{addr}/")
    }

    async fn stub(router: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("an ephemeral loopback port binds");
        let addr = listener.local_addr().expect("the bound address is readable");
        tokio::spawn(async move {
            axum::serve(listener, router).await.expect("the stub serves");
        });
        format!("http://{addr}/")
    }

    fn services(dynamic_url: Option<String>, timeout_ms: u64, cap: usize) -> ExternalServices {
        ExternalServices::new(Externals {
            timeout: Duration::from_millis(timeout_ms),
            max_body_bytes: cap,
            authorities: BTreeMap::new(),
            sanitizers: BTreeMap::new(),
            dynamic: dynamic_url.map(|url| Endpoint { url, token: None }),
        })
    }

    async fn resolve(services: &ExternalServices) -> DynamicResolution {
        services
            .resolve_dynamic("recipients", "send_message", "channel", "#general")
            .await
    }

    #[tokio::test]
    async fn a_wellformed_resolution_returns_the_readers() {
        let url = stub(Router::new().route(
            "/",
            post(|body: String| async move {
                let request: serde_json::Value = serde_json::from_str(&body).expect("the request is JSON");
                for field in ["version", "resolver", "tool", "argument", "value"] {
                    assert!(request.get(field).is_some(), "missing request field {field}");
                }
                r#"{"version":1,"readers":["alice","bob"]}"#
            }),
        ))
        .await;
        let outcome = resolve(&services(Some(url), 2000, 65536)).await;
        assert_eq!(
            outcome,
            DynamicResolution::Resolved {
                readers: vec!["alice".to_string(), "bob".to_string()],
            },
        );
    }

    #[tokio::test]
    async fn every_failure_shape_resolves_nothing() {
        let url = stub(Router::new().route(
            "/",
            post(|| async { (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom") }),
        ))
        .await;
        assert_eq!(
            resolve(&services(Some(url), 2000, 65536)).await,
            DynamicResolution::Unresolved(NoAnswerReason::NonSuccess { status: 500 }),
        );

        let url = stub(Router::new().route("/", post(|| async { "not json at all" }))).await;
        assert_eq!(
            resolve(&services(Some(url), 2000, 65536)).await,
            DynamicResolution::Unresolved(NoAnswerReason::Malformed),
        );

        let url = stub(Router::new().route("/", post(|| async { r#"{"version":1,"readers":[42]}"# }))).await;
        assert_eq!(
            resolve(&services(Some(url), 2000, 65536)).await,
            DynamicResolution::Unresolved(NoAnswerReason::Malformed),
        );

        let url = stub(Router::new().route("/", post(|| async { r#"{"version":1}"# }))).await;
        assert_eq!(
            resolve(&services(Some(url), 2000, 65536)).await,
            DynamicResolution::Unresolved(NoAnswerReason::Malformed),
        );

        let url = stub(Router::new().route("/", post(|| async { r#"{"version":2,"readers":["alice"]}"# }))).await;
        assert_eq!(
            resolve(&services(Some(url), 2000, 65536)).await,
            DynamicResolution::Unresolved(NoAnswerReason::UnsupportedVersion),
        );

        let url =
            stub(Router::new().route("/", post(|| async { r#"{"version":1,"readers":["alice","public"]}"# }))).await;
        assert_eq!(
            resolve(&services(Some(url), 2000, 65536)).await,
            DynamicResolution::Unresolved(NoAnswerReason::Malformed),
        );

        let url = stub(Router::new().route(
            "/",
            post(|| async { format!(r#"{{"version":1,"readers":["{}"]}}"#, "r".repeat(1000)) }),
        ))
        .await;
        assert_eq!(
            resolve(&services(Some(url), 2000, 64)).await,
            DynamicResolution::Unresolved(NoAnswerReason::Oversized),
        );

        let url = stub(Router::new().route(
            "/",
            post(|| async {
                tokio::time::sleep(Duration::from_millis(500)).await;
                r#"{"version":1,"readers":["alice"]}"#
            }),
        ))
        .await;
        assert_eq!(
            resolve(&services(Some(url), 50, 65536)).await,
            DynamicResolution::Unresolved(NoAnswerReason::Timeout),
        );

        assert_eq!(
            resolve(&services(None, 2000, 65536)).await,
            DynamicResolution::Unresolved(NoAnswerReason::Unregistered),
        );

        let url =
            stub(Router::new().route("/", post(|| async { r#"{"version":1,"readers":["alice","@admins"]}"# }))).await;
        assert_eq!(
            resolve(&services(Some(url), 2000, 65536)).await,
            DynamicResolution::Unresolved(NoAnswerReason::Malformed),
        );

        let url = stub(Router::new().route(
            "/",
            post(|| async {
                (
                    axum::http::StatusCode::MOVED_PERMANENTLY,
                    [("location", "http://127.0.0.1:1/elsewhere")],
                    "moved",
                )
            }),
        ))
        .await;
        assert_eq!(
            resolve(&services(Some(url), 2000, 65536)).await,
            DynamicResolution::Unresolved(NoAnswerReason::NonSuccess { status: 301 }),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("an ephemeral loopback port binds");
        let dead = format!("http://{}/", listener.local_addr().expect("addr"));
        drop(listener);
        assert_eq!(
            resolve(&services(Some(dead), 2000, 65536)).await,
            DynamicResolution::Unresolved(NoAnswerReason::Transport),
        );
    }

    #[tokio::test]
    async fn an_undeclared_length_body_still_hits_the_byte_cap() {
        let body = format!("{:x}\r\n{}\r\n0\r\n\r\n", 600, "x".repeat(600));
        let response =
            format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\n\r\n{body}");
        let url = raw_stub(response.leak().as_bytes(), false).await;
        assert_eq!(
            resolve(&services(Some(url), 2000, 64)).await,
            DynamicResolution::Unresolved(NoAnswerReason::Oversized),
        );
    }

    #[tokio::test]
    async fn a_stalled_body_read_is_a_timeout() {
        let response =
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 1000\r\n\r\n{\"version\":1,";
        let url = raw_stub(response.as_bytes(), true).await;
        assert_eq!(
            resolve(&services(Some(url), 200, 65536)).await,
            DynamicResolution::Unresolved(NoAnswerReason::Timeout),
        );
    }

    #[tokio::test]
    async fn a_consult_carries_its_bearer_token_and_returns_the_answer() {
        let url = stub(Router::new().route(
            "/",
            post(|headers: axum::http::HeaderMap, body: String| async move {
                assert_eq!(
                    headers.get("authorization").and_then(|value| value.to_str().ok()),
                    Some("Bearer sekret"),
                );
                let request: serde_json::Value = serde_json::from_str(&body).expect("the request is JSON");
                assert_eq!(request["kind"], "authority");
                assert_eq!(request["name"], "security");
                r#"{"version":1,"answer":{"authorized":true}}"#
            }),
        ))
        .await;
        let mut authorities = BTreeMap::new();
        authorities.insert(
            "security".to_string(),
            Endpoint {
                url,
                token: Some(Token::new("sekret".to_string())),
            },
        );
        let services = ExternalServices::new(Externals {
            timeout: Duration::from_millis(2000),
            max_body_bytes: 65536,
            authorities,
            sanitizers: BTreeMap::new(),
            dynamic: None,
        });
        let outcome = services
            .consult(
                ConsultKind::Authority,
                "security",
                &serde_json::json!({"call": "send_message"}),
            )
            .await;
        assert_eq!(outcome, ConsultOutcome::Answer(serde_json::json!({"authorized": true})),);
    }

    #[tokio::test]
    async fn a_consult_failure_is_no_answer_never_a_denial() {
        let services = services(None, 2000, 65536);
        assert_eq!(
            services
                .consult(ConsultKind::Authority, "directory", &serde_json::json!({}))
                .await,
            ConsultOutcome::NoAnswer(NoAnswerReason::Unregistered),
        );

        let url = stub(Router::new().route("/", post(|| async { (axum::http::StatusCode::FORBIDDEN, "nope") }))).await;
        let mut authorities = BTreeMap::new();
        authorities.insert("directory".to_string(), Endpoint { url, token: None });
        let services = ExternalServices::new(Externals {
            timeout: Duration::from_millis(2000),
            max_body_bytes: 65536,
            authorities,
            sanitizers: BTreeMap::new(),
            dynamic: None,
        });
        assert_eq!(
            services
                .consult(ConsultKind::Authority, "directory", &serde_json::json!({}))
                .await,
            ConsultOutcome::NoAnswer(NoAnswerReason::NonSuccess { status: 403 }),
        );

        let url = stub(Router::new().route("/", post(|| async { "not json" }))).await;
        let mut sanitizers = BTreeMap::new();
        sanitizers.insert("channel".to_string(), Endpoint { url, token: None });
        let services = ExternalServices::new(Externals {
            timeout: Duration::from_millis(2000),
            max_body_bytes: 65536,
            authorities: BTreeMap::new(),
            sanitizers,
            dynamic: None,
        });
        assert_eq!(
            services
                .consult(ConsultKind::Sanitizer, "channel", &serde_json::json!({}))
                .await,
            ConsultOutcome::NoAnswer(NoAnswerReason::Malformed),
        );
    }
}
