//! The single error type for the crate.
//!
//! Session errors split in two: defects of one proposal (`MalformedArguments`,
//! `UnknownValue`) leave the session usable, while `ReplayBlocked`,
//! `MalformedHistoricalCall`, `Record`, `Dispatch`, and `Poisoned` condemn
//! the whole session — it is poisoned when they are raised, and the adapter
//! reconstructs from its source history. No variant hides a policy rejection
//! behind a default.

use appa_core::{RejectedToken, ToolName, UnknownValue};

#[derive(Debug, thiserror::Error)]
pub enum EdgeError {
    #[error("duplicate contract for `{0}` in policy")]
    DuplicateContract(ToolName),
    #[error("duplicate authority registration: {0}")]
    DuplicateAuthority(String),
    /// A historical result no longer passes policy — the session cannot be
    /// rebuilt on it and must fail closed.
    #[error("a previously-executed call to `{tool}` no longer passes policy: {reason}")]
    ReplayBlocked { tool: ToolName, reason: String },
    #[error("a previously-executed call to `{tool}` has arguments that cannot be parsed")]
    MalformedHistoricalCall { tool: ToolName },
    /// A new proposal's arguments are not a JSON object. Per-proposal: the
    /// session stays usable, the call must not run.
    #[error("`{tool}` was called with arguments that are not a valid JSON object")]
    MalformedArguments { tool: ToolName },
    #[error("recording a replayed result failed: {0}")]
    Record(#[from] RejectedToken),
    /// The dispatch cycle's bookkeeping failed after the token or receipt
    /// was consumed — the released action can no longer be closed.
    #[error("the dispatch cycle could not settle: {0}")]
    Dispatch(RejectedToken),
    #[error("replay referenced a value the trajectory does not hold: {0}")]
    UnknownValue(#[from] UnknownValue),
    /// The session was condemned — a future was dropped mid-await while it
    /// held a linear core capability, or an earlier condemning error struck.
    /// The trajectory can no longer be settled; every mutating call fails
    /// closed from here on.
    #[error("the session was poisoned by a failed or cancelled in-flight operation and must be rebuilt")]
    Poisoned,
}
