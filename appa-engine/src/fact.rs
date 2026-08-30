//! The event log's records and the batch/version types.

use serde::{Deserialize, Serialize};

use crate::authority::Transition;
use crate::basis::SubjectKey;
use crate::candidate::{DerivedCandidate, DerivedVia, SanitizerLineage};
use crate::check::{Gap, Narrowing};
use crate::execute::AuthorityReview;
use crate::label::{EstablishedLabel, Label, PartialLabel};
use crate::names::{AuthorityName, SanitizerName};
use crate::plan::PlanId;
use crate::profile::{DeploymentProfile, OpenVector, PolicyDialectVersion, PolicyFileKey, PolicyIdentityV1};
use crate::value::{
    CanonicalDigest, ChildReturnId, DispatchId, ForkId, LabeledValue, Provenance, RawResultDigest, ToolName,
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

/// Why a mandatory return sanitizer was inapplicable at submission. Closed and
/// body-free: the reason names the failed precondition, never the refused bytes. A consumed
/// dimension nothing can establish names every source unresolved on it by value id, with all
/// of that source's unresolved dimensions — never a bare verdict.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReturnRejection {
    MandateUnmet,
    ConsumedDimensionUnresolvable(Vec<crate::check::UnestablishedFact>),
    PreconditionUnmet,
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

/// The content snapshot a fork freezes: the parent's established base, the source
/// values that contributed to its label at that moment, and the partial label they derive.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForkSnapshot {
    base: EstablishedLabel,
    inherited: std::collections::BTreeSet<ValueId>,
    seed: PartialLabel,
}

impl ForkSnapshot {
    /// Freeze a basis: the established base plus every contributing source with its label at this
    /// moment, then the forking branch's absorbed merge-carried contributions. Nested
    /// preparations pass their own flattened basis, so a snapshot never has to walk ancestry to
    /// be understood.
    pub fn freeze<'a>(
        base: EstablishedLabel,
        sources: impl IntoIterator<Item = (ValueId, &'a Label)>,
        absorbed: impl IntoIterator<Item = (ValueId, Label)>,
    ) -> ForkSnapshot {
        let sources: std::collections::BTreeMap<ValueId, &Label> = sources.into_iter().collect();
        let mut seed = PartialLabel::from_basis(base.clone(), sources.iter().map(|(id, label)| (*id, *label)));
        for (id, masked) in absorbed {
            seed.fold_value(id, &masked);
        }
        ForkSnapshot {
            base,
            inherited: sources.into_keys().collect(),
            seed,
        }
    }

    /// The fully established label the child's fold starts from — the deployment's starting label
    /// carried down every fork, since no established contribution outside the frozen set exists.
    pub(crate) fn base(&self) -> &EstablishedLabel {
        &self.base
    }

    /// The frozen inherited source set: the values whose contributions the child inherits, and the
    /// ancestor values it MAY resolve.
    pub(crate) fn inherited(&self) -> &std::collections::BTreeSet<ValueId> {
        &self.inherited
    }

    pub fn seed(&self) -> &PartialLabel {
        &self.seed
    }
}

/// A boundary is punctuation, not a decision: it marks the log, never gates it — an offer stands
/// on its subject's basis rather than on any boundary, and executing one is re-validated against
/// the live state. A fork's branch structure lives on its own two records
/// (`ForkPrepared`, `ForkOpened`); `Merge` carries the consumed child return.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BoundaryKind {
    Merge { child_return: ChildReturnId },
    VoidReturn,
}

/// What the runtime observed when a dispatch succeeded: a usable body, bound by its
/// digest, or none. Recorded on the success checkpoint before any external step runs, so the
/// derivation that comes back is bound to the bytes the tool actually returned and a repeat
/// carrying other bytes is a different observation, not a retry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObservedResult {
    Available(RawResultDigest),
    Unavailable,
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
        policy_file_key: PolicyFileKey,
        open_vectors: Vec<OpenVector>,
    },
    ValueAdmitted {
        trajectory: TrajectoryId,
        value: LabeledValue,
        provenance: Provenance,
    },
    DispatchOpened {
        trajectory: TrajectoryId,
        dispatch: DispatchId,
        tool: ToolName,
        declaration: crate::value::ToolDeclarationId,
        arguments: crate::params::CanonicalArguments,
        proposed_label: EstablishedLabel,
        /// The established bound this dispatch's result is received against, snapshotted here
        /// because the bound is pinned at dispatch: admissions between the opening and the result
        /// move the live fold, so a later comparison against that fold would be race-dependent.
        /// `proposed_label` is the *post*-delta committed bound `L ⊓ δ(c)`; this is the `L` it was
        /// computed from, and what a confined candidate's residual is measured against.
        receiving: EstablishedLabel,
        proposed_effects: EffectSet,
        /// The one complete annotation this dispatch was released under, with the mandate that
        /// authorized it. No default: a record without it is not this engine's record.
        annotation: crate::contract::PinnedAnnotation,
        #[serde(default)]
        memberships: Vec<crate::contract::PinnedMembership>,
        #[serde(default)]
        requirement_cast: Option<crate::contract::PinnedRequirementCast>,
        #[serde(default)]
        resolutions: Vec<crate::groups::GroupResolution>,
        subject: crate::basis::SubjectKey,
    },
    DispatchSucceeded {
        trajectory: TrajectoryId,
        dispatch: DispatchId,
        effects: EffectSet,
        observed: ObservedResult,
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
        #[serde(default)]
        resolutions: Vec<crate::groups::GroupResolution>,
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
    OutputSanitizerBound {
        trajectory: TrajectoryId,
        dispatch: DispatchId,
        plan: PlanId,
        sanitizer: SanitizerName,
        contribution: EstablishedLabel,
        #[serde(default)]
        resolutions: Vec<crate::groups::GroupResolution>,
    },
    CandidateDerived {
        trajectory: TrajectoryId,
        subject: SubjectKey,
        via: DerivedVia,
        derived: DerivedCandidate,
        lineage: SanitizerLineage,
        #[serde(default)]
        resolutions: Vec<crate::groups::GroupResolution>,
    },
    CandidateAccepted {
        trajectory: TrajectoryId,
        subject: SubjectKey,
        offer: crate::value::OfferId,
        narrowing: crate::check::Narrowing,
    },
    ChildReturn {
        trajectory: TrajectoryId,
        id: ChildReturnId,
        value: LabeledValue,
        derivation: ReturnDerivation,
        #[serde(default)]
        resolutions: Vec<crate::groups::GroupResolution>,
    },
    ReturnSubmitted {
        trajectory: TrajectoryId,
        id: ChildReturnId,
        fork: ForkId,
        parent: TrajectoryId,
        label: crate::label::PartialLabel,
        digest: RawResultDigest,
        body: crate::value::ValueBody,
        policy: ReturnPolicy,
        #[serde(default)]
        resolutions: Vec<crate::groups::GroupResolution>,
    },
    ReturnRejected {
        trajectory: TrajectoryId,
        id: ChildReturnId,
        fork: ForkId,
        digest: RawResultDigest,
        reason: ReturnRejection,
        #[serde(default)]
        resolutions: Vec<crate::groups::GroupResolution>,
    },
    /// One proposal batch was decided: the identity the runtime supplied and the
    /// policy content it was bound to. This is the decision boundary itself, so replay reads it
    /// rather than inferring atomic acts from flattened facts. A repeat of the identity with this
    /// payload returns the recorded decision instead of deciding again; a repeat carrying other
    /// content is an identity conflict and is refused.
    ///
    /// The proposals are the payload, so the digest the identity binds is derived from them and
    /// never stored. A refused proposal persists nowhere else — without it the decision would be
    /// the one record replay cannot check, and a rewritten log could turn a refusal into a
    /// release with the payload still agreeing.
    ProposalBatchDecided {
        trajectory: TrajectoryId,
        batch: crate::transition::ProposalBatchId,
        proposals: Vec<crate::value::ResolvedCall>,
        spawn: Option<crate::transition::SpawnMark>,
        released: Vec<DispatchId>,
        #[serde(default)]
        resolutions: Vec<crate::groups::GroupResolution>,
    },
    /// One executable plan of one surfaced block, bound to the identity the model will name to
    /// execute it.
    ///
    /// Offers are durable lifecycle records, not an outer-layer pending collection: APPA has no
    /// turn and no clock to expire them on, so what makes one current is the `PolicyBasis` it
    /// records here. It stays pending while that basis still equals its subject's, and the moment
    /// one component differs it is stale — permanently, because a counter only moves forward.
    ///
    /// The plan travels with the record. Without it the runtime would have to hold the offered
    /// plan in memory to execute it later, which is exactly the process-lifetime cache this record
    /// removes; with it, an offer survives a restart the way every other engine fact does.
    OfferOpened {
        trajectory: TrajectoryId,
        offer: crate::value::OfferId,
        block: crate::value::BlockId,
        act: crate::basis::DecidedAct,
        call: CanonicalDigest,
        subject: crate::basis::SubjectKey,
        plan: crate::plan::ExecutableRemedyPlan,
        basis: crate::basis::PolicyBasis,
        #[serde(default)]
        resolutions: Vec<crate::groups::GroupResolution>,
    },
    /// The agent selected this offer, and the engine prepared what its plan promised. Terminal: an offer is accepted once and never revives.
    ///
    /// For a terminal call plan it lands with the [`Fact::CallApproved`] it prepared. Accepting
    /// consumes the candidate the offer was about, which is why the decision that records this
    /// also advances that subject's generation — the sibling offers on the same candidate lose
    /// their basis in the same act that took it.
    OfferAccepted {
        trajectory: TrajectoryId,
        offer: crate::value::OfferId,
    },
    OfferDenied {
        trajectory: TrajectoryId,
        offer: crate::value::OfferId,
        authority: AuthorityName,
    },
    OfferInvalidated {
        trajectory: TrajectoryId,
        offer: crate::value::OfferId,
    },
    CallApproved {
        trajectory: TrajectoryId,
        offer: crate::value::OfferId,
        call: crate::value::ResolvedCall,
        plan: PlanId,
        acceptance: Option<Narrowing>,
        rulings: Vec<crate::execute::AuthorityEvidence>,
        sanitizer: Option<SanitizerName>,
        basis: crate::basis::PolicyBasis,
        #[serde(default)]
        resolutions: Vec<crate::groups::GroupResolution>,
    },
    CallApprovalConsumed {
        trajectory: TrajectoryId,
        offer: crate::value::OfferId,
        dispatch: DispatchId,
    },
    BasisAdvanced {
        trajectory: TrajectoryId,
        act: crate::basis::DecidedAct,
        advance: crate::basis::BasisAdvance,
    },
    ForkPrepared {
        trajectory: TrajectoryId,
        fork: crate::value::ForkId,
        snapshot: ForkSnapshot,
        return_policy: ReturnPolicy,
        shape: Option<crate::shape::ReturnShape>,
    },
    ForkOpened {
        trajectory: TrajectoryId,
        fork: crate::value::ForkId,
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
            | Fact::ProposalBatchDecided { trajectory, .. }
            | Fact::ValueAdmitted { trajectory, .. }
            | Fact::DispatchOpened { trajectory, .. }
            | Fact::DispatchSucceeded { trajectory, .. }
            | Fact::DispatchClosed { trajectory, .. }
            | Fact::Ruling { trajectory, .. }
            | Fact::Denial { trajectory, .. }
            | Fact::Acceptance { trajectory, .. }
            | Fact::ChildReturnAcceptance { trajectory, .. }
            | Fact::OutputSanitizerBound { trajectory, .. }
            | Fact::CandidateDerived { trajectory, .. }
            | Fact::CandidateAccepted { trajectory, .. }
            | Fact::ChildReturn { trajectory, .. }
            | Fact::ReturnSubmitted { trajectory, .. }
            | Fact::ReturnRejected { trajectory, .. }
            | Fact::OfferOpened { trajectory, .. }
            | Fact::OfferAccepted { trajectory, .. }
            | Fact::OfferDenied { trajectory, .. }
            | Fact::OfferInvalidated { trajectory, .. }
            | Fact::CallApproved { trajectory, .. }
            | Fact::CallApprovalConsumed { trajectory, .. }
            | Fact::BasisAdvanced { trajectory, .. }
            | Fact::ForkPrepared { trajectory, .. }
            | Fact::ForkOpened { trajectory, .. }
            | Fact::Boundary { trajectory, .. } => trajectory,
        }
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
