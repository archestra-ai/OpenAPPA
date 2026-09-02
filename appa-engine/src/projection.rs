//! The one build path: derive every read model from the log by full reprojection.

use std::collections::{BTreeMap, BTreeSet};

use crate::audience::AudienceEvidence;
use crate::basis::SubjectKey;
use crate::candidate::{DerivedCandidate, SanitizerLineage};
use crate::contract::PinnedAnnotation;
use crate::fact::{
    BoundaryKind, CloseOutcome, EffectKind, EffectSet, Fact, ForkSnapshot, ObservedResult, ReturnPolicy,
};
use crate::label::Label;
use crate::names::{AuthorityName, SanitizerName};
use crate::value::{
    CanonicalDigest, ChildReturnId, DispatchId, ForkId, LabeledValue, Provenance, RawResultDigest, ResolvedCall,
    ToolName, TrajectoryId, ValueId,
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct AdmittedValue {
    trajectory: TrajectoryId,
    label: Label,
    provenance: Provenance,
    body: Option<crate::value::ValueBody>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PreparedFork {
    pub(crate) parent: TrajectoryId,
    pub(crate) snapshot: ForkSnapshot,
    pub(crate) return_policy: ReturnPolicy,
    pub(crate) shape: Option<crate::shape::ReturnShape>,
    denials: BTreeMap<CanonicalDigest, BTreeSet<AuthorityName>>,
}

static MISSING_SOURCE: std::sync::LazyLock<Label> = std::sync::LazyLock::new(Label::bottom);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CloseKind {
    Success,
    Failure,
    Indeterminate,
}

static NO_INHERITED: BTreeSet<ValueId> = BTreeSet::new();

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DecidedBatch {
    pub(crate) trajectory: TrajectoryId,
    pub(crate) payload: CanonicalDigest,
    /// What the decision was about, in proposal order. An offer names its proposal by position,
    /// and both a live execution and a replay re-derive the block from that call — so the decision
    /// record is where the call has to be readable from.
    pub(crate) proposals: Vec<ResolvedCall>,
    /// The proposal the runtime marked as the context-controlled spawn, readable by
    /// position: a marked call's block is planned differently on every later read of it.
    pub(crate) spawn: Option<crate::transition::SpawnMark>,
    pub(crate) released: Vec<DispatchId>,
    pub(crate) evidence: AudienceEvidence,
}

/// One offer the log has opened. Whether it is still *pending* is not stored: that is
/// the comparison between `basis` and what its subject stands at now, which only the versions can
/// answer — so an offer cannot be marked fresh by a record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecordedOffer {
    pub(crate) trajectory: TrajectoryId,
    /// The surfaced block this plan is one of. A repeat reports the identity its offers were
    /// derived under rather than one minted for the retry.
    pub(crate) block: crate::value::BlockId,
    pub(crate) call: CanonicalDigest,
    pub(crate) subject: crate::basis::SubjectKey,
    pub(crate) plan: crate::plan::ExecutableRemedyPlan,
    pub(crate) basis: crate::basis::PolicyBasis,
    /// The pinned audience evidence the act that surfaced this offer consumed: its
    /// execution starts from it.
    pub(crate) evidence: AudienceEvidence,
    pub(crate) end: Option<OfferEnd>,
}

/// The live derived candidate of one subject, with the chain that produced it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecordedCandidate {
    pub(crate) derived: DerivedCandidate,
    pub(crate) lineage: SanitizerLineage,
    /// The pinned audience evidence the hop that derived this candidate consumed: what a
    /// later read of the candidate under its own contract inherits, as it inherits an offer's.
    pub(crate) evidence: AudienceEvidence,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OfferEnd {
    Accepted,
    Denied(crate::names::AuthorityName),
    Invalidated,
}

/// One prepared call approval: the whole release its consumption will land. Keyed by the
/// offer it came from, which is also its subject — so a second approval from one offer is not a
/// state the table can hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PreparedApproval {
    pub(crate) trajectory: TrajectoryId,
    pub(crate) call: ResolvedCall,
    pub(crate) plan: crate::plan::PlanId,
    pub(crate) acceptance: Option<crate::check::Narrowing>,
    pub(crate) rulings: Vec<crate::execute::AuthorityEvidence>,
    pub(crate) sanitizer: Option<crate::names::SanitizerName>,
    pub(crate) basis: crate::basis::PolicyBasis,
    /// The pinned audience evidence the approval was prepared under: its consumption
    /// releases under it.
    pub(crate) evidence: AudienceEvidence,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ReturnedChild {
    id: ChildReturnId,
    value: LabeledValue,
}

/// One durable child submission in custody: the raw handoff a `ReturnSubmitted` record
/// transferred, held here for the return lifecycle alone — the parent's label fold never reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SubmittedReturn {
    pub(crate) fork: ForkId,
    pub(crate) parent: TrajectoryId,
    pub(crate) digest: RawResultDigest,
    body: Option<crate::value::ValueBody>,
    pub(crate) policy: ReturnPolicy,
    /// The parent's established bound pinned at the submission fold step: every
    /// candidate of this return measures its residual here, never against the live fold.
    pub(crate) receiving: Label,
}

impl SubmittedReturn {
    /// The raw bytes in custody. Every reader runs on a pending return — established by
    /// [`Views::pending_return`] or an uncrossed-branch guard — where custody still holds them.
    pub(crate) fn body(&self) -> &crate::value::ValueBody {
        self.body
            .as_ref()
            .expect("custody holds the raw bytes until the crossing consumes them")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RejectedReturn {
    pub(crate) digest: RawResultDigest,
    pub(crate) reason: crate::fact::ReturnRejection,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Projection {
    revision: u64,
    values: Vec<AdmittedValue>,
    local: BTreeMap<TrajectoryId, Vec<ValueId>>,
    effects: Vec<EffectKind>,
    open: BTreeSet<DispatchId>,
    reservations: BTreeMap<DispatchId, EffectSet>,
    closed: BTreeMap<DispatchId, CloseKind>,
    occurrences: BTreeMap<(TrajectoryId, CanonicalDigest), u32>,
    dispatch_calls: BTreeMap<DispatchId, ResolvedCall>,
    receiving_bounds: BTreeMap<DispatchId, Label>,
    /// The narrowing accepted at the check of each dispatch released under an acceptance.
    accepted_narrowings: BTreeMap<DispatchId, crate::check::Narrowing>,
    dispatch_evidence: BTreeMap<DispatchId, AudienceEvidence>,
    subject_dispatches: BTreeMap<crate::basis::SubjectKey, DispatchId>,
    observations: BTreeMap<DispatchId, ObservedResult>,
    prepared: BTreeMap<ForkId, PreparedFork>,
    bound: BTreeMap<ForkId, TrajectoryId>,
    fork_of: BTreeMap<TrajectoryId, ForkId>,
    child_returns: Vec<ReturnedChild>,
    submitted_returns: BTreeMap<ChildReturnId, SubmittedReturn>,
    rejected_returns: BTreeMap<ChildReturnId, RejectedReturn>,
    ended: BTreeSet<TrajectoryId>,
    bound_sanitizers: BTreeMap<DispatchId, SanitizerName>,
    /// The live derived candidate of each subject that has one. A successful hop
    /// replaces its subject's entry, so this holds the candidate the next stage plans from — never
    /// a chain, which the engine deliberately does not precompute.
    candidates: BTreeMap<SubjectKey, RecordedCandidate>,
    /// Denied authorities per trajectory scope, keyed by rendered call (the
    /// denial-exclusion consultation). The engine scopes the exclusion "in the trajectory" and is
    /// silent on forks; the settled reading implemented here — a child snapshots its parent's
    /// effective set at its `Fork` boundary, later ancestor denials and siblings' do not bind
    /// it, and a merge propagates nothing upward — awaits its spec clause.
    /// The snapshot is a deliberate deviation from the rebuild's `O(facts)` shape: each fork
    /// copies its parent's accumulated set, so a log with `D` denials and `F` forks can retain
    /// `D×F` entries. Denials are rare governance events and the planner consults one trajectory
    /// at a time, so the flat copy stays the boring choice over an ancestry-cutoff walk.
    denials: BTreeMap<TrajectoryId, BTreeMap<CanonicalDigest, BTreeSet<AuthorityName>>>,
    opening: Option<(TrajectoryId, Label)>,
    decided: BTreeMap<crate::transition::ProposalBatchId, DecidedBatch>,
    admissions: BTreeMap<crate::transition::ProposalBatchId, Vec<ValueId>>,
    offers: BTreeMap<crate::value::OfferId, RecordedOffer>,
    approvals: BTreeMap<crate::value::OfferId, PreparedApproval>,
    versions: crate::basis::Versions,
}

/// The proposal a call subject names in the batches a trajectory decided: the batch it names,
/// decided by the subject's own trajectory, at the position it names.
fn proposal_of<'a>(
    decided: &'a BTreeMap<crate::transition::ProposalBatchId, DecidedBatch>,
    subject: &SubjectKey,
) -> Option<&'a ResolvedCall> {
    let SubjectKey::Call {
        trajectory,
        batch,
        position,
    } = subject
    else {
        return None;
    };
    let decided = decided.get(batch)?;
    (&decided.trajectory == trajectory)
        .then(|| decided.proposals.get(*position as usize))
        .flatten()
}

impl Projection {
    pub(crate) fn empty(revision: u64) -> Self {
        Projection {
            revision,
            values: Vec::new(),
            local: BTreeMap::new(),
            effects: Vec::new(),
            open: BTreeSet::new(),
            reservations: BTreeMap::new(),
            closed: BTreeMap::new(),
            occurrences: BTreeMap::new(),
            dispatch_calls: BTreeMap::new(),
            receiving_bounds: BTreeMap::new(),
            accepted_narrowings: BTreeMap::new(),
            dispatch_evidence: BTreeMap::new(),
            subject_dispatches: BTreeMap::new(),
            observations: BTreeMap::new(),
            prepared: BTreeMap::new(),
            bound: BTreeMap::new(),
            fork_of: BTreeMap::new(),
            child_returns: Vec::new(),
            submitted_returns: BTreeMap::new(),
            rejected_returns: BTreeMap::new(),
            ended: BTreeSet::new(),
            bound_sanitizers: BTreeMap::new(),
            candidates: BTreeMap::new(),
            denials: BTreeMap::new(),
            opening: None,
            decided: BTreeMap::new(),
            admissions: BTreeMap::new(),
            offers: BTreeMap::new(),
            approvals: BTreeMap::new(),
            versions: crate::basis::Versions::default(),
        }
    }

    /// Fold every view from the family log **without** the transition rules.
    #[cfg(test)]
    pub(crate) fn build(log: &[Fact], revision: u64) -> Self {
        let mut projection = Projection::empty(revision);
        for fact in log {
            projection.fold(fact);
        }
        projection
    }

    pub(crate) fn set_revision(&mut self, revision: u64) {
        self.revision = revision;
    }

    /// Fold one record into every view. The one fold: replay, cache rebuild, and the advance of a
    /// held view all reach the log through this function, so no second fold can drift from it.
    pub(crate) fn fold(&mut self, fact: &Fact) {
        let pinned_receiving: Option<Label> = match fact {
            Fact::ReturnSubmitted { parent, .. } => Some(self.fold_for(parent)),
            _ => None,
        };
        let Projection {
            revision: _,
            values,
            local,
            effects,
            open,
            reservations,
            closed,
            occurrences,
            dispatch_calls,
            receiving_bounds,
            accepted_narrowings,
            dispatch_evidence,
            subject_dispatches,
            observations,
            prepared,
            bound,
            fork_of,
            child_returns,
            submitted_returns,
            rejected_returns,
            ended,
            bound_sanitizers,
            candidates,
            denials,
            opening,
            decided,
            admissions,
            offers,
            approvals,
            versions,
        } = self;
        {
            match fact {
                Fact::TrajectoryOpened {
                    trajectory, profile, ..
                } => {
                    let starting = profile.starting_label().clone();
                    assert!(
                        opening.is_none(),
                        "the validator admits one opening per family log, as its first record"
                    );
                    *opening = Some((trajectory.clone(), starting));
                }
                Fact::BasisAdvanced { advance, .. } => versions.advance(advance),
                Fact::OfferOpened {
                    trajectory,
                    offer,
                    block,
                    call,
                    subject,
                    plan,
                    basis,
                    evidence,
                    ..
                } => {
                    offers.insert(
                        *offer,
                        RecordedOffer {
                            trajectory: trajectory.clone(),
                            block: *block,
                            call: *call,
                            subject: subject.clone(),
                            plan: plan.clone(),
                            basis: *basis,
                            evidence: evidence.clone(),
                            end: None,
                        },
                    );
                }
                Fact::OfferAccepted { offer, .. } => {
                    if let Some(open) = offers.get_mut(offer) {
                        open.end = Some(OfferEnd::Accepted);
                    }
                }
                Fact::OfferDenied { offer, authority, .. } => {
                    if let Some(open) = offers.get_mut(offer) {
                        open.end = Some(OfferEnd::Denied(authority.clone()));
                    }
                }
                Fact::OfferInvalidated { offer, .. } => {
                    if let Some(open) = offers.get_mut(offer) {
                        open.end = Some(OfferEnd::Invalidated);
                    }
                }
                Fact::CallApprovalConsumed { .. } | Fact::CandidateAccepted { .. } => {}
                Fact::CallApproved {
                    trajectory,
                    offer,
                    call,
                    plan,
                    acceptance,
                    rulings,
                    sanitizer,
                    basis,
                    evidence,
                } => {
                    approvals.insert(
                        *offer,
                        PreparedApproval {
                            trajectory: trajectory.clone(),
                            call: call.clone(),
                            plan: *plan,
                            acceptance: acceptance.clone(),
                            rulings: rulings.clone(),
                            sanitizer: sanitizer.clone(),
                            basis: *basis,
                            evidence: evidence.clone(),
                        },
                    );
                }
                Fact::ProposalBatchDecided {
                    trajectory,
                    batch,
                    proposals,
                    spawn,
                    released,
                    evidence,
                } => {
                    decided.insert(
                        batch.clone(),
                        DecidedBatch {
                            trajectory: trajectory.clone(),
                            payload: CanonicalDigest::of_batch(proposals, *spawn),
                            proposals: proposals.clone(),
                            spawn: *spawn,
                            released: released.clone(),
                            evidence: evidence.clone(),
                        },
                    );
                }
                Fact::ValueAdmitted {
                    trajectory,
                    value,
                    provenance,
                } => {
                    let id = ValueId::new(values.len() as u64);
                    local.entry(trajectory.clone()).or_default().push(id);
                    if let Provenance::ProviderRun {
                        batch,
                        effects: observed,
                        ..
                    } = provenance
                    {
                        admissions.entry(batch.clone()).or_default().push(id);
                        effects.extend(observed.iter().cloned());
                    }
                    values.push(AdmittedValue {
                        trajectory: trajectory.clone(),
                        label: value.label.clone(),
                        provenance: provenance.clone(),
                        body: match provenance {
                            Provenance::ToolResult { .. } | Provenance::ProviderRun { .. } => Some(value.body.clone()),
                            Provenance::ChildReturn { .. } => None,
                        },
                    });
                    if let Provenance::ToolResult { dispatch } = provenance {
                        candidates.remove(&SubjectKey::ConfinedResult(dispatch.clone()));
                    }
                }
                Fact::DispatchOpened {
                    trajectory,
                    dispatch,
                    tool,
                    declaration,
                    arguments,
                    receiving,
                    proposed_effects,
                    annotation,
                    subject,
                    evidence,
                    proposed_label: _,
                } => {
                    dispatch_calls.insert(
                        dispatch.clone(),
                        ResolvedCall::new_keyed(tool.clone(), *declaration, arguments.clone())
                            .with_annotation(annotation.clone()),
                    );
                    receiving_bounds.insert(dispatch.clone(), receiving.clone());
                    dispatch_evidence.insert(dispatch.clone(), evidence.clone());
                    subject_dispatches.insert(subject.clone(), dispatch.clone());
                    open.insert(dispatch.clone());
                    reservations.insert(dispatch.clone(), proposed_effects.clone());
                    *occurrences.entry((trajectory.clone(), *dispatch.digest())).or_insert(0) += 1;
                }
                Fact::DispatchSucceeded {
                    dispatch,
                    effects: committed,
                    observed,
                    ..
                } => {
                    observations.insert(dispatch.clone(), observed.clone());
                    reservations.remove(dispatch);
                    effects.extend(committed.iter().cloned());
                }
                Fact::DispatchClosed { dispatch, outcome, .. } => {
                    open.remove(dispatch);
                    closed.insert(
                        dispatch.clone(),
                        match outcome {
                            CloseOutcome::Success { .. } => CloseKind::Success,
                            CloseOutcome::Failure => CloseKind::Failure,
                            CloseOutcome::Indeterminate => CloseKind::Indeterminate,
                        },
                    );
                    match outcome {
                        CloseOutcome::Success { effects: committed } => {
                            reservations.remove(dispatch);
                            effects.extend(committed.iter().cloned());
                        }
                        CloseOutcome::Failure => {
                            reservations.remove(dispatch);
                        }
                        CloseOutcome::Indeterminate => {}
                    }
                }
                Fact::Acceptance {
                    dispatch, narrowing, ..
                } => {
                    accepted_narrowings.insert(dispatch.clone(), narrowing.clone());
                }
                Fact::Ruling { .. } | Fact::ChildReturnAcceptance { .. } => {}
                Fact::Denial {
                    trajectory,
                    digest,
                    authority,
                } => {
                    denials
                        .entry(trajectory.clone())
                        .or_default()
                        .entry(*digest)
                        .or_default()
                        .insert(authority.clone());
                }
                Fact::OutputSanitizerBound {
                    dispatch, sanitizer, ..
                } => {
                    bound_sanitizers.insert(dispatch.clone(), sanitizer.clone());
                }
                Fact::CandidateDerived {
                    subject,
                    derived,
                    lineage,
                    evidence,
                    ..
                } => {
                    candidates.insert(
                        subject.clone(),
                        RecordedCandidate {
                            derived: derived.clone(),
                            lineage: lineage.clone(),
                            evidence: match derived {
                                DerivedCandidate::Call { .. } => evidence.clone(),
                                DerivedCandidate::Result { .. } | DerivedCandidate::Return { .. } => {
                                    AudienceEvidence::default()
                                }
                            },
                        },
                    );
                }
                // The spawn's release prepared this fork; the child that binds it comes later.
                Fact::ForkPrepared {
                    trajectory,
                    fork,
                    snapshot,
                    return_policy,
                    shape,
                } => {
                    prepared.insert(
                        fork.clone(),
                        PreparedFork {
                            parent: trajectory.clone(),
                            snapshot: snapshot.clone(),
                            return_policy: return_policy.clone(),
                            shape: shape.clone(),
                            denials: denials.get(trajectory).cloned().unwrap_or_default(),
                        },
                    );
                }
                Fact::ForkOpened { trajectory, fork } => {
                    if let Some(preparation) = prepared.get(fork) {
                        bound.insert(fork.clone(), trajectory.clone());
                        fork_of.insert(trajectory.clone(), fork.clone());
                        if !preparation.denials.is_empty() {
                            denials.insert(trajectory.clone(), preparation.denials.clone());
                        }
                    }
                }
                Fact::ChildReturn { id, value, .. } => {
                    ended.insert(id.child().clone());
                    child_returns.push(ReturnedChild {
                        id: id.clone(),
                        value: value.clone(),
                    });
                    candidates.remove(&SubjectKey::Return(id.clone()));
                    if let Some(submitted) = submitted_returns.get_mut(id) {
                        submitted.body = None;
                    }
                }
                Fact::ReturnSubmitted {
                    id,
                    fork,
                    parent,
                    digest,
                    body,
                    policy,
                    ..
                } => {
                    ended.insert(id.child().clone());
                    submitted_returns.insert(
                        id.clone(),
                        SubmittedReturn {
                            fork: fork.clone(),
                            parent: parent.clone(),
                            digest: *digest,
                            body: Some(body.clone()),
                            policy: policy.clone(),
                            receiving: pinned_receiving
                                .clone()
                                .expect("a submission's receiving bound was read above"),
                        },
                    );
                }
                Fact::ReturnRejected { id, digest, reason, .. } => {
                    ended.insert(id.child().clone());
                    rejected_returns.insert(
                        id.clone(),
                        RejectedReturn {
                            digest: *digest,
                            reason: reason.clone(),
                        },
                    );
                }
                Fact::Boundary { trajectory, kind } => match kind {
                    BoundaryKind::Merge { .. } => {}
                    BoundaryKind::VoidReturn => {
                        ended.insert(trajectory.clone());
                    }
                },
            }
        }
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn value_label(&self, id: ValueId) -> Option<&Label> {
        usize::try_from(id.index())
            .ok()
            .and_then(|i| self.values.get(i))
            .map(|v| &v.label)
    }

    fn snapshot_of(&self, trajectory: &TrajectoryId) -> Option<&ForkSnapshot> {
        self.fork_of
            .get(trajectory)
            .and_then(|fork| self.prepared.get(fork))
            .map(|prepared| &prepared.snapshot)
    }

    fn basis_sources<'a>(&'a self, trajectory: &'a TrajectoryId) -> impl Iterator<Item = (ValueId, &'a Label)> + 'a {
        let inherited = self
            .snapshot_of(trajectory)
            .map_or(&NO_INHERITED, ForkSnapshot::inherited);
        static NO_VALUES: Vec<ValueId> = Vec::new();
        inherited
            .iter()
            .chain(self.local.get(trajectory).unwrap_or(&NO_VALUES))
            .map(|id| (*id, self.value_label(*id).unwrap_or(&MISSING_SOURCE)))
    }

    fn base_of(&self, trajectory: &TrajectoryId) -> Option<Label> {
        match self.snapshot_of(trajectory) {
            Some(snapshot) => Some(snapshot.base().clone()),
            None => self.root_opening(trajectory).cloned(),
        }
    }

    fn opened_base(&self, trajectory: &TrajectoryId) -> Label {
        self.base_of(trajectory)
            .expect("a fold is read only for a trajectory its opening record or its fork opened")
    }

    fn root_opening(&self, trajectory: &TrajectoryId) -> Option<&Label> {
        match &self.opening {
            Some((root, starting)) if root == trajectory => Some(starting),
            _ => None,
        }
    }

    /// Was this trajectory opened — the root by its `TrajectoryOpened` record, a child by its
    /// `ForkOpened` binding? Nothing else names a trajectory the validator admits a
    /// record on, so opened and named coincide: a fork may open only a child the log has never
    /// opened, since a trajectory opened before the fork was decided under no parent restriction
    /// at all, and the fork cannot retract that afterwards.
    pub(crate) fn is_opened(&self, trajectory: &TrajectoryId) -> bool {
        self.root_opening(trajectory).is_some() || self.fork_of.contains_key(trajectory)
    }

    fn fold_for(&self, trajectory: &TrajectoryId) -> Label {
        let mut fold = self.opened_base(trajectory);
        for (_, label) in self.basis_sources(trajectory) {
            fold.fold(label);
        }
        fold
    }

    /// Would admitting a value at `label` move this trajectory's label? An admission
    /// joins the local sources, so the fold after it is the fold before it with the
    /// value folded in.
    pub(crate) fn admission_moves_label(&self, trajectory: &TrajectoryId, label: &Label) -> bool {
        let before = self.fold_for(trajectory);
        before.combine(label) != before
    }

    fn freeze_basis(&self, trajectory: &TrajectoryId) -> ForkSnapshot {
        ForkSnapshot::freeze(self.opened_base(trajectory), self.basis_sources(trajectory))
    }

    /// The exposed provider-run results one batch identity admitted, in order: the
    /// trajectory, tool and body of each. See [`Views::provider_admissions`].
    pub(crate) fn provider_admissions(
        &self,
        batch: &crate::transition::ProposalBatchId,
    ) -> impl ExactSizeIterator<Item = (&TrajectoryId, &crate::value::ToolName, &crate::value::ValueBody)> {
        self.admissions
            .get(batch)
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .map(|id| {
                let value = &self.values[id.index() as usize];
                let Provenance::ProviderRun { tool, .. } = &value.provenance else {
                    unreachable!("the admissions index holds only provider-run admissions")
                };
                let body = value
                    .body
                    .as_ref()
                    .expect("a provider-run admission retains the body a repeat is compared against");
                (&value.trajectory, tool, body)
            })
    }

    pub(crate) fn admitted_dispatches(&self) -> BTreeSet<DispatchId> {
        self.values
            .iter()
            .filter_map(|value| match &value.provenance {
                Provenance::ToolResult { dispatch } => Some(dispatch.clone()),
                Provenance::ChildReturn { .. } | Provenance::ProviderRun { .. } => None,
            })
            .collect()
    }

    /// The fork a release prepared, if this identity names one. Family-global: a fork
    /// belongs to the log, and the child it will open is not a trajectory of it yet.
    pub(crate) fn prepared_fork(&self, fork: &ForkId) -> Option<&PreparedFork> {
        self.prepared.get(fork)
    }

    pub(crate) fn bound_child(&self, fork: &ForkId) -> Option<&TrajectoryId> {
        self.bound.get(fork)
    }

    pub(crate) fn prepared_forks(&self) -> impl Iterator<Item = &ForkId> {
        self.prepared.keys()
    }

    pub(crate) fn is_dispatch_open(&self, dispatch: &DispatchId) -> bool {
        self.open.contains(dispatch)
    }

    /// Which trajectory surfaced this offer. Family-wide, because the caller that
    /// needs it has only the offer's identity.
    pub(crate) fn offer_trajectory(&self, offer: &crate::value::OfferId) -> Option<&TrajectoryId> {
        self.offers.get(offer).map(|recorded| &recorded.trajectory)
    }

    /// The canonical call one dispatch opened under, as the validator held its record.
    pub(crate) fn dispatch_call_of(&self, dispatch: &DispatchId) -> Option<&ResolvedCall> {
        self.dispatch_calls.get(dispatch)
    }

    pub(crate) fn view<'a>(&'a self, trajectory: &'a TrajectoryId) -> Views<'a> {
        Views {
            projection: self,
            trajectory,
        }
    }
}

pub struct Views<'a> {
    projection: &'a Projection,
    trajectory: &'a TrajectoryId,
}

impl Views<'_> {
    /// The annotation an Annotator pinned to this same canonical call, in a proposal this
    /// trajectory still has an act prepared on: an open offer that stands at its basis, or an
    /// approval it has not spent. The re-proposal that pursues the offer or spends the approval
    /// spells the call it was prepared for instead of consulting again. Once nothing stands on
    /// the pinned call, a new proposal is annotated afresh: the Annotator may judge it
    /// differently since. Standing acts whose pins disagree name no single answer.
    pub fn pinned_annotation(&self, call: &ResolvedCall) -> Option<&PinnedAnnotation> {
        let offered = self
            .projection
            .offers
            .values()
            .filter(|open| {
                &open.trajectory == self.trajectory && open.end.is_none() && open.basis == self.basis_for(&open.subject)
            })
            .filter_map(|open| self.proposed_call(&open.subject));
        let approved = self
            .projection
            .approvals
            .iter()
            .filter(|(offer, approval)| {
                &approval.trajectory == self.trajectory
                    && approval.basis == self.basis_for(&SubjectKey::Approval(**offer))
            })
            .map(|(_, approval)| &approval.call);
        let digest = call.digest();
        let mut pins = offered
            .chain(approved)
            .filter(|standing| standing.digest() == digest && standing.declaration_id() == call.declaration_id())
            .filter_map(ResolvedCall::annotation);
        let first = pins.next()?;
        pins.all(|pin| pin == first).then_some(first)
    }

    pub(crate) fn trajectory(&self) -> &TrajectoryId {
        self.trajectory
    }

    /// The provenance of an admitted value by id — what an Authority reviews for a referenced
    /// argument. Read-only audit context; the fold never consumes it.
    pub fn value_provenance(&self, id: ValueId) -> Option<&Provenance> {
        usize::try_from(id.index())
            .ok()
            .and_then(|i| self.projection.values.get(i))
            .map(|value| &value.provenance)
    }

    /// The tool an opened dispatch called — the originating tool behind a
    /// [`Provenance::ToolResult`], read by readers naming a
    /// value's producer. The fold never consumes it.
    pub fn dispatch_tool(&self, dispatch: &DispatchId) -> Option<&ToolName> {
        self.projection.dispatch_calls.get(dispatch).map(ResolvedCall::tool)
    }

    /// The canonical call a dispatch released. An outcome names its dispatch, and this
    /// is what the engine reports on — never a call the caller re-supplies.
    pub fn dispatch_call(&self, dispatch: &DispatchId) -> Option<&ResolvedCall> {
        self.projection.dispatch_calls.get(dispatch)
    }

    /// What the runtime observed at this dispatch's success checkpoint, if it recorded
    /// one. A later report of the same dispatch is bound to it.
    pub(crate) fn observed_result(&self, dispatch: &DispatchId) -> Option<&ObservedResult> {
        self.projection.observations.get(dispatch)
    }

    /// The body a dispatch's result admitted into the trajectory, if one crossed — the recorded
    /// terminal outcome a repeated report hears.
    pub(crate) fn admitted_body(&self, dispatch: &DispatchId) -> Option<&crate::value::ValueBody> {
        self.projection.values.iter().find_map(|value| match &value.provenance {
            Provenance::ToolResult { dispatch: opened } if opened == dispatch => value.body.as_ref(),
            _ => None,
        })
    }

    /// Does this value belong to the scoped trajectory? Read by the block feedback that names a
    /// value's producing tool: a value this branch did not admit itself stays id-only.
    pub fn owns_value(&self, id: ValueId) -> bool {
        usize::try_from(id.index())
            .ok()
            .and_then(|i| self.projection.values.get(i))
            .is_some_and(|value| &value.trajectory == self.trajectory)
    }

    pub(crate) fn bound_child_of(&self, fork: &ForkId) -> Option<&TrajectoryId> {
        self.projection.bound_child(fork)
    }

    /// Was this fork prepared by a release in this family? Family-global, like the fork
    /// itself; the scoping trajectory plays no part.
    pub(crate) fn is_prepared(&self, fork: &ForkId) -> bool {
        self.projection.prepared_fork(fork).is_some()
    }

    /// What this batch identity is already bound to, family-wide: the trajectory that decided it
    /// and the payload digest. `None` means the identity is fresh.
    pub(crate) fn decided_batch(&self, batch: &crate::transition::ProposalBatchId) -> Option<&DecidedBatch> {
        self.projection.decided.get(batch)
    }

    /// The exposed provider-run results this batch identity has already admitted, in order:
    /// the trajectory that admitted each, its originating tool and its body,
    /// which together are the whole of what a repeat is compared against. The trajectory is part of
    /// it because an identity names one trajectory's payload, and a batch whose siblings were
    /// malformed records it nowhere else. Empty for an identity that admitted none.
    pub(crate) fn provider_admissions(
        &self,
        batch: &crate::transition::ProposalBatchId,
    ) -> impl ExactSizeIterator<Item = (&TrajectoryId, &crate::value::ToolName, &crate::value::ValueBody)> {
        self.projection.provider_admissions(batch)
    }

    /// The offers of one subject that are still **pending**: opened, and recording the basis that
    /// subject stands at now. An offer whose basis has moved is stale and never revives,
    /// so this is what a repeat of the same act is answered with — current guidance, derived from
    /// the facts rather than from anything the runtime remembered.
    pub(crate) fn pending_block(
        &self,
        subject: &crate::basis::SubjectKey,
    ) -> Option<(crate::value::BlockId, Vec<(crate::value::OfferId, crate::plan::PlanId)>)> {
        let current = self.basis_for(subject);
        let pending: Vec<_> = self
            .projection
            .offers
            .iter()
            .filter(|(_, open)| &open.subject == subject && open.basis == current && open.end.is_none())
            .collect();
        let block = pending.first().map(|(_, open)| open.block)?;
        let mut offers: Vec<_> = pending.into_iter().map(|(id, open)| (*id, open.plan.id)).collect();
        offers.sort_by_key(|(_, plan)| *plan);
        Some((block, offers))
    }

    /// Every pending offer of this trajectory whose plan names `authority` for exactly this
    /// rendered call — the set one denial ends together.
    pub(crate) fn offers_naming(
        &self,
        call: &CanonicalDigest,
        authority: &crate::names::AuthorityName,
    ) -> Vec<crate::value::OfferId> {
        self.projection
            .offers
            .iter()
            .filter(|(_, open)| {
                &open.trajectory == self.trajectory
                    && &open.call == call
                    && open.end.is_none()
                    && open.basis == self.basis_for(&open.subject)
                    && open.plan.names_authority(authority)
            })
            .map(|(id, _)| *id)
            .collect()
    }

    pub(crate) fn approval(&self, offer: &crate::value::OfferId) -> Option<&PreparedApproval> {
        self.projection.approvals.get(offer)
    }

    /// Every approval prepared for this exact call in this trajectory, whatever their freshness.
    /// The caller decides what current means: a live decision compares against the basis it stands
    /// at, and the validator against the basis its decision began from.
    pub(crate) fn approvals_for(
        &self,
        call: &ResolvedCall,
    ) -> impl Iterator<Item = (crate::value::OfferId, &PreparedApproval)> {
        self.projection
            .approvals
            .iter()
            .filter(move |(_, approval)| &approval.trajectory == self.trajectory && &approval.call == call)
            .map(|(offer, approval)| (*offer, approval))
    }

    /// The approval this exact call may consume right now. Spending one advances its own
    /// subject, so a spent approval is not current and cannot be found here a second time.
    pub(crate) fn current_approval(&self, call: &ResolvedCall) -> Option<(crate::value::OfferId, &PreparedApproval)> {
        self.approvals_for(call)
            .find(|(offer, approval)| approval.basis == self.basis_for(&crate::basis::SubjectKey::Approval(*offer)))
    }

    /// One opened offer, whatever its freshness. `None` means the identity is unknown to
    /// this family — which is the first thing an execution has to refuse.
    pub(crate) fn offer(&self, offer: &crate::value::OfferId) -> Option<&RecordedOffer> {
        self.projection.offers.get(offer)
    }

    /// Has the log already surfaced this block? Family-wide, like the identity itself,
    /// and read off the offers that name it — a block is nothing but its offers.
    pub(crate) fn block_surfaced(&self, block: &crate::value::BlockId) -> bool {
        self.projection.offers.values().any(|offer| offer.block == *block)
    }

    /// Does this dispatch still hold an unsettled effect reservation? A close that
    /// evaporates one changes what `no_prior` sees family-wide.
    pub(crate) fn reserves(&self, dispatch: &DispatchId) -> bool {
        self.projection
            .reservations
            .get(dispatch)
            .is_some_and(|effects| !effects.is_empty())
    }

    /// The `PolicyBasis` one subject stands at right now: the family's version, this
    /// trajectory's flow version, and the subject's own generation. An offer is pending exactly
    /// while the basis it recorded still equals this one.
    pub(crate) fn basis_for(&self, subject: &crate::basis::SubjectKey) -> crate::basis::PolicyBasis {
        self.projection.versions.basis_for(self.trajectory, subject)
    }

    /// The basis a subject will stand at once `advance` has been applied: the **post-decision**
    /// value an offer or an approval records.
    pub(crate) fn basis_after(
        &self,
        advance: &crate::basis::BasisAdvance,
        subject: &crate::basis::SubjectKey,
    ) -> crate::basis::PolicyBasis {
        self.basis_for(subject).advanced_by(advance, self.trajectory, subject)
    }

    /// The snapshot a fork of this branch freezes — the parent-side basis the spawn
    /// release pins.
    pub(crate) fn freeze_basis(&self) -> ForkSnapshot {
        self.projection.freeze_basis(self.trajectory)
    }

    /// The branch's current label: the fold of every value admitted to this
    /// trajectory, seeded from its fork (a child begins at the parent's current label, never at
    /// top). Branch-local — a value in a sibling branch does not lower this fold.
    pub fn current_label(&self) -> Label {
        self.projection.fold_for(self.trajectory)
    }

    /// The branch-local fold of another opened trajectory in the family — used to validate that
    /// a child's returned value does not raise trust above what the child legitimately holds.
    pub(crate) fn branch_label(&self, trajectory: &TrajectoryId) -> Label {
        self.projection.fold_for(trajectory)
    }

    pub fn parent_of(&self, child: &TrajectoryId) -> Option<&TrajectoryId> {
        self.projection
            .fork_of
            .get(child)
            .and_then(|fork| self.projection.prepared.get(fork))
            .map(|prepared| &prepared.parent)
    }

    /// The child's immutable fork return policy — the binding every `submit_result` crossing is
    /// derived from. `None` for a trajectory that was never forked.
    pub(crate) fn return_policy_of(&self, child: &TrajectoryId) -> Option<&ReturnPolicy> {
        self.projection
            .fork_of
            .get(child)
            .and_then(|fork| self.projection.prepared.get(fork))
            .map(|prepared| &prepared.return_policy)
    }

    /// The structured-return shape the child's fork froze: every non-void submission
    /// validates against exactly this stored form. `None` for an unshaped or unforked child.
    pub(crate) fn return_shape_of(&self, child: &TrajectoryId) -> Option<&crate::shape::ReturnShape> {
        self.projection
            .fork_of
            .get(child)
            .and_then(|fork| self.projection.prepared.get(fork))
            .and_then(|prepared| prepared.shape.as_ref())
    }

    pub(crate) fn child_return(&self, id: &ChildReturnId) -> Option<&LabeledValue> {
        self.projection
            .child_returns
            .iter()
            .find(|returned| &returned.id == id)
            .map(|returned| &returned.value)
    }

    /// How many values `child` has already returned. Nonzero refuses a further return (a child
    /// returns at most once); the count also mints the crossing's occurrence.
    pub(crate) fn returns_by(&self, child: &TrajectoryId) -> u32 {
        self.projection
            .child_returns
            .iter()
            .filter(|returned| returned.id.child() == child)
            .count() as u32
    }

    /// Has this branch ended its errand? True after its one value crossing, its void
    /// terminal, a durable submission that transferred custody, or a terminal
    /// rejection. The one replay-derived ended-branch predicate: an ended branch is
    /// closed to new turns, further returns, and forking, and every gate reads this — never the
    /// raw counts.
    pub fn has_ended(&self, branch: &TrajectoryId) -> bool {
        self.projection.ended.contains(branch)
    }

    pub(crate) fn submitted_return(&self, id: &ChildReturnId) -> Option<&SubmittedReturn> {
        self.projection.submitted_returns.get(id)
    }

    /// The submission still awaiting its crossing: submitted, and no crossing of this identity
    /// has consumed it yet. The return lifecycle plans and validates against exactly this.
    pub(crate) fn pending_return(&self, id: &ChildReturnId) -> Option<&SubmittedReturn> {
        if self.child_return(id).is_some() {
            return None;
        }
        self.projection.submitted_returns.get(id)
    }

    pub(crate) fn rejected_return(&self, id: &ChildReturnId) -> Option<&RejectedReturn> {
        self.projection.rejected_returns.get(id)
    }

    /// The fork that opened this child through the two-stage binding. `None` for a
    /// root, which no fork opened.
    pub(crate) fn fork_of(&self, child: &TrajectoryId) -> Option<&ForkId> {
        self.projection.fork_of.get(child)
    }

    /// The derived fork-time pin a child's fork snapshot froze — the parent's fold
    /// at the fork, which `attest-schema`'s ceiling precondition reads: the answer cannot
    /// come back cleaner than the context that asked.
    pub(crate) fn fork_seed(&self, child: &TrajectoryId) -> Option<&Label> {
        self.projection.snapshot_of(child).map(ForkSnapshot::seed)
    }

    /// How many dispatches of this digest this branch has already opened — the occurrence of the
    /// next one (a repeat identical call is a new dispatch, not a re-issue).
    pub(crate) fn dispatch_count(&self, digest: &CanonicalDigest) -> u32 {
        self.projection
            .occurrences
            .get(&(self.trajectory.clone(), *digest))
            .copied()
            .unwrap_or(0)
    }

    pub(crate) fn has_effect(&self, kind: &EffectKind) -> bool {
        self.projection.effects.iter().any(|e| e == kind)
    }

    /// Does an unsettled reservation anywhere in the family contain a matching emit? `no_prior(k)`
    /// additionally fails on this; `prior(k)` never reads it — both
    /// directions fail closed.
    pub(crate) fn has_reservation(&self, kind: &EffectKind) -> bool {
        self.projection
            .reservations
            .values()
            .any(|reserved| reserved.iter().any(|e| e == kind))
    }

    /// The dispatches this trajectory has open, with the exact call each released: the payload is persisted once, on the opening record, so this is where an outer
    /// layer reads a live call back rather than keeping a row of its own.
    pub fn open_dispatches(&self) -> impl Iterator<Item = (&DispatchId, &ResolvedCall)> {
        let trajectory = self.trajectory;
        self.projection
            .open
            .iter()
            .filter(move |dispatch| dispatch.trajectory() == trajectory)
            .filter_map(|dispatch| {
                self.projection
                    .dispatch_calls
                    .get(dispatch)
                    .map(|call| (dispatch, call))
            })
    }

    /// Did a decided proposal batch release this dispatch? False for a call an offer
    /// execution released on its own: the agent never proposed it, so the
    /// outer layer still owes the harness the call rather than holding it as one in flight.
    pub fn released_by_proposal(&self, dispatch: &DispatchId) -> bool {
        self.projection
            .decided
            .values()
            .any(|batch| batch.released.contains(dispatch))
    }

    pub(crate) fn is_open(&self, dispatch: &DispatchId) -> bool {
        self.projection.open.contains(dispatch)
    }

    pub(crate) fn closed_successfully(&self, dispatch: &DispatchId) -> bool {
        matches!(self.projection.closed.get(dispatch), Some(CloseKind::Success))
    }

    pub(crate) fn dispatch_failed(&self, dispatch: &DispatchId) -> bool {
        matches!(self.projection.closed.get(dispatch), Some(CloseKind::Failure))
    }

    /// Did this dispatch close as indeterminate with nothing observed? The reservation
    /// stands, because the call may have executed, and the log holds no observation that
    /// a later report could agree or disagree with.
    pub(crate) fn closed_unobserved(&self, dispatch: &DispatchId) -> bool {
        matches!(self.projection.closed.get(dispatch), Some(CloseKind::Indeterminate))
            && !self.projection.observations.contains_key(dispatch)
    }

    /// Has this still-open dispatch's success checkpoint already committed its effects? Gates the
    /// close (success-family only, no duplicate effects) and the runtime's once-only checkpoint.
    /// Derived: a checkpoint is exactly a recorded observation on a dispatch that is still open.
    pub(crate) fn is_succeeded(&self, dispatch: &DispatchId) -> bool {
        self.is_open(dispatch) && self.projection.observations.contains_key(dispatch)
    }

    /// The output sanitizer an executed sanitize plan bound to this dispatch, if any.
    /// While one stands, admission takes only that sanitizer's derivation and refuses the raw
    /// result; the runtime also reads it to know which backend to call.
    pub(crate) fn bound_sanitizer(&self, dispatch: &DispatchId) -> Option<&SanitizerName> {
        self.projection.bound_sanitizers.get(dispatch)
    }

    /// The live derived candidate of this subject, if a hop has produced one. The next
    /// stage plans from it, and a successor replaces it.
    pub(crate) fn candidate(&self, subject: &SubjectKey) -> Option<&DerivedCandidate> {
        self.projection.candidates.get(subject).map(|held| &held.derived)
    }

    /// Where this call subject's candidate stands: the label its substituted bytes
    /// carry and the sanitizers its chain has spent. A subject no hop has touched stands at the
    /// origin, so a first proposal and an unspent chain read alike.
    pub(crate) fn call_stage(&self, subject: &SubjectKey) -> crate::candidate::CallStage {
        crate::candidate::CallStage::of(self.candidate(subject), self.lineage(subject))
    }

    /// What the call this subject stands for is to its deployment: the one proposal its batch's
    /// decision marked as the context-controlled spawn, or an ordinary call. A subject
    /// of no decided batch — a stage that is not a call's — is ordinary.
    pub(crate) fn call_role(&self, subject: &SubjectKey) -> crate::plan::CallRole {
        match subject {
            SubjectKey::Call { batch, position, .. }
                if self.decided_batch(batch).is_some_and(|decided| {
                    decided.spawn == Some(crate::transition::SpawnMark::at(*position as usize))
                }) =>
            {
                crate::plan::CallRole::MarkedSpawn
            }
            SubjectKey::Call { .. }
            | SubjectKey::Approval(_)
            | SubjectKey::ConfinedResult(_)
            | SubjectKey::Return(_) => crate::plan::CallRole::Ordinary,
        }
    }

    /// The substituted call standing for this subject, where an input hop derived one.
    /// Every later stage plans and checks against it rather than against the original proposal.
    pub(crate) fn call_candidate(&self, subject: &SubjectKey) -> Option<&ResolvedCall> {
        match self.candidate(subject) {
            Some(DerivedCandidate::Call { call, .. }) => Some(call),
            Some(DerivedCandidate::Result { .. } | DerivedCandidate::Return { .. }) | None => None,
        }
    }

    /// The call this subject was proposed as, before any input hop rewrote it. `None` wherever
    /// the record does not name one: a subject that is not a call's, a batch this view's
    /// trajectory did not decide, or a position that batch does not hold.
    pub(crate) fn proposed_call(&self, subject: &SubjectKey) -> Option<&ResolvedCall> {
        let SubjectKey::Call { trajectory, .. } = subject else {
            return None;
        };
        if trajectory != self.trajectory() {
            return None;
        }
        proposal_of(&self.projection.decided, subject)
    }

    /// The pinned audience evidence the hop that derived this subject's candidate consumed;
    /// empty for a subject no hop has touched.
    pub(crate) fn candidate_evidence(&self, subject: &SubjectKey) -> &AudienceEvidence {
        const NONE: &AudienceEvidence = &AudienceEvidence {
            sources: Vec::new(),
            lookups: Vec::new(),
            identity: Vec::new(),
        };
        self.projection
            .candidates
            .get(subject)
            .map_or(NONE, |held| &held.evidence)
    }

    /// The call this subject stands on now: the candidate an input hop derived, or the proposal
    /// where no hop has run. The one home of that precedence — every read of "the call this
    /// subject is about" goes through it.
    pub(crate) fn standing_call(&self, subject: &SubjectKey) -> Option<&ResolvedCall> {
        self.call_candidate(subject).or_else(|| self.proposed_call(subject))
    }

    /// The sanitizers this subject's chain has already spent. Empty for a subject no
    /// hop has touched, so a first hop and an unspent chain read alike.
    pub(crate) fn lineage(&self, subject: &SubjectKey) -> SanitizerLineage {
        self.projection
            .candidates
            .get(subject)
            .map(|held| held.lineage.clone())
            .unwrap_or_default()
    }

    /// The established bound this dispatch pinned to receive its result against. Every
    /// confined candidate on this dispatch measures its residual here, so admission never asks for
    /// a race-dependent second acceptance when the live fold has moved since the opening.
    pub(crate) fn receiving_bound(&self, dispatch: &DispatchId) -> Option<&Label> {
        self.projection.receiving_bounds.get(dispatch)
    }

    pub(crate) fn dispatch_evidence(&self, dispatch: &DispatchId) -> Option<&AudienceEvidence> {
        self.projection.dispatch_evidence.get(dispatch)
    }

    /// The dispatch this subject's decision released, if one did. A repeat answers with
    /// the act its own position performed: two subjects rendering — or substituting to — the same
    /// call each open their own dispatch, and call equality alone cannot tell them apart.
    pub(crate) fn subject_dispatch(&self, subject: &crate::basis::SubjectKey) -> Option<&DispatchId> {
        self.projection.subject_dispatches.get(subject)
    }

    /// The authorities denied for this rendered call in this trajectory's scope:
    /// recorded here, or inherited from an ancestor at fork time. Plan enumeration is the one
    /// sanctioned consumer (the denial-exclusion consultation), so this stays crate-only.
    pub(crate) fn denied_authorities(&self, digest: &CanonicalDigest) -> Option<&BTreeSet<AuthorityName>> {
        self.projection.denials.get(self.trajectory)?.get(digest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fact::{BoundaryKind, CloseOutcome};
    use crate::label::{Audience, ReaderId, Trust};
    use crate::value::{LabeledValue, Provenance, ResolvedCall, ToolName, ValueBody};
    use serde_json::json;

    fn traj(name: &str) -> TrajectoryId {
        TrajectoryId::new(name)
    }

    fn labeled(trust: u8, aud: Audience) -> LabeledValue {
        LabeledValue::new(ValueBody::new("body"), Label::new(Trust::new(trust), aud))
    }

    fn base() -> Label {
        Label::new(Trust::new(3), Audience::public())
    }

    fn opened(t: &str) -> Fact {
        crate::profile::opening_at(traj(t), base())
    }

    fn admit(t: &str, value: LabeledValue) -> Fact {
        Fact::ValueAdmitted {
            trajectory: traj(t),
            value,
            provenance: Provenance::ToolResult { dispatch: dispatch(t) },
        }
    }

    #[test]
    fn an_admission_moves_the_label_when_it_narrows_the_bound() {
        let identity = || LabeledValue::new(ValueBody::new("body"), base());
        let log = vec![opened("a"), admit("a", identity())];
        let projection = Projection::build(&log, 2);
        assert!(
            !projection.admission_moves_label(&traj("a"), &identity().label),
            "a value at the trajectory's own label folds to the same label"
        );
        assert!(projection.admission_moves_label(&traj("a"), &labeled(1, Audience::public()).label));
        assert!(projection.admission_moves_label(
            &traj("a"),
            &labeled(3, Audience::restricted([ReaderId::new("insider")])).label
        ));
    }

    #[test]
    fn a_pinned_annotation_is_not_reused_once_no_act_stands_on_its_call() {
        let call = ResolvedCall::new(
            ToolName::new("lookup"),
            crate::params::test_arguments(&json!({ "id": 7 })),
        );
        let pin = crate::contract::PinnedAnnotation::new(
            crate::names::AnnotatorName::new("classifier"),
            call.digest(),
            crate::contract::ProducedAnnotation {
                delta: crate::contract::Delta::NONE,
                emits: EffectSet::default(),
                requires: crate::contract::Requires::default(),
            },
        );
        let log = vec![
            opened("a"),
            Fact::ProposalBatchDecided {
                trajectory: traj("a"),
                batch: crate::transition::ProposalBatchId::new("b1"),
                proposals: vec![call.clone().with_annotation(Some(pin))],
                spawn: None,
                released: vec![],
                evidence: crate::audience::AudienceEvidence::default(),
            },
        ];
        assert_eq!(
            build(&log).view(&traj("a")).pinned_annotation(&call),
            None,
            "a decided call with no open offer and no unspent approval is history, not a standing act"
        );
    }

    #[test]
    fn a_proposal_is_read_back_only_for_the_position_its_own_trajectory_decided() {
        let proposal = |body: &str| {
            ResolvedCall::new(
                ToolName::new("post"),
                crate::params::test_arguments(&json!({ "body": body })),
            )
        };
        let batch = crate::transition::ProposalBatchId::new("b1");
        let mut log = vec![opened("a")];
        log.extend(fork_pair("a", "b", ForkSnapshot::freeze(base(), [])));
        log.push(Fact::ProposalBatchDecided {
            trajectory: traj("a"),
            batch: batch.clone(),
            proposals: vec![proposal("first"), proposal("second")],
            spawn: None,
            released: vec![],
            evidence: crate::audience::AudienceEvidence::default(),
        });
        let projection = build(&log);
        let subject = |trajectory: &str, position: u32| SubjectKey::Call {
            trajectory: traj(trajectory),
            batch: batch.clone(),
            position,
        };
        let a = traj("a");
        let views = projection.view(&a);

        assert_eq!(views.proposed_call(&subject("a", 1)), Some(&proposal("second")));
        assert_eq!(
            views.proposed_call(&subject("a", 2)),
            None,
            "a position the batch does not hold"
        );
        assert_eq!(
            views.proposed_call(&subject("b", 0)),
            None,
            "a subject of another trajectory, whatever batch it names"
        );
        assert_eq!(
            projection.view(&traj("b")).proposed_call(&subject("b", 0)),
            None,
            "the batch belongs to trajectory a; b decided nothing"
        );
        assert_eq!(
            views.proposed_call(&SubjectKey::Call {
                trajectory: traj("a"),
                batch: crate::transition::ProposalBatchId::new("absent"),
                position: 0,
            }),
            None
        );
        let block = crate::value::BlockId::of_proposal(
            &crate::value::OfferNonce::new([7u8; 32]),
            &traj("a"),
            &batch,
            0,
            &proposal("first").digest(),
        );
        for other in [
            SubjectKey::Approval(crate::value::OfferId::of_plan(&block, 0, b"plan")),
            SubjectKey::ConfinedResult(dispatch("a")),
        ] {
            assert_eq!(views.proposed_call(&other), None, "a subject that is not a call's");
        }
    }

    fn dispatch(t: &str) -> DispatchId {
        let call = ResolvedCall::new(ToolName::new("tool"), crate::params::test_arguments(&json!({ "t": t })));
        DispatchId::new(traj(t), call.digest(), 0)
    }

    fn fork_pair(parent: &str, child: &str, snapshot: ForkSnapshot) -> Vec<Fact> {
        let fork = ForkId::of(&dispatch(parent));
        vec![
            Fact::ForkPrepared {
                trajectory: traj(parent),
                fork: fork.clone(),
                snapshot,
                return_policy: ReturnPolicy::Raw,
                shape: None,
            },
            Fact::ForkOpened {
                trajectory: traj(child),
                fork,
            },
        ]
    }

    fn build(log: &[Fact]) -> Projection {
        Projection::build(log, log.len() as u64)
    }

    #[test]
    fn label_fold_is_branch_local() {
        let internal = Audience::restricted([ReaderId::new("emp")]);
        let mut log = vec![opened("a")];
        log.extend(fork_pair("a", "b", ForkSnapshot::freeze(base(), [])));
        log.push(admit("a", labeled(1, internal.clone())));
        log.push(admit("b", labeled(3, Audience::public())));
        let p = build(&log);
        assert_eq!(p.view(&traj("a")).current_label(), Label::new(Trust::new(1), internal));
        assert_eq!(
            p.view(&traj("b")).current_label(),
            Label::new(Trust::new(3), Audience::public())
        );
        assert!(!p.is_opened(&traj("c")));
    }

    #[test]
    fn effects_are_family_wide_and_commit_only_on_success() {
        let egress = EffectKind::new("egress");
        let log = vec![
            Fact::DispatchOpened {
                trajectory: traj("a"),
                dispatch: dispatch("a"),
                tool: ToolName::new("tool"),
                declaration: Default::default(),
                arguments: crate::params::test_arguments(&json!({ "t": "a" })),
                proposed_label: Label::top(),
                receiving: Label::top(),
                proposed_effects: EffectSet::new([egress.clone()]).unwrap(),
                annotation: None,
                subject: crate::basis::fixture_subject(&traj("a")),
                evidence: crate::audience::AudienceEvidence::default(),
            },
            Fact::DispatchClosed {
                trajectory: traj("a"),
                dispatch: dispatch("a"),
                outcome: CloseOutcome::Success {
                    effects: EffectSet::new([egress.clone()]).unwrap(),
                },
            },
        ];
        let p = build(&log);
        assert!(p.view(&traj("b")).has_effect(&egress));
        assert!(!p.view(&traj("a")).is_open(&dispatch("a")));
    }

    #[test]
    fn failure_commits_nothing() {
        let egress = EffectKind::new("egress");
        let log = vec![
            Fact::DispatchOpened {
                trajectory: traj("a"),
                dispatch: dispatch("a"),
                tool: ToolName::new("tool"),
                declaration: Default::default(),
                arguments: crate::params::test_arguments(&json!({ "t": "a" })),
                proposed_label: Label::top(),
                receiving: Label::top(),
                proposed_effects: EffectSet::new([egress.clone()]).unwrap(),
                annotation: None,
                subject: crate::basis::fixture_subject(&traj("a")),
                evidence: crate::audience::AudienceEvidence::default(),
            },
            Fact::DispatchClosed {
                trajectory: traj("a"),
                dispatch: dispatch("a"),
                outcome: CloseOutcome::Failure,
            },
        ];
        let p = build(&log);
        assert!(!p.view(&traj("a")).has_effect(&egress));
        assert!(!p.view(&traj("a")).has_reservation(&egress));
        assert!(!p.view(&traj("a")).is_open(&dispatch("a")));
    }

    #[test]
    fn an_indeterminate_close_leaves_the_reservation_standing() {
        let egress = EffectKind::new("egress");
        let log = vec![
            Fact::DispatchOpened {
                trajectory: traj("a"),
                dispatch: dispatch("a"),
                tool: ToolName::new("tool"),
                declaration: Default::default(),
                arguments: crate::params::test_arguments(&json!({ "t": "a" })),
                proposed_label: Label::top(),
                receiving: Label::top(),
                proposed_effects: EffectSet::new([egress.clone()]).unwrap(),
                annotation: None,
                subject: crate::basis::fixture_subject(&traj("a")),
                evidence: crate::audience::AudienceEvidence::default(),
            },
            Fact::DispatchClosed {
                trajectory: traj("a"),
                dispatch: dispatch("a"),
                outcome: CloseOutcome::Indeterminate,
            },
        ];
        let p = build(&log);
        assert!(!p.view(&traj("a")).has_effect(&egress));
        assert!(p.view(&traj("a")).has_reservation(&egress));
        assert!(!p.view(&traj("a")).is_open(&dispatch("a")));
    }

    #[test]
    fn a_denial_scopes_to_its_trajectory_and_rendered_call() {
        use crate::names::AuthorityName;

        let wire = ResolvedCall::new(
            ToolName::new("wire"),
            crate::params::test_arguments(&json!({"amount": 5})),
        );
        let other = ResolvedCall::new(
            ToolName::new("wire"),
            crate::params::test_arguments(&json!({"amount": 6})),
        );
        let log = vec![Fact::Denial {
            trajectory: traj("a"),
            digest: wire.digest(),
            authority: AuthorityName::new("officer"),
        }];
        let p = build(&log);
        let (a, b) = (traj("a"), traj("b"));
        let view = p.view(&a);
        let denied = view.denied_authorities(&wire.digest()).expect("the denial is recorded");
        assert!(denied.contains(&AuthorityName::new("officer")));
        assert!(view.denied_authorities(&other.digest()).is_none());
        let sibling = p.view(&b);
        assert!(sibling.denied_authorities(&wire.digest()).is_none());
    }

    #[test]
    fn a_child_inherits_denials_recorded_before_its_fork_and_not_after() {
        use crate::names::AuthorityName;

        let wire = ResolvedCall::new(ToolName::new("wire"), crate::params::test_arguments(&json!({})));
        let denial = |t: &str, authority: &str| Fact::Denial {
            trajectory: traj(t),
            digest: wire.digest(),
            authority: AuthorityName::new(authority),
        };
        let fork =
            |child: &str, parent: &str| fork_pair(parent, child, ForkSnapshot::freeze(base(), std::iter::empty()));
        let log = [
            vec![opened("root"), denial("root", "early")],
            fork("child", "root"),
            vec![denial("root", "late")],
            fork("grandchild", "child"),
            vec![denial("child", "own")],
            vec![Fact::Boundary {
                trajectory: traj("root"),
                kind: BoundaryKind::Merge {
                    child_return: crate::value::ChildReturnId::new(traj("child"), 0),
                },
            }],
        ]
        .concat();
        let p = build(&log);
        let names = |t: &TrajectoryId| -> Vec<String> {
            p.view(t)
                .denied_authorities(&wire.digest())
                .map(|set| set.iter().map(|name| name.as_str().to_string()).collect())
                .unwrap_or_default()
        };
        let (child, grandchild, root) = (traj("child"), traj("grandchild"), traj("root"));
        assert_eq!(names(&child), ["early", "own"]);
        assert_eq!(names(&grandchild), ["early"]);
        assert_eq!(names(&root), ["early", "late"]);
    }

    #[test]
    fn a_cold_replay_reports_the_ended_trajectory() {
        let log = vec![
            opened("a"),
            admit("a", labeled(2, Audience::public())),
            Fact::Boundary {
                trajectory: traj("a"),
                kind: BoundaryKind::VoidReturn,
            },
        ];
        assert!(build(&log).view(&traj("a")).has_ended(&traj("a")));
    }

    #[test]
    fn a_tool_results_provenance_resolves_to_its_producing_tool() {
        let log = vec![
            opened("a"),
            Fact::DispatchOpened {
                trajectory: traj("a"),
                dispatch: dispatch("a"),
                tool: ToolName::new("fetch_meeting"),
                declaration: Default::default(),
                arguments: crate::params::test_arguments(&json!({ "t": "a" })),
                proposed_label: Label::top(),
                receiving: Label::top(),
                proposed_effects: EffectSet::new([]).unwrap(),
                annotation: None,
                subject: crate::basis::fixture_subject(&traj("a")),
                evidence: crate::audience::AudienceEvidence::default(),
            },
            Fact::ValueAdmitted {
                trajectory: traj("a"),
                value: labeled(1, Audience::public()),
                provenance: Provenance::ToolResult {
                    dispatch: dispatch("a"),
                },
            },
            Fact::ValueAdmitted {
                trajectory: traj("a"),
                value: labeled(1, Audience::public()),
                provenance: Provenance::ChildReturn {
                    child: traj("kid"),
                    id: ChildReturnId::new(traj("kid"), 0),
                },
            },
        ];
        let p = build(&log);
        let a = traj("a");
        let view = p.view(&a);
        let Some(Provenance::ToolResult { dispatch: produced }) = view.value_provenance(ValueId::new(0)) else {
            panic!("the admitted value carries its dispatch provenance");
        };
        assert_eq!(
            view.dispatch_tool(produced).map(ToolName::as_str),
            Some("fetch_meeting")
        );
        assert!(matches!(
            view.value_provenance(ValueId::new(1)),
            Some(Provenance::ChildReturn { .. })
        ));
        assert!(view.dispatch_tool(&dispatch("b")).is_none());
    }

    #[test]
    fn a_dangling_basis_source_folds_fail_closed_to_bottom() {
        let mut log = vec![opened("a")];
        let ghost = labeled(2, Audience::public());
        log.extend(fork_pair(
            "a",
            "b",
            ForkSnapshot::freeze(base(), [(ValueId::new(7), &ghost.label)]),
        ));
        let fold = build(&log).view(&traj("b")).current_label();
        assert_eq!(fold, Label::new(Trust::new(0), Audience::restricted([])));
        let within = crate::label::WithinAssertions::default();
        let providers = std::collections::BTreeSet::new();
        let expansions = crate::label::Expansions::default();
        let context = crate::label::MembershipContext::new(&within, &providers, &expansions);
        assert_eq!(
            fold.covers(
                &crate::label::DeclaredAudience::restricted([ReaderId::new("anyone")]),
                &context,
            ),
            crate::label::Evaluation::Fails,
        );
    }

    #[test]
    fn value_ids_index_in_log_order() {
        let log = vec![
            admit("a", labeled(3, Audience::public())),
            admit("a", labeled(1, Audience::public())),
        ];
        let p = build(&log);
        assert_eq!(p.value_label(ValueId::new(0)).unwrap().trust, Trust::new(3));
        assert_eq!(p.value_label(ValueId::new(1)).unwrap().trust, Trust::new(1));
        assert!(p.value_label(ValueId::new(2)).is_none());
    }
}
