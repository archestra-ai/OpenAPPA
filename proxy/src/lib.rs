//! appa-proxy: block policy-violating tool calls at the inference layer.
//!
//! The proxy sits between an agent harness and an OpenAI-compatible LLM. On
//! every `/v1/chat/completions` response it rebuilds an OpenAPPA [`Trajectory`] from
//! the request `messages`, evaluates each returned tool call against a
//! [`appa_core::PolicyEngine`], and rewrites the response when a call fails
//! its contract: the offending message is replaced with a stop explanation, so
//! the blocked call never reaches the harness and is never executed.
//!
//! Authorities come in two kinds. Inline `allow` authorities rule
//! in-process. `escalate` (external) authorities are served over HTTP: each
//! must declare a `webhook` endpoint (rejected at load otherwise), a *new*
//! call's escalation POSTs the pending approval there and applies the ruling
//! back, and any non-ruling — timeout, transport error, malformed body —
//! leaves the call blocked, fail closed. History replay never re-fires a
//! webhook: a tool result in the request `messages` is admitted under the
//! proxy's standing trust decision that harness-supplied history is genuine
//! (see `replay::TrustedHistoryResolver`). A flow no declared authority
//! covers is blocked, fail closed.
//!
//! A policy may also declare inline transformers. When the remedy walk
//! derives a call's payload through one, the proxy ships the *canonical*
//! arguments — the exact bytes the engine checked — in place of the model's
//! proposal (see `rewrite`), and the decision log carries the transform
//! trail. Replay re-derives deterministically; no webhook is involved.
//!
//! Nothing here is cryptographic: authenticity rests on the harness only
//! recording tool results that real MCP servers returned, and on the proxy
//! port being reachable only by that harness. See `README.md`.
//!
//! [`Trajectory`]: appa_core::Trajectory

pub mod config;
pub mod replay;
pub mod rewrite;
pub mod wire;

pub use config::{ConfigError, Policy};
pub use replay::{CallOutcome, ReplayError, Session};
pub use rewrite::{TurnDecision, rewrite_response};
