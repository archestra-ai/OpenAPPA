//! # appa-sdk - OpenAPPA embedded in a trusted harness
//!
//! This narrow facade exposes the per-call lifecycle for frameworks that own inference and tool
//! execution. Canonical state, policy mediation, and backend execution live in [`appa_runtime`].

pub use appa_runtime::{
    AdmittedResult, BodyDisposition, CallDecision, CallError, CallSession, Config, DispatchHandle, Label, OpenError,
    RemedyDecision, RenderedCall, ReportError, SdkOptions, SessionBusy, ToolOutcome, ToolSurfaceError,
    WireFunctionCall, WireMessage, WireTool, WireToolCall, WireToolSchema,
};
