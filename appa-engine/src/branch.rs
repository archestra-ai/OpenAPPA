//! Branching label semantics over one shared family log.

use thiserror::Error;

use crate::audience::AudienceEvidence;
use crate::fact::{BoundaryKind, Fact, ReturnDerivation};
use crate::projection::Views;
use crate::value::{ChildReturnId, LabeledValue, Provenance, TrajectoryId};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BranchError {
    #[error("the branch already ended its errand by its void terminal")]
    AlreadyEnded,
    #[error("the child was not forked from this parent (reparenting/cross-family merge refused)")]
    NotDirectParent,
    #[error("the trajectory has no fork binding — only a child may return")]
    NotForked,
}

/// Record a child's **void return**: the child-attributed terminal ends the branch and
/// crosses no value — no merge, no label contribution. Refused for a non-child and for a branch
/// that already ended. The batch is on the family's revision, so competing terminals linearize
/// at the store's revisioned append and at most one lands.
pub(crate) fn submit_void_return(parent: &Views, child: &TrajectoryId) -> Result<Vec<Fact>, BranchError> {
    match parent.parent_of(child) {
        Some(direct) if direct == parent.trajectory() => {}
        _ => return Err(BranchError::NotDirectParent),
    }
    if parent.has_ended(child) {
        return Err(BranchError::AlreadyEnded);
    }
    Ok(vec![Fact::Boundary {
        trajectory: child.clone(),
        kind: BoundaryKind::VoidReturn,
    }])
}

/// The one place a return's facts are assembled: the child's `ChildReturn` record, the parent's
/// `ValueAdmitted` under the returned value's own label, and the `Merge` boundary — always one
/// batch, never split across commit points. The parent *fold* absorbs the crossing at projection
/// (intersect readers, min trust) — identical to folding `parent.combine(returned)`, since
/// `combine` is idempotent — while the stored per-value label stays the value's intrinsic one, so
/// authority review context and cast targeting see what the value *is*, not the parent's
/// unrelated restrictions.
pub(crate) fn crossing_facts(
    parent: &Views,
    child: &TrajectoryId,
    value: LabeledValue,
    derivation: ReturnDerivation,
    evidence: AudienceEvidence,
) -> Vec<Fact> {
    let id = ChildReturnId::new(child.clone(), parent.returns_by(child));
    vec![
        Fact::ChildReturn {
            trajectory: child.clone(),
            id: id.clone(),
            value: value.clone(),
            derivation,
            evidence,
        },
        Fact::ValueAdmitted {
            trajectory: parent.trajectory().clone(),
            value,
            provenance: Provenance::ChildReturn {
                child: child.clone(),
                id: id.clone(),
            },
        },
        Fact::Boundary {
            trajectory: parent.trajectory().clone(),
            kind: BoundaryKind::Merge { child_return: id },
        },
    ]
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    use crate::fact::{CloseOutcome, EffectKind, EffectSet, ForkSnapshot, ReturnPolicy};

    use crate::label::{Audience, Label, ReaderId, Trust};
    use crate::projection::Projection;

    use crate::value::{DispatchId, LabeledValue, Provenance, ResolvedCall, ToolName, ValueBody, ValueId};
    use serde_json::json;

    const SUSPICIOUS: Trust = Trust::new(0);
    const TRUSTED: Trust = Trust::new(1);

    fn parent() -> TrajectoryId {
        TrajectoryId::new("parent")
    }

    fn child() -> TrajectoryId {
        TrajectoryId::new("child")
    }

    fn known(trust: Trust, audience: Audience) -> Label {
        Label::new(trust, audience)
    }

    fn established(trust: Trust, audience: Audience) -> Label {
        Label::new(trust, audience)
    }

    fn internal() -> Audience {
        Audience::restricted([ReaderId::new("insider")])
    }

    fn opened(trajectory: TrajectoryId, label: Label) -> Fact {
        crate::profile::opening_at(trajectory, label)
    }

    fn admit(log: &mut Vec<Fact>, trajectory: TrajectoryId, label: Label) {
        let call = ResolvedCall::new(ToolName::new("read"), crate::params::test_arguments(&json!({})));
        let occurrence = log
            .iter()
            .filter(|fact| {
                matches!(
                    fact,
                    Fact::DispatchOpened { trajectory: on, tool, .. } if on == &trajectory && tool == call.tool()
                )
            })
            .count() as u32;
        let dispatch = DispatchId::new(trajectory.clone(), call.digest(), occurrence);
        log.push(Fact::DispatchOpened {
            trajectory: trajectory.clone(),
            dispatch: dispatch.clone(),
            tool: call.tool().clone(),
            declaration: call.declaration_id(),
            arguments: call.canonical_arguments().clone(),
            proposed_label: Label::top(),
            receiving: Label::top(),
            proposed_effects: EffectSet::default(),
            annotation: None,
            subject: crate::basis::fixture_subject(&trajectory),
            evidence: crate::audience::AudienceEvidence::default(),
        });
        log.push(Fact::DispatchClosed {
            trajectory: trajectory.clone(),
            dispatch: dispatch.clone(),
            outcome: CloseOutcome::Success {
                effects: EffectSet::default(),
            },
        });
        log.push(Fact::ValueAdmitted {
            trajectory,
            value: LabeledValue::new(ValueBody::new("body"), label),
            provenance: Provenance::ToolResult { dispatch },
        });
    }

    fn build(log: &[Fact]) -> Projection {
        Projection::build(log, log.len() as u64)
    }

    fn fork_records(log: &[Fact], parent: &TrajectoryId, child: &TrajectoryId) -> Vec<Fact> {
        let projection = build(log);
        let call = ResolvedCall::new(
            ToolName::new("fork"),
            crate::params::test_arguments(&json!({ "child": child.as_str() })),
        );
        let dispatch = DispatchId::new(parent.clone(), call.digest(), 0);
        let fork = crate::value::ForkId::of(&dispatch);
        vec![
            Fact::ForkPrepared {
                trajectory: parent.clone(),
                fork: fork.clone(),
                snapshot: projection.view(parent).freeze_basis(),
                return_policy: ReturnPolicy {
                    floor: Label::bottom(),
                    sanitizer: None,
                },
            },
            Fact::ForkOpened {
                trajectory: child.clone(),
                fork,
            },
        ]
    }

    fn forked(parent_label: Label) -> Vec<Fact> {
        let mut log = vec![opened(parent(), parent_label)];
        let seed = fork_records(&log, &parent(), &child());
        log.extend(seed);
        log
    }

    fn raw(body: &str) -> ValueBody {
        ValueBody::new(body)
    }

    #[test]
    fn fork_seeds_child_at_parent_current_label() {
        let log = forked(known(SUSPICIOUS, internal()));
        let projection = build(&log);
        assert_eq!(
            projection.view(&child()).current_label(),
            established(SUSPICIOUS, internal())
        );
        assert_ne!(projection.view(&child()).current_label(), Label::top());
    }

    #[test]
    fn a_crossing_lands_in_one_batch_and_folds_into_the_parent() {
        let mut log = forked(known(TRUSTED, Audience::public()));
        admit(&mut log, child(), known(SUSPICIOUS, internal()));
        let projection = build(&log);
        let root = parent();
        let views = projection.view(&root);
        let fold = views.branch_label(&child());
        let ret = crossing_facts(
            &views,
            &child(),
            LabeledValue::new(raw("secret"), fold.clone()),
            ReturnDerivation::Raw,
            AudienceEvidence::default(),
        );
        assert!(matches!(&ret[0], Fact::ChildReturn { .. }));
        assert!(matches!(&ret[1], Fact::ValueAdmitted { .. }));
        assert!(matches!(
            &ret[2],
            Fact::Boundary {
                kind: BoundaryKind::Merge { .. },
                ..
            }
        ));
        log.extend(ret);
        let projection = build(&log);
        let root = parent();
        let views = projection.view(&root);
        assert_eq!(
            views
                .child_return(&ChildReturnId::new(child(), 0))
                .map(|value| &value.label),
            Some(&known(SUSPICIOUS, internal()))
        );
        assert_eq!(views.current_label(), established(SUSPICIOUS, internal()));
        // A crossing ends nothing: the child may return again, at the next occurrence.
        assert!(!views.has_ended(&child()));
        assert_eq!(views.returns_by(&child()), 1);
        assert_eq!(
            views.latest_return(&child()).map(|value| value.body.as_str()),
            Some("secret")
        );
    }

    #[test]
    fn a_void_return_ends_the_branch_and_contributes_nothing() {
        let mut log = forked(known(SUSPICIOUS, internal()));
        let projection = build(&log);
        assert_eq!(
            submit_void_return(&projection.view(&parent()), &TrajectoryId::new("stranger")),
            Err(BranchError::NotDirectParent)
        );
        let label_before = projection.view(&parent()).current_label();
        let batch = submit_void_return(&projection.view(&parent()), &child()).unwrap();
        assert_eq!(
            batch,
            [Fact::Boundary {
                trajectory: child(),
                kind: BoundaryKind::VoidReturn,
            }]
        );
        log.extend(batch);
        let projection = build(&log);
        assert!(projection.view(&parent()).has_ended(&child()));
        assert_eq!(projection.view(&parent()).returns_by(&child()), 0);
        assert_eq!(projection.view(&parent()).current_label(), label_before);

        assert_eq!(
            submit_void_return(&projection.view(&parent()), &child()),
            Err(BranchError::AlreadyEnded)
        );
    }

    #[test]
    fn a_resume_folds_the_parents_label_into_the_child() {
        let mut log = forked(known(TRUSTED, Audience::public()));
        admit(&mut log, parent(), known(SUSPICIOUS, internal()));
        let projection = build(&log);
        assert_eq!(
            projection.view(&child()).current_label(),
            established(TRUSTED, Audience::public())
        );
        log.push(Fact::Boundary {
            trajectory: child(),
            kind: BoundaryKind::Resume {
                seed: projection.view(&parent()).current_label(),
            },
        });
        let projection = build(&log);
        assert_eq!(
            projection.view(&child()).current_label(),
            established(SUSPICIOUS, internal())
        );
        assert!(!projection.view(&parent()).has_ended(&child()));
    }

    #[test]
    fn abandoned_child_egress_is_visible_to_the_parent() {
        let mut log = forked(known(TRUSTED, Audience::public()));
        let egress = EffectKind::new("egress");
        let call = ResolvedCall::new(ToolName::new("send"), crate::params::test_arguments(&json!({})));
        let dispatch = DispatchId::new(child(), call.digest(), 0);
        log.push(Fact::DispatchOpened {
            trajectory: child(),
            dispatch: dispatch.clone(),
            tool: call.tool().clone(),
            declaration: call.declaration_id(),
            arguments: call.canonical_arguments().clone(),
            proposed_label: Label::top(),
            receiving: Label::top(),
            proposed_effects: EffectSet::new([egress.clone()]).unwrap(),
            annotation: None,
            subject: crate::basis::fixture_subject(&child()),
            evidence: crate::audience::AudienceEvidence::default(),
        });
        log.push(Fact::DispatchClosed {
            trajectory: child(),
            dispatch,
            outcome: CloseOutcome::Success {
                effects: EffectSet::new([egress.clone()]).unwrap(),
            },
        });
        let projection = build(&log);
        assert!(projection.view(&parent()).has_effect(&egress));
    }

    fn narrowing_source(log: &mut Vec<Fact>, trajectory: TrajectoryId) {
        admit(log, trajectory, Label::new(SUSPICIOUS, Audience::public()));
    }

    fn opened_narrowed_parent() -> Vec<Fact> {
        let mut log = vec![opened(parent(), known(TRUSTED, Audience::public()))];
        narrowing_source(&mut log, parent());
        log
    }

    fn fork_under(log: &mut Vec<Fact>, parent: &TrajectoryId, child: &TrajectoryId) {
        let facts = fork_records(log, parent, child);
        log.extend(facts);
    }

    fn snapshot_of(log: &[Fact], child: &TrajectoryId) -> ForkSnapshot {
        let fork = log
            .iter()
            .find_map(|fact| match fact {
                Fact::ForkOpened { trajectory, fork } if trajectory == child => Some(fork.clone()),
                _ => None,
            })
            .expect("the child has a fork binding");
        log.iter()
            .find_map(|fact| match fact {
                Fact::ForkPrepared {
                    fork: prepared,
                    snapshot,
                    ..
                } if *prepared == fork => Some(snapshot.clone()),
                _ => None,
            })
            .expect("the fork was prepared with a snapshot")
    }

    #[test]
    fn a_fork_snapshot_carries_the_parents_sources_and_seed() {
        let mut log = opened_narrowed_parent();
        fork_under(&mut log, &parent(), &child());

        let projection = build(&log);
        let at_fork = projection.view(&parent()).current_label();
        assert_eq!(projection.view(&child()).current_label(), at_fork);
        assert!(!projection.view(&child()).current_label().meets_floor(TRUSTED));
        let snapshot = snapshot_of(&log, &child());
        assert_eq!(snapshot.inherited(), &BTreeSet::from([ValueId::new(0)]));
        assert_eq!(snapshot.seed(), &at_fork);
    }

    #[test]
    fn a_post_fork_source_stays_private_to_the_branch_that_admitted_it() {
        let sibling = TrajectoryId::new("sibling");
        let mut log = opened_narrowed_parent();
        fork_under(&mut log, &parent(), &child());
        fork_under(&mut log, &parent(), &sibling);
        admit(&mut log, parent(), Label::new(SUSPICIOUS, internal()));
        admit(
            &mut log,
            child(),
            Label::new(SUSPICIOUS, Audience::restricted([ReaderId::new("child")])),
        );

        let projection = build(&log);
        assert_eq!(
            projection.view(&parent()).current_label(),
            established(SUSPICIOUS, internal())
        );
        assert_eq!(
            projection.view(&child()).current_label(),
            established(SUSPICIOUS, Audience::restricted([ReaderId::new("child")]))
        );
        assert_eq!(
            projection.view(&sibling).current_label(),
            established(SUSPICIOUS, Audience::public())
        );
    }

    #[test]
    fn a_nested_fork_flattens_its_inherited_set() {
        let grandchild = TrajectoryId::new("grandchild");
        let mut log = opened_narrowed_parent();
        fork_under(&mut log, &parent(), &child());
        narrowing_source(&mut log, child());
        fork_under(&mut log, &child(), &grandchild);

        let snapshot = snapshot_of(&log, &grandchild);
        assert_eq!(
            snapshot.inherited(),
            &BTreeSet::from([ValueId::new(0), ValueId::new(1)])
        );
    }

    #[test]
    fn an_in_flight_child_dispatch_reserves_against_the_parent() {
        let mut log = forked(known(TRUSTED, Audience::public()));
        let egress = EffectKind::new("egress");
        let call = ResolvedCall::new(ToolName::new("send"), crate::params::test_arguments(&json!({})));
        let dispatch = DispatchId::new(child(), call.digest(), 0);
        log.push(Fact::DispatchOpened {
            trajectory: child(),
            dispatch: dispatch.clone(),
            tool: call.tool().clone(),
            declaration: call.declaration_id(),
            arguments: call.canonical_arguments().clone(),
            proposed_label: Label::top(),
            receiving: Label::top(),
            proposed_effects: EffectSet::new([egress.clone()]).unwrap(),
            annotation: None,
            subject: crate::basis::fixture_subject(&child()),
            evidence: crate::audience::AudienceEvidence::default(),
        });
        let projection = build(&log);
        let parent_id = parent();
        let parent_view = projection.view(&parent_id);
        assert!(!parent_view.has_effect(&egress));
        assert!(parent_view.has_reservation(&egress));
        log.push(Fact::DispatchClosed {
            trajectory: child(),
            dispatch,
            outcome: CloseOutcome::Success {
                effects: EffectSet::new([egress.clone()]).unwrap(),
            },
        });
        let projection = build(&log);
        let parent_view = projection.view(&parent_id);
        assert!(parent_view.has_effect(&egress));
        assert!(!parent_view.has_reservation(&egress));
    }
}
