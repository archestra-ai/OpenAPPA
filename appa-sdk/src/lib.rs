//! # appa-sdk — OpenAPPA embedded in the harness
//!
//! The spec's first deployment home ("the harness itself"): the harness keeps its own agent loop —
//! it calls the model, it executes tools — and embeds this SDK as the policy layer between the two.
//! The SDK is the spec's *outer layer* packaged as a library: it owns the trajectory (the append-only
//! Fact log), renders the model-visible transcript from it, checks every proposed tool call through
//! the pure [`appa_engine`] before the harness may execute it, and admits or seals every tool result
//! before anything enters model context. All decisions are the engine's; all IO except tool
//! execution and inference stays inside the SDK (authority backends for remedy rulings).
//!
//! The host's contract is three commitments, serially:
//! 1. build the model's context **only** from [`AppaSession::transcript`];
//! 2. execute **only** the one call [`Step::Execute`] surfaces, then report it through
//!    [`AppaSession::report_outcome`] before anything else;
//! 3. advertise to the model **exactly** the tool surface [`AppaSession::bind_tools`] returns.
//!
//! Mediation is serial by construction — one surfaced call at a time, each checked against a
//! projection that already contains the previous call's admitted result — which is the same
//! discipline `appa-runtime`'s internal turn-drive enforces, expressed in-process. Blocked calls
//! never surface: their feedback (with remedy-plan handles) lands in the log and reaches the model
//! through the next transcript; `execute_remedy_plan` is handled inside [`AppaSession::mediate`]
//! (authorities consulted, the atomic authorize+dispatch batch landed) and only the now-authorized
//! call surfaces.
//!
//! This deployment protects against the *injected model*, not the harness: the harness is a trusted
//! host, and enforcement is by integration discipline, not mechanical refusal.

mod assemble;
mod session;

pub use session::{
    AdmittedResult, AppaSession, DispatchHandle, MediateError, OpenError, Outcome, ReportError, SdkOptions,
    SessionBusy, Step, ToolSurfaceError,
};

// The wire and outcome types a host needs to drive the session, re-exported so a harness depends
// only on this crate for the mediation loop itself.
pub use appa_engine::label::Label;
pub use appa_runtime::config::Config;
pub use appa_runtime::inference::Completion;
pub use appa_runtime::tool::{BodyDisposition, RenderedCall, ToolOutcome};
pub use appa_runtime::wire::{WireFunctionCall, WireMessage, WireTool, WireToolCall, WireToolSchema};
