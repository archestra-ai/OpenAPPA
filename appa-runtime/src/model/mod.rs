//! The model builtins.
//!
//! - `claude-code`, `llm` and, with the `archestra` feature, `archestra` answer a rendered
//!   [`ModelPrompt`].
//! - `jev`, TypeSafe's classifier, asks its own questions about the call.
//!
//! [`ModelPrompt`]: crate::consult::ModelPrompt

#[cfg(feature = "archestra")]
pub(crate) mod archestra;
pub(crate) mod claude_code;
pub(crate) mod jev;
pub(crate) mod llm;

use std::time::Duration;

use appa_policy::AnnotatorBuiltin;
use claude_code::ClaudeCodeBackend;
use llm::LlmBackend;

use crate::external::{ConsultGates, Diagnostics, NoAnswerReason, Transcript, acquire_within};

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
    #[cfg(feature = "archestra")]
    Archestra(archestra::ArchestraBackend),
}

/// Consults an API model, retries transient failures, and parses the answer as JSON.
///
/// Each call of `send` makes one request and returns the model's answer text.
///
/// - **Budget.** One deadline, `budget` from now, covers the whole consult: waits for a
///   permit of `builtin`'s gate, every attempt, and every backoff.
/// - **Concurrency.** Each attempt holds one gate permit. The permit is released before the
///   backoff and acquired again for the next attempt.
/// - **Retries.** A transport failure, a 429 or a 5xx is retried after [`RETRY_BACKOFF`].
///   Any other failure is returned at once. No retry starts after [`MAX_ATTEMPTS`]
///   attempts, or when less than [`RETRY_BACKOFF`] plus [`MIN_ATTEMPT`] of the budget remains.
/// - **Answer.** An answer longer than `max_body_bytes` is [`NoAnswerReason::Oversized`]; an
///   answer that is not JSON is [`NoAnswerReason::Malformed`].
/// - **Record.** If `seen` is given, it records the last attempt: the answer text (cut to
///   `max_body_bytes`), or the status and detail of a non-success response.
///
/// `name` identifies the Annotator in log messages.
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

/// Runs one request until `deadline`, parses its answer, and writes the outcome to `seen`.
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
        // A non-success response's detail, such as the provider's error message, becomes
        // the consult's diagnostics.
        seen.diagnostics = match &answered {
            Err(NoAnswerReason::NonSuccess {
                detail: Some(detail), ..
            }) => Some(Diagnostics::head(detail.as_bytes())),
            _ => None,
        };
    }
    answered
}
