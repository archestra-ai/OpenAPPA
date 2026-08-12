//! The event log's records and the batch/version types.

use serde::{Deserialize, Serialize};

use crate::authority::Transition;
use crate::check::{Gap, Narrowing};
use crate::execute::AuthorityReview;
use crate::label::{DimValue, Dimension, Label};
use crate::names::{AuthorityName, CastName, SanitizerName};
use crate::plan::PlanId;
use crate::profile::{DeploymentProfile, OpenVector, PolicyDialectVersion, PolicyIdentityV1};
use crate::value::{
    CanonicalDigest, ChildReturnId, DispatchId, LabeledValue, Provenance, RawResultDigest, ToolCallId, ToolName,
    TrajectoryId, ValueId,
};

/// How a child bound at fork may return: the immutable policy recorded on the `Fork` boundary.
/// The submission path is **derived from this binding**, never selected by the caller, so no
/// engine client can route a return through a transformer the fork did not declare — that would
/// be a trust-laundering selector.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReturnPolicy {
    Raw,
    Sanitized(SanitizerName),
}

/// How a child's returned value crossed to the parent — the audit half of [`Fact::ChildReturn`]. A
/// sanitized crossing records the declared transition and the raw submission's digest; the raw
/// text itself stays confined in the child.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReturnDerivation {
    Raw,
    Sanitized {
        sanitizer: SanitizerName,
        raw_digest: RawResultDigest,
        transition: Transition,
    },
}

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

/// The canonical set of effect kinds a contract declares or a dispatch commits: unique and
/// sorted, serialized as exactly that sorted sequence — so permutation-equivalent declarations
/// converge to one value, engine-produced facts are byte-identical, and replayed histories agree.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct EffectSet(Vec<EffectKind>);

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
#[error("duplicate declared effect {}", (self.0).as_str())]
pub struct DuplicateEffect(pub EffectKind);

impl EffectSet {
    pub fn new(kinds: impl IntoIterator<Item = EffectKind>) -> Result<EffectSet, DuplicateEffect> {
        let mut kinds: Vec<EffectKind> = kinds.into_iter().collect();
        kinds.sort();
        if let Some(pair) = kinds.windows(2).find(|pair| pair[0] == pair[1]) {
            return Err(DuplicateEffect(pair[0].clone()));
        }
        Ok(EffectSet(kinds))
    }

    pub fn iter(&self) -> impl Iterator<Item = &EffectKind> {
        self.0.iter()
    }

    pub fn contains(&self, kind: &EffectKind) -> bool {
        self.0.binary_search(kind).is_ok()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl<'de> Deserialize<'de> for EffectSet {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let kinds = Vec::<EffectKind>::deserialize(deserializer)?;
        EffectSet::new(kinds).map_err(serde::de::Error::custom)
    }
}

/// A boundary is punctuation, not a decision: it marks the log, never gates it (pending offers
/// die with their turn, and execution is always re-validated against the live state). The engine
/// appends one at the end of each assistant turn, at fork, and at merge. `Fork` and `Merge` carry
/// the branch structure — the fork's parent binding and seed label, the merge's consumed child
/// return.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BoundaryKind {
    TurnEnd,
    Fork {
        parent: TrajectoryId,
        seed: Label,
        return_policy: ReturnPolicy,
    },
    Merge {
        child_return: ChildReturnId,
    },
    VoidReturn,
}

/// How a dispatch closed. Effects commit **only** on success — a call that dispatched but failed
/// appends nothing. A success that admits no value (e.g. an oversized body) still commits effects.
/// `Indeterminate` records a dispatch whose south outcome was never observed (a timeout or a
/// cancelled turn): like a failure it commits nothing, but the audit distinguishes "the tool said
/// no" from "no one knows whether the tool ran".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CloseOutcome {
    Success { effects: EffectSet },
    Failure,
    Indeterminate,
}

/// One record in the log. New variants are added by the slice that both emits and consumes them
/// (`dead_code = "deny"` keeps the enum honest — no speculative records).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Fact {
    TrajectoryOpened {
        trajectory: TrajectoryId,
        dialect: PolicyDialectVersion,
        profile: DeploymentProfile,
        policy_digest: PolicyIdentityV1,
        open_vectors: Vec<OpenVector>,
    },
    ValueAdmitted {
        trajectory: TrajectoryId,
        value: LabeledValue,
        provenance: Provenance,
    },
    AssistantMessage {
        trajectory: TrajectoryId,
        content: Option<String>,
        calls: Vec<ProposedCall>,
    },
    BlockFeedback {
        trajectory: TrajectoryId,
        call_id: ToolCallId,
        content: String,
    },
    DispatchOpened {
        trajectory: TrajectoryId,
        dispatch: DispatchId,
        tool: ToolName,
        arguments: crate::params::CanonicalArguments,
        proposed_label: Label,
        proposed_effects: EffectSet,
        #[serde(default)]
        dynamic_resolutions: Vec<crate::contract::PinnedDynamicResolution>,
    },
    DispatchSucceeded {
        trajectory: TrajectoryId,
        dispatch: DispatchId,
        effects: EffectSet,
    },
    DispatchClosed {
        trajectory: TrajectoryId,
        dispatch: DispatchId,
        outcome: CloseOutcome,
    },
    Ruling {
        trajectory: TrajectoryId,
        dispatch: DispatchId,
        plan: PlanId,
        authority: AuthorityName,
        covers: Vec<Gap>,
        reviewed: AuthorityReview,
    },
    Denial {
        trajectory: TrajectoryId,
        digest: CanonicalDigest,
        authority: AuthorityName,
    },
    Acceptance {
        trajectory: TrajectoryId,
        dispatch: DispatchId,
        plan: PlanId,
        narrowing: Narrowing,
    },
    ChildReturnAcceptance {
        trajectory: TrajectoryId,
        child_return: ChildReturnId,
        narrowing: Narrowing,
    },
    CastApplied {
        trajectory: TrajectoryId,
        value: ValueId,
        dimension: Dimension,
        resolved: DimValue,
        cast: CastName,
    },
    OutputCastApplied {
        trajectory: TrajectoryId,
        dispatch: DispatchId,
        cast: CastName,
        dimension: Dimension,
        resolved: DimValue,
        raw_digest: RawResultDigest,
    },
    OutputCastAccepted {
        trajectory: TrajectoryId,
        dispatch: DispatchId,
        narrowing: Narrowing,
    },
    OutputCastLapsed {
        trajectory: TrajectoryId,
        dispatch: DispatchId,
        cast: CastName,
        dimension: Dimension,
        resolved: DimValue,
        raw_digest: RawResultDigest,
    },
    OutputSanitizerBound {
        trajectory: TrajectoryId,
        dispatch: DispatchId,
        plan: PlanId,
        sanitizer: SanitizerName,
    },
    OutputSanitizerApplied {
        trajectory: TrajectoryId,
        dispatch: DispatchId,
        sanitizer: SanitizerName,
        transition: Transition,
        raw_digest: RawResultDigest,
    },
    ChildReturn {
        trajectory: TrajectoryId,
        id: ChildReturnId,
        value: LabeledValue,
        derivation: ReturnDerivation,
    },
    Boundary {
        trajectory: TrajectoryId,
        kind: BoundaryKind,
    },
}

impl Fact {
    pub fn trajectory(&self) -> &TrajectoryId {
        match self {
            Fact::TrajectoryOpened { trajectory, .. }
            | Fact::ValueAdmitted { trajectory, .. }
            | Fact::AssistantMessage { trajectory, .. }
            | Fact::BlockFeedback { trajectory, .. }
            | Fact::DispatchOpened { trajectory, .. }
            | Fact::DispatchSucceeded { trajectory, .. }
            | Fact::DispatchClosed { trajectory, .. }
            | Fact::Ruling { trajectory, .. }
            | Fact::Denial { trajectory, .. }
            | Fact::Acceptance { trajectory, .. }
            | Fact::ChildReturnAcceptance { trajectory, .. }
            | Fact::CastApplied { trajectory, .. }
            | Fact::OutputCastApplied { trajectory, .. }
            | Fact::OutputCastAccepted { trajectory, .. }
            | Fact::OutputCastLapsed { trajectory, .. }
            | Fact::OutputSanitizerBound { trajectory, .. }
            | Fact::OutputSanitizerApplied { trajectory, .. }
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::value::ResolvedCall;

    #[test]
    fn an_effect_set_is_canonical_and_refuses_duplicates() {
        let ab = EffectSet::new([EffectKind::new("b"), EffectKind::new("a")]).unwrap();
        let ba = EffectSet::new([EffectKind::new("a"), EffectKind::new("b")]).unwrap();
        assert_eq!(ab, ba);
        assert_eq!(serde_json::to_string(&ab).unwrap(), r#"["a","b"]"#);
        assert_eq!(
            EffectSet::new([EffectKind::new("a"), EffectKind::new("a")]),
            Err(DuplicateEffect(EffectKind::new("a")))
        );
        assert!(serde_json::from_str::<EffectSet>(r#"["a","a"]"#).is_err());
        let normalized: EffectSet = serde_json::from_str(r#"["b","a"]"#).unwrap();
        assert_eq!(normalized, ab);
    }

    #[test]
    fn a_denial_fact_round_trips_through_serde() {
        let call = ResolvedCall::new(
            ToolName::new("wire"),
            crate::params::test_arguments(&json!({"to": "hr"})),
        );
        let fact = Fact::Denial {
            trajectory: TrajectoryId::new("t"),
            digest: call.digest(),
            authority: AuthorityName::new("officer"),
        };
        let wire = serde_json::to_string(&fact).expect("a fact serializes");
        assert_eq!(serde_json::from_str::<Fact>(&wire).expect("a fact deserializes"), fact);
    }
}
