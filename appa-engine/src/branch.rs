//! Branching label semantics over one shared family log.

use thiserror::Error;

use crate::check::Narrowing;
use crate::fact::{BoundaryKind, Fact, ReturnDerivation, ReturnPolicy};
use crate::groups::{Expansions, GroupResolution};
use crate::projection::Views;
use crate::registry::Registry;
use crate::value::{ChildReturnId, LabeledValue, Provenance, TrajectoryId, ValueBody};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BranchError {
    #[error("the branch already ended its errand — by one value crossing or one void, at most once")]
    AlreadyEnded,
    #[error("the child was not forked from this parent (reparenting/cross-family merge refused)")]
    NotDirectParent,
    #[error("the trajectory has no fork binding — only a child may return")]
    NotForked,
    #[error("the submission does not match the child's fork return policy")]
    ReturnPolicyMismatch,
}

/// What a raw-bound child's submission comes to: the atomic crossing batch, or the exact
/// narrowing the parent must accept first. One verdict, so a caller cannot merge a batch the gate
/// would have priced — the two are alternatives of one decision, never two questions asked twice.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RawCrossing {
    Merged(Vec<Fact>),
    Narrows(Narrowing),
}

pub(crate) fn submit_child_return(
    registry: &Registry,
    parent: &Views,
    child: &TrajectoryId,
    body: &ValueBody,
    expansions: &Expansions,
) -> Result<RawCrossing, BranchError> {
    if let Some(narrowing) = raw_return_narrowing(parent, child)? {
        return Ok(RawCrossing::Narrows(narrowing));
    }
    let fold = parent.branch_label(child);
    Ok(RawCrossing::Merged(crossing_facts(
        parent,
        child,
        LabeledValue::new(body.clone(), fold.bound().clone().into_label()),
        ReturnDerivation::Raw,
        None,
        registry.resolutions(expansions),
    )))
}

/// Record a child's **void return**: the child-attributed terminal ends the branch and
/// crosses no value — no merge, no label contribution. Refused for a non-child and for a branch
/// that already ended by value or void. The batch is on the family's revision, so
/// competing terminals linearize at the store's revisioned append and at most one lands.
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

/// The one place a return's facts are assembled: the child's `ChildReturn` record, the optional
/// return-scoped acceptance, the parent's `ValueAdmitted` under the returned value's own label,
/// and the `Merge` boundary — always one batch, never split across commit points. The parent
/// *fold* absorbs the crossing at projection (audience meet, min trust) — identical to folding
/// `parent.combine(returned)`, since `combine` is idempotent — while the stored per-value label
/// stays the value's intrinsic one, so authority review context and cast targeting see what the
/// value *is*, not the parent's unrelated restrictions.
pub(crate) fn crossing_facts(
    parent: &Views,
    child: &TrajectoryId,
    value: LabeledValue,
    derivation: ReturnDerivation,
    acceptance: Option<Narrowing>,
    resolutions: Vec<GroupResolution>,
) -> Vec<Fact> {
    let id = ChildReturnId::new(child.clone(), parent.returns_by(child));
    let mut facts = vec![Fact::ChildReturn {
        trajectory: child.clone(),
        id: id.clone(),
        value: value.clone(),
        derivation,
        resolutions,
    }];
    if let Some(narrowing) = acceptance {
        facts.push(Fact::ChildReturnAcceptance {
            trajectory: parent.trajectory().clone(),
            child_return: id.clone(),
            narrowing,
        });
    }
    facts.push(Fact::ValueAdmitted {
        trajectory: parent.trajectory().clone(),
        value,
        provenance: Provenance::ChildReturn {
            child: child.clone(),
            id: id.clone(),
        },
    });
    facts.push(Fact::Boundary {
        trajectory: parent.trajectory().clone(),
        kind: BoundaryKind::Merge { child_return: id },
    });
    facts
}

/// What a raw crossing of `child`'s fold would cost the parent: `None` where
/// the merge narrows nothing and the raw value may cross, the exact narrowing otherwise. Refused
/// for a non-child, an ended branch, and a fork whose policy is not raw — the blocked-return flow
/// exists only under a Raw policy: a bound sanitizer crosses unconditionally, and the model never
/// chooses a path.
pub(crate) fn raw_return_narrowing(parent: &Views, child: &TrajectoryId) -> Result<Option<Narrowing>, BranchError> {
    match parent.parent_of(child) {
        Some(direct) if direct == parent.trajectory() => {}
        _ => return Err(BranchError::NotDirectParent),
    }
    if parent.has_ended(child) {
        return Err(BranchError::AlreadyEnded);
    }
    match parent.return_policy_of(child) {
        Some(ReturnPolicy::Raw) => {}
        Some(_) => return Err(BranchError::ReturnPolicyMismatch),
        None => return Err(BranchError::NotForked),
    }
    let fold = parent.branch_label(child);
    let current = parent.current_label();
    let candidate = current.bound().combine(fold.bound());
    if &candidate == current.bound() {
        return Ok(None);
    }
    Ok(Some(Narrowing {
        from: current.bound().clone(),
        to: candidate,
    }))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::admit::{CastAnswer, CastError, admit_cast};
    use crate::authority::{
        Cast, CastResolution, DeclaredLabel, DeclaredTransition, Sanitizer, SanitizerPoints, Scope,
    };
    use crate::fact::{CloseOutcome, EffectKind, EffectSet, ForkSnapshot};
    use crate::groups::DeclaredAudience;
    use crate::label::Adequacy;
    use crate::label::Dimension;
    use crate::label::EstablishedLabel;
    use crate::label::PartialLabel;
    use crate::label::{Audience, Dim, Label, ReaderId, Trust};
    use crate::names::{CastName, SanitizerName};
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

    fn established(trust: Trust, audience: Audience) -> EstablishedLabel {
        EstablishedLabel::new(trust, audience)
    }

    fn partial(trust: Trust, audience: Audience) -> PartialLabel {
        PartialLabel::established(EstablishedLabel::new(trust, audience))
    }

    fn internal() -> Audience {
        Audience::restricted([ReaderId::new("internal")])
    }

    fn opened(trajectory: TrajectoryId, label: Label) -> Fact {
        crate::profile::opening_at(trajectory, label)
    }

    fn read_tool() -> crate::contract::ToolContract {
        crate::contract::ToolContract {
            description: Some("A test tool.".to_string()),
            uses: vec![],
            name: ToolName::new("read"),
            tags: vec![],
            delta: Some(crate::contract::Delta::NONE),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: crate::contract::Requires::default(),
        }
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
            arguments: call.canonical_arguments().clone(),
            proposed_label: EstablishedLabel::top(),
            receiving: EstablishedLabel::top(),
            proposed_effects: EffectSet::default(),
            tool_resolutions: Vec::new(),
            memberships: Vec::new(),
            subject: crate::basis::fixture_subject(&trajectory),
            resolutions: vec![],
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

    fn registry() -> Registry {
        let declassify = Sanitizer {
            name: SanitizerName::new("declassify"),
            on: SanitizerPoints {
                input: false,
                output: true,
            },
            transition: DeclaredTransition::Audience {
                from_includes: DeclaredAudience::literal(internal()),
                to: DeclaredAudience::literal(Audience::Public),
            },
            scope: Scope::default(),
            hint: None,
        };
        Registry::build_covered(RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![],
            authorities: vec![],
            sanitizers: vec![declassify],
            casts: vec![],
            membership: None,
        })
        .unwrap()
    }

    fn build(log: &[Fact]) -> Projection {
        Projection::build(log, log.len() as u64)
    }

    fn fork_records(log: &[Fact], parent: &TrajectoryId, child: &TrajectoryId, policy: ReturnPolicy) -> Vec<Fact> {
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
                return_policy: policy,
                shape: None,
            },
            Fact::ForkOpened {
                trajectory: child.clone(),
                fork,
            },
        ]
    }

    fn forked(parent_label: Label) -> Vec<Fact> {
        forked_bound(parent_label, ReturnPolicy::Raw)
    }

    fn forked_bound(parent_label: Label, policy: ReturnPolicy) -> Vec<Fact> {
        let mut log = vec![opened(parent(), parent_label)];
        let seed = fork_records(&log, &parent(), &child(), policy);
        log.extend(seed);
        log
    }

    fn sanitized_policy() -> ReturnPolicy {
        ReturnPolicy::Sanitized(SanitizerName::new("declassify"))
    }

    fn raw(body: &str) -> ValueBody {
        ValueBody::new(body)
    }

    fn merged(crossing: RawCrossing) -> Vec<Fact> {
        match crossing {
            RawCrossing::Merged(facts) => facts,
            RawCrossing::Narrows(narrowing) => panic!("expected a merged crossing, got {narrowing:?}"),
        }
    }

    #[test]
    fn fork_seeds_child_at_parent_current_label() {
        let log = forked(known(SUSPICIOUS, internal()));
        let projection = build(&log);
        assert_eq!(
            projection.view(&child()).current_label(),
            partial(SUSPICIOUS, internal())
        );
        assert_ne!(
            projection.view(&child()).current_label(),
            PartialLabel::established(EstablishedLabel::top())
        );
    }

    #[test]
    fn a_non_narrowing_raw_return_crosses_in_one_batch() {
        let mut log = forked(known(SUSPICIOUS, internal()));
        let projection = build(&log);
        let ret = merged(
            submit_child_return(
                &registry(),
                &projection.view(&parent()),
                &child(),
                &raw("secret"),
                &Expansions::default(),
            )
            .unwrap(),
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
        match projection.view(&parent()).child_return(&ChildReturnId::new(child(), 0)) {
            Some(value) => assert_eq!(value.label, known(SUSPICIOUS, internal())),
            None => panic!("child return not recorded"),
        }
        assert_eq!(
            projection.view(&parent()).current_label(),
            partial(SUSPICIOUS, internal())
        );
    }

    #[test]
    fn a_narrowing_raw_return_is_priced_not_merged() {
        let mut log = forked(known(TRUSTED, Audience::Public));
        admit(&mut log, child(), known(SUSPICIOUS, internal()));
        let projection = build(&log);
        assert_eq!(
            submit_child_return(
                &registry(),
                &projection.view(&parent()),
                &child(),
                &raw("secret"),
                &Expansions::default()
            ),
            Ok(RawCrossing::Narrows(Narrowing {
                from: EstablishedLabel::new(TRUSTED, Audience::Public),
                to: EstablishedLabel::new(SUSPICIOUS, internal()),
            }))
        );
    }

    #[test]
    fn an_unknown_dimension_crosses_as_identity_not_a_block() {
        let mut log = forked(known(TRUSTED, Audience::Public));
        admit(
            &mut log,
            child(),
            Label::new(Dim::Unknown, Dim::Known(Audience::Public)),
        );
        let unknown_value = ValueId::new(0);
        let projection = build(&log);
        assert_eq!(raw_return_narrowing(&projection.view(&parent()), &child()), Ok(None));
        assert_eq!(resolve(&log, &parent(), 0), Err(CastError::ForeignValue));

        let projection = build(&log);
        let ret = merged(
            submit_child_return(
                &cast_registry(),
                &projection.view(&parent()),
                &child(),
                &raw("found"),
                &Expansions::default(),
            )
            .unwrap(),
        );
        log.extend(ret);
        let projection = build(&log);
        let parent_label = projection.view(&parent()).current_label();
        assert_eq!(parent_label.bound(), &established(TRUSTED, Audience::Public));
        assert_eq!(
            parent_label.unresolved(Dimension::Trust).collect::<Vec<_>>(),
            vec![unknown_value]
        );
        assert!(parent_label.is_established(Dimension::Audience));

        let batch = resolve(&log, &parent(), 0).expect("a merge-carried identity is in reach");
        log.extend(batch);
        let projection = build(&log);
        assert_eq!(
            projection.view(&parent()).current_label(),
            partial(SUSPICIOUS, Audience::Public)
        );
        assert_eq!(
            projection.view(&child()).current_label(),
            partial(SUSPICIOUS, Audience::Public)
        );
    }

    #[test]
    fn a_crossing_carries_each_unresolved_dimension_it_rode_in_with() {
        let mut log = forked(known(TRUSTED, Audience::Public));
        admit(&mut log, child(), Label::new(Dim::Unknown, Dim::Unknown));
        admit(&mut log, parent(), Label::new(Dim::Known(TRUSTED), Dim::Unknown));
        let projection = build(&log);
        assert_eq!(raw_return_narrowing(&projection.view(&parent()), &child()), Ok(None));
        let ret = merged(
            submit_child_return(
                &registry(),
                &projection.view(&parent()),
                &child(),
                &raw("found"),
                &Expansions::default(),
            )
            .unwrap(),
        );
        log.extend(ret);
        let label = build(&log).view(&parent()).current_label();
        assert_eq!(
            label.unresolved(Dimension::Trust).collect::<Vec<_>>(),
            vec![ValueId::new(0)]
        );
        assert_eq!(
            label.unresolved(Dimension::Audience).collect::<Vec<_>>(),
            vec![ValueId::new(0), ValueId::new(1)]
        );
    }

    #[test]
    fn absorption_activates_only_at_the_merge() {
        let mut log = forked(known(SUSPICIOUS, Audience::Public));
        admit(&mut log, child(), known(SUSPICIOUS, Audience::Public));
        admit(&mut log, child(), Label::new(Dim::Known(SUSPICIOUS), Dim::Unknown));
        let unknown_value = ValueId::new(1);
        assert!(
            build(&log)
                .view(&parent())
                .current_label()
                .is_established(Dimension::Audience)
        );
        assert_eq!(resolve(&log, &parent(), 1), Err(CastError::ForeignValue));

        let projection = build(&log);
        let batch = merged(
            submit_child_return(
                &registry(),
                &projection.view(&parent()),
                &child(),
                &raw("found"),
                &Expansions::default(),
            )
            .unwrap(),
        );
        let (merge, before) = batch.split_last().expect("the crossing ends at its merge");
        assert!(matches!(
            merge,
            Fact::Boundary {
                kind: BoundaryKind::Merge { .. },
                ..
            }
        ));
        let truncated = [log.clone(), before.to_vec()].concat();
        assert!(
            build(&truncated)
                .view(&parent())
                .current_label()
                .is_established(Dimension::Audience)
        );
        let complete = [log, batch.clone()].concat();
        assert_eq!(
            build(&complete)
                .view(&parent())
                .current_label()
                .unresolved(Dimension::Audience)
                .collect::<Vec<_>>(),
            vec![unknown_value]
        );
    }

    #[test]
    fn a_known_return_merges_into_an_unknown_parent() {
        let mut log = forked(known(TRUSTED, Audience::Public));
        admit(&mut log, parent(), Label::new(Dim::Known(TRUSTED), Dim::Unknown));
        let projection = build(&log);
        assert_eq!(raw_return_narrowing(&projection.view(&parent()), &child()), Ok(None));

        let batch = merged(
            submit_child_return(
                &registry(),
                &projection.view(&parent()),
                &child(),
                &raw("result"),
                &Expansions::default(),
            )
            .expect("a known return merges into an Unknown parent"),
        );
        assert!(
            batch
                .iter()
                .any(|fact| matches!(fact, Fact::ChildReturn { trajectory, .. } if *trajectory == child()))
        );
        assert!(batch.iter().any(|fact| matches!(
            fact,
            Fact::Boundary {
                kind: BoundaryKind::Merge { .. },
                ..
            }
        )));
    }

    #[test]
    fn a_return_narrowing_check_for_a_non_child_is_refused() {
        let log = vec![opened(parent(), known(TRUSTED, Audience::Public))];
        let projection = build(&log);
        assert_eq!(
            raw_return_narrowing(&projection.view(&parent()), &TrajectoryId::new("stranger")),
            Err(BranchError::NotDirectParent)
        );
    }

    #[test]
    fn a_second_return_from_one_child_is_refused() {
        let mut log = forked(known(SUSPICIOUS, internal()));
        let projection = build(&log);
        let ret = merged(
            submit_child_return(
                &registry(),
                &projection.view(&parent()),
                &child(),
                &raw("first"),
                &Expansions::default(),
            )
            .unwrap(),
        );
        log.extend(ret);
        let projection = build(&log);
        assert_eq!(
            submit_child_return(
                &registry(),
                &projection.view(&parent()),
                &child(),
                &raw("second"),
                &Expansions::default()
            ),
            Err(BranchError::AlreadyEnded)
        );
        assert_eq!(
            raw_return_narrowing(&projection.view(&parent()), &child()),
            Err(BranchError::AlreadyEnded)
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
        assert_eq!(
            submit_child_return(
                &registry(),
                &projection.view(&parent()),
                &child(),
                &raw("late"),
                &Expansions::default()
            ),
            Err(BranchError::AlreadyEnded)
        );
        assert_eq!(
            raw_return_narrowing(&projection.view(&parent()), &child()),
            Err(BranchError::AlreadyEnded)
        );
    }

    #[test]
    fn competing_terminals_linearize_to_at_most_one() {
        let mut log = forked(known(SUSPICIOUS, internal()));
        let projection = build(&log);
        let ret = merged(
            submit_child_return(
                &registry(),
                &projection.view(&parent()),
                &child(),
                &raw("finding"),
                &Expansions::default(),
            )
            .unwrap(),
        );
        submit_void_return(&projection.view(&parent()), &child()).unwrap();
        log.extend(ret);
        let projection = build(&log);
        assert_eq!(
            submit_void_return(&projection.view(&parent()), &child()),
            Err(BranchError::AlreadyEnded)
        );
    }

    #[test]
    fn a_submission_off_the_fork_policy_is_refused() {
        let log = forked_bound(known(SUSPICIOUS, internal()), sanitized_policy());
        let projection = build(&log);
        assert_eq!(
            submit_child_return(
                &registry(),
                &projection.view(&parent()),
                &child(),
                &raw("leak"),
                &Expansions::default()
            ),
            Err(BranchError::ReturnPolicyMismatch)
        );
    }

    #[test]
    fn a_return_narrowing_check_applies_only_under_a_raw_policy() {
        let log = forked_bound(known(TRUSTED, internal()), sanitized_policy());
        let projection = build(&log);
        assert_eq!(
            raw_return_narrowing(&projection.view(&parent()), &child()),
            Err(BranchError::ReturnPolicyMismatch)
        );
    }

    #[test]
    fn return_facts_audit_their_derivation() {
        let log = forked(known(SUSPICIOUS, internal()));
        let projection = build(&log);
        let ret = merged(
            submit_child_return(
                &registry(),
                &projection.view(&parent()),
                &child(),
                &raw("secret"),
                &Expansions::default(),
            )
            .unwrap(),
        );
        match &ret[0] {
            Fact::ChildReturn { derivation, .. } => assert_eq!(derivation, &ReturnDerivation::Raw),
            other => panic!("expected ChildReturn, got {other:?}"),
        }
    }

    #[test]
    fn a_return_submitted_toward_a_stranger_is_refused() {
        let log = forked(known(TRUSTED, Audience::Public));
        let projection = build(&log);
        let stranger = TrajectoryId::new("stranger");
        assert_eq!(
            submit_child_return(
                &registry(),
                &projection.view(&stranger),
                &child(),
                &raw("r"),
                &Expansions::default()
            ),
            Err(BranchError::NotDirectParent)
        );
    }

    #[test]
    fn abandoned_child_egress_is_visible_to_the_parent() {
        let mut log = forked(known(TRUSTED, Audience::Public));
        let egress = EffectKind::new("egress");
        let call = ResolvedCall::new(ToolName::new("send"), crate::params::test_arguments(&json!({})));
        let dispatch = DispatchId::new(child(), call.digest(), 0);
        log.push(Fact::DispatchOpened {
            trajectory: child(),
            dispatch: dispatch.clone(),
            tool: call.tool().clone(),
            arguments: call.canonical_arguments().clone(),
            proposed_label: EstablishedLabel::top(),
            receiving: EstablishedLabel::top(),
            proposed_effects: EffectSet::new([egress.clone()]).unwrap(),
            tool_resolutions: Vec::new(),
            memberships: Vec::new(),
            subject: crate::basis::fixture_subject(&child()),
            resolutions: vec![],
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

    fn cast_registry() -> Registry {
        Registry::build_covered(RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![read_tool()],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![Cast {
                name: CastName::new("classify"),
                resolution: CastResolution::Constant(DeclaredLabel::literal(established(SUSPICIOUS, Audience::Public))),
                scope: Scope::default(),
            }],
            membership: None,
        })
        .unwrap()
    }

    fn unknown_source(log: &mut Vec<Fact>, trajectory: TrajectoryId) {
        admit(log, trajectory, Label::new(Dim::Unknown, Dim::Known(Audience::Public)));
    }

    fn opened_unknown_parent() -> Vec<Fact> {
        let mut log = vec![opened(parent(), known(TRUSTED, Audience::Public))];
        unknown_source(&mut log, parent());
        log
    }

    fn fork_under(log: &mut Vec<Fact>, parent: &TrajectoryId, child: &TrajectoryId) {
        let facts = fork_records(log, parent, child, ReturnPolicy::Raw);
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

    fn resolve(log: &[Fact], actor: &TrajectoryId, value: u64) -> Result<Vec<Fact>, CastError> {
        let projection = build(log);
        admit_cast(
            &cast_registry(),
            &projection.view(actor),
            ValueId::new(value),
            CastAnswer {
                cast: CastName::new("classify"),
                resolved: established(SUSPICIOUS, Audience::Public),
            },
            &Expansions::default(),
        )
    }

    fn unresolved_trust(log: &[Fact], trajectory: &TrajectoryId) -> Vec<ValueId> {
        build(log)
            .view(trajectory)
            .current_label()
            .unresolved(Dimension::Trust)
            .collect()
    }

    #[test]
    fn a_fork_at_an_unresolved_parent_carries_the_source_and_shares_its_resolution() {
        let mut log = opened_unknown_parent();
        fork_under(&mut log, &parent(), &child());

        let projection = build(&log);
        let at_fork = projection.view(&parent()).current_label();
        assert_eq!(projection.view(&child()).current_label(), at_fork);
        assert_eq!(
            projection.view(&child()).current_label().meets_floor(SUSPICIOUS),
            Adequacy::Unresolved
        );
        let snapshot = snapshot_of(&log, &child());
        assert_eq!(snapshot.inherited(), &BTreeSet::from([ValueId::new(0)]));
        assert_eq!(snapshot.seed(), &at_fork);

        let batch = resolve(&log, &parent(), 0).expect("a branch resolves its own source");
        log.extend(batch);
        let projection = build(&log);
        assert_eq!(
            projection.view(&child()).current_label(),
            partial(SUSPICIOUS, Audience::Public)
        );
        assert_eq!(
            projection.view(&child()).current_label().meets_floor(SUSPICIOUS),
            Adequacy::Holds
        );
        assert_eq!(snapshot_of(&log, &child()).seed(), &at_fork);
    }

    #[test]
    fn a_childs_resolution_of_an_inherited_source_reaches_the_parent_and_its_siblings() {
        let sibling = TrajectoryId::new("sibling");
        let mut log = opened_unknown_parent();
        fork_under(&mut log, &parent(), &child());
        fork_under(&mut log, &parent(), &sibling);

        let batch = resolve(&log, &child(), 0).expect("an inherited source is in reach");
        log.extend(batch);
        let projection = build(&log);
        for branch in [parent(), child(), sibling] {
            assert_eq!(
                projection.view(&branch).current_label(),
                partial(SUSPICIOUS, Audience::Public),
                "the resolution is shared with every branch holding the source"
            );
        }
        assert_eq!(resolve(&log, &parent(), 0), Err(CastError::AlreadyEstablished));
    }

    #[test]
    fn a_branch_may_not_resolve_a_sibling_or_post_fork_value() {
        let sibling = TrajectoryId::new("sibling");
        let mut log = opened_unknown_parent();
        fork_under(&mut log, &parent(), &child());
        fork_under(&mut log, &parent(), &sibling);
        unknown_source(&mut log, parent());
        unknown_source(&mut log, child());

        assert_eq!(resolve(&log, &child(), 1), Err(CastError::ForeignValue));
        assert_eq!(resolve(&log, &sibling, 2), Err(CastError::ForeignValue));
        assert_eq!(resolve(&log, &parent(), 2), Err(CastError::ForeignValue));

        assert_eq!(
            unresolved_trust(&log, &parent()),
            vec![ValueId::new(0), ValueId::new(1)]
        );
        assert_eq!(unresolved_trust(&log, &child()), vec![ValueId::new(0), ValueId::new(2)]);
        assert_eq!(unresolved_trust(&log, &sibling), vec![ValueId::new(0)]);
    }

    #[test]
    fn a_nested_fork_flattens_its_inherited_set() {
        let grandchild = TrajectoryId::new("grandchild");
        let mut log = opened_unknown_parent();
        fork_under(&mut log, &parent(), &child());
        unknown_source(&mut log, child());
        fork_under(&mut log, &child(), &grandchild);

        let snapshot = snapshot_of(&log, &grandchild);
        assert_eq!(
            snapshot.inherited(),
            &BTreeSet::from([ValueId::new(0), ValueId::new(1)])
        );

        let batch = resolve(&log, &grandchild, 0).expect("an inherited ancestor source is in reach");
        log.extend(batch);
        assert_eq!(unresolved_trust(&log, &parent()), vec![]);
        assert_eq!(unresolved_trust(&log, &child()), vec![ValueId::new(1)]);
        assert_eq!(unresolved_trust(&log, &grandchild), vec![ValueId::new(1)]);
    }

    #[test]
    fn competing_first_answers_admit_exactly_one() {
        let mut log = opened_unknown_parent();
        fork_under(&mut log, &parent(), &child());
        let by_child = resolve(&log, &child(), 0).unwrap();
        resolve(&log, &parent(), 0).unwrap();

        log.extend(by_child);
        assert_eq!(resolve(&log, &parent(), 0), Err(CastError::AlreadyEstablished));
    }

    #[test]
    fn an_in_flight_child_dispatch_reserves_against_the_parent() {
        let mut log = forked(known(TRUSTED, Audience::Public));
        let egress = EffectKind::new("egress");
        let call = ResolvedCall::new(ToolName::new("send"), crate::params::test_arguments(&json!({})));
        let dispatch = DispatchId::new(child(), call.digest(), 0);
        log.push(Fact::DispatchOpened {
            trajectory: child(),
            dispatch: dispatch.clone(),
            tool: call.tool().clone(),
            arguments: call.canonical_arguments().clone(),
            proposed_label: EstablishedLabel::top(),
            receiving: EstablishedLabel::top(),
            proposed_effects: EffectSet::new([egress.clone()]).unwrap(),
            tool_resolutions: Vec::new(),
            memberships: Vec::new(),
            subject: crate::basis::fixture_subject(&child()),
            resolutions: vec![],
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
