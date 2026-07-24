//! The one build path: derive every read model from the log by full reprojection.
//!
//! A [`Projection`] is rebuilt from the whole family log after each mutation — never folded
//! incrementally (that is the design this deletes: a second fold to police). Its cost is O(facts),
//! and it is the single source of truth for the views the engine's check consumes: the
//! **branch-local** label fold (over `ValueAdmitted` of one trajectory) and the **family-wide**
//! effect/history and open-dispatch views (shared across the family in realtime).
//!
//! [`Views`] scopes a projection to one trajectory — the label fold is that branch's, the effect
//! and dispatch views are the family's.

use std::collections::BTreeSet;

use crate::fact::{BoundaryKind, CloseOutcome, EffectKind, Fact, ReturnPolicy, Revision};
use crate::label::{Dim, DimValue, Label};
use crate::value::{CanonicalDigest, ChildReturnId, DispatchId, LabeledValue, Provenance, TrajectoryId, ValueId};

/// One admitted value as the fold and the Authority review need it: which branch it belongs to,
/// its own label, and where it came from (the provenance an Authority reviews for a referenced
/// argument — the fold never reads it).
#[derive(Clone, Debug, PartialEq, Eq)]
struct AdmittedValue {
    trajectory: TrajectoryId,
    label: Label,
    provenance: Provenance,
}

/// One opened dispatch's identity, for occurrence counting.
#[derive(Clone, Debug, PartialEq, Eq)]
struct OpenedDispatch {
    trajectory: TrajectoryId,
    digest: CanonicalDigest,
}

/// One fork's immutable structure: the child, its parent, and the label the child was seeded at.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Fork {
    child: TrajectoryId,
    parent: TrajectoryId,
    seed: Label,
    return_policy: ReturnPolicy,
}

/// One value a child returned through `submit_result`, awaiting (or having undergone) a merge.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ReturnedChild {
    id: ChildReturnId,
    value: LabeledValue,
}

/// All read models derived from the family log in one pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Projection {
    revision: Revision,
    /// Indexed by [`ValueId`]: admitted values in log order.
    values: Vec<AdmittedValue>,
    /// Family-wide committed effects in log order; checks consume only kind-containment.
    effects: Vec<EffectKind>,
    /// Family-wide dispatches currently open (opened, not yet closed).
    open: BTreeSet<DispatchId>,
    /// Still-open dispatches whose success checkpoint already committed their effects (a
    /// pending-cast offer): the eventual close must be success-family and contributes none.
    succeeded: BTreeSet<DispatchId>,
    /// Every dispatch ever opened, for per-digest occurrence counting.
    opened: Vec<OpenedDispatch>,
    /// Boundaries per trajectory, in log order (punctuation, counted for audit views).
    boundaries: Vec<TrajectoryId>,
    /// Fork structure: each child's immutable parent binding and seed label.
    forks: Vec<Fork>,
    /// Values children have returned, keyed by their return id. A child's crossing lands its
    /// `Merge` boundary in the same batch, so one record here means the child has returned
    /// (the at-most-once guard reads this).
    child_returns: Vec<ReturnedChild>,
}

impl Projection {
    /// Build every view from the family log. `revision` is the log's current version (the runtime's
    /// batch count); the projection is a pure function of `(log, revision)`.
    pub fn build(log: &[Fact], revision: Revision) -> Self {
        let mut values = Vec::new();
        let mut effects = Vec::new();
        let mut open = BTreeSet::new();
        let mut succeeded = BTreeSet::new();
        let mut opened = Vec::new();
        let mut boundaries = Vec::new();
        let mut forks = Vec::new();
        let mut child_returns = Vec::new();

        for fact in log {
            match fact {
                Fact::ValueAdmitted {
                    trajectory,
                    value,
                    provenance,
                } => values.push(AdmittedValue {
                    trajectory: trajectory.clone(),
                    label: value.label.clone(),
                    provenance: provenance.clone(),
                }),
                Fact::DispatchOpened {
                    trajectory, dispatch, ..
                } => {
                    open.insert(dispatch.clone());
                    opened.push(OpenedDispatch {
                        trajectory: trajectory.clone(),
                        digest: *dispatch.digest(),
                    });
                }
                // The success checkpoint commits effects while the dispatch stays open for value
                // finalization — the one append point at success, moved to when success is
                // observed. The eventual close carries none (enforced at admission).
                Fact::DispatchSucceeded {
                    dispatch,
                    effects: committed,
                    ..
                } => {
                    succeeded.insert(dispatch.clone());
                    effects.extend(committed.iter().cloned());
                }
                Fact::DispatchClosed { dispatch, outcome, .. } => {
                    open.remove(dispatch);
                    succeeded.remove(dispatch);
                    if let CloseOutcome::Success { effects: committed } = outcome {
                        effects.extend(committed.iter().cloned());
                    }
                }
                // A cast overrides its value's Unknown dimension in the fold; the body is untouched.
                Fact::CastApplied { value, resolved, .. } => {
                    if let Some(v) = usize::try_from(value.index()).ok().and_then(|i| values.get_mut(i)) {
                        match resolved {
                            DimValue::Trust(t) => v.label.trust = Dim::Known(*t),
                            DimValue::Audience(a) => v.label.audience = Dim::Known(a.clone()),
                        }
                    }
                }
                // Rulings and acceptances are audit only — a ruling never edits the label, and a
                // narrowing's fold happens through the admitted value, not the acceptance record.
                Fact::Ruling { .. } | Fact::Acceptance { .. } | Fact::ChildReturnAcceptance { .. } => {}
                // Transcript memory (CC2/RP1): inert in the algebra — the runtime's transcript builder
                // reads these; the fold and effect views never do.
                Fact::AssistantMessage { .. } | Fact::BlockFeedback { .. } => {}
                // Transformer applications are audit only — the labels they establish ride the
                // ValueAdmitted appended beside them, so the fold reads nothing here.
                Fact::SanitizerApplied { .. }
                | Fact::OutputCastApplied { .. }
                | Fact::OutputCastAccepted { .. }
                | Fact::OutputCastLapsed { .. } => {}
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
                            seed,
                            return_policy,
                        } => forks.push(Fork {
                            child: trajectory.clone(),
                            parent: parent.clone(),
                            seed: seed.clone(),
                            return_policy: return_policy.clone(),
                        }),
                        // The merge is audit punctuation here: the crossing's ChildReturn record
                        // (same batch) is what the read models key on.
                        BoundaryKind::Merge { .. } => {}
                    }
                }
            }
        }

        Projection {
            revision,
            values,
            effects,
            open,
            succeeded,
            opened,
            boundaries,
            forks,
            child_returns,
        }
    }

    pub fn revision(&self) -> Revision {
        self.revision
    }

    /// The label of an admitted value, or `None` if the id is out of range.
    pub fn value_label(&self, id: ValueId) -> Option<&Label> {
        usize::try_from(id.index())
            .ok()
            .and_then(|i| self.values.get(i))
            .map(|v| &v.label)
    }

    /// The branch-local restrictive fold for `trajectory`: start from its fork seed (the parent's
    /// current label at the fork) if it is a child, else [`Label::top`], then fold the branch's own
    /// admitted values. A sibling's value never lowers this — the fold is ancestry-local.
    fn fold_for(&self, trajectory: &TrajectoryId) -> Label {
        let seed = self
            .forks
            .iter()
            .find(|fork| &fork.child == trajectory)
            .map(|fork| fork.seed.clone())
            .unwrap_or_else(Label::top);
        self.values
            .iter()
            .filter(|value| &value.trajectory == trajectory)
            .fold(seed, |acc, value| acc.combine(&value.label))
    }

    /// Scope the projection to one trajectory.
    pub fn view<'a>(&'a self, trajectory: &'a TrajectoryId) -> Views<'a> {
        Views {
            projection: self,
            trajectory,
        }
    }
}

/// A projection scoped to one trajectory: branch-local label fold, family-wide effects/dispatches.
pub struct Views<'a> {
    projection: &'a Projection,
    trajectory: &'a TrajectoryId,
}

impl Views<'_> {
    pub fn revision(&self) -> Revision {
        self.projection.revision
    }

    pub fn trajectory(&self) -> &TrajectoryId {
        self.trajectory
    }

    /// The (cast-resolved) label of an admitted value by id.
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

    /// Does this value belong to the scoped trajectory? A cast may only resolve its own branch's
    /// values, never a sibling's.
    pub fn owns_value(&self, id: ValueId) -> bool {
        usize::try_from(id.index())
            .ok()
            .and_then(|i| self.projection.values.get(i))
            .is_some_and(|value| &value.trajectory == self.trajectory)
    }

    /// The branch's current label: the restrictive fold of every value admitted to this trajectory,
    /// seeded from its fork (a child begins at the parent's current label, never at `top()`).
    /// Branch-local — a value in a sibling branch does not lower this fold.
    pub fn current_label(&self) -> Label {
        self.projection.fold_for(self.trajectory)
    }

    /// The branch-local fold of an arbitrary trajectory in the family — used to validate that a
    /// child's returned value does not raise trust above what the child legitimately holds.
    pub fn branch_label(&self, trajectory: &TrajectoryId) -> Label {
        self.projection.fold_for(trajectory)
    }

    /// The parent this trajectory was forked from, if it is a child (its immutable fork binding).
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

    /// The child and value a return id names, if it exists in the family log.
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

    /// The values admitted to this branch, with their ids and labels — for finding the Unknown
    /// dimensions a cast must resolve.
    pub fn branch_values(&self) -> impl Iterator<Item = (ValueId, &Label)> {
        self.branch_values_of(self.trajectory)
    }

    /// The values admitted to an arbitrary family trajectory — the return check names a child's
    /// (or the parent's own) unresolved values from this one snapshot.
    pub(crate) fn branch_values_of<'a>(
        &'a self,
        trajectory: &'a TrajectoryId,
    ) -> impl Iterator<Item = (ValueId, &'a Label)> {
        self.projection
            .values
            .iter()
            .enumerate()
            .filter(move |(_, v)| &v.trajectory == trajectory)
            .map(|(i, v)| (ValueId::new(i as u64), &v.label))
    }

    /// How many dispatches of this digest this branch has already opened — the occurrence of the
    /// next one (a repeat identical call is a new dispatch, not a re-issue).
    pub fn dispatch_count(&self, digest: &CanonicalDigest) -> u32 {
        self.projection
            .opened
            .iter()
            .filter(|d| &d.trajectory == self.trajectory && &d.digest == digest)
            .count() as u32
    }

    /// Does a matching effect exist anywhere in the family? `prior(k)` reads this.
    pub fn has_effect(&self, kind: &EffectKind) -> bool {
        self.projection.effects.iter().any(|e| e == kind)
    }

    /// The set of effect kinds the family has committed — the history half of a remedy-planning state.
    pub fn present_effects(&self) -> BTreeSet<EffectKind> {
        self.projection.effects.iter().cloned().collect()
    }

    /// Is this dispatch currently open (opened, not yet closed) anywhere in the family?
    pub fn is_open(&self, dispatch: &DispatchId) -> bool {
        self.projection.open.contains(dispatch)
    }

    /// Has this still-open dispatch's success checkpoint already committed its effects? Gates the
    /// close (success-family only, no duplicate effects) and the runtime's once-only checkpoint.
    pub fn is_succeeded(&self, dispatch: &DispatchId) -> bool {
        self.projection.succeeded.contains(dispatch)
    }

    /// How many boundaries this trajectory has recorded.
    pub fn boundary_count(&self) -> usize {
        self.projection
            .boundaries
            .iter()
            .filter(|t| *t == self.trajectory)
            .count()
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

    fn dispatch(t: &str) -> DispatchId {
        let call = ResolvedCall::new(ToolName::new("tool"), json!({ "t": t }), vec![]);
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
        // Branch a folds only a's value: suspicious + internal.
        let a = p.view(&traj("a")).current_label();
        assert_eq!(a.trust, Dim::Known(Trust::new(1)));
        assert_eq!(a.audience, Dim::Known(internal));
        // Branch b is unaffected by a: trusted + public.
        let b = p.view(&traj("b")).current_label();
        assert_eq!(b.trust, Dim::Known(Trust::new(3)));
        assert_eq!(b.audience, Dim::Known(Audience::Public));
        // An untouched branch is top().
        assert_eq!(p.view(&traj("c")).current_label(), Label::top());
    }

    #[test]
    fn effects_are_family_wide_and_commit_only_on_success() {
        let egress = EffectKind::new("egress");
        let log = vec![
            Fact::DispatchOpened {
                trajectory: traj("a"),
                dispatch: dispatch("a"),
                proposed_label: Label::top(),
                proposed_effects: vec![egress.clone()],
            },
            Fact::DispatchClosed {
                trajectory: traj("a"),
                dispatch: dispatch("a"),
                outcome: CloseOutcome::Success {
                    effects: vec![egress.clone()],
                },
            },
        ];
        let p = build(&log);
        // A committed effect in branch a is visible family-wide (branch b sees it too).
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
                proposed_label: Label::top(),
                proposed_effects: vec![egress.clone()],
            },
            Fact::DispatchClosed {
                trajectory: traj("a"),
                dispatch: dispatch("a"),
                outcome: CloseOutcome::Failure,
            },
        ];
        let p = build(&log);
        assert!(!p.view(&traj("a")).has_effect(&egress));
        assert!(!p.view(&traj("a")).is_open(&dispatch("a")));
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
        // Building twice from the same log yields identical views (replayable, no hidden state).
        assert_eq!(build(&log), build(&log));
        assert_eq!(build(&log).view(&traj("a")).boundary_count(), 1);
    }

    #[test]
    fn transcript_facts_are_inert_in_the_fold_and_effects() {
        use crate::fact::ProposedCall;
        use crate::value::{ToolCallId, ToolName};

        let egress = EffectKind::new("egress");
        // A branch with one admitted value, then transcript memory interleaved with it.
        let log = vec![
            admit("a", labeled(2, Audience::Public)),
            Fact::AssistantMessage {
                trajectory: traj("a"),
                content: None,
                calls: vec![ProposedCall {
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
        // The transcript facts move neither the branch fold nor the family effects.
        assert_eq!(
            with.view(&traj("a")).current_label(),
            without.view(&traj("a")).current_label()
        );
        assert!(!with.view(&traj("a")).has_effect(&egress));
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
