//! The model builtins: `claude-code` and `llm`, which answer a rendered [`ModelPrompt`],
//! and `jev`, TypeSafe's classifier, which asks its own questions about the call.
//!
//! [`ModelPrompt`]: crate::consult::ModelPrompt

pub(crate) mod claude_code;
pub(crate) mod jev;
pub(crate) mod llm;

use claude_code::ClaudeCodeBackend;
use llm::LlmBackend;

/// The transports that answer a rendered [`ModelPrompt`](crate::consult::ModelPrompt).
#[derive(Clone)]
pub(crate) enum PromptModel {
    ClaudeCode(ClaudeCodeBackend),
    Llm(LlmBackend),
}
