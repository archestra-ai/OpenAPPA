//! Calls to the externals: authorities, sanitizers, dynamic resolvers,
//! and the membership resolver.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::builtins::{
    BuiltinAuthority, BuiltinSanitizer, LoadedModule, MODULE_OUTPUT_CEILING, ModuleRegistry, ModulesError,
};
use crate::config::{Endpoint, Externals, Implementation};
use crate::elicit::Elicitation;

const HITL: &str = "hitl";

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
    Unreachable,
    Dismissed,
    NonSuccess {
        status: u16,
    },
    Timeout,
    Transport,
    Malformed,
    Oversized,
    UnsupportedVersion,
    ModuleError,
    ModulePanicked,
}

/// The outcome of one consult: a typed answer for the engine to
/// validate, or no answer.
#[derive(Debug, Clone, PartialEq)]
pub enum ConsultOutcome {
    Answer(serde_json::Value),
    NoAnswer(NoAnswerReason),
}

/// The outcome of one reader-set resolution — dynamic or membership:
/// the literal readers, or no answer. An empty
/// reader set is a successful answer.
#[derive(Debug, Clone, PartialEq)]
pub enum ReadersResolution {
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

#[derive(Debug, Serialize)]
struct MembershipRequest<'a> {
    version: u32,
    resolver: &'a str,
    group: &'a str,
}

#[derive(Debug, Deserialize)]
struct ReadersResponse {
    version: u32,
    readers: Vec<String>,
}

enum AuthorityBackend {
    Resolver(Endpoint),
    Stock(BuiltinAuthority),
    Module(Arc<LoadedModule>),
    Hitl,
}

enum SanitizerBackend {
    Resolver(Endpoint),
    Stock(BuiltinSanitizer),
    Module(Arc<LoadedModule>),
}

/// The dispatch tables over the configured implementations. Async and
/// lock-free on the HTTP path; a module call serializes on its own
/// gate inside a blocking task. The store's mutex is never in scope
/// here.
pub struct ExternalServices {
    http: reqwest::Client,
    max_body_bytes: usize,
    authorities: BTreeMap<String, AuthorityBackend>,
    sanitizers: BTreeMap<String, SanitizerBackend>,
    dynamic: Option<Endpoint>,
    membership: Option<Endpoint>,
}

impl ExternalServices {
    /// Resolves every configured `builtin` reference against the stock
    /// implementations and the loaded modules. An unknown reference is
    /// a refusal: a deployment never opens with a dangling
    /// implementation name.
    pub fn new(config: Externals, registry: ModuleRegistry) -> Result<ExternalServices, ModulesError> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(config.timeout)
            .build()
            .expect("the reqwest client builds: no TLS or resolver overrides are set");
        let mut authorities = BTreeMap::new();
        for (name, implementation) in config.authorities {
            let backend = match implementation {
                Implementation::Resolver(endpoint) => AuthorityBackend::Resolver(endpoint),
                Implementation::Builtin(builtin) if builtin == HITL => AuthorityBackend::Hitl,
                Implementation::Builtin(builtin) => match BuiltinAuthority::from_name(&builtin) {
                    Some(stock) => AuthorityBackend::Stock(stock),
                    None => match registry.authority(&builtin) {
                        Some(module) => AuthorityBackend::Module(Arc::clone(module)),
                        None => {
                            return Err(ModulesError::UnknownBuiltin {
                                section: "authorities",
                                name,
                                builtin,
                            });
                        }
                    },
                },
            };
            authorities.insert(name, backend);
        }
        let mut sanitizers = BTreeMap::new();
        for (name, implementation) in config.sanitizers {
            let backend = match implementation {
                Implementation::Resolver(endpoint) => SanitizerBackend::Resolver(endpoint),
                Implementation::Builtin(builtin) => match BuiltinSanitizer::from_name(&builtin) {
                    Some(stock) => SanitizerBackend::Stock(stock),
                    None => match registry.sanitizer(&builtin) {
                        Some(module) => SanitizerBackend::Module(Arc::clone(module)),
                        None => {
                            return Err(ModulesError::UnknownBuiltin {
                                section: "sanitizers",
                                name,
                                builtin,
                            });
                        }
                    },
                },
            };
            sanitizers.insert(name, backend);
        }
        Ok(ExternalServices {
            http,
            max_body_bytes: config.max_body_bytes,
            authorities,
            sanitizers,
            dynamic: config.dynamic,
            membership: config.membership,
        })
    }

    /// One consult of a registered authority or sanitizer, dispatched
    /// on the component's configured implementation. `elicitation` is
    /// the open request that asked for the ruling; it is present only
    /// for an authority consult raised inside the remedy tool, and only
    /// the `hitl` backend reads it.
    pub async fn consult(
        &self,
        kind: ConsultKind,
        name: &str,
        payload: &serde_json::Value,
        elicitation: Option<&Elicitation>,
    ) -> ConsultOutcome {
        match kind {
            ConsultKind::Authority => match self.authorities.get(name) {
                None => unregistered(kind, name),
                Some(AuthorityBackend::Resolver(endpoint)) => self.post_consult(endpoint, kind, name, payload).await,
                Some(AuthorityBackend::Stock(stock)) => ConsultOutcome::Answer(stock.answer()),
                Some(AuthorityBackend::Module(module)) => self.call_module(module, kind, name, payload).await,
                Some(AuthorityBackend::Hitl) => match elicitation {
                    Some(elicitation) => elicitation.ask(payload).await,
                    // No live request to ask through — a `hitl`
                    // authority reachable from anywhere but the remedy
                    // tool would be a configuration this runtime cannot
                    // serve. It abstains rather than invent an answer.
                    None => {
                        tracing::warn!(name, "a hitl consult raised with no open request abstains");
                        ConsultOutcome::NoAnswer(NoAnswerReason::Unreachable)
                    }
                },
            },
            ConsultKind::Sanitizer => match self.sanitizers.get(name) {
                None => unregistered(kind, name),
                Some(SanitizerBackend::Resolver(endpoint)) => self.post_consult(endpoint, kind, name, payload).await,
                Some(SanitizerBackend::Stock(stock)) => match stock.answer(payload) {
                    Some(answer) => ConsultOutcome::Answer(answer),
                    None => ConsultOutcome::NoAnswer(NoAnswerReason::Malformed),
                },
                Some(SanitizerBackend::Module(module)) => self.call_module(module, kind, name, payload).await,
            },
        }
    }

    async fn post_consult(
        &self,
        endpoint: &Endpoint,
        kind: ConsultKind,
        name: &str,
        payload: &serde_json::Value,
    ) -> ConsultOutcome {
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

    async fn call_module(
        &self,
        module: &Arc<LoadedModule>,
        kind: ConsultKind,
        name: &str,
        payload: &serde_json::Value,
    ) -> ConsultOutcome {
        let request = ConsultRequest {
            version: 1,
            kind: kind.wire_name(),
            name,
            payload,
        };
        let input = match serde_json::to_vec(&request) {
            Ok(input) => input,
            Err(_) => return ConsultOutcome::NoAnswer(NoAnswerReason::ModuleError),
        };
        let capacity = self.max_body_bytes.min(MODULE_OUTPUT_CEILING);
        let module = Arc::clone(module);
        let outcome = tokio::task::spawn_blocking(move || {
            let Ok(_gate) = module.gate.lock() else {
                return Err(NoAnswerReason::ModuleError);
            };
            let mut output = vec![0u8; capacity];
            let mut written: usize = 0;
            let status =
                unsafe { (module.answer)(input.as_ptr(), input.len(), output.as_mut_ptr(), capacity, &mut written) };
            match status {
                appa_builtin::STATUS_OK => {
                    // A dishonest length never becomes a slice.
                    if written > capacity {
                        return Err(NoAnswerReason::Malformed);
                    }
                    output.truncate(written);
                    Ok(output)
                }
                appa_builtin::STATUS_PANICKED => Err(NoAnswerReason::ModulePanicked),
                appa_builtin::STATUS_OUTPUT_TOO_LARGE => Err(NoAnswerReason::Oversized),
                _ => Err(NoAnswerReason::ModuleError),
            }
        })
        .await;
        let reason = match outcome {
            Ok(Ok(bytes)) => match serde_json::from_slice(&bytes) {
                Ok(answer) => return ConsultOutcome::Answer(answer),
                Err(_) => NoAnswerReason::Malformed,
            },
            Ok(Err(reason)) => reason,
            Err(_join) => NoAnswerReason::ModuleError,
        };
        tracing::debug!(
            kind = kind.wire_name(),
            name,
            ?reason,
            "module consult produced no answer"
        );
        ConsultOutcome::NoAnswer(reason)
    }

    /// One dynamic resolution: the named string argument's
    /// value in, literal readers out.
    pub async fn resolve_dynamic(&self, resolver: &str, tool: &str, argument: &str, value: &str) -> ReadersResolution {
        let Some(endpoint) = &self.dynamic else {
            tracing::debug!(resolver, "dynamic resolution without a configured endpoint");
            return ReadersResolution::Unresolved(NoAnswerReason::Unregistered);
        };
        let request = DynamicRequest {
            version: 1,
            resolver,
            tool,
            argument,
            value,
        };
        match self.literal_readers(endpoint, &request).await {
            Ok(readers) => ReadersResolution::Resolved { readers },
            Err(reason) => ReadersResolution::Unresolved(reason),
        }
    }

    /// One membership resolution: a group name in, the
    /// group's literal readers out.
    pub async fn resolve_membership(&self, resolver: &str, group: &str) -> ReadersResolution {
        let Some(endpoint) = &self.membership else {
            tracing::debug!(resolver, group, "membership resolution without a configured endpoint");
            return ReadersResolution::Unresolved(NoAnswerReason::Unregistered);
        };
        let request = MembershipRequest {
            version: 1,
            resolver,
            group,
        };
        match self.literal_readers(endpoint, &request).await {
            Ok(readers) => ReadersResolution::Resolved { readers },
            Err(reason) => ReadersResolution::Unresolved(reason),
        }
    }

    async fn literal_readers(
        &self,
        endpoint: &Endpoint,
        request: &impl Serialize,
    ) -> Result<Vec<String>, NoAnswerReason> {
        let body = self.post(endpoint, request).await?;
        let response: ReadersResponse = serde_json::from_slice(&body).map_err(|_| NoAnswerReason::Malformed)?;
        if response.version != 1 {
            return Err(NoAnswerReason::UnsupportedVersion);
        }
        if response
            .readers
            .iter()
            .any(|reader| reader == "public" || reader.starts_with('@'))
        {
            return Err(NoAnswerReason::Malformed);
        }
        Ok(response.readers)
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
        let cap = self.max_body_bytes as u64;
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

fn unregistered(kind: ConsultKind, name: &str) -> ConsultOutcome {
    tracing::debug!(kind = kind.wire_name(), name, "consult of an unregistered external");
    ConsultOutcome::NoAnswer(NoAnswerReason::Unregistered)
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

    fn externals(dynamic_url: Option<String>, timeout_ms: u64, cap: usize) -> Externals {
        Externals {
            timeout: Duration::from_millis(timeout_ms),
            review_timeout: Duration::from_millis(timeout_ms),
            max_body_bytes: cap,
            authorities: BTreeMap::new(),
            sanitizers: BTreeMap::new(),
            dynamic: dynamic_url.clone().map(|url| Endpoint { url, token: None }),
            membership: dynamic_url.map(|url| Endpoint { url, token: None }),
        }
    }

    fn services_over(config: Externals) -> ExternalServices {
        ExternalServices::new(config, ModuleRegistry::empty()).expect("no builtin references are configured")
    }

    fn services(dynamic_url: Option<String>, timeout_ms: u64, cap: usize) -> ExternalServices {
        services_over(externals(dynamic_url, timeout_ms, cap))
    }

    async fn resolve(services: &ExternalServices) -> ReadersResolution {
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
            ReadersResolution::Resolved {
                readers: vec!["alice".to_string(), "bob".to_string()],
            },
        );
    }

    #[tokio::test]
    async fn a_membership_resolution_returns_the_groups_readers_or_nothing() {
        let url = stub(Router::new().route(
            "/",
            post(|body: String| async move {
                let request: serde_json::Value = serde_json::from_str(&body).expect("the request is JSON");
                assert_eq!(request["version"], 1);
                assert_eq!(request["resolver"], "directory");
                assert_eq!(request["group"], "auditors");
                r#"{"version":1,"readers":[]}"#
            }),
        ))
        .await;
        assert_eq!(
            services(Some(url), 2000, 65536)
                .resolve_membership("directory", "auditors")
                .await,
            ReadersResolution::Resolved { readers: vec![] },
        );

        let url = stub(Router::new().route("/", post(|| async { r#"{"version":1,"readers":["alice","bob"]}"# }))).await;
        assert_eq!(
            services(Some(url), 2000, 65536)
                .resolve_membership("directory", "auditors")
                .await,
            ReadersResolution::Resolved {
                readers: vec!["alice".to_string(), "bob".to_string()]
            },
        );

        let url = stub(Router::new().route("/", post(|| async { r#"{"version":1,"readers":["public"]}"# }))).await;
        assert_eq!(
            services(Some(url), 2000, 65536)
                .resolve_membership("directory", "auditors")
                .await,
            ReadersResolution::Unresolved(NoAnswerReason::Malformed),
        );
        let url = stub(Router::new().route("/", post(|| async { r#"{"version":1,"readers":["@nested"]}"# }))).await;
        assert_eq!(
            services(Some(url), 2000, 65536)
                .resolve_membership("directory", "auditors")
                .await,
            ReadersResolution::Unresolved(NoAnswerReason::Malformed),
        );
        let url = stub(Router::new().route(
            "/",
            post(|| async { (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom") }),
        ))
        .await;
        assert_eq!(
            services(Some(url), 2000, 65536)
                .resolve_membership("directory", "auditors")
                .await,
            ReadersResolution::Unresolved(NoAnswerReason::NonSuccess { status: 500 }),
        );
        assert_eq!(
            services(None, 2000, 65536)
                .resolve_membership("directory", "auditors")
                .await,
            ReadersResolution::Unresolved(NoAnswerReason::Unregistered),
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
            ReadersResolution::Unresolved(NoAnswerReason::NonSuccess { status: 500 }),
        );

        let url = stub(Router::new().route("/", post(|| async { "not json at all" }))).await;
        assert_eq!(
            resolve(&services(Some(url), 2000, 65536)).await,
            ReadersResolution::Unresolved(NoAnswerReason::Malformed),
        );

        let url = stub(Router::new().route("/", post(|| async { r#"{"version":1,"readers":[42]}"# }))).await;
        assert_eq!(
            resolve(&services(Some(url), 2000, 65536)).await,
            ReadersResolution::Unresolved(NoAnswerReason::Malformed),
        );

        let url = stub(Router::new().route("/", post(|| async { r#"{"version":1}"# }))).await;
        assert_eq!(
            resolve(&services(Some(url), 2000, 65536)).await,
            ReadersResolution::Unresolved(NoAnswerReason::Malformed),
        );

        let url = stub(Router::new().route("/", post(|| async { r#"{"version":2,"readers":["alice"]}"# }))).await;
        assert_eq!(
            resolve(&services(Some(url), 2000, 65536)).await,
            ReadersResolution::Unresolved(NoAnswerReason::UnsupportedVersion),
        );

        let url =
            stub(Router::new().route("/", post(|| async { r#"{"version":1,"readers":["alice","public"]}"# }))).await;
        assert_eq!(
            resolve(&services(Some(url), 2000, 65536)).await,
            ReadersResolution::Unresolved(NoAnswerReason::Malformed),
        );

        let url = stub(Router::new().route(
            "/",
            post(|| async { format!(r#"{{"version":1,"readers":["{}"]}}"#, "r".repeat(1000)) }),
        ))
        .await;
        assert_eq!(
            resolve(&services(Some(url), 2000, 64)).await,
            ReadersResolution::Unresolved(NoAnswerReason::Oversized),
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
            ReadersResolution::Unresolved(NoAnswerReason::Timeout),
        );

        assert_eq!(
            resolve(&services(None, 2000, 65536)).await,
            ReadersResolution::Unresolved(NoAnswerReason::Unregistered),
        );

        let url =
            stub(Router::new().route("/", post(|| async { r#"{"version":1,"readers":["alice","@admins"]}"# }))).await;
        assert_eq!(
            resolve(&services(Some(url), 2000, 65536)).await,
            ReadersResolution::Unresolved(NoAnswerReason::Malformed),
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
            ReadersResolution::Unresolved(NoAnswerReason::NonSuccess { status: 301 }),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("an ephemeral loopback port binds");
        let dead = format!("http://{}/", listener.local_addr().expect("addr"));
        drop(listener);
        assert_eq!(
            resolve(&services(Some(dead), 2000, 65536)).await,
            ReadersResolution::Unresolved(NoAnswerReason::Transport),
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
            ReadersResolution::Unresolved(NoAnswerReason::Oversized),
        );
    }

    #[tokio::test]
    async fn a_stalled_body_read_is_a_timeout() {
        let response =
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 1000\r\n\r\n{\"version\":1,";
        let url = raw_stub(response.as_bytes(), true).await;
        assert_eq!(
            resolve(&services(Some(url), 200, 65536)).await,
            ReadersResolution::Unresolved(NoAnswerReason::Timeout),
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
        let mut config = externals(None, 2000, 65536);
        config.authorities.insert(
            "security".to_string(),
            Implementation::Resolver(Endpoint {
                url,
                token: Some(Token::new("sekret".to_string())),
            }),
        );
        let services = services_over(config);
        let outcome = services
            .consult(
                ConsultKind::Authority,
                "security",
                &serde_json::json!({"call": "send_message"}),
                None,
            )
            .await;
        assert_eq!(outcome, ConsultOutcome::Answer(serde_json::json!({"authorized": true})),);
    }

    #[tokio::test]
    async fn a_consult_failure_is_no_answer_never_a_denial() {
        let services = services(None, 2000, 65536);
        assert_eq!(
            services
                .consult(ConsultKind::Authority, "directory", &serde_json::json!({}), None)
                .await,
            ConsultOutcome::NoAnswer(NoAnswerReason::Unregistered),
        );

        let url = stub(Router::new().route("/", post(|| async { (axum::http::StatusCode::FORBIDDEN, "nope") }))).await;
        let mut config = externals(None, 2000, 65536);
        config.authorities.insert(
            "directory".to_string(),
            Implementation::Resolver(Endpoint { url, token: None }),
        );
        let services = services_over(config);
        assert_eq!(
            services
                .consult(ConsultKind::Authority, "directory", &serde_json::json!({}), None)
                .await,
            ConsultOutcome::NoAnswer(NoAnswerReason::NonSuccess { status: 403 }),
        );

        let url = stub(Router::new().route("/", post(|| async { "not json" }))).await;
        let mut config = externals(None, 2000, 65536);
        config.sanitizers.insert(
            "channel".to_string(),
            Implementation::Resolver(Endpoint { url, token: None }),
        );
        let services = services_over(config);
        assert_eq!(
            services
                .consult(ConsultKind::Sanitizer, "channel", &serde_json::json!({}), None)
                .await,
            ConsultOutcome::NoAnswer(NoAnswerReason::Malformed),
        );
    }

    #[tokio::test]
    async fn a_stock_builtin_answers_without_any_endpoint() {
        let mut config = externals(None, 2000, 65536);
        config
            .authorities
            .insert("auto".to_string(), Implementation::Builtin("approve".to_string()));
        config
            .sanitizers
            .insert("pii".to_string(), Implementation::Builtin("redact-email".to_string()));
        let services = services_over(config);
        assert_eq!(
            services
                .consult(ConsultKind::Authority, "auto", &serde_json::json!({"call": "x"}), None)
                .await,
            ConsultOutcome::Answer(serde_json::json!({"ruling": "approve"})),
        );

        assert_eq!(
            services
                .consult(
                    ConsultKind::Sanitizer,
                    "pii",
                    &serde_json::json!({"body": "mail bob@corp.example now"}),
                    None,
                )
                .await,
            ConsultOutcome::Answer(serde_json::json!({"body": "mail [redacted-email] now"})),
        );

        assert_eq!(
            services
                .consult(ConsultKind::Sanitizer, "pii", &serde_json::json!({"content": 7}), None)
                .await,
            ConsultOutcome::NoAnswer(NoAnswerReason::Malformed),
        );
    }

    #[tokio::test]
    async fn a_dangling_builtin_reference_refuses_the_services() {
        let mut config = externals(None, 2000, 65536);
        config
            .authorities
            .insert("auto".to_string(), Implementation::Builtin("no-such".to_string()));
        match ExternalServices::new(config, ModuleRegistry::empty()) {
            Err(ModulesError::UnknownBuiltin { section, name, builtin }) => {
                assert_eq!(
                    (section, name.as_str(), builtin.as_str()),
                    ("authorities", "auto", "no-such")
                );
            }
            Err(other) => panic!("a dangling reference must refuse as unknown, got {other}"),
            Ok(_) => panic!("a dangling reference must refuse"),
        }
    }

    #[tokio::test]
    async fn a_builtin_of_the_wrong_kind_is_a_dangling_reference() {
        let mut config = externals(None, 2000, 65536);
        config
            .sanitizers
            .insert("pii".to_string(), Implementation::Builtin("approve".to_string()));
        assert!(matches!(
            ExternalServices::new(config, ModuleRegistry::empty()),
            Err(ModulesError::UnknownBuiltin {
                section: "sanitizers",
                ..
            }),
        ));
    }

    fn build_fixture(package: &str, features: Option<&str>) -> std::path::PathBuf {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("the workspace root resolves");
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
        let mut command = std::process::Command::new(cargo);
        command
            .current_dir(&root)
            .args(["build", "-p", package, "--message-format=json-render-diagnostics"])
            .arg("--target-dir")
            .arg(root.join("target/module-fixtures").join(features.unwrap_or("default")));
        if let Some(features) = features {
            command.args(["--features", features]);
        }
        let output = command.output().expect("cargo runs");
        assert!(
            output.status.success(),
            "the fixture build failed:\n{}",
            String::from_utf8_lossy(&output.stderr),
        );
        let stdout = String::from_utf8(output.stdout).expect("cargo messages are UTF-8");
        let extension = std::env::consts::DLL_EXTENSION;
        let target_name = package.replace('-', "_");
        stdout
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|message| {
                message["reason"] == "compiler-artifact" && message["target"]["name"] == target_name.as_str()
            })
            .filter_map(|message| {
                message["filenames"].as_array().and_then(|filenames| {
                    filenames
                        .iter()
                        .filter_map(|filename| filename.as_str())
                        .find(|path| path.ends_with(extension))
                        .map(std::path::PathBuf::from)
                })
            })
            .next()
            .expect("the fixture build produced a library artifact")
    }

    fn module_services(
        package: &str,
        features: Option<&str>,
        implementation: &str,
        max_body_bytes: usize,
    ) -> (ExternalServices, tempfile::TempDir) {
        let artifact = build_fixture(package, features);
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let filename = format!("libmodule.{}", std::env::consts::DLL_EXTENSION);
        std::fs::copy(&artifact, dir.path().join(filename)).expect("the module copies");
        let registry = crate::builtins::load(Some(dir.path())).expect("the fixture module loads");
        let mut config = externals(None, 2000, max_body_bytes);
        config
            .authorities
            .insert("auto".to_string(), Implementation::Builtin(implementation.to_string()));
        let services = ExternalServices::new(config, registry).expect("the module reference resolves");
        (services, dir)
    }

    #[tokio::test]
    async fn a_loaded_module_answers_the_consult_with_its_component() {
        let (services, _dir) = module_services("appa-module-fixture", None, "fixture-auth", 65536);
        let outcome = services
            .consult(ConsultKind::Authority, "auto", &serde_json::json!({"call": "x"}), None)
            .await;
        assert_eq!(
            outcome,
            ConsultOutcome::Answer(serde_json::json!({"ruling": "approve", "component": "auto"})),
        );
    }

    #[tokio::test]
    async fn every_module_failure_is_no_answer_never_a_denial() {
        let (services, _dir) = module_services("appa-module-fixture", None, "fixture-auth", 65536);

        assert_eq!(
            services
                .consult(
                    ConsultKind::Authority,
                    "auto",
                    &serde_json::json!({"mode": "error"}),
                    None
                )
                .await,
            ConsultOutcome::NoAnswer(NoAnswerReason::ModuleError),
        );

        assert_eq!(
            services
                .consult(
                    ConsultKind::Authority,
                    "auto",
                    &serde_json::json!({"mode": "panic"}),
                    None
                )
                .await,
            ConsultOutcome::NoAnswer(NoAnswerReason::ModulePanicked),
        );

        let (small, _dir) = module_services("appa-module-fixture", None, "fixture-auth", 64);
        assert_eq!(
            small
                .consult(
                    ConsultKind::Authority,
                    "auto",
                    &serde_json::json!({"mode": "big"}),
                    None
                )
                .await,
            ConsultOutcome::NoAnswer(NoAnswerReason::Oversized),
        );
    }

    #[tokio::test]
    async fn a_dishonest_output_length_is_malformed_never_a_slice() {
        let (services, _dir) = module_services("appa-module-fixture-bad", Some("dishonest-length"), "liar", 65536);
        assert_eq!(
            services
                .consult(ConsultKind::Authority, "auto", &serde_json::json!({}), None)
                .await,
            ConsultOutcome::NoAnswer(NoAnswerReason::Malformed),
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_module_gate_serializes_concurrent_calls() {
        let (services, _dir) = module_services("appa-module-fixture", None, "fixture-auth", 65536);
        let payload = serde_json::json!({"mode": "gate"});
        let (first, second) = tokio::join!(
            services.consult(ConsultKind::Authority, "auto", &payload, None),
            services.consult(ConsultKind::Authority, "auto", &payload, None),
        );
        for outcome in [first, second] {
            match outcome {
                ConsultOutcome::Answer(answer) => {
                    assert_eq!(answer["overlapped"], false, "the gate must serialize module calls");
                }
                ConsultOutcome::NoAnswer(reason) => panic!("the gate consult must answer, got {reason:?}"),
            }
        }
    }
}
