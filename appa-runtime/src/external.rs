//! External decision backends: authorities, sanitizers, and casts — the runtime's
//! trusted base of dynamic judgment, invoked south of the engine.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use appa_engine::check::Gap;
use appa_engine::execute::{AuthorityReview, ReviewedRef};
use appa_engine::label::{Audience, DimValue, Label, ReaderId};
use appa_engine::names::AuthorityName;
use appa_engine::projection::Views;
use appa_engine::registry::TrustChain;
use appa_engine::value::{CanonicalDigest, ResolvedCall, ToolName, ValueId};

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

/// The request an authority rules on. It names the call by **identity** — tool and canonical digest
/// — and carries the typed review context: the trajectory label fold at review time and, per
/// referenced argument Value, its label and provenance, plus the requirement `gaps` a ruling would
/// cover (the violations it targets, spec §Rulings). It deliberately does **not** carry the call's
/// result body or its non-recipient argument payload: the tool arguments a downstream call
/// substitutes (e.g. an email body) never reach the authority.
///
/// The one call-derived datum that legitimately appears is the **recipients of the proposed release**
/// — a `Gap::Includes` names the readers the flow would expose the data to (`send_email(.., to: $r)`
/// → the readers in `$r`). That is the *subject* the authority authorizes ("disclose to whom?"), not
/// leaked payload, and the spec has the approver see exactly it; a ruling could not otherwise be made.
/// The digest binds the ruling to the exact call; the review context put to the authority rides the
/// `Ruling` fact verbatim ([`AuthorityReview`]), so what was reviewed is replayable, never hidden
/// behind a hash.
/// Fields are private: the validating constructor is the only way to build one, so a request with
/// a dangling or foreign reference is unrepresentable at this boundary (serialization reads the
/// private fields; consumers go through [`AuthorityRequest::new`] and [`AuthorityRequest::review`]).
#[derive(Clone, Debug, Serialize)]
pub struct AuthorityRequest {
    authority: AuthorityName,
    tool: ToolName,
    digest: CanonicalDigest,
    trajectory_label: Label,
    arg_refs: Vec<ReviewedRef>,
    gaps: Vec<Gap>,
}

/// Why an [`AuthorityRequest`] could not be constructed: an argument reference that does not
/// resolve to a live value of this branch. Fail-closed — no request with an unresolvable reference
/// is ever put to an authority.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("argument reference {0:?} is dangling or belongs to another branch")]
pub struct DanglingArgRef(pub ValueId);

impl AuthorityRequest {
    /// Assemble the request from the live views: every argument reference resolves to its label and
    /// provenance or the construction is refused — a dangling or foreign reference never reaches an
    /// authority.
    pub fn new(
        authority: AuthorityName,
        call: &ResolvedCall,
        gaps: Vec<Gap>,
        views: &Views,
    ) -> Result<Self, DanglingArgRef> {
        let mut arg_refs = Vec::with_capacity(call.arg_refs().len());
        for id in call.arg_refs() {
            if !views.owns_value(*id) {
                return Err(DanglingArgRef(*id));
            }
            let label = views.value_label(*id).ok_or(DanglingArgRef(*id))?.clone();
            let provenance = views.value_provenance(*id).ok_or(DanglingArgRef(*id))?.clone();
            arg_refs.push(ReviewedRef {
                value: *id,
                label,
                provenance,
            });
        }
        Ok(AuthorityRequest {
            authority,
            tool: call.tool().clone(),
            digest: call.digest(),
            trajectory_label: views.current_label(),
            arg_refs,
            gaps,
        })
    }

    /// The review context this request put to the authority — recorded verbatim on the `Ruling` it
    /// produced, tool identity included.
    pub fn review(&self) -> AuthorityReview {
        AuthorityReview {
            tool: self.tool.clone(),
            trajectory_label: self.trajectory_label.clone(),
            arg_refs: self.arg_refs.clone(),
        }
    }
}

/// The in-process `approve` builtin — the one competence a policy grants itself. It approves the
/// gaps put to it (cover-free by construction: the load lint refuses it a cover-bearing mandate).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuiltinAuthority {
    Approve,
}

impl BuiltinAuthority {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "approve" => Some(BuiltinAuthority::Approve),
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
    Hitl,
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
            AuthorityBackend::Hitl => AuthorityAnswer::Abstain,
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
}

impl BuiltinSanitizer {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "redact-email" => Some(BuiltinSanitizer::RedactEmail),
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

// --- casts ---------------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
pub struct CastInput {
    pub body: String,
}

/// A cast's decision. `Unresolved` fails closed — the Unknown dimension stays Unknown. The engine
/// still bounds a `Resolved` proposal by `may_cast`; this layer only proposes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CastAnswer {
    Resolved(DimValue),
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
    pub async fn resolve(&self, input: &CastInput, chain: &TrustChain) -> CastAnswer {
        match self {
            CastBackend::Http { url, timeout, client } => {
                match post_json::<CastWire>(client, url, *timeout, input).await {
                    Some(wire) => parse_cast_answer(wire, chain),
                    None => CastAnswer::Unresolved,
                }
            }
        }
    }
}

fn parse_cast_answer(wire: CastWire, chain: &TrustChain) -> CastAnswer {
    match (wire.trust, wire.audience) {
        (Some(rank), None) => match chain.rank_of(&rank) {
            Some(trust) => CastAnswer::Resolved(DimValue::Trust(trust)),
            None => CastAnswer::Unresolved,
        },
        (None, Some(readers)) => match parse_readers(&readers) {
            Some(audience) => CastAnswer::Resolved(DimValue::Audience(audience)),
            None => CastAnswer::Unresolved,
        },
        // A resolver must name exactly one dimension; anything else is a decline.
        _ => CastAnswer::Unresolved,
    }
}

fn parse_readers(readers: &[String]) -> Option<Audience> {
    if readers.iter().any(|r| r == "public") {
        return (readers.len() == 1).then_some(Audience::Public);
    }
    if readers.is_empty() {
        return None;
    }
    Some(Audience::restricted(readers.iter().map(ReaderId::new)))
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
    use appa_engine::value::{ResolvedCall, ValueId};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use appa_engine::fact::{Fact, Revision};
    use appa_engine::label::Dim;
    use appa_engine::projection::Projection;
    use appa_engine::value::{DispatchId, LabeledValue, Provenance, TrajectoryId, ValueBody};

    const SECRET: &str = "SECRET-TICKET-9999";
    const STORE_SECRET: &str = "STORE-VALUE-CONTENTS-7777";

    fn chain() -> TrustChain {
        TrustChain::new(vec!["suspicious".into(), "trusted".into()])
    }

    fn traj() -> TrajectoryId {
        TrajectoryId::new("t")
    }

    fn secret_call() -> ResolvedCall {
        ResolvedCall::new(
            ToolName::new("send"),
            serde_json::json!({ "body": SECRET, "to": "auditor" }),
            vec![ValueId::new(0)],
        )
    }

    fn seeded_projection() -> Projection {
        let call = ResolvedCall::new(ToolName::new("fetch"), serde_json::json!({}), vec![]);
        let log = vec![Fact::ValueAdmitted {
            trajectory: traj(),
            value: LabeledValue::new(
                ValueBody::new(STORE_SECRET),
                Label::new(
                    Dim::Known(Trust::new(0)),
                    Dim::Known(Audience::restricted([ReaderId::new("internal")])),
                ),
            ),
            provenance: Provenance::ToolResult {
                dispatch: DispatchId::new(traj(), call.digest(), 0),
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
        .unwrap()
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
            AuthorityBackend::Hitl.rule(&authority_request()).await,
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
    async fn http_authority_request_shows_context_but_no_bytes() {
        let projection = seeded_projection();
        let request = AuthorityRequest::new(
            AuthorityName::new("officer"),
            &secret_call(),
            vec![Gap::Includes {
                recipients: Audience::restricted([ReaderId::new("auditor")]),
            }],
            &projection.view(&traj()),
        )
        .unwrap();
        let (addr, handle) = spawn_server("200 OK", r#"{"ruling":"approve"}"#).await;
        let backend = AuthorityBackend::Http {
            url: format!("http://{addr}/rule"),
            timeout: Duration::from_secs(5),
            client: HttpClient::new(),
        };
        backend.rule(&request).await;
        let wire = handle.await.unwrap();

        assert!(!wire.contains(SECRET), "argument bytes leaked to the authority");
        assert!(
            !wire.contains(STORE_SECRET),
            "store value bytes leaked to the authority"
        );
        let body = wire.split("\r\n\r\n").nth(1).expect("request has a body");
        let parsed: serde_json::Value = serde_json::from_str(body).expect("authority request is JSON");
        assert_eq!(parsed["tool"], "send");
        assert!(parsed["digest"].is_array(), "digest binds the call");
        assert!(
            parsed["trajectory_label"].is_object(),
            "the trajectory fold is reviewed"
        );
        assert_eq!(parsed["arg_refs"][0]["value"], serde_json::json!(0));
        assert!(
            parsed["arg_refs"][0]["label"].is_object(),
            "the referenced value's label is reviewed"
        );
        assert!(
            parsed["arg_refs"][0]["provenance"].is_object() || parsed["arg_refs"][0]["provenance"].is_string(),
            "the referenced value's provenance is reviewed"
        );
        assert!(
            wire.contains("auditor"),
            "the release recipient must reach the authority"
        );
    }

    #[test]
    fn a_dangling_argument_reference_refuses_the_request() {
        let projection = seeded_projection();
        let call = ResolvedCall::new(
            ToolName::new("send"),
            serde_json::json!({ "to": "auditor" }),
            vec![ValueId::new(7)],
        );
        assert_eq!(
            AuthorityRequest::new(AuthorityName::new("officer"), &call, vec![], &projection.view(&traj())).unwrap_err(),
            DanglingArgRef(ValueId::new(7))
        );
        let foreign = TrajectoryId::new("sibling");
        let call = ResolvedCall::new(
            ToolName::new("send"),
            serde_json::json!({ "to": "auditor" }),
            vec![ValueId::new(0)],
        );
        assert_eq!(
            AuthorityRequest::new(AuthorityName::new("officer"), &call, vec![], &projection.view(&foreign))
                .unwrap_err(),
            DanglingArgRef(ValueId::new(0))
        );
    }

    #[test]
    fn redact_email_replaces_addresses_only() {
        let out = redact_email("mail alice@corp.com and bob then done");
        assert_eq!(out, "mail [redacted-email] and bob then done");
        assert_eq!(redact_email("no addresses here"), "no addresses here");
        assert_eq!(redact_email("a\nb@x.io\tc"), "a\n[redacted-email]\tc");
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
    fn cast_answer_parses_one_dimension_or_declines() {
        let c = chain();
        assert_eq!(
            parse_cast_answer(
                CastWire {
                    trust: Some("suspicious".into()),
                    audience: None
                },
                &c
            ),
            CastAnswer::Resolved(DimValue::Trust(Trust::new(0)))
        );
        assert_eq!(
            parse_cast_answer(
                CastWire {
                    trust: None,
                    audience: Some(vec!["public".into()])
                },
                &c
            ),
            CastAnswer::Resolved(DimValue::Audience(Audience::Public))
        );
        assert_eq!(
            parse_cast_answer(
                CastWire {
                    trust: Some("godmode".into()),
                    audience: None
                },
                &c
            ),
            CastAnswer::Unresolved
        );
        assert_eq!(
            parse_cast_answer(
                CastWire {
                    trust: None,
                    audience: None
                },
                &c
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
        assert_eq!(
            backend.resolve(&CastInput { body: "x".into() }, &chain()).await,
            CastAnswer::Resolved(DimValue::Trust(Trust::new(0)))
        );
        handle.await.unwrap();

        let dead = CastBackend::Http {
            url: "http://127.0.0.1:1/resolve".into(),
            timeout: Duration::from_millis(200),
            client: HttpClient::new(),
        };
        assert_eq!(
            dead.resolve(&CastInput { body: "x".into() }, &chain()).await,
            CastAnswer::Unresolved
        );
    }
}
