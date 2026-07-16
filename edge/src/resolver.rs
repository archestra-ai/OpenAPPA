//! Outbound resolution of external [`Authority`] rulings.

use std::future::Future;
use std::time::Duration;

use appa_core::{PendingApproval, Ruling};
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
    #[error("authority transport failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("authority response is not a ruling: {0}")]
    MalformedRuling(String),
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
/// violations, ancestry — labels and provenance, never value bodies) to one
/// operator-configured URL and parse the ruling strictly.
#[derive(Debug, Clone)]
pub struct WebhookResolver {
    url: String,
    client: reqwest::Client,
}

const MAX_RULING_BYTES: usize = 64 * 1024;

impl WebhookResolver {
    pub fn new(url: impl Into<String>, timeout: Duration) -> Result<Self, ResolveError> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            url: url.into(),
            client,
        })
    }
}

impl AuthorityResolver for WebhookResolver {
    async fn resolve(&self, approval: &PendingApproval) -> Result<Ruling, ResolveError> {
        let mut response = self
            .client
            .post(&self.url)
            .json(approval)
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
