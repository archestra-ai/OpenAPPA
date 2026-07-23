//! # appa-sdk — OpenAPPA embedded in the harness
//!
//! The spec's first deployment home ("the harness itself"): the harness keeps its own agent loop —
//! it calls the model, it executes tools — and embeds this SDK as the policy layer between the two.
//! The SDK is the spec's *outer layer* packaged as a library: it owns the trajectory (the append-only
//! Fact log), checks every proposed tool call through the pure [`appa_engine`] before the harness may
//! execute it, and admits or seals every tool result before anything enters model context. All
//! decisions are the engine's; the only IO the SDK performs is consulting authority backends for
//! remedy rulings.
//!
//! Two facades sit over the same engine/store core ([`crate::common`]), for the two ways a harness
//! can be shaped:
//!
//! - [`AppaSession`] — **the host owns the loop.** Turn-shaped: the host asks for the transcript
//!   (rebuilt from the log, so the log *is* the context), sends it to the model, and feeds the
//!   completion back; the SDK surfaces one allowed call at a time. The strong deployment — the log
//!   and the context cannot diverge, and the final answer is reachable as a checkable emission.
//! - [`CallSession`] — **a framework owns the loop.** Per-call: a framework (e.g. rig) runs the loop
//!   and mediates each call through a hook that calls `check_call` before it runs and
//!   `report_outcome` after. The framework owns the transcript, so the SDK log is label-only and
//!   must be trusted to agree with the model's context; the response sink is out of reach. The
//!   trusted-harness deployment — the natural fit for dropping APPA into an existing agent framework.
//!
//! Both protect against the *injected model*, not the harness: the harness is a trusted host.

mod assemble;
mod call;
mod common;
mod session;
mod types;

pub use call::{CallDecision, CallError, CallSession, RemedyDecision};
pub use session::{AppaSession, MediateError, Outcome, Step};
pub use types::{AdmittedResult, DispatchHandle, OpenError, ReportError, SdkOptions, SessionBusy, ToolSurfaceError};

// The wire and outcome types a harness needs to drive a session, re-exported so a harness depends
// only on this crate for the mediation loop itself.
pub use appa_engine::label::Label;
pub use appa_runtime::config::Config;
pub use appa_runtime::inference::Completion;
pub use appa_runtime::tool::{BodyDisposition, RenderedCall, ToolOutcome};
pub use appa_runtime::wire::{WireFunctionCall, WireMessage, WireTool, WireToolCall, WireToolSchema};
