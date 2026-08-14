//! The one build path: derive every read model from the log by full reprojection.

use std::collections::{BTreeMap, BTreeSet};

use crate::basis::SubjectKey;
use crate::candidate::{DerivedCandidate, SanitizerLineage};
use crate::check::Narrowing;
use crate::contract::PinnedDynamicResolution;
use crate::fact::{
    BoundaryKind, CloseOutcome, EffectKind, EffectSet, Fact, ForkSnapshot, ObservedResult, ReturnPolicy, Revision,
};
use crate::label::{EstablishedLabel, Label, PartialLabel};
use crate::names::{AuthorityName, SanitizerName};
use crate::value::{
    CanonicalDigest, ChildReturnId, DispatchId, ForkId, LabeledValue, Provenance, ResolvedCall, ToolName, TrajectoryId,
    ValueId,
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct AdmittedValue {
    trajectory: TrajectoryId,
    label: Label,
    provenance: Provenance,
    body: Option<crate::value::ValueBody>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Fork {
    child: TrajectoryId,
    parent: TrajectoryId,
    snapshot: ForkSnapshot,
    return_policy: ReturnPolicy,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PreparedFork {
    pub(crate) parent: TrajectoryId,
    pub(crate) snapshot: ForkSnapshot,
    pub(crate) return_policy: ReturnPolicy,
    denials: BTreeMap<CanonicalDigest, BTreeSet<AuthorityName>>,
}

static MISSING_SOURCE: Label = Label::unknown();

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
    pub(crate) released: Vec<DispatchId>,
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
    pub(crate) end: Option<OfferEnd>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecordedCandidate {
    pub(crate) derived: DerivedCandidate,
    pub(crate) lineage: SanitizerLineage,
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ReturnedChild {
    id: ChildReturnId,
    value: LabeledValue,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Projection {
    revision: Revision,
    values: Vec<AdmittedValue>,
    local: BTreeMap<TrajectoryId, Vec<ValueId>>,
    effects: Vec<EffectKind>,
    open: BTreeSet<DispatchId>,
    reservations: BTreeMap<DispatchId, EffectSet>,
    lapsed: BTreeSet<DispatchId>,
    closed: BTreeMap<DispatchId, CloseKind>,
    occurrences: BTreeMap<(TrajectoryId, CanonicalDigest), u32>,
    dispatch_calls: BTreeMap<DispatchId, ResolvedCall>,
    receiving_bounds: BTreeMap<DispatchId, EstablishedLabel>,
    subject_dispatches: BTreeMap<crate::basis::SubjectKey, DispatchId>,
    observations: BTreeMap<DispatchId, ObservedResult>,
    boundaries: Vec<TrajectoryId>,
    forks: Vec<Fork>,
    prepared: BTreeMap<ForkId, PreparedFork>,
    bound: BTreeMap<ForkId, TrajectoryId>,
    child_returns: Vec<ReturnedChild>,
    voided: BTreeSet<TrajectoryId>,
    bound_sanitizers: BTreeMap<DispatchId, SanitizerName>,
    accepted: BTreeMap<DispatchId, Narrowing>,
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
    active: BTreeSet<TrajectoryId>,
    decided: BTreeMap<crate::transition::ProposalBatchId, DecidedBatch>,
    admissions: BTreeMap<crate::transition::ProposalBatchId, Vec<ValueId>>,
    offers: BTreeMap<crate::value::OfferId, RecordedOffer>,
    approvals: BTreeMap<crate::value::OfferId, PreparedApproval>,
    versions: crate::basis::Versions,
}

impl Projection {
    pub(crate) fn empty(revision: Revision) -> Self {
        Projection {
            revision,
            values: Vec::new(),
            local: BTreeMap::new(),
            effects: Vec::new(),
            open: BTreeSet::new(),
            reservations: BTreeMap::new(),
            lapsed: BTreeSet::new(),
            closed: BTreeMap::new(),
            occurrences: BTreeMap::new(),
            dispatch_calls: BTreeMap::new(),
            receiving_bounds: BTreeMap::new(),
            subject_dispatches: BTreeMap::new(),
            observations: BTreeMap::new(),
            boundaries: Vec::new(),
            forks: Vec::new(),
            prepared: BTreeMap::new(),
            bound: BTreeMap::new(),
            child_returns: Vec::new(),
            voided: BTreeSet::new(),
            bound_sanitizers: BTreeMap::new(),
            accepted: BTreeMap::new(),
            candidates: BTreeMap::new(),
            denials: BTreeMap::new(),
            active: BTreeSet::new(),
            decided: BTreeMap::new(),
            admissions: BTreeMap::new(),
            offers: BTreeMap::new(),
            approvals: BTreeMap::new(),
            versions: crate::basis::Versions::default(),
        }
    }

    /// Fold every view from the family log **without** the transition rules.
    pub fn build(log: &[Fact], revision: Revision) -> Self {
        let mut projection = Projection::empty(revision);
        for fact in log {
            projection.fold(fact);
        }
        projection
    }

    pub(crate) fn set_revision(&mut self, revision: Revision) {
        self.revision = revision;
    }

    /// Fold one record into every view. The one fold: replay, cache rebuild, and the advance of a
    /// held view all reach the log through this function, so no second fold can drift from it.
    pub(crate) fn fold(&mut self, fact: &Fact) {
        let Projection {
            revision: _,
            values,
            local,
            effects,
            open,
            reservations,
            lapsed,
            closed,
            occurrences,
            dispatch_calls,
            receiving_bounds,
            subject_dispatches,
            observations,
            boundaries,
            forks,
            prepared,
            bound,
            child_returns,
            voided,
            bound_sanitizers,
            accepted,
            candidates,
            denials,
            active,
            decided,
            admissions,
            offers,
            approvals,
            versions,
        } = self;
        {
            active.insert(fact.trajectory().clone());
            match fact {
                Fact::TrajectoryOpened { .. } => {}
                Fact::BasisAdvanced { advance, .. } => versions.advance(advance),
                Fact::OfferOpened {
                    trajectory,
                    offer,
                    block,
                    call,
                    subject,
                    plan,
                    basis,
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
                        },
                    );
                }
                Fact::ProposalBatchDecided {
                    trajectory,
                    batch,
                    proposals,
                    spawn,
                    released,
                } => {
                    decided.insert(
                        batch.clone(),
                        DecidedBatch {
                            trajectory: trajectory.clone(),
                            payload: CanonicalDigest::of_batch(proposals, *spawn),
                            proposals: proposals.clone(),
                            released: released.clone(),
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
                            Provenance::UserInput | Provenance::ChildReturn { .. } => None,
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
                    arguments,
                    receiving,
                    proposed_effects,
                    dynamic_resolutions: resolutions,
                    subject,
                    proposed_label: _,
                } => {
                    dispatch_calls.insert(
                        dispatch.clone(),
                        ResolvedCall::new(tool.clone(), arguments.clone())
                            .with_dynamic_resolutions(resolutions.clone()),
                    );
                    receiving_bounds.insert(dispatch.clone(), receiving.clone());
                    if let Some(subject) = subject {
                        subject_dispatches.insert(subject.clone(), dispatch.clone());
                    }
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
                Fact::CastApplied { value, resolved, .. } => {
                    if let Some(v) = usize::try_from(value.index()).ok().and_then(|i| values.get_mut(i)) {
                        v.label = resolved.clone().into_label();
                    }
                }
                Fact::Acceptance {
                    dispatch, narrowing, ..
                } => {
                    accepted.insert(dispatch.clone(), narrowing.clone());
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
                Fact::AssistantMessage { .. } | Fact::BlockFeedback { .. } => {}
                Fact::OutputCastApplied { .. } | Fact::OutputCastAccepted { .. } => {}
                Fact::OutputCastLapsed { dispatch, .. } => {
                    lapsed.insert(dispatch.clone());
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
                    ..
                } => {
                    candidates.insert(
                        subject.clone(),
                        RecordedCandidate {
                            derived: derived.clone(),
                            lineage: lineage.clone(),
                        },
                    );
                }
                // The spawn's release prepared this fork; the child that binds it comes later.
                Fact::ForkPrepared {
                    trajectory,
                    fork,
                    snapshot,
                    return_policy,
                } => {
                    prepared.insert(
                        fork.clone(),
                        PreparedFork {
                            parent: trajectory.clone(),
                            snapshot: snapshot.clone(),
                            return_policy: return_policy.clone(),
                            denials: denials.get(trajectory).cloned().unwrap_or_default(),
                        },
                    );
                }
                Fact::ForkOpened { trajectory, fork } => {
                    if let Some(preparation) = prepared.get(fork) {
                        bound.insert(fork.clone(), trajectory.clone());
                        if !preparation.denials.is_empty() {
                            denials.insert(trajectory.clone(), preparation.denials.clone());
                        }
                        forks.push(Fork {
                            child: trajectory.clone(),
                            parent: preparation.parent.clone(),
                            snapshot: preparation.snapshot.clone(),
                            return_policy: preparation.return_policy.clone(),
                        });
                    }
                }
                Fact::ChildReturn { id, value, .. } => child_returns.push(ReturnedChild {
                    id: id.clone(),
                    value: value.clone(),
                }),
                Fact::Boundary { trajectory, kind } => {
                    boundaries.push(trajectory.clone());
                    match kind {
                        BoundaryKind::TurnEnd => {}
                        BoundaryKind::Fork {
                            parent,
                            snapshot,
                            return_policy,
                        } => {
                            if let Some(inherited) = denials.get(parent).cloned() {
                                denials.insert(trajectory.clone(), inherited);
                            }
                            forks.push(Fork {
                                child: trajectory.clone(),
                                parent: parent.clone(),
                                snapshot: snapshot.clone(),
                                return_policy: return_policy.clone(),
                            });
                        }
                        BoundaryKind::Merge { .. } => {}
                        BoundaryKind::VoidReturn => {
                            voided.insert(trajectory.clone());
                        }
                    }
                }
            }
        }
    }

    pub fn revision(&self) -> Revision {
        self.revision
    }

    pub fn value_label(&self, id: ValueId) -> Option<&Label> {
        usize::try_from(id.index())
            .ok()
            .and_then(|i| self.values.get(i))
            .map(|v| &v.label)
    }

    fn snapshot_of(&self, trajectory: &TrajectoryId) -> Option<&ForkSnapshot> {
        self.forks
            .iter()
            .find(|fork| &fork.child == trajectory)
            .map(|fork| &fork.snapshot)
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

    fn base_of(&self, trajectory: &TrajectoryId) -> EstablishedLabel {
        self.snapshot_of(trajectory)
            .map_or_else(EstablishedLabel::top, |snapshot| snapshot.base().clone())
    }

    fn fold_for(&self, trajectory: &TrajectoryId) -> PartialLabel {
        PartialLabel::from_basis(self.base_of(trajectory), self.basis_sources(trajectory))
    }

    fn freeze_basis(&self, trajectory: &TrajectoryId) -> ForkSnapshot {
        ForkSnapshot::freeze(self.base_of(trajectory), self.basis_sources(trajectory))
    }

    /// Every trajectory the log names — the family membership the transition validator carries
    /// forward when it resumes from a validated view.
    pub(crate) fn trajectories(&self) -> impl Iterator<Item = &TrajectoryId> {
        self.active.iter()
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
                Provenance::UserInput | Provenance::ChildReturn { .. } | Provenance::ProviderRun { .. } => None,
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

    pub fn view<'a>(&'a self, trajectory: &'a TrajectoryId) -> Views<'a> {
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
    /// The dynamic answers a dispatch pinned, read off the canonical call the opening
    /// recorded — the one representation the validator held that record to.
    pub fn dynamic_resolutions(&self, dispatch: &DispatchId) -> Option<&[PinnedDynamicResolution]> {
        self.projection
            .dispatch_calls
            .get(dispatch)
            .map(ResolvedCall::dynamic_resolutions)
    }
    pub fn revision(&self) -> Revision {
        self.projection.revision
    }

    pub fn trajectory(&self) -> &TrajectoryId {
        self.trajectory
    }

    pub fn value_label(&self, id: ValueId) -> Option<&Label> {
        self.projection.value_label(id)
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
    /// [`Provenance::ToolResult`], read by the cast scope gate and by readers naming a
    /// value's producer. The fold never consumes it.
    pub fn dispatch_tool(&self, dispatch: &DispatchId) -> Option<&ToolName> {
        self.projection.dispatch_calls.get(dispatch).map(ResolvedCall::tool)
    }

    /// The canonical call a dispatch released. An outcome names its dispatch, and this
    /// is what the engine reports on — never a call the caller re-supplies.
    pub(crate) fn dispatch_call(&self, dispatch: &DispatchId) -> Option<&ResolvedCall> {
        self.projection.dispatch_calls.get(dispatch)
    }

    /// The dispatch this trajectory opened for exactly this call, oldest first.
    pub(crate) fn dispatch_of(&self, call: &ResolvedCall) -> Option<DispatchId> {
        let digest = call.digest();
        (0..self.dispatch_count(&digest))
            .map(|occurrence| DispatchId::new(self.trajectory.clone(), digest, occurrence))
            .find(|dispatch| self.dispatch_call(dispatch) == Some(call))
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

    /// May this branch resolve `id`? A locally admitted value, or an ancestor value in
    /// its frozen inherited set — the resolution belongs to the immutable source, not the actor,
    /// so whoever holds the source may establish it. A sibling-only or post-fork value is in no
    /// snapshot of this branch and stays out of reach.
    pub(crate) fn may_resolve(&self, id: ValueId) -> bool {
        self.owns_value(id)
            || self
                .projection
                .snapshot_of(self.trajectory)
                .is_some_and(|snapshot| snapshot.inherited().contains(&id))
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

    /// Does the family log name `trajectory` at all? A fork takes an unused child id:
    /// activity recorded before the fork was decided under no parent restriction at all, and the
    /// fork cannot retract it afterwards.
    pub(crate) fn is_active(&self, trajectory: &TrajectoryId) -> bool {
        self.projection.active.contains(trajectory)
    }

    pub(crate) fn freeze_basis(&self) -> ForkSnapshot {
        self.projection.freeze_basis(self.trajectory)
    }

    /// The branch's current partial label: the fold of every value admitted to this
    /// trajectory, seeded from its fork (a child begins at the parent's current label, never at
    /// top). Branch-local — a value in a sibling branch does not lower this fold. The
    /// established bound carries every known restriction; the unresolved sets name
    /// the sources casts have not yet established.
    pub fn current_label(&self) -> PartialLabel {
        self.projection.fold_for(self.trajectory)
    }

    /// The branch-local fold of an arbitrary trajectory in the family — used to validate that a
    /// child's returned value does not raise trust above what the child legitimately holds.
    pub fn branch_label(&self, trajectory: &TrajectoryId) -> PartialLabel {
        self.projection.fold_for(trajectory)
    }

    pub fn parent_of(&self, child: &TrajectoryId) -> Option<&TrajectoryId> {
        self.projection
            .forks
            .iter()
            .find(|fork| &fork.child == child)
            .map(|fork| &fork.parent)
    }

    /// The child's immutable fork return policy — the binding every `submit_result` crossing is
    /// derived from. `None` for a trajectory that was never forked.
    pub fn return_policy_of(&self, child: &TrajectoryId) -> Option<&ReturnPolicy> {
        self.projection
            .forks
            .iter()
            .find(|fork| &fork.child == child)
            .map(|fork| &fork.return_policy)
    }

    pub fn child_return(&self, id: &ChildReturnId) -> Option<&LabeledValue> {
        self.projection
            .child_returns
            .iter()
            .find(|returned| &returned.id == id)
            .map(|returned| &returned.value)
    }

    /// How many values `child` has already returned. Nonzero refuses a further return (a child
    /// returns at most once); the count also mints the crossing's occurrence.
    pub fn returns_by(&self, child: &TrajectoryId) -> u32 {
        self.projection
            .child_returns
            .iter()
            .filter(|returned| returned.id.child() == child)
            .count() as u32
    }

    /// Has this branch ended its errand? True after its one value crossing or its void
    /// terminal. The one replay-derived ended-branch predicate: an ended branch is
    /// closed to new turns, further returns, and forking, and every gate reads this — never the
    /// raw counts.
    pub fn has_ended(&self, branch: &TrajectoryId) -> bool {
        self.returns_by(branch) > 0 || self.projection.voided.contains(branch)
    }

    /// How many dispatches of this digest this branch has already opened — the occurrence of the
    /// next one (a repeat identical call is a new dispatch, not a re-issue).
    pub fn dispatch_count(&self, digest: &CanonicalDigest) -> u32 {
        self.projection
            .occurrences
            .get(&(self.trajectory.clone(), *digest))
            .copied()
            .unwrap_or(0)
    }

    pub fn has_effect(&self, kind: &EffectKind) -> bool {
        self.projection.effects.iter().any(|e| e == kind)
    }

    /// Does an unsettled reservation anywhere in the family contain a matching emit? `no_prior(k)`
    /// additionally fails on this; `prior(k)` never reads it — both
    /// directions fail closed.
    pub fn has_reservation(&self, kind: &EffectKind) -> bool {
        self.projection
            .reservations
            .values()
            .any(|reserved| reserved.iter().any(|e| e == kind))
    }

    pub fn is_open(&self, dispatch: &DispatchId) -> bool {
        self.projection.open.contains(dispatch)
    }

    pub(crate) fn has_lapsed(&self, dispatch: &DispatchId) -> bool {
        self.projection.lapsed.contains(dispatch)
    }

    pub(crate) fn closed_successfully(&self, dispatch: &DispatchId) -> bool {
        matches!(self.projection.closed.get(dispatch), Some(CloseKind::Success))
    }

    pub(crate) fn dispatch_failed(&self, dispatch: &DispatchId) -> bool {
        matches!(self.projection.closed.get(dispatch), Some(CloseKind::Failure))
    }

    /// Has this still-open dispatch's success checkpoint already committed its effects? Gates the
    /// close (success-family only, no duplicate effects) and the runtime's once-only checkpoint.
    /// Derived: a checkpoint is exactly a recorded observation on a dispatch that is still open.
    pub fn is_succeeded(&self, dispatch: &DispatchId) -> bool {
        self.is_open(dispatch) && self.projection.observations.contains_key(dispatch)
    }

    /// The output sanitizer an executed sanitize plan bound to this dispatch, if any.
    /// While one stands, admission takes only that sanitizer's derivation and refuses the raw
    /// result; the runtime also reads it to know which backend to call.
    pub fn bound_sanitizer(&self, dispatch: &DispatchId) -> Option<&SanitizerName> {
        self.projection.bound_sanitizers.get(dispatch)
    }

    /// The narrowing this dispatch's release accepted before it opened. `UNK-16` makes
    /// it sufficient for what that dispatch admits, whatever the live fold has done since: a
    /// pinned contribution that did not narrow its receiving bound cannot become newly narrowing,
    /// and one that did already carries this acceptance.
    pub(crate) fn accepted_narrowing(&self, dispatch: &DispatchId) -> Option<&Narrowing> {
        self.projection.accepted.get(dispatch)
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

    /// The substituted call standing for this subject, where an input hop derived one.
    /// Every later stage plans and checks against it rather than against the original proposal.
    pub(crate) fn call_candidate(&self, subject: &SubjectKey) -> Option<&ResolvedCall> {
        match self.candidate(subject) {
            Some(DerivedCandidate::Call { call, .. }) => Some(call),
            Some(DerivedCandidate::Result { .. }) | None => None,
        }
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
    pub(crate) fn receiving_bound(&self, dispatch: &DispatchId) -> Option<&EstablishedLabel> {
        self.projection.receiving_bounds.get(dispatch)
    }

    /// The dispatch this subject's decision released, if one did. A repeat answers with
    /// the act its own position performed: two subjects rendering — or substituting to — the same
    /// call each open their own dispatch, and call equality alone cannot tell them apart.
    pub(crate) fn subject_dispatch(&self, subject: &crate::basis::SubjectKey) -> Option<&DispatchId> {
        self.projection.subject_dispatches.get(subject)
    }

    pub fn boundary_count(&self) -> usize {
        self.projection
            .boundaries
            .iter()
            .filter(|t| *t == self.trajectory)
            .count()
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
    use crate::label::{Audience, Dim, ReaderId, Trust};
    use crate::value::{LabeledValue, Provenance, ResolvedCall, ToolName, ValueBody};
    use serde_json::json;

    fn traj(name: &str) -> TrajectoryId {
        TrajectoryId::new(name)
    }

    fn labeled(trust: u8, aud: Audience) -> LabeledValue {
        LabeledValue::new(
            ValueBody::new("body"),
            Label::new(Dim::Known(Trust::new(trust)), Dim::Known(aud)),
        )
    }

    fn admit(t: &str, value: LabeledValue) -> Fact {
        Fact::ValueAdmitted {
            trajectory: traj(t),
            value,
            provenance: Provenance::UserInput,
        }
    }

    #[test]
    fn a_cast_fact_rebuilds_the_same_fold_as_a_resolved_admission() {
        let resolved = EstablishedLabel::new(Trust::new(0), Audience::restricted([ReaderId::new("internal")]));
        let via_cast = vec![
            admit(
                "t",
                LabeledValue::new(ValueBody::new("body"), Label::new(Dim::Unknown, Dim::Unknown)),
            ),
            Fact::CastApplied {
                trajectory: traj("t"),
                value: ValueId::new(0),
                resolved: resolved.clone(),
                cast: crate::names::CastName::new("classifier"),
            },
        ];
        let direct = vec![admit(
            "t",
            LabeledValue::new(ValueBody::new("body"), resolved.clone().into_label()),
        )];

        let cast_fold = Projection::build(&via_cast, Revision::new(2))
            .view(&traj("t"))
            .current_label();
        let direct_fold = Projection::build(&direct, Revision::new(1))
            .view(&traj("t"))
            .current_label();
        assert_eq!(cast_fold, direct_fold);
        assert!(cast_fold.is_fully_established());
        assert_eq!(cast_fold.bound(), &resolved);
        let p = Projection::build(&via_cast, Revision::new(2));
        assert_eq!(
            p.view(&traj("t")).value_label(ValueId::new(0)),
            Some(&resolved.into_label())
        );
    }

    fn dispatch(t: &str) -> DispatchId {
        let call = ResolvedCall::new(ToolName::new("tool"), crate::params::test_arguments(&json!({ "t": t })));
        DispatchId::new(traj(t), call.digest(), 0)
    }

    fn build(log: &[Fact]) -> Projection {
        Projection::build(log, Revision::new(log.len() as u64))
    }

    #[test]
    fn label_fold_is_branch_local() {
        let internal = Audience::restricted([ReaderId::new("emp")]);
        let log = vec![
            admit("a", labeled(1, internal.clone())),
            admit("b", labeled(3, Audience::Public)),
        ];
        let p = build(&log);
        assert_eq!(
            p.view(&traj("a")).current_label(),
            PartialLabel::established(EstablishedLabel::new(Trust::new(1), internal))
        );
        assert_eq!(
            p.view(&traj("b")).current_label(),
            PartialLabel::established(EstablishedLabel::new(Trust::new(3), Audience::Public))
        );
        assert_eq!(
            p.view(&traj("c")).current_label(),
            PartialLabel::established(EstablishedLabel::top())
        );
    }

    #[test]
    fn effects_are_family_wide_and_commit_only_on_success() {
        let egress = EffectKind::new("egress");
        let log = vec![
            Fact::DispatchOpened {
                trajectory: traj("a"),
                dispatch: dispatch("a"),
                tool: ToolName::new("tool"),
                arguments: crate::params::test_arguments(&json!({ "t": "a" })),
                proposed_label: EstablishedLabel::top(),
                receiving: EstablishedLabel::top(),
                proposed_effects: EffectSet::new([egress.clone()]).unwrap(),
                dynamic_resolutions: Vec::new(),
                subject: None,
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
                arguments: crate::params::test_arguments(&json!({ "t": "a" })),
                proposed_label: EstablishedLabel::top(),
                receiving: EstablishedLabel::top(),
                proposed_effects: EffectSet::new([egress.clone()]).unwrap(),
                dynamic_resolutions: Vec::new(),
                subject: None,
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
                arguments: crate::params::test_arguments(&json!({ "t": "a" })),
                proposed_label: EstablishedLabel::top(),
                receiving: EstablishedLabel::top(),
                proposed_effects: EffectSet::new([egress.clone()]).unwrap(),
                dynamic_resolutions: Vec::new(),
                subject: None,
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
        let fork = |child: &str, parent: &str| Fact::Boundary {
            trajectory: traj(child),
            kind: BoundaryKind::Fork {
                parent: traj(parent),
                snapshot: ForkSnapshot::freeze(EstablishedLabel::top(), std::iter::empty()),
                return_policy: ReturnPolicy::Raw,
            },
        };
        let log = vec![
            denial("root", "early"),
            fork("child", "root"),
            denial("root", "late"),
            fork("grandchild", "child"),
            denial("child", "own"),
            Fact::Boundary {
                trajectory: traj("root"),
                kind: BoundaryKind::Merge {
                    child_return: crate::value::ChildReturnId::new(traj("child"), 0),
                },
            },
        ];
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
    fn cold_replay_is_deterministic() {
        let log = vec![
            admit("a", labeled(2, Audience::Public)),
            Fact::Boundary {
                trajectory: traj("a"),
                kind: BoundaryKind::TurnEnd,
            },
        ];
        assert_eq!(build(&log), build(&log));
        assert_eq!(build(&log).view(&traj("a")).boundary_count(), 1);
    }

    #[test]
    fn transcript_facts_are_inert_in_the_fold_and_effects() {
        use crate::fact::TranscriptCall;
        use crate::value::{ToolCallId, ToolName};

        let egress = EffectKind::new("egress");
        let log = vec![
            admit("a", labeled(2, Audience::Public)),
            Fact::AssistantMessage {
                trajectory: traj("a"),
                content: None,
                calls: vec![TranscriptCall {
                    id: ToolCallId::new("call_1"),
                    tool: ToolName::new("send_email"),
                    arguments: json!({ "to": "auditor" }),
                }],
            },
            Fact::BlockFeedback {
                trajectory: traj("a"),
                call_id: ToolCallId::new("call_1"),
                content: "blocked: releasing to auditor is not permitted".to_string(),
            },
        ];
        let with = build(&log);
        let without = build(&log[..1]);
        assert_eq!(
            with.view(&traj("a")).current_label(),
            without.view(&traj("a")).current_label()
        );
        assert!(!with.view(&traj("a")).has_effect(&egress));
    }

    #[test]
    fn a_tool_results_provenance_resolves_to_its_producing_tool() {
        let log = vec![
            Fact::DispatchOpened {
                trajectory: traj("a"),
                dispatch: dispatch("a"),
                tool: ToolName::new("fetch_meeting"),
                arguments: crate::params::test_arguments(&json!({ "t": "a" })),
                proposed_label: EstablishedLabel::top(),
                receiving: EstablishedLabel::top(),
                proposed_effects: EffectSet::new([]).unwrap(),
                dynamic_resolutions: Vec::new(),
                subject: None,
            },
            Fact::ValueAdmitted {
                trajectory: traj("a"),
                value: labeled(1, Audience::Public),
                provenance: Provenance::ToolResult {
                    dispatch: dispatch("a"),
                },
            },
            admit("a", labeled(1, Audience::Public)),
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
            Some(Provenance::UserInput)
        ));
        assert!(view.dispatch_tool(&dispatch("b")).is_none());
    }

    #[test]
    fn value_ids_index_in_log_order() {
        let log = vec![
            admit("a", labeled(3, Audience::Public)),
            admit("a", labeled(1, Audience::Public)),
        ];
        let p = build(&log);
        assert_eq!(p.value_label(ValueId::new(0)).unwrap().trust, Dim::Known(Trust::new(3)));
        assert_eq!(p.value_label(ValueId::new(1)).unwrap().trust, Dim::Known(Trust::new(1)));
        assert!(p.value_label(ValueId::new(2)).is_none());
    }
}
