//! Outbound resolution of external [`Authority`] rulings.
//!
//! When a verdict defers to an external authority, the session performs the
//! outbound call through an [`AuthorityResolver`] and feeds the ruling back
//! into the engine. The edge never rules: a deny is an authority's decision;
//! on timeout, transport error, or no resolver, no ruling is applied at all —
//! the pending flow is abandoned and reported blocked. The flow fails closed
//! by the absence of a grant, never by a ruling the authority did not make.
//!
//! [`Authority`]: appa_core::Authority

use std::collections::HashMap;
use std::future::Future;
use std::io::Write;

use appa_contracts::AuthorityEndpoint;
use appa_core::{AuthorityName, PendingApproval, Ruling};
use serde::Deserialize;

/// Resolves one [`PendingApproval`] to a [`Ruling`] by asking the named
/// authority out-of-process. Outbound only — the edge is a client everywhere,
/// it never listens.
pub trait AuthorityResolver {
    fn resolve(&self, approval: &PendingApproval) -> impl Future<Output = Result<Ruling, ResolveError>> + Send;
}

/// Why no ruling was obtained. Every variant means the same thing to the
/// session: abandon the pending flow and report it blocked.
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("no resolver is configured for external authorities")]
    NoResolver,
    #[error("no ruling endpoint is declared for authority `{0}`")]
    NoEndpoint(AuthorityName),
    #[error("authority transport failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("authority response is not a ruling: {0}")]
    MalformedRuling(String),
    #[error("approval exceeds the {MAX_APPROVAL_BYTES}-byte request bound")]
    OversizedApproval,
    #[error("approval could not be serialized: {0}")]
    UnserializableApproval(String),
}

/// The no-op resolver: every escalation remains blocked. What an adapter
/// without an approval channel uses.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoResolver;

impl AuthorityResolver for NoResolver {
    async fn resolve(&self, _approval: &PendingApproval) -> Result<Ruling, ResolveError> {
        Err(ResolveError::NoResolver)
    }
}

/// The ruling wire format, strictly validated. Anything else — unknown
/// fields, unknown ruling values, non-2xx, parse failure, timeout — is a
/// [`ResolveError`], never a default ruling.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRuling {
    ruling: WireRulingKind,
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum WireRulingKind {
    Approve,
    Deny,
}

/// Shipped resolver: POST the serialized approval (authority, grant, resolved
/// violations, ancestry — labels and provenance, never value bodies) to the
/// declared endpoint of the authority the approval names, and parse the
/// ruling strictly. An authority with no declared endpoint fails closed as
/// [`ResolveError::NoEndpoint`] without any HTTP call.
#[derive(Debug, Clone)]
pub struct WebhookResolver {
    endpoints: HashMap<AuthorityName, AuthorityEndpoint>,
    client: reqwest::Client,
}

/// A ruling is one short JSON object; anything bigger is not a ruling.
const MAX_RULING_BYTES: usize = 64 * 1024;

/// Ancestry closures grow with the trajectory; an approval bigger than this
/// is not sent — the flow fails closed instead of shipping an unbounded body.
const MAX_APPROVAL_BYTES: usize = 1024 * 1024;

impl WebhookResolver {
    /// One resolver for every declared endpoint (`Contracts::endpoints`);
    /// each approval is posted to the endpoint of the authority it names,
    /// with that endpoint's timeout.
    pub fn new(endpoints: impl IntoIterator<Item = (AuthorityName, AuthorityEndpoint)>) -> Result<Self, ResolveError> {
        // No redirects: a redirect would re-POST the approval payload to a
        // destination the operator did not configure. No proxies: an ambient
        // HTTP_PROXY must not silently reroute rulings. No retries: one
        // ruling per approval — after a transport failure the edge cannot
        // know whether the authority acted, so the flow fails closed rather
        // than asking twice.
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .retry(reqwest::retry::never())
            .build()?;
        Ok(Self {
            endpoints: endpoints.into_iter().collect(),
            client,
        })
    }
}

impl AuthorityResolver for WebhookResolver {
    async fn resolve(&self, approval: &PendingApproval) -> Result<Ruling, ResolveError> {
        let endpoint = self
            .endpoints
            .get(approval.authority())
            .ok_or_else(|| ResolveError::NoEndpoint(approval.authority().clone()))?;
        let mut body = CappedWriter::new(MAX_APPROVAL_BYTES);
        if let Err(e) = serde_json::to_writer(&mut body, approval) {
            return Err(if body.overflowed {
                ResolveError::OversizedApproval
            } else {
                ResolveError::UnserializableApproval(e.to_string())
            });
        }
        let mut response = self
            .client
            .post(endpoint.url())
            .timeout(endpoint.timeout())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.written)
            .send()
            .await?
            .error_for_status()?;
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if body.len() + chunk.len() > MAX_RULING_BYTES {
                return Err(ResolveError::MalformedRuling(format!(
                    "response exceeds the {MAX_RULING_BYTES}-byte ruling bound"
                )));
            }
            body.extend_from_slice(&chunk);
        }
        let wire: WireRuling =
            serde_json::from_slice(&body).map_err(|e| ResolveError::MalformedRuling(e.to_string()))?;
        Ok(match wire.ruling {
            WireRulingKind::Approve => Ruling::Approve { reason: wire.reason },
            WireRulingKind::Deny => Ruling::Deny { reason: wire.reason },
        })
    }
}

/// Bounds the approval *during* serialization, so an oversized ancestry never
/// materializes past the cap.
struct CappedWriter {
    written: Vec<u8>,
    cap: usize,
    overflowed: bool,
}

impl CappedWriter {
    fn new(cap: usize) -> Self {
        Self {
            written: Vec::new(),
            cap,
            overflowed: false,
        }
    }
}

impl Write for CappedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.written.len() + buf.len() > self.cap {
            self.overflowed = true;
            return Err(std::io::Error::other("approval exceeds the request bound"));
        }
        self.written.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
