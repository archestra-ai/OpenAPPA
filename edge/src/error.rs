//! The single error type for the crate.

use appa_core::{RejectedToken, ToolName, UnknownValue};

#[derive(Debug, thiserror::Error)]
pub enum EdgeError {
    #[error("duplicate contract for `{0}` in policy")]
    DuplicateContract(ToolName),
    #[error("duplicate authority registration: {0}")]
    DuplicateAuthority(String),
    #[error("duplicate transformer registration: {0}")]
    DuplicateTransformer(String),
    #[error("a previously-executed call to `{tool}` no longer passes policy: {reason}")]
    ReplayBlocked { tool: ToolName, reason: String },
    #[error("a previously-executed call to `{tool}` has arguments that cannot be parsed")]
    MalformedHistoricalCall { tool: ToolName },
    #[error("`{tool}` was called with arguments that are not a valid JSON object")]
    MalformedArguments { tool: ToolName },
    #[error("recording a replayed result failed: {0}")]
    Record(#[from] RejectedToken),
    #[error("the dispatch cycle could not settle: {0}")]
    Dispatch(RejectedToken),
    #[error("replay referenced a value the trajectory does not hold: {0}")]
    UnknownValue(#[from] UnknownValue),
    #[error("the session was poisoned by a failed or cancelled in-flight operation and must be rebuilt")]
    Poisoned,
}
