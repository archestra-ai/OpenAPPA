//! Outbound resolution of external [`Authority`] rulings.

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

const MAX_RULING_BYTES: usize = 64 * 1024;

const MAX_APPROVAL_BYTES: usize = 1024 * 1024;

impl WebhookResolver {
    /// One resolver for every declared endpoint (`Contracts::endpoints`);
    /// each approval is posted to the endpoint of the authority it names,
    /// with that endpoint's timeout. Takes the map directly — the contracts
    /// parser already guarantees one endpoint per authority.
    pub fn new(endpoints: HashMap<AuthorityName, AuthorityEndpoint>) -> Result<Self, ResolveError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .retry(reqwest::retry::never())
            .build()?;
        Ok(Self { endpoints, client })
    }
}

impl AuthorityResolver for WebhookResolver {
    async fn resolve(&self, approval: &PendingApproval) -> Result<Ruling, ResolveError> {
        let endpoint = self
            .endpoints
            .get(approval.authority())
            .ok_or_else(|| ResolveError::NoEndpoint(approval.authority().clone()))?;
        let body = to_capped_json(approval, MAX_APPROVAL_BYTES)?;
        let mut response = self
            .client
            .post(endpoint.url())
            .timeout(endpoint.timeout())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(ResolveError::MalformedRuling(format!(
                "non-success status {}",
                response.status()
            )));
        }
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

fn to_capped_json<T: serde::Serialize>(value: &T, cap: usize) -> Result<Vec<u8>, ResolveError> {
    let mut writer = CappedWriter::new(cap);
    match serde_json::to_writer(&mut writer, value) {
        Ok(()) => Ok(writer.written),
        Err(_) if writer.overflowed => Err(ResolveError::OversizedApproval),
        Err(e) => Err(ResolveError::UnserializableApproval(e.to_string())),
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capped_serialization_survives_serde_buffering() {
        let big = "x".repeat(1024);
        assert!(matches!(
            to_capped_json(&big, 512),
            Err(ResolveError::OversizedApproval)
        ));
        let small = serde_json::json!({"authority": "auditor"});
        assert_eq!(
            to_capped_json(&small, 512).unwrap(),
            serde_json::to_vec(&small).unwrap()
        );
    }
}
