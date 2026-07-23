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
//! [`CallSession`] is the harness-facing facade — **a framework owns the loop.** A framework
//! (e.g. rig) runs the agent loop and mediates each proposed tool call through a hook that calls
//! [`CallSession::check_call`] before the tool runs and [`CallSession::report_outcome`] after (and
//! [`CallSession::resolve_remedy`] for `execute_remedy_plan`). The framework owns the model's
//! conversation, so the SDK log is label-only and is trusted to agree with the model's context; the
//! response sink is out of reach. This is the trusted-harness deployment — the natural fit for
//! dropping APPA into an existing agent framework. It protects against the *injected model*, not the
//! harness: the harness is a trusted host. The engine/store operations live in [`crate::common`].

mod assemble;
mod call;
mod common;
mod types;

pub use call::{CallDecision, CallError, CallSession, RemedyDecision};
pub use types::{AdmittedResult, DispatchHandle, OpenError, ReportError, SdkOptions, SessionBusy, ToolSurfaceError};

// The wire and outcome types a harness needs to drive a session, re-exported so a harness depends
// only on this crate for the mediation itself.
pub use appa_engine::label::Label;
pub use appa_runtime::config::Config;
pub use appa_runtime::tool::{BodyDisposition, RenderedCall, ToolOutcome};
pub use appa_runtime::wire::{WireFunctionCall, WireMessage, WireTool, WireToolCall, WireToolSchema};
