//! Branching label semantics over one shared family log.

use thiserror::Error;

use crate::fact::{BoundaryKind, Fact, FactBatch};
use crate::label::{Adequacy, Dim, Label};
use crate::names::SanitizerName;
use crate::projection::Views;
use crate::registry::Registry;
use crate::value::{ChildReturnId, LabeledValue, Provenance, TrajectoryId, ValueBody};

#[derive(Clone)]
pub enum ChildReturn {
    Raw { body: ValueBody },
    Sanitized { body: ValueBody, sanitizer: SanitizerName },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BranchError {
    #[error("a trajectory cannot fork itself")]
    SelfFork,
    #[error("the child is already forked from a parent (reparenting refused)")]
    AlreadyForked,
    #[error("the parent's current label has an unresolved dimension — resolve it before forking")]
    ParentUnresolved,
    #[error("no child return registered for the given id")]
    UnknownChildReturn,
    #[error("this child return was already merged")]
    AlreadyMerged,
    #[error("the child was not forked from this parent (reparenting/cross-family merge refused)")]
    NotDirectParent,
    #[error("no sanitizer registered as {0}")]
    UnknownSanitizer(String),
    #[error("sanitizer {0} is not registered for output")]
    SanitizerNotOutput(String),
    #[error("the child fold does not satisfy the sanitizer's `from` precondition")]
    TransitionSourceUnmet,
}

/// Seed a child branch at the parent's current label with an immutable, unique `Fork` binding. Refuses
/// a self-fork, a re-fork of an already-bound child, and a fork at an unresolved parent label (a
/// child cannot inherit an Unknown it has no value to cast). The batch is on the family's revision.
pub(crate) fn seed_child(parent: &Views, child: &TrajectoryId) -> Result<FactBatch, BranchError> {
    if child == parent.trajectory() {
        return Err(BranchError::SelfFork);
    }
    if parent.parent_of(child).is_some() {
        return Err(BranchError::AlreadyForked);
    }
    let seed = parent.current_label();
    if matches!(seed.trust, Dim::Unknown) || matches!(seed.audience, Dim::Unknown) {
        return Err(BranchError::ParentUnresolved);
    }
    let fact = Fact::Boundary {
        trajectory: child.clone(),
        kind: BoundaryKind::Fork {
            parent: parent.trajectory().clone(),
            seed,
        },
    };
    Ok(FactBatch::new(parent.revision(), vec![fact]))
}

/// Record a child's returned value at an **engine-derived** label. A raw return carries the child
/// fold; a sanitized return preserves the fold's trust and relabels audience to the sanitizer's `to`
/// (only if the fold satisfies `from`). Trust can never rise on return — it is copied, not asserted.
pub(crate) fn submit_child_return(
    registry: &Registry,
    child: &Views,
    ret: ChildReturn,
) -> Result<FactBatch, BranchError> {
    let fold = child.current_label();
    let value = match ret {
        ChildReturn::Raw { body } => LabeledValue::new(body, fold),
        ChildReturn::Sanitized { body, sanitizer } => {
            let registered = registry
                .sanitizer(&sanitizer)
                .ok_or_else(|| BranchError::UnknownSanitizer(sanitizer.as_str().to_string()))?;
            if !registered.on.output {
                return Err(BranchError::SanitizerNotOutput(sanitizer.as_str().to_string()));
            }
            if fold.audience.covers(&registered.can_reduce.from_includes) != Adequacy::Holds {
                return Err(BranchError::TransitionSourceUnmet);
            }
            // Trust is preserved from the child fold; audience becomes the sanitizer's declared `to`.
            LabeledValue::new(
                body,
                Label::new(fold.trust, Dim::Known(registered.can_reduce.to.clone())),
            )
        }
    };
    let id = ChildReturnId::new(child.trajectory().clone(), child.returns_by(child.trajectory()));
    let fact = Fact::ChildReturn {
        trajectory: child.trajectory().clone(),
        id,
        value,
    };
    Ok(FactBatch::new(child.revision(), vec![fact]))
}

/// Merge a child return into its direct parent: admit the value at the engine-derived
/// `parent.combine(returned)` (value-granular, can only narrow the parent) and record a `Merge`
/// boundary. Refuses an unknown, already-merged, or non-direct-parent return.
pub(crate) fn merge(parent: &Views, child_return: &ChildReturnId) -> Result<FactBatch, BranchError> {
    let returned = parent
        .child_return(child_return)
        .ok_or(BranchError::UnknownChildReturn)?
        .clone();
    if parent.is_merged(child_return) {
        return Err(BranchError::AlreadyMerged);
    }
    match parent.parent_of(child_return.child()) {
        Some(direct) if direct == parent.trajectory() => {}
        _ => return Err(BranchError::NotDirectParent),
    }

    let merged_label = parent.current_label().combine(&returned.label);
    let admitted = Fact::ValueAdmitted {
        trajectory: parent.trajectory().clone(),
        value: LabeledValue::new(returned.body, merged_label),
        provenance: Provenance::ChildReturn {
            child: child_return.child().clone(),
            id: child_return.clone(),
        },
    };
    let boundary = Fact::Boundary {
        trajectory: parent.trajectory().clone(),
        kind: BoundaryKind::Merge {
            child_return: child_return.clone(),
        },
    };
    Ok(FactBatch::new(parent.revision(), vec![admitted, boundary]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::{AudienceTransition, Sanitizer, SanitizerPoints};
    use crate::fact::{CloseOutcome, EffectKind, Revision};
    use crate::label::{Audience, Label, ReaderId, Trust};
    use crate::projection::Projection;
    use crate::registry::{RegistryConfig, TrustChain};
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
        Label::new(Dim::Known(trust), Dim::Known(audience))
    }

    fn internal() -> Audience {
        Audience::restricted([ReaderId::new("internal")])
    }

    fn admit(trajectory: TrajectoryId, label: Label) -> Fact {
        Fact::ValueAdmitted {
            trajectory,
            value: LabeledValue::new(ValueBody::new("body"), label),
            provenance: Provenance::UserInput,
        }
    }

    fn registry() -> Registry {
        let declassify = Sanitizer {
            name: SanitizerName::new("declassify"),
            on: SanitizerPoints {
                input: false,
                output: true,
            },
            can_reduce: AudienceTransition {
                from_includes: internal(),
                to: Audience::Public,
            },
        };
        Registry::build(RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![],
            authorities: vec![],
            sanitizers: vec![declassify],
            casts: vec![],
        })
        .unwrap()
    }

    fn build(log: &[Fact]) -> Projection {
        Projection::build(log, Revision::new(log.len() as u64))
    }

    fn forked(parent_label: Label) -> Vec<Fact> {
        let mut log = vec![admit(parent(), parent_label)];
        let projection = build(&log);
        let seed = seed_child(&projection.view(&parent()), &child()).unwrap();
        log.extend(seed.facts);
        log
    }

    fn raw(body: &str) -> ChildReturn {
        ChildReturn::Raw {
            body: ValueBody::new(body),
        }
    }

    #[test]
    fn fork_seeds_child_at_parent_current_label() {
        let log = forked(known(SUSPICIOUS, internal()));
        let projection = build(&log);
        assert_eq!(projection.view(&child()).current_label(), known(SUSPICIOUS, internal()));
        assert_ne!(projection.view(&child()).current_label(), Label::top());
    }

    #[test]
    fn fork_refuses_self_reparent_and_unresolved_parent() {
        let log = vec![admit(parent(), known(TRUSTED, Audience::Public))];
        let projection = build(&log);
        assert_eq!(
            seed_child(&projection.view(&parent()), &parent()),
            Err(BranchError::SelfFork)
        );
        let log = forked(known(TRUSTED, Audience::Public));
        let projection = build(&log);
        let other = TrajectoryId::new("other");
        assert_eq!(
            seed_child(&projection.view(&other), &child()),
            Err(BranchError::AlreadyForked)
        );
        let log = vec![admit(parent(), Label::new(Dim::Unknown, Dim::Known(Audience::Public)))];
        let projection = build(&log);
        assert_eq!(
            seed_child(&projection.view(&parent()), &child()),
            Err(BranchError::ParentUnresolved)
        );
    }

    #[test]
    fn raw_return_carries_the_child_fold() {
        let mut log = forked(known(SUSPICIOUS, internal()));
        let projection = build(&log);
        let ret = submit_child_return(&registry(), &projection.view(&child()), raw("secret")).unwrap();
        log.extend(ret.facts);
        let projection = build(&log);
        match projection.view(&parent()).child_return(&ChildReturnId::new(child(), 0)) {
            Some(value) => assert_eq!(value.label, known(SUSPICIOUS, internal())),
            None => panic!("child return not recorded"),
        }
    }

    #[test]
    fn sanitized_return_relabels_audience_preserving_trust() {
        let mut log = forked(known(SUSPICIOUS, internal()));
        let projection = build(&log);
        let ret = submit_child_return(
            &registry(),
            &projection.view(&child()),
            ChildReturn::Sanitized {
                body: ValueBody::new("redacted"),
                sanitizer: SanitizerName::new("declassify"),
            },
        )
        .unwrap();
        log.extend(ret.facts);
        let projection = build(&log);
        let value = projection
            .view(&parent())
            .child_return(&ChildReturnId::new(child(), 0))
            .unwrap()
            .clone();
        assert_eq!(value.label.trust, Dim::Known(SUSPICIOUS));
        assert_eq!(value.label.audience, Dim::Known(Audience::Public));
    }

    #[test]
    fn sanitized_return_with_unmet_from_is_refused() {
        let finance = Audience::restricted([ReaderId::new("finance")]);
        let log = forked(known(TRUSTED, finance));
        let projection = build(&log);
        assert_eq!(
            submit_child_return(
                &registry(),
                &projection.view(&child()),
                ChildReturn::Sanitized {
                    body: ValueBody::new("x"),
                    sanitizer: SanitizerName::new("declassify"),
                },
            ),
            Err(BranchError::TransitionSourceUnmet)
        );
    }

    #[test]
    fn merge_result_value_is_engine_derived_not_the_returned_label() {
        let mut log = forked(known(SUSPICIOUS, internal()));
        let projection = build(&log);
        let ret = submit_child_return(
            &registry(),
            &projection.view(&child()),
            ChildReturn::Sanitized {
                body: ValueBody::new("redacted"),
                sanitizer: SanitizerName::new("declassify"),
            },
        )
        .unwrap();
        log.extend(ret.facts);
        let values_before = log.iter().filter(|f| matches!(f, Fact::ValueAdmitted { .. })).count();
        let projection = build(&log);
        let merged = merge(&projection.view(&parent()), &ChildReturnId::new(child(), 0)).unwrap();
        log.extend(merged.facts);
        let projection = build(&log);
        assert_eq!(
            projection.value_label(ValueId::new(values_before as u64)),
            Some(&known(SUSPICIOUS, internal()))
        );
        assert_eq!(
            projection.view(&parent()).current_label(),
            known(SUSPICIOUS, internal())
        );
    }

    #[test]
    fn merge_absorbs_a_narrower_return() {
        let mut log = forked(known(TRUSTED, Audience::Public));
        log.push(admit(child(), known(SUSPICIOUS, internal())));
        let projection = build(&log);
        let ret = submit_child_return(&registry(), &projection.view(&child()), raw("r")).unwrap();
        log.extend(ret.facts);
        let projection = build(&log);
        let merged = merge(&projection.view(&parent()), &ChildReturnId::new(child(), 0)).unwrap();
        log.extend(merged.facts);
        let projection = build(&log);
        assert_eq!(
            projection.view(&parent()).current_label(),
            known(SUSPICIOUS, internal())
        );
    }

    #[test]
    fn reparenting_merge_is_refused() {
        let mut log = forked(known(TRUSTED, Audience::Public));
        let projection = build(&log);
        let ret = submit_child_return(&registry(), &projection.view(&child()), raw("r")).unwrap();
        log.extend(ret.facts);
        let projection = build(&log);
        let stranger = TrajectoryId::new("stranger");
        assert_eq!(
            merge(&projection.view(&stranger), &ChildReturnId::new(child(), 0)),
            Err(BranchError::NotDirectParent)
        );
    }

    #[test]
    fn double_merge_is_refused() {
        let mut log = forked(known(TRUSTED, Audience::Public));
        let projection = build(&log);
        let ret = submit_child_return(&registry(), &projection.view(&child()), raw("r")).unwrap();
        log.extend(ret.facts);
        let projection = build(&log);
        let id = ChildReturnId::new(child(), 0);
        let merged = merge(&projection.view(&parent()), &id).unwrap();
        log.extend(merged.facts);
        let projection = build(&log);
        assert_eq!(merge(&projection.view(&parent()), &id), Err(BranchError::AlreadyMerged));
    }

    #[test]
    fn abandoned_child_egress_is_visible_to_the_parent() {
        let mut log = forked(known(TRUSTED, Audience::Public));
        let egress = EffectKind::new("egress");
        let call = ResolvedCall::new(ToolName::new("send"), json!({}), vec![]);
        let dispatch = DispatchId::new(child(), call.digest(), 0);
        log.push(Fact::DispatchOpened {
            trajectory: child(),
            dispatch: dispatch.clone(),
            proposed_label: Label::top(),
            proposed_effects: vec![egress.clone()],
        });
        log.push(Fact::DispatchClosed {
            trajectory: child(),
            dispatch,
            outcome: CloseOutcome::Success {
                effects: vec![egress.clone()],
            },
        });
        let projection = build(&log);
        assert!(projection.view(&parent()).has_effect(&egress));
    }
}
