//! The one build path: derive every read model from the log by full reprojection.

use std::collections::{BTreeMap, BTreeSet};

use imbl::{OrdMap, OrdSet, Vector};

use crate::audience::AudienceEvidence;
use crate::basis::SubjectKey;
use crate::candidate::{DerivedCandidate, SanitizerLineage};
use crate::contract::PinnedAnnotation;
use crate::fact::{
    BoundaryKind, CloseOutcome, EffectKind, EffectSet, Fact, ForkSnapshot, ObservedResult, ReturnPolicy, RootForkOrigin,
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
    pub(crate) kind: crate::transition::SpawnKind,
    denials: BTreeMap<CanonicalDigest, BTreeSet<AuthorityName>>,
}

static MISSING_SOURCE: std::sync::LazyLock<Label> = std::sync::LazyLock::new(Label::bottom);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CloseKind {
    Success,
    Failure,
    FailureWithBody(RawResultDigest),
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
    /// What this candidate's own subject stood at when the derivation landed. A proposal may
    /// take the candidate only while the subject has not moved since, which spending it does —
    /// so a spent candidate cannot be taken twice. Only the generation is kept, never the whole
    /// basis: the flow moves on every admitted value, and a narrowing must leave the derivation
    /// takeable so the proposal that takes it is judged against the narrowed label.
    pub(crate) generation: crate::basis::SubjectGeneration,
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
    pub(crate) output: Option<crate::plan::OutputRemedy>,
    pub(crate) return_policy: Option<ReturnPolicy>,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Projection {
    /// How many batches the family log held when this was folded.
    position: u64,
    values: Vector<AdmittedValue>,
    local: OrdMap<TrajectoryId, Vector<ValueId>>,
    /// The meet of every label in `local`, met with only terms the trajectory's live fold also
    /// holds: its opened base, the inherited sources that resolved, and its current resumed
    /// seed. The meet is associative, commutative and idempotent, so the fold stays exact, and
    /// the narrow terms keep clauses they subsume from accumulating.
    local_label: OrdMap<TrajectoryId, Label>,
    effects: Vector<EffectKind>,
    open: OrdSet<DispatchId>,
    reservations: OrdMap<DispatchId, EffectSet>,
    /// The kinds the parent family's unsettled reservations held when this root opened as a
    /// fork of one of its trajectories; empty for any other root. No call of this family can
    /// settle them, so they stand for the family's whole life and answer `no_prior(k)` alone.
    origin_reservations: EffectSet,
    closed: OrdMap<DispatchId, CloseKind>,
    occurrences: OrdMap<(TrajectoryId, CanonicalDigest), u32>,
    dispatch_calls: OrdMap<DispatchId, ResolvedCall>,
    receiving_bounds: OrdMap<DispatchId, Label>,
    /// The terms each dispatch's release pinned, with the effects it reserved.
    release_terms: OrdMap<DispatchId, (crate::fact::ReleaseTerms, EffectSet)>,
    dispatch_evidence: OrdMap<DispatchId, AudienceEvidence>,
    subject_dispatches: OrdMap<crate::basis::SubjectKey, DispatchId>,
    observations: OrdMap<DispatchId, ObservedResult>,
    prepared: OrdMap<ForkId, PreparedFork>,
    bound: OrdMap<ForkId, TrajectoryId>,
    fork_of: OrdMap<TrajectoryId, ForkId>,
    child_returns: Vector<ReturnedChild>,
    /// Values returned per child so far: the occurrence its next crossing mints.
    returns_by: OrdMap<TrajectoryId, u32>,
    /// Children addressed again since their latest return: what such a child says next is a
    /// new crossing, whatever its bytes, because it may answer a different errand at a
    /// different label.
    resumed_since_return: OrdSet<TrajectoryId>,
    /// The label each resumed child last inherited from its parent, folded beside its fork seed.
    resumed: OrdMap<TrajectoryId, Label>,
    ended: OrdSet<TrajectoryId>,
    bound_sanitizers: OrdMap<DispatchId, SanitizerName>,
    withheld_outputs: OrdSet<DispatchId>,
    /// Dispatches whose result a value was admitted for.
    admitted_results: OrdSet<DispatchId>,
    /// The live derived candidate of each subject that has one. A successful hop
    /// replaces its subject's entry, so this holds the candidate the next stage plans from — never
    /// a chain, which the engine deliberately does not precompute.
    candidates: OrdMap<SubjectKey, RecordedCandidate>,
    /// Denied authorities per trajectory scope, keyed by rendered call (the
    /// denial-exclusion consultation). The engine scopes the exclusion "in the trajectory" and is
    /// silent on forks; the settled reading implemented here — a child snapshots its parent's
    /// effective set at its `Fork` boundary, later ancestor denials and siblings' do not bind
    /// it, and a merge propagates nothing upward — awaits its spec clause.
    /// The snapshot is a deliberate deviation from the rebuild's `O(facts)` shape: each fork
    /// copies its parent's accumulated set, so a log with `D` denials and `F` forks can retain
    /// `D×F` entries. Denials are rare governance events and the planner consults one trajectory
    /// at a time, so the flat copy stays the boring choice over an ancestry-cutoff walk.
    denials: OrdMap<TrajectoryId, BTreeMap<CanonicalDigest, BTreeSet<AuthorityName>>>,
    opening: Option<(TrajectoryId, Label)>,
    principal: Option<crate::label::ReaderId>,
    decided: OrdMap<crate::transition::ProposalBatchId, DecidedBatch>,
    admissions: OrdMap<crate::transition::ProposalBatchId, Vec<ValueId>>,
    offers: OrdMap<crate::value::OfferId, RecordedOffer>,
    approvals: OrdMap<crate::value::OfferId, PreparedApproval>,
    /// The approvals not yet spent: a spend is the one advance an approval's subject ever
    /// takes, so a spent approval is never current again and scans skip it.
    unspent: OrdSet<crate::value::OfferId>,
    versions: crate::basis::Versions,
}

/// The call a [`Fact::DispatchOpened`] record dispatched, rebuilt from its fields.
pub(crate) fn opened_call(
    tool: &ToolName,
    arguments: &crate::params::CanonicalArguments,
    annotation: &Option<PinnedAnnotation>,
    interceptions: &Option<crate::intercept::Interceptions>,
    file_basis: &Option<crate::value::FileBasis>,
) -> ResolvedCall {
    ResolvedCall::new(tool.clone(), arguments.clone())
        .with_annotation(annotation.clone())
        .with_interceptions(interceptions.clone())
        .with_file_basis(file_basis.clone())
}

/// The proposal a call subject names in the batches a trajectory decided: the batch it names,
/// decided by the subject's own trajectory, at the position it names.
fn proposal_of<'a>(
    decided: &'a OrdMap<crate::transition::ProposalBatchId, DecidedBatch>,
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

/// A trajectory's cached local fold, rebuilt from its opened base, the inherited sources that
/// resolve now (an admitted value never changes; a dangling one stays to the live walk), its
/// resumed seed, and every local label.
fn rebuilt_local_label(
    base: Option<&Label>,
    snapshot: Option<&ForkSnapshot>,
    seed: Option<&Label>,
    values: &Vector<AdmittedValue>,
    local: Option<&Vector<ValueId>>,
) -> Label {
    let inherited = snapshot.into_iter().flat_map(|snapshot| snapshot.inherited().iter());
    let resolved = inherited
        .chain(local.into_iter().flatten())
        .filter_map(|id| usize::try_from(id.index()).ok().and_then(|index| values.get(index)))
        .map(|value| &value.label);
    let mut fold = Label::top();
    for term in base.into_iter().chain(seed).chain(resolved) {
        fold.fold(term);
    }
    fold
}

/// Apply `change` to every value of `map` in place. `OrdMap` has no mutable iterator, so the keys
/// are collected first and each value is reached through `get_mut`: O(n log n), paid once per
/// trust chain crossing of a family, never on the decision path.
fn each_mut<K: Ord + Clone, V: Clone>(map: &mut OrdMap<K, V>, mut change: impl FnMut(&mut V)) {
    let keys: Vec<K> = map.keys().cloned().collect();
    for key in keys {
        if let Some(value) = map.get_mut(&key) {
            change(value);
        }
    }
}

fn end_offer(offers: &mut OrdMap<crate::value::OfferId, RecordedOffer>, offer: &crate::value::OfferId, end: OfferEnd) {
    if let Some(open) = offers.get_mut(offer) {
        open.end = Some(end);
    }
}

impl Projection {
    pub(crate) fn empty(position: u64) -> Self {
        Projection {
            position,
            values: Default::default(),
            local: Default::default(),
            local_label: Default::default(),
            effects: Default::default(),
            open: Default::default(),
            reservations: Default::default(),
            origin_reservations: EffectSet::default(),
            closed: Default::default(),
            occurrences: Default::default(),
            dispatch_calls: Default::default(),
            receiving_bounds: Default::default(),
            release_terms: Default::default(),
            dispatch_evidence: Default::default(),
            subject_dispatches: Default::default(),
            observations: Default::default(),
            prepared: Default::default(),
            bound: Default::default(),
            fork_of: Default::default(),
            child_returns: Default::default(),
            returns_by: Default::default(),
            resumed_since_return: Default::default(),
            resumed: Default::default(),
            ended: Default::default(),
            bound_sanitizers: Default::default(),
            withheld_outputs: Default::default(),
            admitted_results: Default::default(),
            candidates: Default::default(),
            denials: Default::default(),
            opening: None,
            principal: None,
            decided: Default::default(),
            admissions: Default::default(),
            offers: Default::default(),
            approvals: Default::default(),
            unspent: Default::default(),
            versions: crate::basis::Versions::default(),
        }
    }

    /// Fold every view from the family log **without** the transition rules.
    #[cfg(test)]
    pub(crate) fn build(log: &[Fact], position: u64) -> Self {
        let mut projection = Projection::empty(position);
        projection.fold_all(log);
        projection
    }

    pub(crate) fn fold_all(&mut self, facts: &[Fact]) {
        for fact in facts {
            self.fold(fact);
        }
    }

    pub(crate) fn set_position(&mut self, position: u64) {
        self.position = position;
    }

    /// Carry every label this projection decides from into the trust chain `crossing` arrives
    /// at. Values, bounds, seeds and candidates cross as values; a fork's return floor crosses as
    /// a requirement. What the log recorded as an identity — a call, an offer's plan, an
    /// approval's terms — is not rewritten: it either still matches what the arriving policy
    /// derives when it is used, or it retires there. A cached local fold is rebuilt from its
    /// crossed terms, never crossed itself: crossing by rank name need not keep the order of
    /// ranks, so a crossed meet is not the meet of the crossed terms.
    pub(crate) fn cross(&mut self, crossing: &crate::label::TrustCrossing) {
        for value in self.values.iter_mut() {
            value.label.cross_value(crossing);
        }
        each_mut(&mut self.receiving_bounds, |bound| bound.cross_value(crossing));
        each_mut(&mut self.release_terms, |(terms, _)| {
            // Audience atoms stay symbolic across revisions; only trust ranks need crossing.
            if let Some(trust) = &mut terms.delta.trust {
                *trust = crossing.value(*trust);
            }
        });
        each_mut(&mut self.prepared, |fork| {
            fork.snapshot.cross(crossing);
            fork.return_policy.cross(crossing);
        });
        for returned in self.child_returns.iter_mut() {
            returned.value.label.cross_value(crossing);
        }
        each_mut(&mut self.resumed, |label| label.cross_value(crossing));
        if let Some((_, label)) = self.opening.as_mut() {
            label.cross_value(crossing);
        }
        each_mut(&mut self.candidates, |candidate| candidate.derived.cross(crossing));
        let trajectories: Vec<TrajectoryId> = self.local_label.keys().cloned().collect();
        for trajectory in trajectories {
            let snapshot = self
                .fork_of
                .get(&trajectory)
                .and_then(|fork| self.prepared.get(fork))
                .map(|prepared| &prepared.snapshot);
            let base = match snapshot {
                Some(snapshot) => Some(snapshot.base()),
                None => self
                    .opening
                    .as_ref()
                    .filter(|(root, _)| *root == trajectory)
                    .map(|(_, starting)| starting),
            };
            let rebuilt = rebuilt_local_label(
                base,
                snapshot,
                self.resumed.get(&trajectory),
                &self.values,
                self.local.get(&trajectory),
            );
            self.local_label.insert(trajectory, rebuilt);
        }
    }

    /// Carry this fold to a later policy revision: the family's basis advances, so every offer and
    /// approval an earlier revision recorded stops being current. A call one of them stood for is
    /// decided afresh under the later revision, and a confined result is staged again from its
    /// standing candidate. The advance is no record: replay, the advance of a held view and a live
    /// decision each apply it where the revision they fold at moves, so all three agree.
    pub(crate) fn cross_revision(&mut self) {
        self.versions.advance(&crate::basis::BasisAdvance::family());
    }

    /// Fold one record into every view. The one fold: replay, cache rebuild, and the advance of a
    /// held view all reach the log through this function, so no second fold can drift from it.
    pub(crate) fn fold(&mut self, fact: &Fact) {
        let Projection {
            position: _,
            values,
            local,
            local_label,
            effects,
            open,
            reservations,
            origin_reservations,
            closed,
            occurrences,
            dispatch_calls,
            receiving_bounds,
            release_terms,
            dispatch_evidence,
            subject_dispatches,
            observations,
            prepared,
            bound,
            fork_of,
            child_returns,
            returns_by,
            resumed_since_return,
            resumed,
            ended,
            bound_sanitizers,
            withheld_outputs,
            admitted_results,
            candidates,
            denials,
            opening,
            principal,
            decided,
            admissions,
            offers,
            approvals,
            unspent,
            versions,
        } = self;
        match fact {
            Fact::TrajectoryOpened(crate::fact::TrajectoryOpening {
                trajectory,
                profile,
                forked_from,
                principal: opened_for,
                ..
            }) => {
                let mut starting = profile.starting_label().clone();
                // A root opened as a fork starts where its parent stood: the parent's label
                // folds into the base every later admission folds onto, and the parent
                // family's effects and the parent's denials hold here from the first record.
                // So do the parent family's unsettled reservations, which nothing here can
                // settle.
                if let Some(origin) = forked_from {
                    starting.fold(&origin.label);
                    effects.extend(origin.effects.iter().cloned());
                    *origin_reservations = origin.reservations.clone();
                    if !origin.denials.is_empty() {
                        denials.insert(trajectory.clone(), origin.denials.clone());
                    }
                }
                assert!(
                    opening.is_none(),
                    "the validator admits one opening per family log, as its first record"
                );
                local_label.insert(
                    trajectory.clone(),
                    rebuilt_local_label(
                        Some(&starting),
                        None,
                        resumed.get(trajectory),
                        values,
                        local.get(trajectory),
                    ),
                );
                *opening = Some((trajectory.clone(), starting));
                *principal = opened_for.clone();
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
            Fact::OfferAccepted { offer, .. } => end_offer(offers, offer, OfferEnd::Accepted),
            Fact::OfferDenied { offer, authority, .. } => end_offer(offers, offer, OfferEnd::Denied(authority.clone())),
            Fact::OfferInvalidated { offer, .. } => end_offer(offers, offer, OfferEnd::Invalidated),
            Fact::CallApprovalConsumed { offer, .. } => {
                unspent.remove(offer);
            }
            // Both only advance their subject, which the prepended `BasisAdvanced` applies:
            // the record they spend falls out of "current" by that alone.
            Fact::CandidateAccepted { .. } | Fact::CandidateConsumed { .. } => {}
            Fact::CallApproved {
                trajectory,
                offer,
                call,
                plan,
                acceptance,
                rulings,
                output,
                return_policy,
                basis,
                evidence,
            } => {
                unspent.insert(*offer);
                approvals.insert(
                    *offer,
                    PreparedApproval {
                        trajectory: trajectory.clone(),
                        call: call.clone(),
                        plan: *plan,
                        acceptance: acceptance.clone(),
                        rulings: rulings.clone(),
                        output: output.clone(),
                        return_policy: return_policy.clone(),
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
                local.entry(trajectory.clone()).or_default().push_back(id);
                local_label
                    .entry(trajectory.clone())
                    .or_insert_with(Label::top)
                    .fold(&value.label);
                if let Provenance::ProviderRun {
                    batch,
                    effects: observed,
                    ..
                } = provenance
                {
                    admissions.entry(batch.clone()).or_default().push(id);
                    effects.extend(observed.iter().cloned());
                }
                values.push_back(AdmittedValue {
                    trajectory: trajectory.clone(),
                    label: value.label.clone(),
                    provenance: provenance.clone(),
                    body: match provenance {
                        Provenance::ToolResult { .. } | Provenance::ProviderRun { .. } => Some(value.body.clone()),
                        Provenance::ChildReturn { .. } => None,
                    },
                });
                if let Provenance::ToolResult { dispatch } = provenance {
                    admitted_results.insert(dispatch.clone());
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
                annotation,
                interceptions,
                file_basis,
                subject,
                evidence,
                terms,
                proposed_label: _,
            } => {
                release_terms.insert(dispatch.clone(), (terms.clone(), proposed_effects.clone()));
                dispatch_calls.insert(
                    dispatch.clone(),
                    opened_call(tool, arguments, annotation, interceptions, file_basis),
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
                if outcome.settles_reservation() {
                    reservations.remove(dispatch);
                }
                let kind = match outcome {
                    CloseOutcome::Success { effects: committed } => {
                        effects.extend(committed.iter().cloned());
                        CloseKind::Success
                    }
                    CloseOutcome::Failure => CloseKind::Failure,
                    CloseOutcome::FailureWithBody { observed } => CloseKind::FailureWithBody(*observed),
                    CloseOutcome::Indeterminate => CloseKind::Indeterminate,
                };
                closed.insert(dispatch.clone(), kind);
            }
            Fact::Acceptance { .. } | Fact::Ruling { .. } => {}
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
            Fact::OutputWithheld { dispatch, .. } => {
                withheld_outputs.insert(dispatch.clone());
            }
            Fact::CandidateDerived {
                trajectory,
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
                        // The derivation's own advance is prepended, so this reads the
                        // generation the subject stands at once the derivation has landed.
                        generation: versions.basis_for(trajectory, subject).subject,
                    },
                );
            }
            // The spawn's release prepared this fork; the child that binds it comes later.
            Fact::ForkPrepared {
                trajectory,
                fork,
                snapshot,
                return_policy,
                kind,
            } => {
                prepared.insert(
                    fork.clone(),
                    PreparedFork {
                        parent: trajectory.clone(),
                        snapshot: snapshot.clone(),
                        return_policy: return_policy.clone(),
                        kind: *kind,
                        denials: denials.get(trajectory).cloned().unwrap_or_default(),
                    },
                );
            }
            Fact::ForkOpened { trajectory, fork } => {
                if let Some(preparation) = prepared.get(fork) {
                    bound.insert(fork.clone(), trajectory.clone());
                    fork_of.insert(trajectory.clone(), fork.clone());
                    local_label.insert(
                        trajectory.clone(),
                        rebuilt_local_label(
                            Some(preparation.snapshot.base()),
                            Some(&preparation.snapshot),
                            resumed.get(trajectory),
                            values,
                            local.get(trajectory),
                        ),
                    );
                    if !preparation.denials.is_empty() {
                        denials.insert(trajectory.clone(), preparation.denials.clone());
                    }
                }
            }
            Fact::ChildReturn { id, value, .. } => {
                child_returns.push_back(ReturnedChild {
                    id: id.clone(),
                    value: value.clone(),
                });
                *returns_by.entry(id.child().clone()).or_insert(0) += 1;
                resumed_since_return.remove(id.child());
                candidates.remove(&SubjectKey::Return(id.clone()));
            }
            Fact::Boundary { trajectory, kind } => match kind {
                BoundaryKind::Merge { .. } => {}
                BoundaryKind::VoidReturn => {
                    ended.insert(trajectory.clone());
                }
                BoundaryKind::Resume { seed } => {
                    resumed.insert(trajectory.clone(), seed.clone());
                    // The seed it replaces may be narrower, so the cache drops it with a rebuild.
                    let snapshot = fork_of
                        .get(trajectory)
                        .and_then(|fork| prepared.get(fork))
                        .map(|prepared| &prepared.snapshot);
                    let base = match snapshot {
                        Some(snapshot) => Some(snapshot.base()),
                        None => opening
                            .as_ref()
                            .filter(|(root, _)| root == trajectory)
                            .map(|(_, starting)| starting),
                    };
                    local_label.insert(
                        trajectory.clone(),
                        rebuilt_local_label(base, snapshot, Some(seed), values, local.get(trajectory)),
                    );
                    resumed_since_return.insert(trajectory.clone());
                    let occurrence = returns_by.get(trajectory).copied().unwrap_or(0);
                    candidates.remove(&SubjectKey::Return(ChildReturnId::new(trajectory.clone(), occurrence)));
                }
            },
        }
    }

    pub(crate) fn position(&self) -> u64 {
        self.position
    }

    fn value(&self, id: ValueId) -> Option<&AdmittedValue> {
        usize::try_from(id.index()).ok().and_then(|i| self.values.get(i))
    }

    pub(crate) fn value_label(&self, id: ValueId) -> Option<&Label> {
        self.value(id).map(|v| &v.label)
    }

    fn prepared_of(&self, child: &TrajectoryId) -> Option<&PreparedFork> {
        self.fork_of.get(child).and_then(|fork| self.prepared.get(fork))
    }

    fn snapshot_of(&self, trajectory: &TrajectoryId) -> Option<&ForkSnapshot> {
        self.prepared_of(trajectory).map(|prepared| &prepared.snapshot)
    }

    fn source(&self, id: ValueId) -> (ValueId, &Label) {
        (id, self.value_label(id).unwrap_or(&MISSING_SOURCE))
    }

    /// The ancestor values a fork snapshot froze for this trajectory. Read at every fold rather
    /// than cached: an id the snapshot names may resolve to no admitted value.
    fn inherited_sources<'a>(
        &'a self,
        trajectory: &'a TrajectoryId,
    ) -> impl Iterator<Item = (ValueId, &'a Label)> + 'a {
        self.snapshot_of(trajectory)
            .map_or(&NO_INHERITED, ForkSnapshot::inherited)
            .iter()
            .map(|id| self.source(*id))
    }

    fn basis_sources<'a>(&'a self, trajectory: &'a TrajectoryId) -> impl Iterator<Item = (ValueId, &'a Label)> + 'a {
        self.inherited_sources(trajectory).chain(
            self.local
                .get(trajectory)
                .into_iter()
                .flatten()
                .map(|id| self.source(*id)),
        )
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
        if let Some(seed) = self.resumed.get(trajectory) {
            fold.fold(seed);
        }
        for (_, label) in self.inherited_sources(trajectory) {
            fold.fold(label);
        }
        if let Some(local) = self.local_label.get(trajectory) {
            fold.fold(local);
        }
        fold
    }

    /// The fold by its definition: every basis source walked in order. The reference the cached
    /// `fold_for` is checked against.
    #[cfg(test)]
    fn fold_by_walk(&self, trajectory: &TrajectoryId) -> Label {
        let mut fold = self.opened_base(trajectory);
        if let Some(seed) = self.resumed.get(trajectory) {
            fold.fold(seed);
        }
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

    /// What an independent conversation-root fork of this trajectory carries over, at this
    /// revision: the trajectory's current label, the family's committed effects, the kinds the
    /// family's unsettled reservations hold (those its own origin carried among them), and the
    /// trajectory's denials. `None` for a trajectory that is not opened or has ended.
    pub(crate) fn root_fork_origin(&self, family: &TrajectoryId, trajectory: &TrajectoryId) -> Option<RootForkOrigin> {
        if !self.is_opened(trajectory) || self.ended.contains(trajectory) {
            return None;
        }
        let reserved = self
            .reservations
            .values()
            .flat_map(EffectSet::iter)
            .chain(self.origin_reservations.iter());
        Some(RootForkOrigin {
            parent_root: family.clone(),
            parent: trajectory.clone(),
            basis: self.position,
            label: self.fold_for(trajectory),
            effects: EffectSet::distinct(&self.effects),
            reservations: EffectSet::distinct(reserved),
            denials: self.denials.get(trajectory).cloned().unwrap_or_default(),
            principal: self.principal.clone(),
        })
    }

    /// The reader the family's opening says it acts for: the member `self` answers with.
    pub(crate) fn principal(&self) -> Option<&crate::label::ReaderId> {
        self.principal.as_ref()
    }

    /// Has a value been admitted for this dispatch's result?
    pub(crate) fn admitted_result(&self, dispatch: &DispatchId) -> bool {
        self.admitted_results.contains(dispatch)
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

    /// Nothing the family started is still in flight or owed: no open dispatch (a spawn in
    /// flight holds its dispatch open), no unsettled reservation, and every bound child has
    /// ended or answered its latest errand.
    pub(crate) fn is_quiescent(&self) -> bool {
        self.open.is_empty()
            && self.origin_reservations.is_empty()
            && self.reservations.values().all(EffectSet::is_empty)
            && self.bound.values().all(|child| {
                self.ended.contains(child)
                    || (self.returns_by.contains_key(child) && !self.resumed_since_return.contains(child))
            })
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
    /// The reader the family acts for, as its opening pinned it.
    pub(crate) fn principal(&self) -> Option<&crate::label::ReaderId> {
        self.projection.principal()
    }

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
            .filter(|open| &open.trajectory == self.trajectory && self.is_pending(open))
            .filter_map(|open| self.proposed_call(&open.subject));
        let approved = self
            .unspent_approvals()
            .filter(|(offer, approval)| {
                &approval.trajectory == self.trajectory
                    && approval.basis == self.basis_for(&SubjectKey::Approval(*offer))
            })
            .map(|(_, approval)| &approval.call);
        let digest = call.digest();
        let mut pins = offered
            .chain(approved)
            .filter(|standing| standing.digest() == digest)
            .filter_map(ResolvedCall::annotation);
        let first = pins.next()?;
        pins.all(|pin| pin == first).then_some(first)
    }

    /// The answers of the Interceptors pinned to this call in an act the trajectory still has
    /// prepared — an open offer or an unspent approval — when every standing pin agrees. The
    /// re-proposal of a prepared call carries them, as it carries the annotation.
    pub fn pinned_interceptions(&self, call: &ResolvedCall) -> Option<&crate::intercept::Interceptions> {
        let offered = self
            .projection
            .offers
            .values()
            .filter(|open| &open.trajectory == self.trajectory && self.is_pending(open))
            .filter_map(|open| self.proposed_call(&open.subject));
        let approved = self
            .unspent_approvals()
            .filter(|(offer, approval)| {
                &approval.trajectory == self.trajectory
                    && approval.basis == self.basis_for(&SubjectKey::Approval(*offer))
            })
            .map(|(_, approval)| &approval.call);
        let digest = call.digest();
        let mut pins = offered
            .chain(approved)
            .filter(|standing| standing.digest() == digest)
            .filter_map(ResolvedCall::interceptions);
        let first = pins.next()?;
        pins.all(|pin| pin == first).then_some(first)
    }

    pub(crate) fn trajectory(&self) -> &TrajectoryId {
        self.trajectory
    }

    /// The provenance of an admitted value by id — what an Authority reviews for a referenced
    /// argument. Read-only audit context; the fold never consumes it.
    pub fn value_provenance(&self, id: ValueId) -> Option<&Provenance> {
        self.projection.value(id).map(|value| &value.provenance)
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

    pub(crate) fn bound_child(&self, fork: &ForkId) -> Option<&TrajectoryId> {
        self.projection.bound_child(fork)
    }

    /// The fork a release in this family prepared, before or after a child is bound to it.
    /// Family-global, like the fork itself; the scoping trajectory plays no part.
    pub(crate) fn prepared_fork(&self, fork: &ForkId) -> Option<&PreparedFork> {
        self.projection.prepared_fork(fork)
    }

    /// What this batch identity is already bound to, family-wide: the trajectory that decided it
    /// and the payload digest. `None` means the identity is fresh.
    pub(crate) fn decided_batch(&self, batch: &crate::transition::ProposalBatchId) -> Option<&DecidedBatch> {
        self.projection.decided.get(batch)
    }

    /// The batch identity whose decision released this dispatch, if one did: how a repeat
    /// of the released call names the decision it is answered from.
    pub(crate) fn batch_of_dispatch(&self, dispatch: &DispatchId) -> Option<&crate::transition::ProposalBatchId> {
        self.projection
            .decided
            .iter()
            .find(|(_, decided)| decided.released.contains(dispatch))
            .map(|(batch, _)| batch)
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
        self.projection
            .admissions
            .get(batch)
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .map(|id| {
                let value = self
                    .projection
                    .value(*id)
                    .expect("the admissions index holds only admitted values");
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

    /// The offers of one subject that are still **pending**: opened, and recording the basis that
    /// subject stands at now. An offer whose basis has moved is stale and never revives,
    /// so this is what a repeat of the same act is answered with — current guidance, derived from
    /// the facts rather than from anything the runtime remembered.
    pub(crate) fn pending_block(
        &self,
        subject: &crate::basis::SubjectKey,
    ) -> Option<(crate::value::BlockId, Vec<(crate::value::OfferId, crate::plan::PlanId)>)> {
        let pending: Vec<_> = self
            .projection
            .offers
            .iter()
            .filter(|(_, open)| &open.subject == subject && self.is_pending(open))
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
                    && self.is_pending(open)
                    && open.plan.names_authority(authority)
            })
            .map(|(id, _)| *id)
            .collect()
    }

    /// Is this offer still pending: opened, not ended, and recording the basis its subject stands
    /// at now?
    fn is_pending(&self, open: &RecordedOffer) -> bool {
        open.end.is_none() && open.basis == self.basis_for(&open.subject)
    }

    pub(crate) fn approval(&self, offer: &crate::value::OfferId) -> Option<&PreparedApproval> {
        self.projection.approvals.get(offer)
    }

    /// Every current approval in this trajectory, found through the unspent set.
    #[cfg(test)]
    pub(crate) fn current_approvals(&self) -> Vec<crate::value::OfferId> {
        self.unspent_approvals()
            .filter(|(offer, approval)| {
                &approval.trajectory == self.trajectory
                    && approval.basis == self.basis_for(&SubjectKey::Approval(*offer))
            })
            .map(|(offer, _)| offer)
            .collect()
    }

    /// Every current approval in this trajectory by its definition: every approval ever
    /// recorded, filtered by freshness. The reference `current_approvals` is checked against.
    #[cfg(test)]
    pub(crate) fn current_approvals_by_scan(&self) -> Vec<crate::value::OfferId> {
        self.projection
            .approvals
            .iter()
            .filter(|(offer, approval)| {
                &approval.trajectory == self.trajectory
                    && approval.basis == self.basis_for(&SubjectKey::Approval(**offer))
            })
            .map(|(offer, _)| *offer)
            .collect()
    }

    fn unspent_approvals(&self) -> impl Iterator<Item = (crate::value::OfferId, &PreparedApproval)> {
        self.projection.unspent.iter().map(|offer| {
            let approval = self
                .projection
                .approvals
                .get(offer)
                .expect("unspent names only recorded approvals");
            (*offer, approval)
        })
    }

    /// Every unspent approval prepared for this exact call in this trajectory, whatever their
    /// freshness. The caller decides what current means: a live decision compares against the
    /// basis it stands at, and the validator against the basis its decision began from.
    pub(crate) fn approvals_for(
        &self,
        call: &ResolvedCall,
    ) -> impl Iterator<Item = (crate::value::OfferId, &PreparedApproval)> {
        self.unspent_approvals()
            .filter(move |(_, approval)| &approval.trajectory == self.trajectory && &approval.call == call)
    }

    /// Every call candidate staged for exactly these substituted bytes in this trajectory,
    /// whatever their freshness. Two blocked calls can derive the same bytes — a redaction
    /// collapses distinct inputs by construction — so this yields more than one, and the caller
    /// decides which is current.
    ///
    /// The subject's own key names the trajectory, and a derivation is claimable only within it.
    pub(crate) fn call_candidates_for(
        &self,
        call: &ResolvedCall,
    ) -> impl Iterator<Item = (&crate::basis::SubjectKey, &RecordedCandidate)> {
        self.projection.candidates.iter().filter(move |(subject, candidate)| {
            matches!(subject, crate::basis::SubjectKey::Call { trajectory, .. } if trajectory == self.trajectory)
                && matches!(&candidate.derived, DerivedCandidate::Call { call: staged, .. } if staged.renders(call))
        })
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
        ForkSnapshot::freeze(
            self.projection.opened_base(self.trajectory),
            self.projection.basis_sources(self.trajectory),
        )
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
        self.projection.prepared_of(child).map(|prepared| &prepared.parent)
    }

    /// The child's immutable fork return policy — the binding every `submit_result` crossing is
    /// derived from. `None` for a trajectory that was never forked.
    pub(crate) fn return_policy_of(&self, child: &TrajectoryId) -> Option<&ReturnPolicy> {
        self.projection
            .prepared_of(child)
            .map(|prepared| &prepared.return_policy)
    }

    pub(crate) fn child_return(&self, id: &ChildReturnId) -> Option<&LabeledValue> {
        self.projection
            .child_returns
            .iter()
            .find(|returned| &returned.id == id)
            .map(|returned| &returned.value)
    }

    /// The value `child` crossed most recently, if any: a stop repeating it is a replay of that
    /// crossing, never a second one. A child addressed again since has no replayable return:
    /// what it says next crosses anew, at the label it stands at then.
    pub(crate) fn latest_return(&self, child: &TrajectoryId) -> Option<&LabeledValue> {
        if self.projection.resumed_since_return.contains(child) {
            return None;
        }
        self.projection
            .child_returns
            .iter()
            .rev()
            .find(|returned| returned.id.child() == child)
            .map(|returned| &returned.value)
    }

    /// How many values `child` has already returned — the occurrence its next crossing mints.
    pub(crate) fn returns_by(&self, child: &TrajectoryId) -> u32 {
        self.projection.returns_by.get(child).copied().unwrap_or(0)
    }

    /// Has this branch ended its errand by its void terminal? A value crossing ends nothing:
    /// a child returns again after the parent addresses it again. The one replay-derived
    /// ended-branch predicate: an ended branch is closed to new turns, further returns, and
    /// forking, and every gate reads this — never the raw counts.
    pub fn has_ended(&self, branch: &TrajectoryId) -> bool {
        self.projection.ended.contains(branch)
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
    pub fn dispatch_count(&self, digest: &CanonicalDigest) -> u32 {
        self.projection
            .occurrences
            .get(&(self.trajectory.clone(), *digest))
            .copied()
            .unwrap_or(0)
    }

    pub(crate) fn has_effect(&self, kind: &EffectKind) -> bool {
        self.projection.effects.iter().any(|e| e == kind)
    }

    /// Does an unsettled reservation anywhere in the family contain a matching emit? A root
    /// opened as a fork also holds the ones its origin carried, which nothing in the family can
    /// settle. `no_prior(k)` additionally fails on this; `prior(k)` never reads it — both
    /// directions fail closed.
    pub(crate) fn has_reservation(&self, kind: &EffectKind) -> bool {
        self.projection.origin_reservations.contains(kind)
            || self
                .projection
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

    pub(crate) fn is_open(&self, dispatch: &DispatchId) -> bool {
        self.projection.open.contains(dispatch)
    }

    pub(crate) fn closed_successfully(&self, dispatch: &DispatchId) -> bool {
        matches!(self.projection.closed.get(dispatch), Some(CloseKind::Success))
    }

    pub(crate) fn dispatch_failed(&self, dispatch: &DispatchId) -> bool {
        matches!(
            self.projection.closed.get(dispatch),
            Some(CloseKind::Failure | CloseKind::FailureWithBody(_))
        )
    }

    pub(crate) fn failed_with_body(&self, dispatch: &DispatchId) -> bool {
        matches!(
            self.projection.closed.get(dispatch),
            Some(CloseKind::FailureWithBody(_))
        )
    }

    pub(crate) fn failure_body_digest(&self, dispatch: &DispatchId) -> Option<RawResultDigest> {
        match self.projection.closed.get(dispatch) {
            Some(CloseKind::FailureWithBody(digest)) => Some(*digest),
            _ => None,
        }
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

    /// The terms this dispatch's release pinned and the effects it reserved; `None` for a
    /// dispatch the family never opened.
    pub(crate) fn release_terms(&self, dispatch: &DispatchId) -> Option<&(crate::fact::ReleaseTerms, EffectSet)> {
        self.projection.release_terms.get(dispatch)
    }

    pub(crate) fn withholds_output(&self, dispatch: &DispatchId) -> bool {
        self.projection.withheld_outputs.contains(dispatch)
    }

    /// The live derived candidate of this subject, if a hop has produced one. The next
    /// stage plans from it, and a successor replaces it.
    pub(crate) fn candidate(&self, subject: &SubjectKey) -> Option<&DerivedCandidate> {
        self.recorded_candidate(subject).map(|held| &held.derived)
    }

    pub(crate) fn recorded_candidate(&self, subject: &SubjectKey) -> Option<&RecordedCandidate> {
        self.projection.candidates.get(subject)
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
    pub(crate) fn call_role(&self, subject: &SubjectKey) -> crate::check::CallRole {
        match subject {
            SubjectKey::Call { batch, position, .. } => match self.decided_batch(batch) {
                Some(decided) => crate::check::CallRole::of(decided.spawn, *position as usize),
                None => crate::check::CallRole::Ordinary,
            },
            SubjectKey::Approval(_) | SubjectKey::ConfinedResult(_) | SubjectKey::Return(_) => {
                crate::check::CallRole::Ordinary
            }
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
                forwards: crate::contract::Delta::NONE,
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
                return_policy: ReturnPolicy::Crossing(crate::fact::CrossingReturn {
                    floor: Label::top(),
                    sanitizer: None,
                }),
                kind: crate::transition::SpawnKind::Single,
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
                interceptions: None,
                trajectory: traj("a"),
                dispatch: dispatch("a"),
                tool: ToolName::new("tool"),
                arguments: crate::params::test_arguments(&json!({ "t": "a" })),
                proposed_label: Label::top(),
                receiving: Label::top(),
                proposed_effects: EffectSet::new([egress.clone()]).unwrap(),
                annotation: None,
                file_basis: None,
                subject: crate::basis::fixture_subject(&traj("a")),
                evidence: crate::audience::AudienceEvidence::default(),
                terms: crate::fact::ReleaseTerms::none(),
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
                interceptions: None,
                trajectory: traj("a"),
                dispatch: dispatch("a"),
                tool: ToolName::new("tool"),
                arguments: crate::params::test_arguments(&json!({ "t": "a" })),
                proposed_label: Label::top(),
                receiving: Label::top(),
                proposed_effects: EffectSet::new([egress.clone()]).unwrap(),
                annotation: None,
                file_basis: None,
                subject: crate::basis::fixture_subject(&traj("a")),
                evidence: crate::audience::AudienceEvidence::default(),
                terms: crate::fact::ReleaseTerms::none(),
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
                interceptions: None,
                trajectory: traj("a"),
                dispatch: dispatch("a"),
                tool: ToolName::new("tool"),
                arguments: crate::params::test_arguments(&json!({ "t": "a" })),
                proposed_label: Label::top(),
                receiving: Label::top(),
                proposed_effects: EffectSet::new([egress.clone()]).unwrap(),
                annotation: None,
                file_basis: None,
                subject: crate::basis::fixture_subject(&traj("a")),
                evidence: crate::audience::AudienceEvidence::default(),
                terms: crate::fact::ReleaseTerms::none(),
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
                interceptions: None,
                trajectory: traj("a"),
                dispatch: dispatch("a"),
                tool: ToolName::new("fetch_meeting"),
                arguments: crate::params::test_arguments(&json!({ "t": "a" })),
                proposed_label: Label::top(),
                receiving: Label::top(),
                proposed_effects: EffectSet::new([]).unwrap(),
                annotation: None,
                file_basis: None,
                subject: crate::basis::fixture_subject(&traj("a")),
                evidence: crate::audience::AudienceEvidence::default(),
                terms: crate::fact::ReleaseTerms::none(),
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

    /// Audiences whose meets exercise every canonicalization rule: stable readers merging and
    /// emptying to nobody, a chain clause a narrower clause drops, a group clause the bare
    /// `self` clause subsumes, and a clause holding a provider-qualified reader that never merges.
    fn palette(index: usize) -> Audience {
        use crate::label::{ChainAudience, Clause, GroupRef};
        let clause = |chain: Option<ChainAudience>, groups: &[&str], readers: &[&str]| {
            Clause::new(
                chain,
                groups
                    .iter()
                    .map(|group| GroupRef::Named(crate::names::GroupName::new(*group))),
                readers.iter().map(|reader| ReaderId::new(*reader)),
            )
            .expect("palette readers are literal")
        };
        match index % 8 {
            0 => Audience::public(),
            1 => Audience::restricted([ReaderId::new("emp"), ReaderId::new("boss")]),
            2 => Audience::restricted([ReaderId::new("emp")]),
            3 => Audience::restricted([ReaderId::new("other")]),
            4 => Audience::of_clauses([clause(Some(ChainAudience::Internal), &[], &["emp"])]),
            5 => Audience::of_clauses([clause(Some(ChainAudience::Self_), &["finance"], &[])]),
            6 => Audience::of_clauses([clause(None, &[], &["slack:U1", "emp"])]),
            _ => Audience::of_clauses([clause(Some(ChainAudience::Self_), &[], &[])]),
        }
    }

    /// Fold `log` record by record and, after each, hold the cached fold of every opened
    /// trajectory to the walk over its sources.
    fn assert_cached_fold_matches_walk(log: &[Fact]) {
        let mut projection = Projection::empty(0);
        for (position, fact) in log.iter().enumerate() {
            projection.fold(fact);
            for trajectory in [traj("a"), traj("b")] {
                if projection.is_opened(&trajectory) {
                    assert_eq!(
                        projection.fold_for(&trajectory),
                        projection.fold_by_walk(&trajectory),
                        "{trajectory:?} after record {position}"
                    );
                }
            }
        }
    }

    /// A root and a child, each admitting labels that merge, collapse to nobody and drop
    /// clauses; the child resumed with a seed, and inheriting a source that dangles until the log
    /// grows past it.
    #[test]
    fn the_cached_label_fold_equals_the_walk_over_every_source() {
        let first = labeled(3, palette(1));
        let ghost = labeled(2, palette(5));
        let mut log = vec![
            opened("a"),
            admit("a", first.clone()),
            admit("a", labeled(2, palette(4))),
        ];
        log.extend(fork_pair(
            "a",
            "b",
            ForkSnapshot::freeze(
                base(),
                [(ValueId::new(0), &first.label), (ValueId::new(9), &ghost.label)],
            ),
        ));
        log.extend([
            admit("b", labeled(3, palette(2))),
            admit("b", labeled(1, palette(6))),
            Fact::Boundary {
                trajectory: traj("b"),
                kind: BoundaryKind::Resume {
                    seed: Label::new(Trust::new(2), palette(5)),
                },
            },
            admit("a", labeled(3, palette(3))),
            admit("b", labeled(2, palette(1))),
            Fact::Boundary {
                trajectory: traj("b"),
                kind: BoundaryKind::Resume {
                    seed: Label::new(Trust::new(3), palette(0)),
                },
            },
        ]);
        log.extend((0..6).map(|index| admit("a", labeled(3, palette(index)))));
        assert_cached_fold_matches_walk(&log);
    }

    fn own_self() -> Audience {
        use crate::label::{ChainAudience, Clause};
        Audience::of_clauses([Clause::new([ChainAudience::Self_], [], []).unwrap()])
    }

    /// `self ∪ @group-<group>`: broader than `self`, and incomparable with every other group.
    fn self_or_group(group: usize) -> Audience {
        use crate::label::{ChainAudience, Clause, GroupRef};
        Audience::of_clauses([Clause::new(
            [ChainAudience::Self_],
            [GroupRef::Named(crate::names::GroupName::new(format!("group-{group}")))],
            [],
        )
        .unwrap()])
    }

    /// Admit 64 audiences broader than `self` on each of `trajectories` after `log`, hold the
    /// cache to the walk after every record, and hold each cache to the one `self` clause the
    /// narrow term of its fold keeps it at.
    fn assert_broad_admissions_stay_one_clause(mut log: Vec<Fact>, trajectories: &[&str]) {
        for group in 0..64 {
            log.extend(trajectories.iter().map(|t| admit(t, labeled(3, self_or_group(group)))));
        }
        assert_cached_fold_matches_walk(&log);
        let projection = build(&log);
        for trajectory in trajectories {
            let cached = &projection.local_label[&traj(trajectory)];
            assert_eq!(cached, &Label::new(Trust::new(3), own_self()));
            assert_eq!(cached.audience.clauses().count(), 1);
        }
    }

    /// A trajectory opened at `self` that admits many incomparable broader audiences: the base
    /// subsumes every one of them, so the cached fold stays the one `self` clause instead of
    /// growing a clause per admission.
    #[test]
    fn a_narrow_base_keeps_the_cached_fold_from_accumulating_subsumed_clauses() {
        let narrow = Label::new(Trust::new(3), own_self());
        let mut log = vec![crate::profile::opening_at(traj("a"), narrow.clone())];
        log.extend(fork_pair("a", "b", ForkSnapshot::freeze(narrow, [])));
        assert_broad_admissions_stay_one_clause(log, &["a", "b"]);
    }

    /// A child forked at a public base that inherits a `self` source: the inherited source
    /// narrows its fold, and so its cache, from the fork on.
    #[test]
    fn a_narrow_inherited_source_keeps_the_cached_fold_from_accumulating_subsumed_clauses() {
        let narrow = labeled(3, own_self());
        let mut log = vec![opened("a"), admit("a", narrow.clone())];
        log.extend(fork_pair(
            "a",
            "b",
            ForkSnapshot::freeze(base(), [(ValueId::new(0), &narrow.label)]),
        ));
        assert_broad_admissions_stay_one_clause(log, &["b"]);
    }

    /// A child forked at a public base and resumed with a `self` seed: the seed narrows its
    /// fold, and so its cache, until the next resume replaces it.
    #[test]
    fn a_narrow_resumed_seed_keeps_the_cached_fold_from_accumulating_subsumed_clauses() {
        let mut log = vec![opened("a")];
        log.extend(fork_pair("a", "b", ForkSnapshot::freeze(base(), [])));
        log.push(Fact::Boundary {
            trajectory: traj("b"),
            kind: BoundaryKind::Resume {
                seed: Label::new(Trust::new(3), own_self()),
            },
        });
        assert_broad_admissions_stay_one_clause(log, &["b"]);
    }

    proptest::proptest! {
        /// Root admissions before the fork let inherited ids resolve at the fork, later, or
        /// never; resumes replace the seed with one that may be broader than the last.
        #[test]
        fn the_cached_label_fold_equals_the_walk_on_generated_logs(
            // A root's opening is a policy profile, which cannot name an undeclared group.
            root_base in proptest::sample::select(vec![0usize, 1, 2, 3, 4, 6, 7]),
            child_base in 0usize..8,
            before_fork in proptest::collection::vec((0u8..4, 0usize..8), 0..6),
            inherited in proptest::collection::btree_set(0u64..10, 0..4),
            records in proptest::collection::vec((0u8..3, 0u8..4, 0usize..8), 0..40),
        ) {
            let generated = |trust: u8, audience: usize| Label::new(Trust::new(trust), palette(audience));
            let mut log = vec![crate::profile::opening_at(traj("a"), generated(3, root_base))];
            log.extend(
                before_fork
                    .into_iter()
                    .map(|(trust, audience)| admit("a", LabeledValue::new(ValueBody::new("body"), generated(trust, audience)))),
            );
            let frozen = Label::top();
            log.extend(fork_pair(
                "a",
                "b",
                ForkSnapshot::freeze(generated(3, child_base), inherited.into_iter().map(|id| (ValueId::new(id), &frozen))),
            ));
            log.extend(records.into_iter().map(|(kind, trust, audience)| {
                let label = generated(trust, audience);
                match kind {
                    0 => admit("a", LabeledValue::new(ValueBody::new("body"), label)),
                    1 => admit("b", LabeledValue::new(ValueBody::new("body"), label)),
                    _ => Fact::Boundary {
                        trajectory: traj("b"),
                        kind: BoundaryKind::Resume { seed: label },
                    },
                }
            }));
            assert_cached_fold_matches_walk(&log);
        }
    }
}
