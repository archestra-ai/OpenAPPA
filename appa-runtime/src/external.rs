//! External decision backends: authorities, sanitizers, casts, and dynamic audience resolvers —
//! the outer layer's trusted base of dynamic judgment, invoked south of the engine.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use appa_engine::check::Gap;
use appa_engine::execute::AuthorityReview;
use appa_engine::label::{Audience, Dim, EstablishedLabel, Label, PartialLabel, ReaderId};
use appa_engine::names::{AuthorityName, DynamicResolverName};
use appa_engine::projection::Views;
use appa_engine::registry::TrustChain;
use appa_engine::value::{CanonicalDigest, ResolvedCall, ToolName};

use crate::tool::HttpClient;

// --- authorities ---------------------------------------------------------------

/// What an authority decided. `Abstain` is the fail-closed default — indistinguishable in effect
/// from an unreachable authority: no ruling, block stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthorityAnswer {
    Approve,
    Deny,
    Abstain,
}

/// The request an authority rules on. It names the call by **identity** — tool and canonical
/// digest — and shows the reviewer the release it is asked to authorize: the exact canonical
/// argument object the dispatch would send (`RUL-9` — an authority judging `send_email` sees the
/// text it is asked to release), plus the trajectory label fold at review time and the
/// requirement `gaps` a ruling would cover (the violations it targets, spec §Rulings).
#[derive(Clone, Debug, Serialize)]
pub struct AuthorityRequest {
    authority: AuthorityName,
    tool: ToolName,
    digest: CanonicalDigest,
    trajectory_label: PartialLabel,
    arguments: Box<serde_json::value::RawValue>,
    gaps: Vec<Gap>,
}

impl AuthorityRequest {
    pub fn new(authority: AuthorityName, call: &ResolvedCall, gaps: Vec<Gap>, views: &Views) -> Self {
        let text = call.canonical_arguments().canonical_text().to_owned();
        AuthorityRequest {
            authority,
            tool: call.tool().clone(),
            digest: call.digest(),
            trajectory_label: views.current_label(),
            arguments: serde_json::value::RawValue::from_string(text).expect("canonical bytes are one JSON value"),
            gaps,
        }
    }

    /// The typed review context this request put to the authority — recorded verbatim on the
    /// `Ruling` it produced, tool identity included.
    pub fn review(&self) -> AuthorityReview {
        AuthorityReview {
            tool: self.tool.clone(),
            trajectory_label: self.trajectory_label.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuiltinAuthority {
    Approve,
    Hitl,
}

impl BuiltinAuthority {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "approve" => Some(BuiltinAuthority::Approve),
            "hitl" => Some(BuiltinAuthority::Hitl),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum AuthorityBackend {
    Builtin(BuiltinAuthority),
    Http {
        url: String,
        timeout: Duration,
        client: HttpClient,
    },
}

#[derive(Debug, Deserialize)]
struct RulingWire {
    ruling: String,
}

impl AuthorityBackend {
    pub async fn rule(&self, request: &AuthorityRequest) -> AuthorityAnswer {
        match self {
            AuthorityBackend::Builtin(BuiltinAuthority::Approve) => {
                tracing::info!(
                    authority = request.authority.as_str(),
                    "builtin approve: cleared under policy"
                );
                AuthorityAnswer::Approve
            }
            AuthorityBackend::Http { url, timeout, client } => {
                match post_json::<RulingWire>(client, url, *timeout, request).await {
                    Some(wire) => match wire.ruling.as_str() {
                        "approve" => AuthorityAnswer::Approve,
                        "deny" => AuthorityAnswer::Deny,
                        _ => AuthorityAnswer::Abstain,
                    },
                    None => AuthorityAnswer::Abstain,
                }
            }
            AuthorityBackend::Builtin(BuiltinAuthority::Hitl) => AuthorityAnswer::Abstain,
        }
    }
}

// --- sanitizers ----------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
pub struct SanitizerInput {
    pub body: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SanitizerAnswer {
    Derived(String),
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuiltinSanitizer {
    RedactEmail,
    RedactNumbers,
    Hosted,
}

impl BuiltinSanitizer {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "redact-email" => Some(BuiltinSanitizer::RedactEmail),
            "redact-numbers" => Some(BuiltinSanitizer::RedactNumbers),
            "hosted" => Some(BuiltinSanitizer::Hosted),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum SanitizerBackend {
    Builtin(BuiltinSanitizer),
    Http {
        url: String,
        timeout: Duration,
        client: HttpClient,
    },
}

#[derive(Debug, Deserialize)]
struct DerivedWire {
    body: String,
}

impl SanitizerBackend {
    pub async fn derive(&self, input: &SanitizerInput) -> SanitizerAnswer {
        match self {
            SanitizerBackend::Builtin(BuiltinSanitizer::RedactEmail) => {
                SanitizerAnswer::Derived(redact_email(&input.body))
            }
            SanitizerBackend::Builtin(BuiltinSanitizer::RedactNumbers) => {
                SanitizerAnswer::Derived(redact_numbers(&input.body))
            }
            SanitizerBackend::Builtin(BuiltinSanitizer::Hosted) => SanitizerAnswer::Failed,
            SanitizerBackend::Http { url, timeout, client } => {
                match post_json::<DerivedWire>(client, url, *timeout, input).await {
                    Some(wire) => SanitizerAnswer::Derived(wire.body),
                    None => SanitizerAnswer::Failed,
                }
            }
        }
    }
}

/// Replace every email-like token with a fixed placeholder. A deliberately simple scan — a builtin
/// fixture, not a hardened redactor (registration is a trust decision, not verification).
fn redact_email(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut word = String::new();
    for ch in input.chars() {
        if ch.is_whitespace() {
            flush_word(&mut word, &mut out);
            out.push(ch);
        } else {
            word.push(ch);
        }
    }
    flush_word(&mut word, &mut out);
    out
}

fn flush_word(word: &mut String, out: &mut String) {
    if is_emailish(word) {
        out.push_str("[redacted-email]");
    } else {
        out.push_str(word);
    }
    word.clear();
}

fn is_emailish(token: &str) -> bool {
    match token.find('@') {
        Some(at) => {
            let (local, rest) = token.split_at(at);
            let domain = &rest[1..];
            !local.is_empty() && !domain.starts_with('.') && domain.contains('.')
        }
        None => false,
    }
}

/// Replace every maximal ASCII-digit run with a fixed placeholder. Like [`redact_email`], a
/// deliberately simple builtin fixture — it strips numeric identifiers (SSNs, salaries, account
/// digits) and keeps everything else verbatim; registration is a trust decision, not verification.
fn redact_numbers(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_digits = false;
    for ch in input.chars() {
        if ch.is_ascii_digit() {
            if !in_digits {
                out.push_str("[redacted-number]");
                in_digits = true;
            }
        } else {
            in_digits = false;
            out.push(ch);
        }
    }
    out
}

// --- casts ---------------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
pub struct CastInput {
    pub body: String,
}

/// A cast's decision: one complete source label, or a decline. `Unresolved` fails closed — the
/// source stays unresolved. The engine still bounds a `Resolved` proposal by the whole-source
/// validator; this layer only proposes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CastAnswer {
    Resolved(EstablishedLabel),
    Unresolved,
}

#[derive(Clone, Debug)]
pub enum CastBackend {
    Http {
        url: String,
        timeout: Duration,
        client: HttpClient,
    },
}

#[derive(Debug, Deserialize)]
struct CastWire {
    trust: Option<String>,
    audience: Option<Vec<String>>,
}

impl CastBackend {
    pub async fn resolve(&self, input: &CastInput, chain: &TrustChain, prior: &Label) -> CastAnswer {
        match self {
            CastBackend::Http { url, timeout, client } => {
                match post_json::<CastWire>(client, url, *timeout, input).await {
                    Some(wire) => parse_cast_answer(wire, chain, prior),
                    None => CastAnswer::Unresolved,
                }
            }
        }
    }
}

fn parse_cast_answer(wire: CastWire, chain: &TrustChain, prior: &Label) -> CastAnswer {
    let trust = match (wire.trust, &prior.trust) {
        (Some(rank), _) => match chain.rank_of(&rank) {
            Some(trust) => trust,
            None => return CastAnswer::Unresolved,
        },
        (None, Dim::Known(trust)) => *trust,
        (None, Dim::Unknown) => return CastAnswer::Unresolved,
    };
    let audience = match (wire.audience, &prior.audience) {
        (Some(readers), _) => match parse_readers(&readers) {
            Some(audience) => audience,
            None => return CastAnswer::Unresolved,
        },
        (None, Dim::Known(audience)) => audience.clone(),
        (None, Dim::Unknown) => return CastAnswer::Unresolved,
    };
    CastAnswer::Resolved(EstablishedLabel::new(trust, audience))
}

fn parse_readers(readers: &[String]) -> Option<Audience> {
    if readers.iter().any(|r| r == "public") {
        return (readers.len() == 1).then_some(Audience::Public);
    }
    Some(Audience::restricted(readers.iter().map(ReaderId::new)))
}

// --- dynamic audience resolvers ------------------------------------------------

#[derive(Clone, Debug)]
pub enum DynamicResolverBackend {
    Http {
        url: String,
        timeout: Duration,
        client: HttpClient,
    },
}

#[derive(Serialize)]
struct DynamicResolverRequest<'a> {
    version: u32,
    resolver: &'a DynamicResolverName,
    tool: &'a ToolName,
    argument: &'a str,
    value: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DynamicResolverWire {
    version: u32,
    readers: Vec<String>,
}

impl DynamicResolverBackend {
    pub async fn resolve(
        &self,
        resolver: &DynamicResolverName,
        tool: &ToolName,
        argument: &str,
        value: &str,
    ) -> Option<Audience> {
        let request = DynamicResolverRequest {
            version: 1,
            resolver,
            tool,
            argument,
            value,
        };
        let DynamicResolverBackend::Http { url, timeout, client } = self;
        let wire = post_json::<DynamicResolverWire>(client, url, *timeout, &request).await?;
        if wire.version != 1
            || wire
                .readers
                .iter()
                .any(|reader| reader == "public" || reader.starts_with('@'))
        {
            return None;
        }
        Some(Audience::restricted(wire.readers.into_iter().map(ReaderId::new)))
    }
}

// --- shared HTTP ---------------------------------------------------------------

const RESOLVER_BODY_CAP: usize = 64 * 1024;

async fn post_json<T: for<'de> Deserialize<'de>>(
    client: &HttpClient,
    url: &str,
    timeout: Duration,
    payload: &impl Serialize,
) -> Option<T> {
    let mut response = client
        .inner()
        .post(url)
        .timeout(timeout)
        .json(payload)
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body = crate::tool::read_body_capped(&mut response, RESOLVER_BODY_CAP).await?;
    if body.len() > RESOLVER_BODY_CAP {
        return None;
    }
    serde_json::from_slice(&body).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use appa_engine::label::Trust;
    use appa_engine::value::ResolvedCall;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use appa_engine::fact::{Fact, Revision};
    use appa_engine::label::Dim;
    use appa_engine::projection::Projection;
    use appa_engine::value::{DispatchId, LabeledValue, Provenance, TrajectoryId, ValueBody};

    const SECRET: &str = "SECRET-TICKET-9999";

    fn chain() -> TrustChain {
        TrustChain::new(vec!["suspicious".into(), "trusted".into()])
    }

    fn traj() -> TrajectoryId {
        TrajectoryId::new("t")
    }

    fn call(tool: &str, arguments: serde_json::Value) -> ResolvedCall {
        crate::common::test_call(tool, arguments)
    }

    fn secret_call() -> ResolvedCall {
        call("send", serde_json::json!({ "body": SECRET, "to": "auditor" }))
    }

    fn seeded_projection() -> Projection {
        let fetch = call("fetch", serde_json::json!({}));
        let log = vec![Fact::ValueAdmitted {
            trajectory: traj(),
            value: LabeledValue::new(
                ValueBody::new("ticket contents"),
                Label::new(
                    Dim::Known(Trust::new(0)),
                    Dim::Known(Audience::restricted([ReaderId::new("internal")])),
                ),
            ),
            provenance: Provenance::ToolResult {
                dispatch: DispatchId::new(traj(), fetch.digest(), 0),
            },
        }];
        Projection::build(&log, Revision::new(1))
    }

    fn authority_request() -> AuthorityRequest {
        let projection = seeded_projection();
        AuthorityRequest::new(
            AuthorityName::new("officer"),
            &secret_call(),
            vec![],
            &projection.view(&traj()),
        )
    }

    async fn spawn_server(
        status_line: &'static str,
        body: impl Into<String>,
    ) -> (std::net::SocketAddr, tokio::task::JoinHandle<String>) {
        let body = body.into();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let received = read_full_request(&mut socket).await;
            let response = format!("HTTP/1.1 {status_line}\r\nContent-Length: {}\r\n\r\n{body}", body.len());
            socket.write_all(response.as_bytes()).await.unwrap();
            received
        });
        (addr, handle)
    }

    async fn read_full_request(socket: &mut tokio::net::TcpStream) -> String {
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
        String::from_utf8_lossy(&received).to_string()
    }

    #[tokio::test]
    async fn builtin_approve_approves() {
        assert_eq!(
            AuthorityBackend::Builtin(BuiltinAuthority::Approve)
                .rule(&authority_request())
                .await,
            AuthorityAnswer::Approve
        );
    }

    #[tokio::test]
    async fn hitl_fails_closed() {
        assert_eq!(
            AuthorityBackend::Builtin(BuiltinAuthority::Hitl)
                .rule(&authority_request())
                .await,
            AuthorityAnswer::Abstain
        );
    }

    #[tokio::test]
    async fn http_authority_maps_approve_deny_and_declines_the_rest() {
        for (body, expected) in [
            (r#"{"ruling":"approve"}"#, AuthorityAnswer::Approve),
            (r#"{"ruling":"deny"}"#, AuthorityAnswer::Deny),
            (r#"{"ruling":"maybe"}"#, AuthorityAnswer::Abstain),
        ] {
            let (addr, handle) = spawn_server("200 OK", body).await;
            let backend = AuthorityBackend::Http {
                url: format!("http://{addr}/rule"),
                timeout: Duration::from_secs(5),
                client: HttpClient::new(),
            };
            assert_eq!(backend.rule(&authority_request()).await, expected);
            handle.await.unwrap();
        }
    }

    #[tokio::test]
    async fn http_authority_non_2xx_abstains() {
        let (addr, handle) = spawn_server("500 Internal Server Error", "boom").await;
        let backend = AuthorityBackend::Http {
            url: format!("http://{addr}/rule"),
            timeout: Duration::from_secs(5),
            client: HttpClient::new(),
        };
        assert_eq!(backend.rule(&authority_request()).await, AuthorityAnswer::Abstain);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn dynamic_resolver_sends_the_versioned_binding_and_accepts_literal_readers() {
        let (addr, handle) = spawn_server("200 OK", r#"{"version":1,"readers":["alice","bob"]}"#).await;
        let backend = DynamicResolverBackend::Http {
            url: format!("http://{addr}/readers"),
            timeout: Duration::from_secs(5),
            client: HttpClient::new(),
        };
        assert_eq!(
            backend
                .resolve(
                    &DynamicResolverName::new("crm-acl"),
                    &ToolName::new("lookup"),
                    "customer_id",
                    "customer-123",
                )
                .await,
            Some(Audience::restricted([ReaderId::new("alice"), ReaderId::new("bob")]))
        );
        let request = handle.await.unwrap();
        let body = request.split("\r\n\r\n").nth(1).expect("request has a body");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(body).unwrap(),
            serde_json::json!({
                "version": 1,
                "resolver": "crm-acl",
                "tool": "lookup",
                "argument": "customer_id",
                "value": "customer-123",
            })
        );
    }

    #[tokio::test]
    async fn dynamic_resolver_accepts_empty_readers_and_rejects_non_literal_answers() {
        for (body, expected) in [
            (r#"{"version":1,"readers":[]}"#, Some(Audience::restricted([]))),
            (r#"{"version":2,"readers":["alice"]}"#, None),
            (r#"{"version":1,"readers":["public"]}"#, None),
            (r#"{"version":1,"readers":["@support"]}"#, None),
            (r#"{"version":1,"readers":"alice"}"#, None),
        ] {
            let (addr, handle) = spawn_server("200 OK", body).await;
            let backend = DynamicResolverBackend::Http {
                url: format!("http://{addr}/readers"),
                timeout: Duration::from_secs(5),
                client: HttpClient::new(),
            };
            assert_eq!(
                backend
                    .resolve(
                        &DynamicResolverName::new("directory"),
                        &ToolName::new("lookup"),
                        "id",
                        "123",
                    )
                    .await,
                expected
            );
            handle.await.unwrap();
        }
    }

    #[tokio::test]
    async fn http_resolver_oversized_response_fails_closed() {
        let huge = "{\"ruling\":\"approve\",\"pad\":\"".to_string() + &"a".repeat(RESOLVER_BODY_CAP) + "\"}";
        let (addr, handle) = spawn_server("200 OK", huge).await;
        let backend = AuthorityBackend::Http {
            url: format!("http://{addr}/rule"),
            timeout: Duration::from_secs(5),
            client: HttpClient::new(),
        };
        assert_eq!(backend.rule(&authority_request()).await, AuthorityAnswer::Abstain);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn http_authority_request_carries_the_rendered_payload() {
        let projection = seeded_projection();
        let request = AuthorityRequest::new(
            AuthorityName::new("officer"),
            &secret_call(),
            vec![Gap::Includes {
                recipients: Audience::restricted([ReaderId::new("auditor")]),
            }],
            &projection.view(&traj()),
        );
        let (addr, handle) = spawn_server("200 OK", r#"{"ruling":"approve"}"#).await;
        let backend = AuthorityBackend::Http {
            url: format!("http://{addr}/rule"),
            timeout: Duration::from_secs(5),
            client: HttpClient::new(),
        };
        backend.rule(&request).await;
        let wire = handle.await.unwrap();

        let body = wire.split("\r\n\r\n").nth(1).expect("request has a body");
        let parsed: serde_json::Value = serde_json::from_str(body).expect("authority request is JSON");
        assert_eq!(
            parsed["arguments"],
            serde_json::json!({ "body": SECRET, "to": "auditor" })
        );
        assert_eq!(parsed["tool"], "send");
        assert!(parsed["digest"].is_array(), "digest binds the call");
        assert!(
            parsed["trajectory_label"].is_object(),
            "the trajectory fold is reviewed"
        );
        let gaps = parsed["gaps"].as_array().expect("gaps cross");
        assert!(
            serde_json::to_string(&gaps)
                .expect("gaps serialize")
                .contains("auditor"),
            "the release recipient must reach the authority"
        );

        let discriminating = call("send", serde_json::json!({ "total": 1.0, "π": "pi" }));
        let request = AuthorityRequest::new(
            AuthorityName::new("officer"),
            &discriminating,
            vec![],
            &projection.view(&traj()),
        );
        let (addr, handle) = spawn_server("200 OK", r#"{"ruling":"approve"}"#).await;
        let backend = AuthorityBackend::Http {
            url: format!("http://{addr}/rule"),
            timeout: Duration::from_secs(5),
            client: HttpClient::new(),
        };
        backend.rule(&request).await;
        let wire = handle.await.unwrap();
        let body = wire.split("\r\n\r\n").nth(1).expect("request has a body");
        let canonical = discriminating.canonical_arguments().canonical_text().to_owned();
        assert_eq!(canonical, r#"{"total":1,"π":"pi"}"#);
        assert!(
            body.contains(&format!(r#""arguments":{canonical}"#)),
            "the wire must carry the exact canonical argument token: {body:?}"
        );
    }

    #[test]
    fn redact_email_replaces_addresses_only() {
        let out = redact_email("mail alice@corp.com and bob then done");
        assert_eq!(out, "mail [redacted-email] and bob then done");
        assert_eq!(redact_email("no addresses here"), "no addresses here");
        assert_eq!(redact_email("a\nb@x.io\tc"), "a\n[redacted-email]\tc");
    }

    #[test]
    fn redact_numbers_replaces_digit_runs_only() {
        assert_eq!(
            redact_numbers("SSN 123-45-4821, base $185,000"),
            "SSN [redacted-number]-[redacted-number]-[redacted-number], base $[redacted-number],[redacted-number]"
        );
        assert_eq!(redact_numbers("buddy is Priya Sharma"), "buddy is Priya Sharma");
        assert_eq!(redact_numbers(""), "");
    }

    #[tokio::test]
    async fn builtin_redact_numbers_sanitizer_derives() {
        let answer = SanitizerBackend::Builtin(BuiltinSanitizer::RedactNumbers)
            .derive(&SanitizerInput {
                body: "extension 4471".into(),
            })
            .await;
        assert_eq!(answer, SanitizerAnswer::Derived("extension [redacted-number]".into()));
    }

    #[tokio::test]
    async fn builtin_redact_sanitizer_derives() {
        let answer = SanitizerBackend::Builtin(BuiltinSanitizer::RedactEmail)
            .derive(&SanitizerInput {
                body: "ping x@y.com".into(),
            })
            .await;
        assert_eq!(answer, SanitizerAnswer::Derived("ping [redacted-email]".into()));
    }

    #[tokio::test]
    async fn http_sanitizer_malformed_fails_closed() {
        let (addr, handle) = spawn_server("200 OK", "not json").await;
        let backend = SanitizerBackend::Http {
            url: format!("http://{addr}/derive"),
            timeout: Duration::from_secs(5),
            client: HttpClient::new(),
        };
        assert_eq!(
            backend.derive(&SanitizerInput { body: "x".into() }).await,
            SanitizerAnswer::Failed
        );
        handle.await.unwrap();
    }

    #[test]
    fn cast_answer_composes_a_complete_label_or_declines() {
        let c = chain();
        let audience_known = Label::new(Dim::Unknown, Dim::Known(Audience::Public));
        assert_eq!(
            parse_cast_answer(
                CastWire {
                    trust: Some("suspicious".into()),
                    audience: None
                },
                &c,
                &audience_known,
            ),
            CastAnswer::Resolved(EstablishedLabel::new(Trust::new(0), Audience::Public))
        );
        let trust_known = Label::new(Dim::Known(Trust::new(1)), Dim::Unknown);
        assert_eq!(
            parse_cast_answer(
                CastWire {
                    trust: None,
                    audience: Some(vec!["public".into()])
                },
                &c,
                &trust_known,
            ),
            CastAnswer::Resolved(EstablishedLabel::new(Trust::new(1), Audience::Public))
        );
        assert_eq!(
            parse_cast_answer(
                CastWire {
                    trust: None,
                    audience: Some(vec![])
                },
                &c,
                &trust_known,
            ),
            CastAnswer::Resolved(EstablishedLabel::new(
                Trust::new(1),
                Audience::restricted(std::iter::empty())
            ))
        );
        assert_eq!(
            parse_cast_answer(
                CastWire {
                    trust: Some("godmode".into()),
                    audience: None
                },
                &c,
                &audience_known,
            ),
            CastAnswer::Unresolved
        );
        assert_eq!(
            parse_cast_answer(
                CastWire {
                    trust: None,
                    audience: None
                },
                &c,
                &audience_known,
            ),
            CastAnswer::Unresolved
        );
    }

    #[tokio::test]
    async fn http_cast_resolves_unknown_then_declines_on_fault() {
        let (addr, handle) = spawn_server("200 OK", r#"{"trust":"suspicious"}"#).await;
        let backend = CastBackend::Http {
            url: format!("http://{addr}/resolve"),
            timeout: Duration::from_secs(5),
            client: HttpClient::new(),
        };
        let prior = Label::new(Dim::Unknown, Dim::Known(Audience::Public));
        assert_eq!(
            backend.resolve(&CastInput { body: "x".into() }, &chain(), &prior).await,
            CastAnswer::Resolved(EstablishedLabel::new(Trust::new(0), Audience::Public))
        );
        handle.await.unwrap();

        let dead = CastBackend::Http {
            url: "http://127.0.0.1:1/resolve".into(),
            timeout: Duration::from_millis(200),
            client: HttpClient::new(),
        };
        assert_eq!(
            dead.resolve(&CastInput { body: "x".into() }, &chain(), &prior).await,
            CastAnswer::Unresolved
        );
    }
}
