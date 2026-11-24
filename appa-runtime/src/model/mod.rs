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
///   backoff and acquired again for the next attempt. A retry whose permit does not come
///   while [`MIN_ATTEMPT`] of the budget remains is not made: the last attempt's outcome is
///   returned.
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
    let mut permit = acquire_within(&gate, deadline, builtin.wire_name(), name).await?;
    loop {
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
        let retry_by = deadline - MIN_ATTEMPT;
        permit = match tokio::time::timeout_at(retry_by, gate.acquire()).await {
            // A permit ready when the task is polled late can still arrive past the cutoff.
            Ok(permit) if tokio::time::Instant::now() <= retry_by => permit.expect("a consult gate is never closed"),
            _ => return answered,
        };
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
            Err(
                NoAnswerReason::NonSuccess {
                    detail: Some(detail), ..
                }
                | NoAnswerReason::CommandExit {
                    detail: Some(detail), ..
                }
                | NoAnswerReason::TemporaryFailure { detail: Some(detail) },
            ) => Some(Diagnostics::head(detail.as_bytes())),
            _ => None,
        };
    }
    answered
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    const BUDGET: Duration = Duration::from_secs(2);
    const UNAVAILABLE: NoAnswerReason = NoAnswerReason::NonSuccess {
        status: 503,
        detail: None,
    };

    /// A one-permit consult whose every attempt answers 503, while another consult takes
    /// the permit during the first attempt and keeps it for `held`: the outcome, and how
    /// many attempts were sent.
    async fn retried_with_the_permit_held_for(held: Duration) -> (Result<serde_json::Value, NoAnswerReason>, usize) {
        let gates = ConsultGates::per_runtime();
        gates.resize(AnnotatorBuiltin::Llm, 1);
        let gate = gates.model(AnnotatorBuiltin::Llm);
        let sent = Arc::new(AtomicUsize::new(0));
        let attempted = Arc::new(tokio::sync::Notify::new());
        // Queued while the first attempt holds the permit, so the fair gate hands it here
        // before the retry asks.
        let holder = tokio::spawn({
            let attempted = Arc::clone(&attempted);
            async move {
                attempted.notified().await;
                let _permit = gate.acquire().await.expect("the gate is open");
                tokio::time::sleep(held).await;
            }
        });
        let answered = consult_with_retries(&gates, AnnotatorBuiltin::Llm, BUDGET, 1024, "judge", None, || {
            sent.fetch_add(1, Ordering::SeqCst);
            attempted.notify_one();
            async {
                tokio::task::yield_now().await;
                Err(UNAVAILABLE)
            }
        })
        .await;
        holder.abort();
        (answered, sent.load(Ordering::SeqCst))
    }

    /// A retry whose permit stays taken past the budget is not made: the consult answers
    /// what the last attempt did, not a timeout, and stops waiting at the retry cutoff.
    #[tokio::test(start_paused = true)]
    async fn a_retry_with_no_permit_in_time_answers_the_last_attempt() {
        let started = tokio::time::Instant::now();
        assert_eq!(
            retried_with_the_permit_held_for(Duration::from_secs(30)).await,
            (Err(UNAVAILABLE), 1)
        );
        assert!(started.elapsed() < BUDGET, "{:?}", started.elapsed());
    }

    /// A permit freed with less than [`MIN_ATTEMPT`] of the budget left starts no retry.
    #[tokio::test(start_paused = true)]
    async fn a_permit_freed_too_late_for_an_attempt_starts_no_retry() {
        let late = BUDGET - MIN_ATTEMPT / 2;
        assert_eq!(retried_with_the_permit_held_for(late).await, (Err(UNAVAILABLE), 1));
    }
}
