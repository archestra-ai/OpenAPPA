//! The model builtins: `claude-code` and `llm`, which answer a rendered [`ModelPrompt`],
//! and `jev`, TypeSafe's classifier, which asks its own questions about the call.
//!
//! [`ModelPrompt`]: crate::consult::ModelPrompt

pub(crate) mod claude_code;
pub(crate) mod jev;
pub(crate) mod llm;

use std::time::Duration;

use appa_policy::AnnotatorBuiltin;
use claude_code::ClaudeCodeBackend;
use llm::LlmBackend;

use crate::external::{ConsultGates, NoAnswerReason, Transcript, acquire_within};

/// No retry or hedge starts with less of the consult's budget left than this.
pub(crate) const MIN_ATTEMPT: std::time::Duration = std::time::Duration::from_millis(300);
/// The most attempts one consult starts, hedges and retries together.
pub(crate) const MAX_ATTEMPTS: usize = 3;
/// The pause before a retry of a transient failure; no permit is held across it.
pub(crate) const RETRY_BACKOFF: Duration = Duration::from_millis(500);

/// The transports that answer a rendered [`ModelPrompt`](crate::consult::ModelPrompt).
#[derive(Clone)]
pub(crate) enum PromptModel {
    ClaudeCode(ClaudeCodeBackend),
    Llm(LlmBackend),
}

/// One consult of an API model transport: `send` makes one request and returns the model's
/// answer text. The deadline covers every permit wait and every attempt: queueing behind
/// `builtin`'s gate spends the same budget the consult itself would. A transport failure, a
/// 429 or a 5xx is retried after [`RETRY_BACKOFF`] while the budget leaves room for another
/// attempt; each attempt holds a permit, the backoff none. `seen` keeps the last attempt's
/// answer text, capped, or its non-success status.
pub(crate) async fn consult_with_retries<S, Fut>(
    gates: &ConsultGates,
    builtin: AnnotatorBuiltin,
    budget: Duration,
    max_body_bytes: usize,
    name: &str,
    mut seen: Option<&mut Transcript>,
    send: S,
) -> Result<serde_json::Value, NoAnswerReason>
where
    S: Fn() -> Fut,
    Fut: Future<Output = Result<String, NoAnswerReason>>,
{
    let deadline = tokio::time::Instant::now() + budget;
    let gate = gates.model(builtin);
    let mut attempts = 1;
    loop {
        let permit = acquire_within(&gate, deadline, builtin.wire_name(), name).await?;
        let answered = attempt(send(), deadline, max_body_bytes, seen.as_deref_mut()).await;
        drop(permit);
        let retryable = matches!(
            answered,
            Err(NoAnswerReason::Transport
                | NoAnswerReason::NonSuccess {
                    status: 429 | 500..,
                    ..
                })
        );
        if !retryable
            || attempts == MAX_ATTEMPTS
            || tokio::time::Instant::now() + RETRY_BACKOFF + MIN_ATTEMPT > deadline
        {
            return answered;
        }
        attempts += 1;
        tokio::time::sleep(RETRY_BACKOFF).await;
    }
}

async fn attempt(
    sent: impl Future<Output = Result<String, NoAnswerReason>>,
    deadline: tokio::time::Instant,
    max_body_bytes: usize,
    seen: Option<&mut Transcript>,
) -> Result<serde_json::Value, NoAnswerReason> {
    let mut raw_response = None;
    let answered = match tokio::time::timeout_at(deadline, sent).await {
        Err(_) => Err(NoAnswerReason::Timeout),
        Ok(Err(reason)) => Err(reason),
        Ok(Ok(text)) if text.len() > max_body_bytes => {
            raw_response = Some(text.as_bytes()[..max_body_bytes].to_vec());
            Err(NoAnswerReason::Oversized)
        }
        Ok(Ok(text)) => {
            let answered = serde_json::from_str(&text).map_err(|_| NoAnswerReason::Malformed);
            raw_response = Some(text.into_bytes());
            answered
        }
    };
    if let Some(seen) = seen {
        seen.raw_response = raw_response;
        seen.http_status = match answered {
            Err(NoAnswerReason::NonSuccess { status, .. }) => Some(status),
            _ => None,
        };
    }
    answered
}
