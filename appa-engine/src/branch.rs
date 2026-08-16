//! Branching label semantics over one shared family log.

use thiserror::Error;

use crate::check::Narrowing;
use crate::fact::{BoundaryKind, Fact, ReturnDerivation, ReturnPolicy};
use crate::label::{EstablishedLabel, PartialLabel};
use crate::names::SanitizerName;
use crate::projection::Views;
use crate::registry::Registry;
use crate::value::{ChildReturnId, LabeledValue, Provenance, RawResultDigest, TrajectoryId, ValueBody};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BranchError {
    #[error("the branch already ended its errand — by one value crossing or one void, at most once")]
    AlreadyEnded,
    #[error("the child was not forked from this parent (reparenting/cross-family merge refused)")]
    NotDirectParent,
    #[error("no sanitizer registered as {0}")]
    UnknownSanitizer(String),
    #[error("sanitizer {0} is not registered for output")]
    SanitizerNotOutput(String),
    #[error("the child fold does not satisfy the sanitizer's `from` precondition")]
    TransitionSourceUnmet,
    #[error("the family state changed since the return block was offered")]
    ReturnOfferStale,
    #[error("the chosen plan is not among the freshly offered return plans")]
    ReturnPlanNotOffered,
    #[error("the submission kind does not match the chosen return plan")]
    SubmissionMismatch,
    #[error("the trajectory has no fork binding — only a child may return")]
    NotForked,
    #[error("the submission does not match the child's fork return policy")]
    ReturnPolicyMismatch,
    #[error("the return policy consumes an unestablished dimension — a cast establishes it, then the return crosses")]
    ReturnFoldUnestablished,
    #[error("a raw return that narrows the parent merges only through an executed return plan")]
    ReturnNarrowsParent,
}

pub(crate) fn submit_child_return(
    registry: &Registry,
    parent: &Views,
    child: &TrajectoryId,
    ret: ReturnSubmission,
) -> Result<Vec<Fact>, BranchError> {
    match parent.parent_of(child) {
        Some(direct) if direct == parent.trajectory() => {}
        _ => return Err(BranchError::NotDirectParent),
    }
    if parent.has_ended(child) {
        return Err(BranchError::AlreadyEnded);
    }
    let policy = parent.return_policy_of(child).ok_or(BranchError::NotForked)?.clone();
    let fold = parent.branch_label(child);
    let (value, derivation) = match (policy, ret) {
        (ReturnPolicy::Raw, ReturnSubmission::Raw { body }) => {
            match check_child_return(registry, parent, child)? {
                ReturnCheck::Allow => {}
                ReturnCheck::Block(_) => return Err(BranchError::ReturnNarrowsParent),
            }
            (
                LabeledValue::new(body, fold.bound().clone().into_label()),
                ReturnDerivation::Raw,
            )
        }
        (ReturnPolicy::Sanitized(bound), ReturnSubmission::Derived { body, raw_digest }) => {
            sanitized_crossing(registry, &fold, &bound, body, raw_digest)?
        }
        _ => return Err(BranchError::ReturnPolicyMismatch),
    };
    Ok(crossing_facts(parent, child, value, derivation, None))
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
/// *fold* absorbs the crossing at projection (intersect readers, min trust) — identical to folding
/// `parent.combine(returned)`, since `combine` is idempotent — while the stored per-value label
/// stays the value's intrinsic one, so authority review context and cast targeting see what the
/// value *is*, not the parent's unrelated restrictions.
pub(crate) fn crossing_facts(
    parent: &Views,
    child: &TrajectoryId,
    value: LabeledValue,
    derivation: ReturnDerivation,
    acceptance: Option<Narrowing>,
) -> Vec<Fact> {
    let id = ChildReturnId::new(child.clone(), parent.returns_by(child));
    let mut facts = vec![Fact::ChildReturn {
        trajectory: child.clone(),
        id: id.clone(),
        value: value.clone(),
        derivation,
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

/// One return remedy the agent may execute on a blocked raw return — closed and return-specific:
/// the tool-block vocabulary (authorities, redispatch, fork advice) is unrepresentable here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReturnPlan {
    Accept(Narrowing),
    Sanitize {
        sanitizer: SanitizerName,
        residual: Option<Narrowing>,
    },
}

/// The verdict on a proposed raw child return: two outcomes, like the tool path's
/// [`crate::check::CheckOutcome`]. Decided from the parent's [`Views`] alone — both folds and the
/// fork linkage come from one projection snapshot, so mixed-snapshot checks are unrepresentable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReturnCheck {
    Allow,
    Block(ReturnBlock),
}

/// What blocked the return: the raw crossing would narrow the parent's established bound.
/// `plans` is non-empty by construction (`Accept` is always offered), in deterministic order —
/// `Accept` first, then sanitizer plans in registry name order. An unresolved child fold blocks
/// nothing here: the bound carries every known restriction, and the unresolved
/// identities cross into the parent's own label at the merge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReturnBlock {
    pub narrowing: Narrowing,
    pub plans: Vec<ReturnPlan>,
}

/// Decide whether a raw return by `child` may merge silently into the parent, and if not, which
/// return plans could cross it. A sanitizer plan carries no residual only when its relabel fully
/// clears the narrowing; when one remains (any trust component included) the plan names exactly
/// that residual. A sanitizer whose relabel changes nothing about the merged label is not offered
/// at all.
pub(crate) fn check_child_return(
    registry: &Registry,
    parent: &Views,
    child: &TrajectoryId,
) -> Result<ReturnCheck, BranchError> {
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

    let fold_bound = fold.bound();
    let candidate = current.bound().combine(fold_bound);
    if &candidate == current.bound() {
        return Ok(ReturnCheck::Allow);
    }
    let narrowing = Narrowing {
        from: current.bound().clone(),
        to: candidate.clone(),
    };

    let fold_label = fold_bound.clone().into_label();
    let mut plans = vec![ReturnPlan::Accept(narrowing.clone())];
    if !registry.profile().confines_child_return() {
        return Ok(ReturnCheck::Block(ReturnBlock { narrowing, plans }));
    }
    for sanitizer in registry.sanitizers() {
        if sanitizer.name.is_attest_schema() {
            continue;
        }
        if !fold.is_established(sanitizer.transition.dimension()) {
            continue;
        }
        let Some(derived) = sanitizer.derive_output(&fold_label, &[]) else {
            continue;
        };
        let sanitized =
            EstablishedLabel::from_label(&derived).expect("a derivation of an established fold is established");
        let merged = current.bound().combine(&sanitized);
        if &merged == current.bound() {
            plans.push(ReturnPlan::Sanitize {
                sanitizer: sanitizer.name.clone(),
                residual: None,
            });
        } else if strictly_improves(&candidate, &merged) {
            plans.push(ReturnPlan::Sanitize {
                sanitizer: sanitizer.name.clone(),
                residual: Some(Narrowing {
                    from: current.bound().clone(),
                    to: merged,
                }),
            });
        }
    }
    Ok(ReturnCheck::Block(ReturnBlock { narrowing, plans }))
}

fn strictly_improves(candidate: &EstablishedLabel, merged: &EstablishedLabel) -> bool {
    &candidate.combine(merged) == candidate && merged != candidate
}

/// What a child submits through `submit_result`, or the runtime submits for a chosen return
/// plan: the raw body, or a registered transformer's derivation with the raw submission's digest
/// (the runtime computes it over the raw bytes before deriving — the raw text itself never
/// reaches the engine). The crossing path is derived from the fork binding or the chosen plan,
/// never selected by this submission: a kind that does not match is refused.
pub enum ReturnSubmission {
    Raw {
        body: ValueBody,
    },
    Derived {
        body: ValueBody,
        raw_digest: RawResultDigest,
    },
}

#[cfg(test)]
pub(crate) fn execute_child_return_plan(
    registry: &Registry,
    parent: &Views,
    child: &TrajectoryId,
    chosen: ReturnPlan,
    submission: ReturnSubmission,
) -> Result<Vec<Fact>, BranchError> {
    let plans = match check_child_return(registry, parent, child)? {
        ReturnCheck::Block(block) => block.plans,
        // Allow: the state moved since the offer — nothing here to execute.
        ReturnCheck::Allow => return Err(BranchError::ReturnOfferStale),
    };
    if !plans.contains(&chosen) {
        return Err(BranchError::ReturnPlanNotOffered);
    }

    let fold = parent.branch_label(child);
    let (value, derivation, acceptance) = match (chosen, submission) {
        (ReturnPlan::Accept(narrowing), ReturnSubmission::Raw { body }) => (
            LabeledValue::new(body, fold.bound().clone().into_label()),
            ReturnDerivation::Raw,
            Some(narrowing),
        ),
        (ReturnPlan::Sanitize { sanitizer, residual }, ReturnSubmission::Derived { body, raw_digest }) => {
            let (value, derivation) = sanitized_crossing(registry, &fold, &sanitizer, body, raw_digest)?;
            (value, derivation, residual)
        }
        // A raw submission for a sanitize plan, or a derivation for Accept.
        _ => return Err(BranchError::SubmissionMismatch),
    };

    Ok(crossing_facts(parent, child, value, derivation, acceptance))
}

fn sanitized_crossing(
    registry: &Registry,
    fold: &PartialLabel,
    sanitizer: &SanitizerName,
    body: ValueBody,
    raw_digest: RawResultDigest,
) -> Result<(LabeledValue, ReturnDerivation), BranchError> {
    let registered = registry
        .sanitizer(sanitizer)
        .ok_or_else(|| BranchError::UnknownSanitizer(sanitizer.as_str().to_string()))?;
    if !fold.is_established(registered.transition.dimension()) {
        return Err(BranchError::ReturnFoldUnestablished);
    }
    if !registered.on.output {
        return Err(BranchError::SanitizerNotOutput(sanitizer.as_str().to_string()));
    }
    let fold_label = fold.bound().clone().into_label();
    let derived = registered
        .derive_output(&fold_label, &[])
        .ok_or(BranchError::TransitionSourceUnmet)?;
    let value = LabeledValue::new(body, derived);
    let derivation = ReturnDerivation::Sanitized {
        sanitizer: sanitizer.clone(),
        raw_digest,
        transition: registered.transition.clone(),
    };
    Ok((value, derivation))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::admit::{CastAnswer, CastError, admit_cast};
    use crate::authority::{Cast, CastResolution, Sanitizer, SanitizerPoints, Scope, Transition};
    use crate::fact::{CloseOutcome, EffectKind, EffectSet, ForkSnapshot};
    use crate::label::Adequacy;
    use crate::label::Dimension;
    use crate::label::{Audience, Dim, Label, ReaderId, Trust};
    use crate::names::CastName;
    use crate::projection::Projection;
    use crate::registry::{RegistryConfig, TrustChain};
    use crate::value::{
        DispatchId, LabeledValue, Provenance, RawResultDigest, ResolvedCall, ToolName, ValueBody, ValueId,
    };
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
            transition: Transition::Audience {
                from_includes: internal(),
                to: Audience::Public,
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
        let mut log = vec![admit(parent(), parent_label)];
        let seed = fork_records(&log, &parent(), &child(), policy);
        log.extend(seed);
        log
    }

    fn sanitized_policy() -> ReturnPolicy {
        ReturnPolicy::Sanitized(SanitizerName::new("declassify"))
    }

    fn raw(body: &str) -> ReturnSubmission {
        ReturnSubmission::Raw {
            body: ValueBody::new(body),
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
        let ret = submit_child_return(&registry(), &projection.view(&parent()), &child(), raw("secret")).unwrap();
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
    fn a_narrowing_raw_return_cannot_merge_silently() {
        let mut log = forked(known(TRUSTED, Audience::Public));
        log.push(admit(child(), known(SUSPICIOUS, internal())));
        let projection = build(&log);
        assert_eq!(
            submit_child_return(&registry(), &projection.view(&parent()), &child(), raw("secret")),
            Err(BranchError::ReturnNarrowsParent)
        );
    }

    #[test]
    fn sanitized_return_relabels_audience_preserving_trust() {
        let mut log = forked_bound(known(SUSPICIOUS, internal()), sanitized_policy());
        let projection = build(&log);
        let ret = submit_child_return(
            &registry(),
            &projection.view(&parent()),
            &child(),
            ReturnSubmission::Derived {
                body: ValueBody::new("redacted"),
                raw_digest: RawResultDigest::of(b"secret"),
            },
        )
        .unwrap();
        log.extend(ret);
        let projection = build(&log);
        let value = projection
            .view(&parent())
            .child_return(&ChildReturnId::new(child(), 0))
            .unwrap()
            .clone();
        assert_eq!(value.label.trust, Dim::Known(SUSPICIOUS));
        assert_eq!(value.label.audience, Dim::Known(Audience::Public));
        assert_eq!(
            projection.view(&parent()).current_label(),
            partial(SUSPICIOUS, internal())
        );
    }

    fn menu_registry() -> Registry {
        let declassify = Sanitizer {
            name: SanitizerName::new("declassify"),
            on: SanitizerPoints {
                input: false,
                output: true,
            },
            transition: Transition::Audience {
                from_includes: internal(),
                to: Audience::Public,
            },
            scope: Scope::default(),
            hint: None,
        };
        let to_finance = Sanitizer {
            name: SanitizerName::new("to-finance"),
            on: SanitizerPoints {
                input: false,
                output: true,
            },
            transition: Transition::Audience {
                from_includes: internal(),
                to: Audience::restricted([ReaderId::new("finance")]),
            },
            scope: Scope::default(),
            hint: None,
        };
        let input_only = Sanitizer {
            name: SanitizerName::new("input-only"),
            on: SanitizerPoints {
                input: true,
                output: false,
            },
            transition: Transition::Audience {
                from_includes: internal(),
                to: Audience::Public,
            },
            scope: Scope::default(),
            hint: None,
        };
        Registry::build_covered(RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![],
            authorities: vec![],
            sanitizers: vec![declassify, to_finance, input_only],
            casts: vec![],
        })
        .unwrap()
    }

    fn check(registry: &Registry, log: &[Fact]) -> ReturnCheck {
        let projection = build(log);
        check_child_return(registry, &projection.view(&parent()), &child()).unwrap()
    }

    #[test]
    fn a_non_narrowing_raw_return_is_allowed() {
        let log = forked(known(SUSPICIOUS, internal()));
        assert_eq!(check(&registry(), &log), ReturnCheck::Allow);
        let mut log = forked(known(SUSPICIOUS, internal()));
        log.push(admit(child(), known(TRUSTED, internal())));
        assert_eq!(check(&registry(), &log), ReturnCheck::Allow);
    }

    #[test]
    fn a_narrowing_raw_return_is_blocked_with_accept_always_offered() {
        let mut log = forked(known(TRUSTED, Audience::Public));
        log.push(admit(child(), known(SUSPICIOUS, internal())));
        match check(&registry(), &log) {
            ReturnCheck::Block(ReturnBlock { narrowing, plans }) => {
                assert_eq!(narrowing.from, established(TRUSTED, Audience::Public));
                assert_eq!(narrowing.to, established(SUSPICIOUS, internal()));
                assert_eq!(
                    plans,
                    vec![
                        ReturnPlan::Accept(Narrowing {
                            from: established(TRUSTED, Audience::Public),
                            to: established(SUSPICIOUS, internal()),
                        }),
                        ReturnPlan::Sanitize {
                            sanitizer: SanitizerName::new("declassify"),
                            residual: Some(Narrowing {
                                from: established(TRUSTED, Audience::Public),
                                to: established(SUSPICIOUS, Audience::Public),
                            }),
                        },
                    ]
                );
            }
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[test]
    fn an_audience_only_narrowing_offers_the_clearing_sanitizer_standalone() {
        let mut log = forked(known(SUSPICIOUS, Audience::Public));
        log.push(admit(child(), known(SUSPICIOUS, internal())));
        match check(&menu_registry(), &log) {
            ReturnCheck::Block(ReturnBlock { plans, .. }) => {
                assert_eq!(
                    plans,
                    vec![
                        ReturnPlan::Accept(Narrowing {
                            from: established(SUSPICIOUS, Audience::Public),
                            to: established(SUSPICIOUS, internal()),
                        }),
                        ReturnPlan::Sanitize {
                            sanitizer: SanitizerName::new("declassify"),
                            residual: None,
                        },
                    ]
                );
            }
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[test]
    fn every_return_menu_fits_the_catalogue_bound() {
        let bound = 3;
        for parent in [
            known(TRUSTED, Audience::Public),
            known(SUSPICIOUS, Audience::Public),
            known(TRUSTED, internal()),
        ] {
            let mut log = forked(parent);
            log.push(admit(child(), known(SUSPICIOUS, internal())));
            match check(&menu_registry(), &log) {
                ReturnCheck::Block(ReturnBlock { plans, .. }) => {
                    assert!(!plans.is_empty(), "acceptance is always offered");
                    assert!(plans.len() <= bound);
                }
                other => panic!("expected a narrowing block, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_worse_or_equal_relabel_is_never_offered() {
        let mut log = forked(known(TRUSTED, internal()));
        log.push(admit(child(), known(SUSPICIOUS, internal())));
        match check(&menu_registry(), &log) {
            ReturnCheck::Block(ReturnBlock { plans, .. }) => {
                assert_eq!(
                    plans,
                    vec![ReturnPlan::Accept(Narrowing {
                        from: established(TRUSTED, internal()),
                        to: established(SUSPICIOUS, internal()),
                    })]
                );
            }
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[test]
    fn an_inapplicable_sanitizer_is_not_offered() {
        let mut log = forked(known(SUSPICIOUS, Audience::Public));
        log.push(admit(
            child(),
            known(SUSPICIOUS, Audience::restricted([ReaderId::new("finance")])),
        ));
        match check(&registry(), &log) {
            ReturnCheck::Block(ReturnBlock { plans, .. }) => {
                assert!(matches!(plans.as_slice(), [ReturnPlan::Accept(_)]))
            }
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_dimension_crosses_as_identity_not_a_block() {
        let mut log = forked(known(TRUSTED, Audience::Public));
        log.push(Fact::ValueAdmitted {
            trajectory: child(),
            value: LabeledValue::new(
                ValueBody::new("body"),
                Label::new(Dim::Unknown, Dim::Known(Audience::Public)),
            ),
            provenance: Provenance::UserInput,
        });
        let unknown_value = ValueId::new(1);
        assert_eq!(check(&cast_registry(), &log), ReturnCheck::Allow);
        assert_eq!(resolve(&log, &parent(), 1), Err(CastError::ForeignValue));

        let projection = build(&log);
        let ret = submit_child_return(&cast_registry(), &projection.view(&parent()), &child(), raw("found")).unwrap();
        log.extend(ret);
        let projection = build(&log);
        let parent_label = projection.view(&parent()).current_label();
        assert_eq!(parent_label.bound(), &established(TRUSTED, Audience::Public));
        assert_eq!(
            parent_label.unresolved(Dimension::Trust).collect::<Vec<_>>(),
            vec![unknown_value]
        );
        assert!(parent_label.is_established(Dimension::Audience));

        let batch = resolve(&log, &parent(), 1).expect("a merge-carried identity is in reach");
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
        log.push(Fact::ValueAdmitted {
            trajectory: child(),
            value: LabeledValue::new(ValueBody::new("body"), Label::new(Dim::Unknown, Dim::Unknown)),
            provenance: Provenance::UserInput,
        });
        log.push(Fact::ValueAdmitted {
            trajectory: parent(),
            value: LabeledValue::new(ValueBody::new("body"), Label::new(Dim::Known(TRUSTED), Dim::Unknown)),
            provenance: Provenance::UserInput,
        });
        assert_eq!(check(&registry(), &log), ReturnCheck::Allow);
        let projection = build(&log);
        let ret = submit_child_return(&registry(), &projection.view(&parent()), &child(), raw("found")).unwrap();
        log.extend(ret);
        let label = build(&log).view(&parent()).current_label();
        assert_eq!(
            label.unresolved(Dimension::Trust).collect::<Vec<_>>(),
            vec![ValueId::new(1)]
        );
        assert_eq!(
            label.unresolved(Dimension::Audience).collect::<Vec<_>>(),
            vec![ValueId::new(1), ValueId::new(2)]
        );
    }

    #[test]
    fn absorption_activates_only_at_the_merge() {
        let mut log = forked(known(TRUSTED, Audience::Public));
        log.push(admit(child(), known(SUSPICIOUS, Audience::Public)));
        log.push(Fact::ValueAdmitted {
            trajectory: child(),
            value: LabeledValue::new(ValueBody::new("body"), Label::new(Dim::Known(SUSPICIOUS), Dim::Unknown)),
            provenance: Provenance::UserInput,
        });
        let unknown_value = ValueId::new(2);
        let blocked = check(&registry(), &log);
        let narrowing = match &blocked {
            ReturnCheck::Block(ReturnBlock { narrowing, plans }) => {
                assert!(matches!(plans.as_slice(), [ReturnPlan::Accept(_)]));
                narrowing.clone()
            }
            other => panic!("expected a narrowing block, got {other:?}"),
        };
        assert!(
            build(&log)
                .view(&parent())
                .current_label()
                .is_established(Dimension::Audience)
        );
        assert_eq!(resolve(&log, &parent(), 2), Err(CastError::ForeignValue));

        let batch = execute(
            &registry(),
            &log,
            &ReturnPlan::Accept(narrowing),
            ReturnSubmission::Raw {
                body: ValueBody::new("found"),
            },
        )
        .unwrap();
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

    fn masking_registry() -> Registry {
        let declassify = Sanitizer {
            name: SanitizerName::new("declassify"),
            on: SanitizerPoints {
                input: false,
                output: true,
            },
            transition: Transition::Audience {
                from_includes: internal(),
                to: Audience::Public,
            },
            scope: Scope::default(),
            hint: None,
        };
        Registry::build_covered(RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![],
            authorities: vec![],
            sanitizers: vec![declassify],
            casts: vec![Cast {
                name: CastName::new("classify"),
                resolution: CastResolution::Constant(established(SUSPICIOUS, internal())),
                scope: Scope::default(),
            }],
        })
        .unwrap()
    }

    fn classify(registry: &Registry, log: &[Fact], actor: &TrajectoryId) -> Result<Vec<Fact>, CastError> {
        admit_cast(
            registry,
            &build(log).view(actor),
            ValueId::new(1),
            CastAnswer {
                cast: CastName::new("classify"),
                resolved: established(SUSPICIOUS, internal()),
            },
        )
    }

    fn masked_crossing(registry: &Registry) -> Vec<Fact> {
        let mut log = forked_bound(known(TRUSTED, Audience::Public), sanitized_policy());
        log.push(Fact::ValueAdmitted {
            trajectory: child(),
            value: LabeledValue::new(ValueBody::new("body"), Label::new(Dim::Unknown, Dim::Known(internal()))),
            provenance: Provenance::UserInput,
        });
        let projection = build(&log);
        let ret = submit_child_return(
            registry,
            &projection.view(&parent()),
            &child(),
            ReturnSubmission::Derived {
                body: ValueBody::new("redacted"),
                raw_digest: RawResultDigest::of(b"secret"),
            },
        )
        .expect("the transitioned dimension is established, so the crossing derives");
        log.extend(ret);
        log
    }

    #[test]
    fn a_sanitized_crossing_rides_the_untouched_dimension_through_masked() {
        let registry = masking_registry();
        let mut log = masked_crossing(&registry);
        let parent_label = build(&log).view(&parent()).current_label();
        assert_eq!(parent_label.bound(), &established(TRUSTED, Audience::Public));
        assert_eq!(
            parent_label.unresolved(Dimension::Trust).collect::<Vec<_>>(),
            vec![ValueId::new(1)]
        );

        let batch = classify(&registry, &log, &parent()).expect("the parent resolves the merge-carried identity");
        log.extend(batch);
        let projection = build(&log);
        assert_eq!(
            projection.view(&parent()).current_label(),
            partial(SUSPICIOUS, Audience::Public)
        );
        assert_eq!(
            projection.view(&child()).current_label(),
            partial(SUSPICIOUS, internal())
        );
    }

    #[test]
    fn a_sanitized_crossing_with_an_unresolved_transitioned_dimension_is_refused() {
        let mut log = forked_bound(known(TRUSTED, internal()), sanitized_policy());
        log.push(Fact::ValueAdmitted {
            trajectory: child(),
            value: LabeledValue::new(ValueBody::new("body"), Label::new(Dim::Known(TRUSTED), Dim::Unknown)),
            provenance: Provenance::UserInput,
        });
        let projection = build(&log);
        assert_eq!(
            submit_child_return(
                &registry(),
                &projection.view(&parent()),
                &child(),
                ReturnSubmission::Derived {
                    body: ValueBody::new("redacted"),
                    raw_digest: RawResultDigest::of(b"secret"),
                },
            ),
            Err(BranchError::ReturnFoldUnestablished)
        );
    }

    #[test]
    fn a_later_fork_inherits_the_absorbed_identities_masked() {
        let registry = masking_registry();
        let mut log = masked_crossing(&registry);
        let sibling = TrajectoryId::new("sibling");
        let seed = fork_records(&log, &parent(), &sibling, ReturnPolicy::Raw);
        log.extend(seed);

        let projection = build(&log);
        let seeded = projection.view(&sibling).current_label();
        assert_eq!(seeded, projection.view(&parent()).current_label());
        let snapshot = snapshot_of(&log, &sibling);
        assert_eq!(snapshot.seed(), &seeded);
        assert!(
            !snapshot.inherited().contains(&ValueId::new(1)),
            "an absorbed identity seeds the pin but never joins the inherited set"
        );

        let batch = classify(&registry, &log, &sibling).expect("the absorbed identity is in the fork's reach");
        log.extend(batch);
        let projection = build(&log);
        assert_eq!(
            projection.view(&parent()).current_label(),
            partial(SUSPICIOUS, Audience::Public)
        );
        assert_eq!(
            projection.view(&sibling).current_label(),
            partial(SUSPICIOUS, Audience::Public)
        );
    }

    #[test]
    fn a_known_return_merges_into_an_unknown_parent() {
        let mut log = forked(known(TRUSTED, Audience::Public));
        log.push(Fact::ValueAdmitted {
            trajectory: parent(),
            value: LabeledValue::new(ValueBody::new("body"), Label::new(Dim::Known(TRUSTED), Dim::Unknown)),
            provenance: Provenance::UserInput,
        });
        assert_eq!(check(&registry(), &log), ReturnCheck::Allow);

        let projection = build(&log);
        let batch = submit_child_return(&registry(), &projection.view(&parent()), &child(), raw("result"))
            .expect("a known return merges into an Unknown parent");
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
    fn a_return_check_for_a_non_child_is_refused() {
        let log = vec![admit(parent(), known(TRUSTED, Audience::Public))];
        let projection = build(&log);
        assert_eq!(
            check_child_return(&registry(), &projection.view(&parent()), &TrajectoryId::new("stranger")),
            Err(BranchError::NotDirectParent)
        );
    }

    fn blocked_family() -> Vec<Fact> {
        let mut log = forked(known(TRUSTED, Audience::Public));
        log.push(admit(child(), known(SUSPICIOUS, internal())));
        log
    }

    fn execute(
        registry: &Registry,
        log: &[Fact],
        chosen: &ReturnPlan,
        submission: ReturnSubmission,
    ) -> Result<Vec<Fact>, BranchError> {
        let projection = build(log);
        execute_child_return_plan(
            registry,
            &projection.view(&parent()),
            &child(),
            chosen.clone(),
            submission,
        )
    }

    fn accept_blocked_family() -> ReturnPlan {
        ReturnPlan::Accept(Narrowing {
            from: established(TRUSTED, Audience::Public),
            to: established(SUSPICIOUS, internal()),
        })
    }

    #[test]
    fn executing_accept_merges_raw_with_a_return_scoped_acceptance() {
        let mut log = blocked_family();
        let batch = execute(
            &registry(),
            &log,
            &accept_blocked_family(),
            ReturnSubmission::Raw {
                body: ValueBody::new("findings"),
            },
        )
        .unwrap();
        assert!(matches!(
            &batch[0],
            Fact::ChildReturn {
                derivation: ReturnDerivation::Raw,
                ..
            }
        ));
        match &batch[1] {
            Fact::ChildReturnAcceptance {
                trajectory,
                child_return,
                narrowing,
            } => {
                assert_eq!(trajectory, &parent());
                assert_eq!(child_return, &ChildReturnId::new(child(), 0));
                assert_eq!(narrowing.from, established(TRUSTED, Audience::Public));
                assert_eq!(narrowing.to, established(SUSPICIOUS, internal()));
            }
            other => panic!("expected ChildReturnAcceptance, got {other:?}"),
        }
        assert!(matches!(&batch[2], Fact::ValueAdmitted { .. }));
        assert!(matches!(
            &batch[3],
            Fact::Boundary {
                kind: BoundaryKind::Merge { .. },
                ..
            }
        ));
        log.extend(batch);
        let projection = build(&log);
        assert_eq!(
            projection.view(&parent()).current_label(),
            partial(SUSPICIOUS, internal())
        );
    }

    #[test]
    fn executing_sanitize_then_accept_merges_the_derivation_with_the_residual() {
        let mut log = blocked_family();
        let chosen = ReturnPlan::Sanitize {
            sanitizer: SanitizerName::new("declassify"),
            residual: Some(Narrowing {
                from: established(TRUSTED, Audience::Public),
                to: established(SUSPICIOUS, Audience::Public),
            }),
        };
        let batch = execute(
            &registry(),
            &log,
            &chosen,
            ReturnSubmission::Derived {
                body: ValueBody::new("redacted"),
                raw_digest: RawResultDigest::of(b"findings"),
            },
        )
        .unwrap();
        match &batch[1] {
            Fact::ChildReturnAcceptance { narrowing, .. } => {
                assert_eq!(narrowing.to, established(SUSPICIOUS, Audience::Public));
            }
            other => panic!("expected ChildReturnAcceptance, got {other:?}"),
        }
        log.extend(batch);
        let projection = build(&log);
        assert_eq!(
            projection.view(&parent()).current_label(),
            partial(SUSPICIOUS, Audience::Public)
        );
        assert!(log.iter().any(|f| matches!(
            f,
            Fact::ChildReturn {
                derivation: ReturnDerivation::Sanitized { .. },
                ..
            }
        )));
    }

    #[test]
    fn executing_a_standalone_sanitize_needs_no_acceptance() {
        let mut log = forked(known(SUSPICIOUS, Audience::Public));
        log.push(admit(child(), known(SUSPICIOUS, internal())));
        let batch = execute(
            &registry(),
            &log,
            &ReturnPlan::Sanitize {
                sanitizer: SanitizerName::new("declassify"),
                residual: None,
            },
            ReturnSubmission::Derived {
                body: ValueBody::new("redacted"),
                raw_digest: RawResultDigest::of(b"findings"),
            },
        )
        .unwrap();
        assert!(!batch.iter().any(|f| matches!(f, Fact::ChildReturnAcceptance { .. })));
        log.extend(batch);
        let projection = build(&log);
        assert_eq!(
            projection.view(&parent()).current_label(),
            partial(SUSPICIOUS, Audience::Public)
        );
    }

    #[test]
    fn an_unoffered_plan_is_refused() {
        let log = blocked_family();
        assert_eq!(
            execute(
                &registry(),
                &log,
                &ReturnPlan::Sanitize {
                    sanitizer: SanitizerName::new("declassify"),
                    residual: None,
                },
                ReturnSubmission::Derived {
                    body: ValueBody::new("redacted"),
                    raw_digest: RawResultDigest::of(b"findings"),
                },
            ),
            Err(BranchError::ReturnPlanNotOffered)
        );
        assert_eq!(
            execute(
                &registry(),
                &log,
                &ReturnPlan::Sanitize {
                    sanitizer: SanitizerName::new("declassify"),
                    residual: Some(Narrowing {
                        from: established(TRUSTED, internal()),
                        to: established(SUSPICIOUS, internal()),
                    }),
                },
                ReturnSubmission::Derived {
                    body: ValueBody::new("redacted"),
                    raw_digest: RawResultDigest::of(b"findings"),
                },
            ),
            Err(BranchError::ReturnPlanNotOffered)
        );
    }

    #[test]
    fn a_mismatched_submission_is_refused() {
        let log = blocked_family();
        assert_eq!(
            execute(
                &registry(),
                &log,
                &accept_blocked_family(),
                ReturnSubmission::Derived {
                    body: ValueBody::new("redacted"),
                    raw_digest: RawResultDigest::of(b"findings"),
                },
            ),
            Err(BranchError::SubmissionMismatch)
        );
    }

    #[test]
    fn a_moved_family_refuses_the_offer_by_value_not_by_identity() {
        let log = blocked_family();
        let mut converged = log.clone();
        converged.push(admit(parent(), known(SUSPICIOUS, internal())));
        assert_eq!(
            execute(
                &registry(),
                &converged,
                &accept_blocked_family(),
                ReturnSubmission::Raw {
                    body: ValueBody::new("findings"),
                },
            ),
            Err(BranchError::ReturnOfferStale)
        );
        let mut punctuated = log.clone();
        punctuated.push(Fact::Boundary {
            trajectory: parent(),
            kind: BoundaryKind::TurnEnd,
        });
        assert!(
            execute(
                &registry(),
                &punctuated,
                &accept_blocked_family(),
                ReturnSubmission::Raw {
                    body: ValueBody::new("findings"),
                },
            )
            .is_ok()
        );
    }

    #[test]
    fn an_executed_plan_consumes_the_childs_return_channel() {
        let mut log = blocked_family();
        let batch = execute(
            &registry(),
            &log,
            &accept_blocked_family(),
            ReturnSubmission::Raw {
                body: ValueBody::new("findings"),
            },
        )
        .unwrap();
        log.extend(batch);
        assert_eq!(
            execute(
                &registry(),
                &log,
                &ReturnPlan::Sanitize {
                    sanitizer: SanitizerName::new("declassify"),
                    residual: Some(Narrowing {
                        from: established(TRUSTED, Audience::Public),
                        to: established(SUSPICIOUS, Audience::Public),
                    }),
                },
                ReturnSubmission::Derived {
                    body: ValueBody::new("redacted"),
                    raw_digest: RawResultDigest::of(b"findings"),
                },
            ),
            Err(BranchError::AlreadyEnded)
        );

        let mut log = blocked_family();
        let batch = execute(
            &registry(),
            &log,
            &ReturnPlan::Sanitize {
                sanitizer: SanitizerName::new("declassify"),
                residual: Some(Narrowing {
                    from: established(TRUSTED, Audience::Public),
                    to: established(SUSPICIOUS, Audience::Public),
                }),
            },
            ReturnSubmission::Derived {
                body: ValueBody::new("redacted"),
                raw_digest: RawResultDigest::of(b"findings"),
            },
        )
        .unwrap();
        log.extend(batch);
        assert_eq!(
            execute(
                &registry(),
                &log,
                &accept_blocked_family(),
                ReturnSubmission::Raw {
                    body: ValueBody::new("findings"),
                },
            ),
            Err(BranchError::AlreadyEnded)
        );
    }

    #[test]
    fn a_second_return_from_one_child_is_refused() {
        let mut log = forked(known(SUSPICIOUS, internal()));
        let projection = build(&log);
        let ret = submit_child_return(&registry(), &projection.view(&parent()), &child(), raw("first")).unwrap();
        log.extend(ret);
        let projection = build(&log);
        assert_eq!(
            submit_child_return(&registry(), &projection.view(&parent()), &child(), raw("second")),
            Err(BranchError::AlreadyEnded)
        );
        assert_eq!(
            check_child_return(&registry(), &projection.view(&parent()), &child()),
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
            submit_child_return(&registry(), &projection.view(&parent()), &child(), raw("late")),
            Err(BranchError::AlreadyEnded)
        );
        assert_eq!(
            check_child_return(&registry(), &projection.view(&parent()), &child()),
            Err(BranchError::AlreadyEnded)
        );
    }

    #[test]
    fn competing_terminals_linearize_to_at_most_one() {
        let mut log = forked(known(SUSPICIOUS, internal()));
        let projection = build(&log);
        let ret = submit_child_return(&registry(), &projection.view(&parent()), &child(), raw("finding")).unwrap();
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
        let log = forked(known(TRUSTED, internal()));
        let projection = build(&log);
        assert_eq!(
            submit_child_return(
                &registry(),
                &projection.view(&parent()),
                &child(),
                ReturnSubmission::Derived {
                    body: ValueBody::new("redacted"),
                    raw_digest: RawResultDigest::of(b"x"),
                },
            ),
            Err(BranchError::ReturnPolicyMismatch)
        );
        let log = forked_bound(known(SUSPICIOUS, internal()), sanitized_policy());
        let projection = build(&log);
        assert_eq!(
            submit_child_return(&registry(), &projection.view(&parent()), &child(), raw("leak")),
            Err(BranchError::ReturnPolicyMismatch)
        );
    }

    #[test]
    fn a_blocked_return_check_applies_only_under_a_raw_policy() {
        let log = forked_bound(known(TRUSTED, internal()), sanitized_policy());
        let projection = build(&log);
        assert_eq!(
            check_child_return(&registry(), &projection.view(&parent()), &child()),
            Err(BranchError::ReturnPolicyMismatch)
        );
    }

    #[test]
    fn return_facts_audit_their_derivation() {
        let log = forked(known(SUSPICIOUS, internal()));
        let projection = build(&log);
        let ret = submit_child_return(&registry(), &projection.view(&parent()), &child(), raw("secret")).unwrap();
        match &ret[0] {
            Fact::ChildReturn { derivation, .. } => assert_eq!(derivation, &ReturnDerivation::Raw),
            other => panic!("expected ChildReturn, got {other:?}"),
        }

        let log = forked_bound(known(SUSPICIOUS, internal()), sanitized_policy());
        let projection = build(&log);
        let ret = submit_child_return(
            &registry(),
            &projection.view(&parent()),
            &child(),
            ReturnSubmission::Derived {
                body: ValueBody::new("redacted"),
                raw_digest: RawResultDigest::of(b"secret"),
            },
        )
        .unwrap();
        match &ret[0] {
            Fact::ChildReturn { derivation, .. } => assert_eq!(
                derivation,
                &ReturnDerivation::Sanitized {
                    sanitizer: SanitizerName::new("declassify"),
                    raw_digest: RawResultDigest::of(b"secret"),
                    transition: Transition::Audience {
                        from_includes: internal(),
                        to: Audience::Public,
                    },
                }
            ),
            other => panic!("expected ChildReturn, got {other:?}"),
        }
    }

    #[test]
    fn sanitized_return_with_unmet_from_is_refused() {
        let finance = Audience::restricted([ReaderId::new("finance")]);
        let log = forked_bound(known(TRUSTED, finance), sanitized_policy());
        let projection = build(&log);
        assert_eq!(
            submit_child_return(
                &registry(),
                &projection.view(&parent()),
                &child(),
                ReturnSubmission::Derived {
                    body: ValueBody::new("x"),
                    raw_digest: RawResultDigest::of(b"secret"),
                },
            ),
            Err(BranchError::TransitionSourceUnmet)
        );
    }

    #[test]
    fn merge_admits_the_returned_label_and_the_parent_fold_still_combines() {
        let mut log = forked_bound(known(SUSPICIOUS, internal()), sanitized_policy());
        let values_before = log.iter().filter(|f| matches!(f, Fact::ValueAdmitted { .. })).count();
        let projection = build(&log);
        let ret = submit_child_return(
            &registry(),
            &projection.view(&parent()),
            &child(),
            ReturnSubmission::Derived {
                body: ValueBody::new("redacted"),
                raw_digest: RawResultDigest::of(b"secret"),
            },
        )
        .unwrap();
        log.extend(ret);
        let projection = build(&log);
        assert_eq!(
            projection.value_label(ValueId::new(values_before as u64)),
            Some(&known(SUSPICIOUS, Audience::Public))
        );
        assert_eq!(
            projection.view(&parent()).current_label(),
            partial(SUSPICIOUS, internal())
        );
    }

    #[test]
    fn a_return_submitted_toward_a_stranger_is_refused() {
        let log = forked(known(TRUSTED, Audience::Public));
        let projection = build(&log);
        let stranger = TrajectoryId::new("stranger");
        assert_eq!(
            submit_child_return(&registry(), &projection.view(&stranger), &child(), raw("r")),
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
            dynamic_resolutions: Vec::new(),
            subject: None,
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
            tools: vec![],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![Cast {
                name: CastName::new("classify"),
                resolution: CastResolution::Constant(established(SUSPICIOUS, Audience::Public)),
                scope: Scope::default(),
            }],
        })
        .unwrap()
    }

    fn unknown_source(trajectory: TrajectoryId) -> Fact {
        admit(trajectory, Label::new(Dim::Unknown, Dim::Known(Audience::Public)))
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
        let mut log = vec![unknown_source(parent())];
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
        let mut log = vec![unknown_source(parent())];
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
        let mut log = vec![unknown_source(parent())];
        fork_under(&mut log, &parent(), &child());
        fork_under(&mut log, &parent(), &sibling);
        log.push(unknown_source(parent()));
        log.push(unknown_source(child()));

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
        let mut log = vec![unknown_source(parent())];
        fork_under(&mut log, &parent(), &child());
        log.push(unknown_source(child()));
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
        let mut log = vec![unknown_source(parent())];
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
            dynamic_resolutions: Vec::new(),
            subject: None,
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
