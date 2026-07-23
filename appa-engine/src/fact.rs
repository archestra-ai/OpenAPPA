//! The event log's records and the batch/version types.
//!
//! Everything historical is one append-only log shared across a trajectory family. A [`Fact`] is
//! never checked before appending — history is recorded, not approved — and each is *branch
//! attributed* (carries the [`TrajectoryId`] it belongs to) so a projection can fold a branch's
//! label locally while reading effects family-wide. The engine emits a validated [`FactBatch`];
//! the runtime appends it against an expected [`Revision`] (compare-and-swap).

use serde::{Deserialize, Serialize};

use crate::check::{Gap, Narrowing};
use crate::execute::Issuer;
use crate::label::{Audience, DimValue, Dimension, Label};
use crate::names::{AuthorityName, CastName, SanitizerName};
use crate::plan::PlanId;
use crate::value::{
    ChildReturnId, DispatchId, LabeledValue, Provenance, RawResultDigest, ToolCallId, ToolName, TrajectoryId, ValueId,
};

/// One tool call the model proposed in an assistant turn, recorded verbatim so the model-transcript
/// view replays from the log alone (CC2/RP1). Algebraically inert: the engine never checks this record
/// — the runtime resolves the call into a [`ResolvedCall`](crate::value::ResolvedCall) for the check
/// separately, and pairs it to its model-visible response by `id`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposedCall {
    pub id: ToolCallId,
    pub tool: ToolName,
    pub arguments: serde_json::Value,
}

/// A configurable effect kind — the log's outer-world vocabulary (`egress`, `mutation`,
/// `finance.spend`, …). Declared by contracts as `emits`, appended when a call succeeds.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EffectKind(String);

impl EffectKind {
    pub fn new(kind: impl Into<String>) -> Self {
        EffectKind(kind.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A boundary is punctuation, not a decision: a mark pending plan executions cannot outlive. The
/// engine appends one at the end of each assistant turn, at fork, and at merge. `Fork` and `Merge`
/// carry the branch structure — the fork's parent binding and seed label, the merge's consumed
/// child return.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BoundaryKind {
    TurnEnd,
    /// The child was seeded from `parent` at `seed` (the parent's current label). Immutable binding.
    Fork {
        parent: TrajectoryId,
        seed: Label,
    },
    /// The parent consumed this child return, once, into itself.
    Merge {
        child_return: ChildReturnId,
    },
}

/// How a dispatch closed. Effects commit **only** on success — a call that dispatched but failed
/// appends nothing. A success that admits no value (e.g. an oversized body) still commits effects.
/// `Indeterminate` records a dispatch whose south outcome was never observed (a timeout or a
/// cancelled turn): like a failure it commits nothing, but the audit distinguishes "the tool said
/// no" from "no one knows whether the tool ran".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CloseOutcome {
    Success { effects: Vec<EffectKind> },
    Failure,
    Indeterminate,
}

/// One record in the log. New variants are added by the slice that both emits and consumes them
/// (`dead_code = "deny"` keeps the enum honest — no speculative records).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Fact {
    /// A value entered the trajectory. The branch-local label fold is over these, and only these.
    ValueAdmitted {
        trajectory: TrajectoryId,
        value: LabeledValue,
        provenance: Provenance,
    },
    /// The model's own output for one inference round, recorded so the model-transcript view replays
    /// from the log (CC2/RP1). **Algebraically inert** — it folds no label and commits no effect; the
    /// projection ignores it and only the runtime's transcript builder reads it. `content` is the free
    /// assistant text (a final answer or interleaved note); `calls` are the tool calls this round
    /// proposed, in order.
    AssistantMessage {
        trajectory: TrajectoryId,
        content: Option<String>,
        calls: Vec<ProposedCall>,
    },
    /// The sealed, model-visible response the runtime surfaced for one proposed call that did **not**
    /// admit a raw result — a blocked call's safe feedback, or the fixed token shown when a result was
    /// withheld or the tool failed (RP3). **Algebraically inert.** An available result is shown from its
    /// `ValueAdmitted` instead; this covers exactly the non-available cases, paired to the call by
    /// `call_id`.
    BlockFeedback {
        trajectory: TrajectoryId,
        call_id: ToolCallId,
        content: String,
    },
    /// A dispatch was opened: its proposed label and the effects it would commit on success.
    DispatchOpened {
        trajectory: TrajectoryId,
        dispatch: DispatchId,
        proposed_label: Label,
        proposed_effects: Vec<EffectKind>,
    },
    /// A dispatch closed. On [`CloseOutcome::Success`] its effects commit (family-wide history).
    DispatchClosed {
        trajectory: TrajectoryId,
        dispatch: DispatchId,
        outcome: CloseOutcome,
    },
    /// A ruling admitted a dispatch over one or more requirement gaps. The label does not move — a
    /// ruling records a release, it never edits the trajectory. Call-scoped: bound to the dispatch's
    /// digest, consumed by that dispatch, one review one review.
    Ruling {
        trajectory: TrajectoryId,
        dispatch: DispatchId,
        plan: PlanId,
        authority: AuthorityName,
        issuer: Issuer,
        covers: Vec<Gap>,
    },
    /// The agent accepted a call's narrowing — its own free plan step, never an authority's to make.
    Acceptance {
        trajectory: TrajectoryId,
        dispatch: DispatchId,
        plan: PlanId,
        narrowing: Narrowing,
    },
    /// An output sanitizer relabeled a confined tool result before admission — audit of the
    /// declared transition, bound to the raw result's digest.
    SanitizerApplied {
        trajectory: TrajectoryId,
        dispatch: DispatchId,
        sanitizer: SanitizerName,
        raw_digest: RawResultDigest,
        from: Audience,
        to: Audience,
    },
    /// A cast resolved an Unknown dimension of an admitted value. The projection applies this
    /// override in the fold; the value's body is untouched.
    CastApplied {
        trajectory: TrajectoryId,
        value: ValueId,
        dimension: Dimension,
        resolved: DimValue,
        cast: CastName,
    },
    /// A cast resolved a **pending-cast output dimension** at admission (RP5): the confined raw
    /// result's label was established before any value entered the trajectory. Audit of the
    /// resolution, bound to the raw result's digest; the resolved label rides the `ValueAdmitted`
    /// appended with it, so the projection folds nothing from this record.
    OutputCastApplied {
        trajectory: TrajectoryId,
        dispatch: DispatchId,
        cast: CastName,
        dimension: Dimension,
        resolved: DimValue,
        raw_digest: RawResultDigest,
    },
    /// A child branch returned a value through `submit_result`. The label is the returned value's
    /// own (the child fold for a raw return, or a mandate-validated sanitizer's output); trust never
    /// rises. Only this crosses to the parent — the child's free final text does not.
    ChildReturn {
        trajectory: TrajectoryId,
        id: ChildReturnId,
        value: LabeledValue,
    },
    /// Turn/fork/merge punctuation.
    Boundary {
        trajectory: TrajectoryId,
        kind: BoundaryKind,
    },
}

impl Fact {
    pub fn trajectory(&self) -> &TrajectoryId {
        match self {
            Fact::ValueAdmitted { trajectory, .. }
            | Fact::AssistantMessage { trajectory, .. }
            | Fact::BlockFeedback { trajectory, .. }
            | Fact::DispatchOpened { trajectory, .. }
            | Fact::DispatchClosed { trajectory, .. }
            | Fact::Ruling { trajectory, .. }
            | Fact::Acceptance { trajectory, .. }
            | Fact::SanitizerApplied { trajectory, .. }
            | Fact::CastApplied { trajectory, .. }
            | Fact::OutputCastApplied { trajectory, .. }
            | Fact::ChildReturn { trajectory, .. }
            | Fact::Boundary { trajectory, .. } => trajectory,
        }
    }
}

/// A monotone version marker over the family log's frontier. Every appended batch advances it; the
/// runtime's conditional append is a compare-and-swap on it (concurrent-branch double-consume
/// protection).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Revision(u64);

impl Revision {
    pub const ZERO: Revision = Revision(0);

    pub const fn new(version: u64) -> Self {
        Revision(version)
    }

    pub const fn next(self) -> Self {
        Revision(self.0 + 1)
    }

    pub const fn value(self) -> u64 {
        self.0
    }
}

/// A validated batch the engine produced: the [`Revision`] it was computed against plus the facts
/// to append atomically. The runtime appends it only if the log is still at `basis`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactBatch {
    pub basis: Revision,
    pub facts: Vec<Fact>,
}

impl FactBatch {
    pub fn new(basis: Revision, facts: Vec<Fact>) -> Self {
        FactBatch { basis, facts }
    }
}
