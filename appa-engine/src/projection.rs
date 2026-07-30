//! The one build path: derive every read model from the log by full reprojection.

use std::collections::BTreeSet;

use crate::fact::{BoundaryKind, CloseOutcome, EffectKind, Fact, ReturnPolicy, Revision};
use crate::label::{Dim, DimValue, Label};
use crate::value::{CanonicalDigest, ChildReturnId, DispatchId, LabeledValue, Provenance, TrajectoryId, ValueId};

#[derive(Clone, Debug, PartialEq, Eq)]
struct AdmittedValue {
    trajectory: TrajectoryId,
    label: Label,
    provenance: Provenance,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OpenedDispatch {
    trajectory: TrajectoryId,
    digest: CanonicalDigest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Fork {
    child: TrajectoryId,
    parent: TrajectoryId,
    seed: Label,
    return_policy: ReturnPolicy,
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
    effects: Vec<EffectKind>,
    open: BTreeSet<DispatchId>,
    succeeded: BTreeSet<DispatchId>,
    opened: Vec<OpenedDispatch>,
    boundaries: Vec<TrajectoryId>,
    forks: Vec<Fork>,
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
                Fact::Ruling { .. } | Fact::Acceptance { .. } | Fact::ChildReturnAcceptance { .. } => {}
                Fact::AssistantMessage { .. } | Fact::BlockFeedback { .. } => {}
                Fact::OutputCastApplied { .. } | Fact::OutputCastAccepted { .. } | Fact::OutputCastLapsed { .. } => {}
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

    pub fn value_label(&self, id: ValueId) -> Option<&Label> {
        usize::try_from(id.index())
            .ok()
            .and_then(|i| self.values.get(i))
            .map(|v| &v.label)
    }

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

    pub fn has_effect(&self, kind: &EffectKind) -> bool {
        self.projection.effects.iter().any(|e| e == kind)
    }

    pub fn present_effects(&self) -> BTreeSet<EffectKind> {
        self.projection.effects.iter().cloned().collect()
    }

    pub fn is_open(&self, dispatch: &DispatchId) -> bool {
        self.projection.open.contains(dispatch)
    }

    /// Has this still-open dispatch's success checkpoint already committed its effects? Gates the
    /// close (success-family only, no duplicate effects) and the runtime's once-only checkpoint.
    pub fn is_succeeded(&self, dispatch: &DispatchId) -> bool {
        self.projection.succeeded.contains(dispatch)
    }

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
        let a = p.view(&traj("a")).current_label();
        assert_eq!(a.trust, Dim::Known(Trust::new(1)));
        assert_eq!(a.audience, Dim::Known(internal));
        let b = p.view(&traj("b")).current_label();
        assert_eq!(b.trust, Dim::Known(Trust::new(3)));
        assert_eq!(b.audience, Dim::Known(Audience::Public));
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
        assert_eq!(build(&log), build(&log));
        assert_eq!(build(&log).view(&traj("a")).boundary_count(), 1);
    }

    #[test]
    fn transcript_facts_are_inert_in_the_fold_and_effects() {
        use crate::fact::ProposedCall;
        use crate::value::{ToolCallId, ToolName};

        let egress = EffectKind::new("egress");
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
