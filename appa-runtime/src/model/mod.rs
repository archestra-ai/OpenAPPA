//! The model builtins: `claude-code` and `llm`, which answer a rendered [`ModelPrompt`],
//! and `jev`, TypeSafe's classifier, which asks its own questions about the call.
//!
//! [`ModelPrompt`]: crate::consult::ModelPrompt

pub(crate) mod claude_code;
pub(crate) mod jev;
pub(crate) mod llm;

use claude_code::ClaudeCodeBackend;
use llm::LlmBackend;

/// No retry or hedge starts with less of the consult's budget left than this.
pub(crate) const MIN_ATTEMPT: std::time::Duration = std::time::Duration::from_millis(300);
/// The most attempts one consult starts, hedges and retries together.
pub(crate) const MAX_ATTEMPTS: usize = 3;

/// The transports that answer a rendered [`ModelPrompt`](crate::consult::ModelPrompt).
#[derive(Clone)]
pub(crate) enum PromptModel {
    ClaudeCode(ClaudeCodeBackend),
    Llm(LlmBackend),
}
