//! Branching label semantics over one shared family log.

use thiserror::Error;

use crate::check::{Narrowing, UnestablishedFact};
use crate::fact::{BoundaryKind, Fact, FactBatch, ReturnDerivation, ReturnPolicy};
use crate::label::{Adequacy, Dim, Dimension, Label};
use crate::names::SanitizerName;
use crate::projection::Views;
use crate::registry::Registry;
use crate::value::{ChildReturnId, LabeledValue, Provenance, RawResultDigest, TrajectoryId, ValueBody};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BranchError {
    #[error(
        "the deployment does not control child context — branching exists only in context-controlling deployments"
    )]
    ContextUncontrolled,
    #[error("a trajectory cannot fork itself")]
    SelfFork,
    #[error("the child is already forked from a parent (reparenting refused)")]
    AlreadyForked,
    #[error("the parent's current label has an unresolved dimension — resolve it before forking")]
    ParentUnresolved,
    #[error("the fork parent already ended its errand — an ended branch cannot fork")]
    ParentEnded,
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
    #[error("the child fold has an unestablished dimension — a cast establishes it, then the return crosses")]
    ReturnFoldUnestablished,
    #[error("a raw return that narrows the parent merges only through an executed return plan")]
    ReturnNarrowsParent,
}

/// Seed a child branch at the parent's current label with an immutable, unique `Fork` binding
/// carrying the child's [`ReturnPolicy`]. Refuses a self-fork, a re-fork of an already-bound
/// child, a fork at an unresolved parent label (a child cannot inherit an Unknown it has no value
/// to cast), and a policy naming an unregistered transformer. The batch is on the family's
/// revision.
pub(crate) fn seed_child(
    registry: &Registry,
    parent: &Views,
    child: &TrajectoryId,
    return_policy: ReturnPolicy,
) -> Result<FactBatch, BranchError> {
    if !registry.profile().context_control() {
        return Err(BranchError::ContextUncontrolled);
    }
    if child == parent.trajectory() {
        return Err(BranchError::SelfFork);
    }
    if parent.parent_of(child).is_some() {
        return Err(BranchError::AlreadyForked);
    }
    if parent.has_ended(parent.trajectory()) {
        return Err(BranchError::ParentEnded);
    }
    match &return_policy {
        ReturnPolicy::Raw => {}
        ReturnPolicy::Sanitized(name) => {
            let registered = registry
                .sanitizer(name)
                .ok_or_else(|| BranchError::UnknownSanitizer(name.as_str().to_string()))?;
            if !registered.on.output {
                return Err(BranchError::SanitizerNotOutput(name.as_str().to_string()));
            }
        }
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
            return_policy,
        },
    };
    Ok(FactBatch::new(parent.revision(), vec![fact]))
}

pub(crate) fn submit_child_return(
    registry: &Registry,
    parent: &Views,
    child: &TrajectoryId,
    ret: ReturnSubmission,
) -> Result<FactBatch, BranchError> {
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
                ReturnCheck::Block(ReturnBlock::Unestablished(_)) => return Err(BranchError::ReturnFoldUnestablished),
                ReturnCheck::Block(ReturnBlock::Narrowing { .. }) => return Err(BranchError::ReturnNarrowsParent),
            }
            (LabeledValue::new(body, fold.clone()), ReturnDerivation::Raw)
        }
        (ReturnPolicy::Sanitized(bound), ReturnSubmission::Derived { body, raw_digest }) => {
            sanitized_crossing(registry, &fold, &bound, body, raw_digest)?
        }
        _ => return Err(BranchError::ReturnPolicyMismatch),
    };
    Ok(FactBatch::new(
        parent.revision(),
        crossing_facts(parent, child, value, derivation, None),
    ))
}

/// Record a child's **void return**: the child-attributed terminal ends the branch and
/// crosses no value — no merge, no label contribution. Refused for a non-child and for a branch
/// that already ended by value or void. The batch is on the family's revision, so
/// competing terminals linearize at the store's revisioned append and at most one lands.
pub(crate) fn submit_void_return(parent: &Views, child: &TrajectoryId) -> Result<FactBatch, BranchError> {
    match parent.parent_of(child) {
        Some(direct) if direct == parent.trajectory() => {}
        _ => return Err(BranchError::NotDirectParent),
    }
    if parent.has_ended(child) {
        return Err(BranchError::AlreadyEnded);
    }
    Ok(FactBatch::new(
        parent.revision(),
        vec![Fact::Boundary {
            trajectory: child.clone(),
            kind: BoundaryKind::VoidReturn,
        }],
    ))
}

/// The one place a return's facts are assembled: the child's `ChildReturn` record, the optional
/// return-scoped acceptance, the parent's `ValueAdmitted` under the returned value's own label,
/// and the `Merge` boundary — always one batch, never split across commit points. The parent
/// *fold* absorbs the crossing at projection (intersect readers, min trust) — identical to folding
/// `parent.combine(returned)`, since `combine` is idempotent — while the stored per-value label
/// stays the value's intrinsic one, so authority review context and cast targeting see what the
/// value *is*, not the parent's unrelated restrictions.
fn crossing_facts(
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

/// What blocked the return. The two causes are structurally disjoint: an Unknown dimension is
/// absorbing under `combine` but not an ordered restriction, so it can never form a narrowing —
/// a block is one or the other, never both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReturnBlock {
    Narrowing {
        narrowing: Narrowing,
        plans: Vec<ReturnPlan>,
    },
    Unestablished(Vec<UnestablishedFact>),
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

    let unestablished = child_fold_unestablished(parent, child);
    if !unestablished.is_empty() {
        return Ok(ReturnCheck::Block(ReturnBlock::Unestablished(unestablished)));
    }

    let candidate = current.combine(&fold);
    if candidate == current {
        return Ok(ReturnCheck::Allow);
    }
    let narrowing = Narrowing {
        from: current.clone(),
        to: candidate.clone(),
    };

    let mut plans = vec![ReturnPlan::Accept(narrowing.clone())];
    if !registry.profile().confines_child_return() {
        return Ok(ReturnCheck::Block(ReturnBlock::Narrowing { narrowing, plans }));
    }
    for sanitizer in registry.sanitizers() {
        if !sanitizer.on.output {
            continue;
        }
        if sanitizer.transition.admits(&fold) != Adequacy::Holds {
            continue;
        }
        let sanitized = sanitizer.transition.derive(&fold);
        let merged = current.combine(&sanitized);
        if merged == current {
            plans.push(ReturnPlan::Sanitize {
                sanitizer: sanitizer.name.clone(),
                residual: None,
            });
        } else if strictly_improves(&candidate, &merged) {
            plans.push(ReturnPlan::Sanitize {
                sanitizer: sanitizer.name.clone(),
                residual: Some(Narrowing {
                    from: current.clone(),
                    to: merged,
                }),
            });
        }
    }
    Ok(ReturnCheck::Block(ReturnBlock::Narrowing { narrowing, plans }))
}

fn strictly_improves(candidate: &Label, merged: &Label) -> bool {
    &candidate.combine(merged) == candidate && merged != candidate
}

/// The child fold's unestablished facts — the values a cast must establish before this child's
/// return can merge. Policy-independent by design: the runtime resolves them *before*
/// the return-policy split, so a bound sanitizer's crossing gets the same resolution a raw one
/// does (a sanitizer transforms content, never establishes a label fact).
pub(crate) fn child_fold_unestablished(parent: &Views, child: &TrajectoryId) -> Vec<UnestablishedFact> {
    let fold = parent.branch_label(child);
    let mut unestablished = Vec::new();
    unestablished_dims(parent, child, &fold, &mut unestablished);
    unestablished
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

pub(crate) fn execute_child_return_plan(
    registry: &Registry,
    parent: &Views,
    child: &TrajectoryId,
    chosen: ReturnPlan,
    submission: ReturnSubmission,
) -> Result<FactBatch, BranchError> {
    let plans = match check_child_return(registry, parent, child)? {
        ReturnCheck::Block(ReturnBlock::Narrowing { plans, .. }) => plans,
        // Allow or unestablished: the state moved since the offer — nothing here to execute.
        ReturnCheck::Allow | ReturnCheck::Block(ReturnBlock::Unestablished(_)) => {
            return Err(BranchError::ReturnOfferStale);
        }
    };
    if !plans.contains(&chosen) {
        return Err(BranchError::ReturnPlanNotOffered);
    }

    let fold = parent.branch_label(child);
    let (value, derivation, acceptance) = match (chosen, submission) {
        (ReturnPlan::Accept(narrowing), ReturnSubmission::Raw { body }) => (
            LabeledValue::new(body, fold.clone()),
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

    Ok(FactBatch::new(
        parent.revision(),
        crossing_facts(parent, child, value, derivation, acceptance),
    ))
}

fn sanitized_crossing(
    registry: &Registry,
    fold: &Label,
    sanitizer: &SanitizerName,
    body: ValueBody,
    raw_digest: RawResultDigest,
) -> Result<(LabeledValue, ReturnDerivation), BranchError> {
    let registered = registry
        .sanitizer(sanitizer)
        .ok_or_else(|| BranchError::UnknownSanitizer(sanitizer.as_str().to_string()))?;
    if matches!(fold.trust, Dim::Unknown) || matches!(fold.audience, Dim::Unknown) {
        return Err(BranchError::ReturnFoldUnestablished);
    }
    if !registered.on.output {
        return Err(BranchError::SanitizerNotOutput(sanitizer.as_str().to_string()));
    }
    if registered.transition.admits(fold) != Adequacy::Holds {
        return Err(BranchError::TransitionSourceUnmet);
    }
    let value = LabeledValue::new(body, registered.transition.derive(fold));
    let derivation = ReturnDerivation::Sanitized {
        sanitizer: sanitizer.clone(),
        raw_digest,
        transition: registered.transition.clone(),
    };
    Ok((value, derivation))
}

fn unestablished_dims(views: &Views, trajectory: &TrajectoryId, fold: &Label, out: &mut Vec<UnestablishedFact>) {
    let trust_unknown = matches!(fold.trust, Dim::Unknown);
    let audience_unknown = matches!(fold.audience, Dim::Unknown);
    if !trust_unknown && !audience_unknown {
        return;
    }
    for (id, label) in views.branch_values_of(trajectory) {
        if trust_unknown && matches!(label.trust, Dim::Unknown) {
            out.push(UnestablishedFact {
                value: id,
                dimension: Dimension::Trust,
            });
        }
        if audience_unknown && matches!(label.audience, Dim::Unknown) {
            out.push(UnestablishedFact {
                value: id,
                dimension: Dimension::Audience,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::{Sanitizer, SanitizerPoints, Transition};
    use crate::fact::{CloseOutcome, EffectKind, EffectSet, Revision};
    use crate::label::{Audience, Label, ReaderId, Trust};
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
        Projection::build(log, Revision::new(log.len() as u64))
    }

    fn forked(parent_label: Label) -> Vec<Fact> {
        forked_bound(parent_label, ReturnPolicy::Raw)
    }

    fn forked_bound(parent_label: Label, policy: ReturnPolicy) -> Vec<Fact> {
        let mut log = vec![admit(parent(), parent_label)];
        let projection = build(&log);
        let seed = seed_child(&registry(), &projection.view(&parent()), &child(), policy).unwrap();
        log.extend(seed.facts);
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
        assert_eq!(projection.view(&child()).current_label(), known(SUSPICIOUS, internal()));
        assert_ne!(projection.view(&child()).current_label(), Label::top());
    }

    #[test]
    fn fork_refuses_self_reparent_and_unresolved_parent() {
        let log = vec![admit(parent(), known(TRUSTED, Audience::Public))];
        let projection = build(&log);
        assert_eq!(
            seed_child(&registry(), &projection.view(&parent()), &parent(), ReturnPolicy::Raw),
            Err(BranchError::SelfFork)
        );
        let log = forked(known(TRUSTED, Audience::Public));
        let projection = build(&log);
        let other = TrajectoryId::new("other");
        assert_eq!(
            seed_child(&registry(), &projection.view(&other), &child(), ReturnPolicy::Raw),
            Err(BranchError::AlreadyForked)
        );
        let log = vec![admit(parent(), Label::new(Dim::Unknown, Dim::Known(Audience::Public)))];
        let projection = build(&log);
        assert_eq!(
            seed_child(&registry(), &projection.view(&parent()), &child(), ReturnPolicy::Raw),
            Err(BranchError::ParentUnresolved)
        );
    }

    #[test]
    fn a_non_narrowing_raw_return_crosses_in_one_batch() {
        let mut log = forked(known(SUSPICIOUS, internal()));
        let projection = build(&log);
        let ret = submit_child_return(&registry(), &projection.view(&parent()), &child(), raw("secret")).unwrap();
        assert!(matches!(&ret.facts[0], Fact::ChildReturn { .. }));
        assert!(matches!(&ret.facts[1], Fact::ValueAdmitted { .. }));
        assert!(matches!(
            &ret.facts[2],
            Fact::Boundary {
                kind: BoundaryKind::Merge { .. },
                ..
            }
        ));
        log.extend(ret.facts);
        let projection = build(&log);
        match projection.view(&parent()).child_return(&ChildReturnId::new(child(), 0)) {
            Some(value) => assert_eq!(value.label, known(SUSPICIOUS, internal())),
            None => panic!("child return not recorded"),
        }
        assert_eq!(
            projection.view(&parent()).current_label(),
            known(SUSPICIOUS, internal())
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
        log.extend(ret.facts);
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
            known(SUSPICIOUS, internal())
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
            ReturnCheck::Block(ReturnBlock::Narrowing { narrowing, plans }) => {
                assert_eq!(narrowing.from, known(TRUSTED, Audience::Public));
                assert_eq!(narrowing.to, known(SUSPICIOUS, internal()));
                assert_eq!(
                    plans,
                    vec![
                        ReturnPlan::Accept(Narrowing {
                            from: known(TRUSTED, Audience::Public),
                            to: known(SUSPICIOUS, internal()),
                        }),
                        ReturnPlan::Sanitize {
                            sanitizer: SanitizerName::new("declassify"),
                            residual: Some(Narrowing {
                                from: known(TRUSTED, Audience::Public),
                                to: known(SUSPICIOUS, Audience::Public),
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
            ReturnCheck::Block(ReturnBlock::Narrowing { plans, .. }) => {
                assert_eq!(
                    plans,
                    vec![
                        ReturnPlan::Accept(Narrowing {
                            from: known(SUSPICIOUS, Audience::Public),
                            to: known(SUSPICIOUS, internal()),
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
                ReturnCheck::Block(ReturnBlock::Narrowing { plans, .. }) => {
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
            ReturnCheck::Block(ReturnBlock::Narrowing { plans, .. }) => {
                assert_eq!(
                    plans,
                    vec![ReturnPlan::Accept(Narrowing {
                        from: known(TRUSTED, internal()),
                        to: known(SUSPICIOUS, internal()),
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
            ReturnCheck::Block(ReturnBlock::Narrowing { plans, .. }) => {
                assert!(matches!(plans.as_slice(), [ReturnPlan::Accept(_)]))
            }
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_dimension_is_unestablished_not_a_narrowing() {
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
        match check(&registry(), &log) {
            ReturnCheck::Block(ReturnBlock::Unestablished(facts)) => {
                assert_eq!(
                    facts,
                    vec![UnestablishedFact {
                        value: unknown_value,
                        dimension: Dimension::Trust,
                    }]
                );
            }
            other => panic!("expected the unestablished block, got {other:?}"),
        }

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
        match check(&registry(), &log) {
            ReturnCheck::Block(ReturnBlock::Unestablished(facts)) => {
                assert_eq!(
                    facts,
                    vec![
                        UnestablishedFact {
                            value: ValueId::new(1),
                            dimension: Dimension::Trust,
                        },
                        UnestablishedFact {
                            value: ValueId::new(1),
                            dimension: Dimension::Audience,
                        },
                    ]
                );
            }
            other => panic!("expected the unestablished block, got {other:?}"),
        }
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
                .facts
                .iter()
                .any(|fact| matches!(fact, Fact::ChildReturn { trajectory, .. } if *trajectory == child()))
        );
        assert!(batch.facts.iter().any(|fact| matches!(
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
    ) -> Result<FactBatch, BranchError> {
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
            from: known(TRUSTED, Audience::Public),
            to: known(SUSPICIOUS, internal()),
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
            &batch.facts[0],
            Fact::ChildReturn {
                derivation: ReturnDerivation::Raw,
                ..
            }
        ));
        match &batch.facts[1] {
            Fact::ChildReturnAcceptance {
                trajectory,
                child_return,
                narrowing,
            } => {
                assert_eq!(trajectory, &parent());
                assert_eq!(child_return, &ChildReturnId::new(child(), 0));
                assert_eq!(narrowing.from, known(TRUSTED, Audience::Public));
                assert_eq!(narrowing.to, known(SUSPICIOUS, internal()));
            }
            other => panic!("expected ChildReturnAcceptance, got {other:?}"),
        }
        assert!(matches!(&batch.facts[2], Fact::ValueAdmitted { .. }));
        assert!(matches!(
            &batch.facts[3],
            Fact::Boundary {
                kind: BoundaryKind::Merge { .. },
                ..
            }
        ));
        log.extend(batch.facts);
        let projection = build(&log);
        assert_eq!(
            projection.view(&parent()).current_label(),
            known(SUSPICIOUS, internal())
        );
    }

    #[test]
    fn executing_sanitize_then_accept_merges_the_derivation_with_the_residual() {
        let mut log = blocked_family();
        let chosen = ReturnPlan::Sanitize {
            sanitizer: SanitizerName::new("declassify"),
            residual: Some(Narrowing {
                from: known(TRUSTED, Audience::Public),
                to: known(SUSPICIOUS, Audience::Public),
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
        match &batch.facts[1] {
            Fact::ChildReturnAcceptance { narrowing, .. } => {
                assert_eq!(narrowing.to, known(SUSPICIOUS, Audience::Public));
            }
            other => panic!("expected ChildReturnAcceptance, got {other:?}"),
        }
        log.extend(batch.facts);
        let projection = build(&log);
        assert_eq!(
            projection.view(&parent()).current_label(),
            known(SUSPICIOUS, Audience::Public)
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
        assert!(
            !batch
                .facts
                .iter()
                .any(|f| matches!(f, Fact::ChildReturnAcceptance { .. }))
        );
        log.extend(batch.facts);
        let projection = build(&log);
        assert_eq!(
            projection.view(&parent()).current_label(),
            known(SUSPICIOUS, Audience::Public)
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
                        from: known(TRUSTED, internal()),
                        to: known(SUSPICIOUS, internal()),
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
        log.extend(batch.facts);
        assert_eq!(
            execute(
                &registry(),
                &log,
                &ReturnPlan::Sanitize {
                    sanitizer: SanitizerName::new("declassify"),
                    residual: Some(Narrowing {
                        from: known(TRUSTED, Audience::Public),
                        to: known(SUSPICIOUS, Audience::Public),
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
                    from: known(TRUSTED, Audience::Public),
                    to: known(SUSPICIOUS, Audience::Public),
                }),
            },
            ReturnSubmission::Derived {
                body: ValueBody::new("redacted"),
                raw_digest: RawResultDigest::of(b"findings"),
            },
        )
        .unwrap();
        log.extend(batch.facts);
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
        log.extend(ret.facts);
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
    fn a_returned_child_cannot_become_a_fork_parent() {
        let mut log = forked(known(SUSPICIOUS, internal()));
        let projection = build(&log);
        let ret = submit_child_return(&registry(), &projection.view(&parent()), &child(), raw("finding")).unwrap();
        log.extend(ret.facts);
        let projection = build(&log);
        assert_eq!(
            seed_child(
                &registry(),
                &projection.view(&child()),
                &TrajectoryId::new("grandchild"),
                ReturnPolicy::Raw,
            )
            .map(|_| ()),
            Err(BranchError::ParentEnded)
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
            batch.facts,
            [Fact::Boundary {
                trajectory: child(),
                kind: BoundaryKind::VoidReturn,
            }]
        );
        log.extend(batch.facts);
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
        assert_eq!(
            seed_child(
                &registry(),
                &projection.view(&child()),
                &TrajectoryId::new("grandchild"),
                ReturnPolicy::Raw,
            )
            .map(|_| ()),
            Err(BranchError::ParentEnded)
        );
    }

    #[test]
    fn competing_terminals_linearize_to_at_most_one() {
        let mut log = forked(known(SUSPICIOUS, internal()));
        let projection = build(&log);
        let ret = submit_child_return(&registry(), &projection.view(&parent()), &child(), raw("finding")).unwrap();
        let competing = submit_void_return(&projection.view(&parent()), &child()).unwrap();
        assert_eq!(competing.basis, ret.basis);
        log.extend(ret.facts);
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
    fn a_fork_policy_naming_an_unregistered_transformer_is_refused() {
        let log = vec![admit(parent(), known(TRUSTED, internal()))];
        let projection = build(&log);
        assert_eq!(
            seed_child(
                &registry(),
                &projection.view(&parent()),
                &child(),
                ReturnPolicy::Sanitized(SanitizerName::new("ghost")),
            ),
            Err(BranchError::UnknownSanitizer("ghost".to_string()))
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
        match &ret.facts[0] {
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
        match &ret.facts[0] {
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
        log.extend(ret.facts);
        let projection = build(&log);
        assert_eq!(
            projection.value_label(ValueId::new(values_before as u64)),
            Some(&known(SUSPICIOUS, Audience::Public))
        );
        assert_eq!(
            projection.view(&parent()).current_label(),
            known(SUSPICIOUS, internal())
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
            proposed_label: Label::top(),
            proposed_effects: EffectSet::new([egress.clone()]).unwrap(),
            dynamic_resolutions: Vec::new(),
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
            proposed_label: Label::top(),
            proposed_effects: EffectSet::new([egress.clone()]).unwrap(),
            dynamic_resolutions: Vec::new(),
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
