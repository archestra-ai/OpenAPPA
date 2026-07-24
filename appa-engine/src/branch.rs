//! Branching label semantics over one shared family log.
//!
//! A child is **seeded** at the parent's current label (a `Fork` boundary with an immutable parent
//! binding carrying its return policy) — never at `top()`, so a child cannot start more permissive
//! than its parent, and never re-parented. A child **returns** through `submit_result` on the path
//! its fork policy binds — the engine **derives** the label (a raw return carries the child fold; a
//! sanitized return carries a mandate-validated audience relabel, trust preserved), never a caller
//! assertion, and the child's free final text does not cross. The crossing records, admits into the
//! direct parent under the engine-derived returned label — the parent *fold* absorbs it like any
//! other read (min trust, ∩ audience) — and lands its `Merge` boundary
//! as **one atomic batch** — value-granular, structurally unable to widen the parent, with no
//! orphanable intermediate state. Reparenting and cross-family crossings are
//! refused, a child returns **at most once** (its first crossing consumes the errand —
//! [`BranchError::AlreadyReturned`]), and a raw crossing that would narrow the parent exists only
//! through an executed return plan.
//!
//! Label folds are branch-local (each branch folds its own ancestry); the revision and the
//! effect/history views are family-wide, so an abandoned child's egress still trips a parent's
//! `no_prior`.

use thiserror::Error;

use crate::check::{Narrowing, UnresolvedFact};
use crate::fact::{BoundaryKind, Fact, FactBatch, ReturnDerivation, ReturnPolicy};
use crate::label::{Adequacy, Dim, Dimension, Label};
use crate::names::SanitizerName;
use crate::projection::Views;
use crate::registry::Registry;
use crate::value::{ChildReturnId, LabeledValue, Provenance, RawResultDigest, TrajectoryId, ValueBody};

/// Why a branch operation could not proceed.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum BranchError {
    #[error("a trajectory cannot fork itself")]
    SelfFork,
    #[error("the child is already forked from a parent (reparenting refused)")]
    AlreadyForked,
    #[error("the parent's current label has an unresolved dimension — resolve it before forking")]
    ParentUnresolved,
    #[error("the fork parent already returned its result — a returned child cannot fork")]
    ParentReturned,
    #[error("the child has already returned — a child returns at most once")]
    AlreadyReturned,
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
    #[error("the child fold has an unresolved dimension — cast it before returning")]
    ReturnFoldUnresolved,
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
    if child == parent.trajectory() {
        return Err(BranchError::SelfFork);
    }
    if parent.parent_of(child).is_some() {
        return Err(BranchError::AlreadyForked);
    }
    // A value return ends the errand: the fork's mandate covers one errand and one result, so a
    // returned child is closed to new work — forking included. A void return does not consume the
    // return channel and leaves the session forkable. Enforced here, inside the store's atomic
    // seed, so every fork entry point shares the one gate.
    if parent.returns_by(parent.trajectory()) > 0 {
        return Err(BranchError::ParentReturned);
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

/// Record a child's returned value at an **engine-derived** label AND merge it into the direct
/// parent, as one atomic batch — record, parent admission under the returned label, and the
/// `Merge` boundary commit together, so no recorded return can be orphaned between two commit
/// points. The crossing path is the one the child's fork [`ReturnPolicy`] binds — a mismatched
/// submission is refused, so the caller never selects it. A raw return carries the child fold
/// **and must not narrow the parent** ([`BranchError::ReturnNarrowsParent`] — a narrowing crossing
/// exists only through an executed return plan, engine-wide, not as a runtime convention); a
/// sanitized return preserves the fold's trust and relabels audience to the sanitizer's `to` (only
/// if the fold satisfies `from`). Trust never rises.
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
    // One errand, one result: a child's first crossing consumes its return channel, whatever the
    // policy — a second `submit_result` is refused, not re-merged.
    if parent.returns_by(child) > 0 {
        return Err(BranchError::AlreadyReturned);
    }
    let policy = parent.return_policy_of(child).ok_or(BranchError::NotForked)?.clone();
    let fold = parent.branch_label(child);
    let (value, derivation) = match (policy, ret) {
        (ReturnPolicy::Raw, ReturnSubmission::Raw { body }) => {
            // The narrowing gate is engine law: a raw crossing that would narrow the parent is
            // never merged silently, whoever the embedder is — it takes an executed return plan.
            match check_child_return(registry, parent, child)? {
                ReturnCheck::Allow => {}
                ReturnCheck::Unresolved(_) => return Err(BranchError::ReturnFoldUnresolved),
                ReturnCheck::Block { .. } => return Err(BranchError::ReturnNarrowsParent),
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
    /// Merge the raw return and record the parent's acceptance of exactly this narrowing. The
    /// offered narrowing is embedded so a stale acceptance mismatches by value, like its
    /// sanitizer siblings.
    Accept(Narrowing),
    /// Merge this output sanitizer's derivation instead. `residual` is the narrowing that remains
    /// after the relabel and is accepted alongside (e.g. a trust component no sanitizer may lift —
    /// audience is the only sanitizer territory); `None` means the relabel fully clears the block.
    Sanitize {
        sanitizer: SanitizerName,
        residual: Option<Narrowing>,
    },
}

/// The verdict on a proposed raw child return. Decided from the parent's [`Views`] alone — both
/// folds and the fork linkage come from one projection snapshot, so mixed-snapshot checks are
/// unrepresentable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReturnCheck {
    /// Merging the raw return leaves the parent's label untouched: merge as today.
    Allow,
    /// A consumed label dimension is Unknown — Unknown is absorbing under `combine` but is not an
    /// ordered restriction, so it can never form a narrowing. Cast these values first; no plans
    /// until resolved (mirrors the tool path's `CheckOutcome::Unresolved`).
    Unresolved(Vec<UnresolvedFact>),
    /// The raw return would narrow the parent: the merge is blocked. `plans` is non-empty by
    /// construction (`Accept` is always offered), in deterministic order — `Accept` first, then
    /// sanitizer plans in registry name order.
    Block {
        narrowing: Narrowing,
        plans: Vec<ReturnPlan>,
    },
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
    if parent.returns_by(child) > 0 {
        return Err(BranchError::AlreadyReturned);
    }
    // The blocked-return flow exists only under a Raw policy: a bound sanitizer crosses
    // unconditionally, and the model never chooses a path.
    match parent.return_policy_of(child) {
        Some(ReturnPolicy::Raw) => {}
        Some(_) => return Err(BranchError::ReturnPolicyMismatch),
        None => return Err(BranchError::NotForked),
    }
    let fold = parent.branch_label(child);
    let current = parent.current_label();

    let mut unresolved = Vec::new();
    unresolved_dims(parent, child, &fold, &mut unresolved);
    unresolved_dims(parent, parent.trajectory(), &current, &mut unresolved);
    if !unresolved.is_empty() {
        return Ok(ReturnCheck::Unresolved(unresolved));
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
    for sanitizer in registry.sanitizers() {
        if !sanitizer.on.output {
            continue;
        }
        if fold.audience.covers(&sanitizer.can_reduce.from_includes) != Adequacy::Holds {
            continue;
        }
        // The label the sanitized crossing would carry: trust preserved, audience relabeled.
        let sanitized = Label::new(fold.trust.clone(), Dim::Known(sanitizer.can_reduce.to.clone()));
        let merged = current.combine(&sanitized);
        if merged == current {
            plans.push(ReturnPlan::Sanitize {
                sanitizer: sanitizer.name.clone(),
                residual: None,
            });
        } else if merged != candidate {
            plans.push(ReturnPlan::Sanitize {
                sanitizer: sanitizer.name.clone(),
                residual: Some(Narrowing {
                    from: current.clone(),
                    to: merged,
                }),
            });
        }
        // merged == candidate: the relabel buys nothing over the raw crossing — not offered.
    }
    Ok(ReturnCheck::Block { narrowing, plans })
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

/// Execute one offered return plan: record the crossing, the acceptance where the plan carries
/// one, and the merge — one atomic engine-derived batch, assembled here and nowhere else. The
/// block is **re-derived from the live views** and the chosen plan must be among the freshly
/// offered ones. That value match is the whole staleness story: a plan carrying labels (an
/// acceptance, a residual) mismatches once the family moved, and a residual-free sanitize plan
/// is re-offered only while its relabel still fully clears the live block — either way a chosen
/// plan that matches re-derives an identical crossing, sound to execute whatever happened in
/// between. No offer identity or epoch survives to track.
pub(crate) fn execute_child_return_plan(
    registry: &Registry,
    parent: &Views,
    child: &TrajectoryId,
    chosen: ReturnPlan,
    submission: ReturnSubmission,
) -> Result<FactBatch, BranchError> {
    let plans = match check_child_return(registry, parent, child)? {
        ReturnCheck::Block { plans, .. } => plans,
        // Allow or Unresolved: the state moved since the offer — nothing here to execute.
        ReturnCheck::Allow | ReturnCheck::Unresolved(_) => return Err(BranchError::ReturnOfferStale),
    };
    if !plans.contains(&chosen) {
        return Err(BranchError::ReturnPlanNotOffered);
    }

    let fold = parent.branch_label(child);
    // The crossing value and its audit, per plan — the same derivations submit_child_return makes,
    // revalidated here against the live registry and fold. The accepted narrowing is the plan's
    // own: `contains` above proved it identical to the freshly derived one.
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

/// The mandate-validated sanitized crossing shared by the static path and the return plans: trust
/// preserved from the fold, audience relabeled to the sanitizer's declared `to`.
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
    // Unresolved-first: an Unknown fold dimension (trust rides through this crossing) would
    // absorb into the parent via `combine` — refuse, never fail open.
    if matches!(fold.trust, Dim::Unknown) || matches!(fold.audience, Dim::Unknown) {
        return Err(BranchError::ReturnFoldUnresolved);
    }
    if !registered.on.output {
        return Err(BranchError::SanitizerNotOutput(sanitizer.as_str().to_string()));
    }
    if fold.audience.covers(&registered.can_reduce.from_includes) != Adequacy::Holds {
        return Err(BranchError::TransitionSourceUnmet);
    }
    let value = LabeledValue::new(
        body,
        Label::new(fold.trust.clone(), Dim::Known(registered.can_reduce.to.clone())),
    );
    let derivation = ReturnDerivation::Sanitized {
        sanitizer: sanitizer.clone(),
        raw_digest,
        from: registered.can_reduce.from_includes.clone(),
        to: registered.can_reduce.to.clone(),
    };
    Ok((value, derivation))
}

/// The `trajectory`'s values with an Unknown in a dimension its fold leaves Unknown — the values a
/// cast must resolve before the return check can decide.
fn unresolved_dims(views: &Views, trajectory: &TrajectoryId, fold: &Label, out: &mut Vec<UnresolvedFact>) {
    let trust_unknown = matches!(fold.trust, Dim::Unknown);
    let audience_unknown = matches!(fold.audience, Dim::Unknown);
    if !trust_unknown && !audience_unknown {
        return;
    }
    for (id, label) in views.branch_values_of(trajectory) {
        if trust_unknown && matches!(label.trust, Dim::Unknown) {
            out.push(UnresolvedFact {
                value: id,
                dimension: Dimension::Trust,
            });
        }
        if audience_unknown && matches!(label.audience, Dim::Unknown) {
            out.push(UnresolvedFact {
                value: id,
                dimension: Dimension::Audience,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::{AudienceTransition, Sanitizer, SanitizerPoints};
    use crate::fact::{CloseOutcome, EffectKind, Revision};
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
        // A declassifier that relabels internal → public (an audience widen, trust preserved).
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

    /// A log where the parent holds a value and has forked a child seeded at the parent's label.
    fn forked(parent_label: Label) -> Vec<Fact> {
        forked_bound(parent_label, ReturnPolicy::Raw)
    }

    /// Like [`forked`], with an explicit return policy on the fork binding.
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
        // Self-fork.
        let log = vec![admit(parent(), known(TRUSTED, Audience::Public))];
        let projection = build(&log);
        assert_eq!(
            seed_child(&registry(), &projection.view(&parent()), &parent(), ReturnPolicy::Raw),
            Err(BranchError::SelfFork)
        );
        // Reparenting an already-forked child.
        let log = forked(known(TRUSTED, Audience::Public));
        let projection = build(&log);
        let other = TrajectoryId::new("other");
        assert_eq!(
            seed_child(&registry(), &projection.view(&other), &child(), ReturnPolicy::Raw),
            Err(BranchError::AlreadyForked)
        );
        // Forking at an unresolved parent label.
        let log = vec![admit(parent(), Label::new(Dim::Unknown, Dim::Known(Audience::Public)))];
        let projection = build(&log);
        assert_eq!(
            seed_child(&registry(), &projection.view(&parent()), &child(), ReturnPolicy::Raw),
            Err(BranchError::ParentUnresolved)
        );
    }

    #[test]
    fn a_non_narrowing_raw_return_crosses_in_one_batch() {
        // Child stayed at the parent's seed: the raw crossing records, admits, and merges as ONE
        // batch — no orphanable intermediate state.
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
        // Parent trusted+public, child read suspicious+internal: the engine itself refuses the
        // silent crossing — it exists only through an executed return plan, whoever the embedder.
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
        // Child seeded suspicious+internal; declassify relabels internal → public, trust preserved.
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
        // The parent absorbed parent.combine(returned) in the same batch — internal, never public.
        assert_eq!(
            projection.view(&parent()).current_label(),
            known(SUSPICIOUS, internal())
        );
    }

    /// A registry with two applicable output sanitizers plus an input-only one, for menu tests.
    fn menu_registry() -> Registry {
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
        // Relabels internal → {finance}: clears less than declassify, leaving an audience residual
        // for a public parent.
        let to_finance = Sanitizer {
            name: SanitizerName::new("to-finance"),
            on: SanitizerPoints {
                input: false,
                output: true,
            },
            can_reduce: AudienceTransition {
                from_includes: internal(),
                to: Audience::restricted([ReaderId::new("finance")]),
            },
        };
        let input_only = Sanitizer {
            name: SanitizerName::new("input-only"),
            on: SanitizerPoints {
                input: true,
                output: false,
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
        // Child stayed at the parent's seed: merging its fold changes nothing.
        let log = forked(known(SUSPICIOUS, internal()));
        assert_eq!(check(&registry(), &log), ReturnCheck::Allow);
        // A child narrower than the parent in no dimension the parent doesn't already hold.
        let mut log = forked(known(SUSPICIOUS, internal()));
        log.push(admit(child(), known(TRUSTED, internal())));
        assert_eq!(check(&registry(), &log), ReturnCheck::Allow);
    }

    #[test]
    fn a_narrowing_raw_return_is_blocked_with_accept_always_offered() {
        // Parent trusted+public; child read suspicious+internal → raw merge would narrow both.
        let mut log = forked(known(TRUSTED, Audience::Public));
        log.push(admit(child(), known(SUSPICIOUS, internal())));
        match check(&registry(), &log) {
            ReturnCheck::Block { narrowing, plans } => {
                assert_eq!(narrowing.from, known(TRUSTED, Audience::Public));
                assert_eq!(narrowing.to, known(SUSPICIOUS, internal()));
                // declassify clears the audience but trust remains → composed, never standalone.
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
        // Parent suspicious+public; child read suspicious+internal → only audience narrows.
        // declassify (internal → public) fully clears; to-finance leaves a residual; input-only
        // is not an output sanitizer and never appears.
        let mut log = forked(known(SUSPICIOUS, Audience::Public));
        log.push(admit(child(), known(SUSPICIOUS, internal())));
        match check(&menu_registry(), &log) {
            ReturnCheck::Block { plans, .. } => {
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
                        ReturnPlan::Sanitize {
                            sanitizer: SanitizerName::new("to-finance"),
                            residual: Some(Narrowing {
                                from: known(SUSPICIOUS, Audience::Public),
                                to: known(SUSPICIOUS, Audience::restricted([ReaderId::new("finance")])),
                            }),
                        },
                    ]
                );
            }
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[test]
    fn a_trust_only_narrowing_offers_no_standalone_sanitize() {
        // Parent trusted+internal; child read suspicious+internal → only trust narrows. A
        // sanitizer moves audience only, so its relabel buys nothing over the raw crossing
        // (declassify would additionally widen audience — same merged outcome as raw here? No:
        // internal parent ∩ public = internal, identical to raw) → Accept is the only plan.
        let mut log = forked(known(TRUSTED, internal()));
        log.push(admit(child(), known(SUSPICIOUS, internal())));
        match check(&registry(), &log) {
            ReturnCheck::Block { plans, .. } => assert_eq!(
                plans,
                vec![ReturnPlan::Accept(Narrowing {
                    from: known(TRUSTED, internal()),
                    to: known(SUSPICIOUS, internal()),
                })]
            ),
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[test]
    fn an_inapplicable_sanitizer_is_not_offered() {
        // Child fold audience {finance} does not include the declassifier's `from` (internal).
        let mut log = forked(known(SUSPICIOUS, Audience::Public));
        log.push(admit(
            child(),
            known(SUSPICIOUS, Audience::restricted([ReaderId::new("finance")])),
        ));
        match check(&registry(), &log) {
            ReturnCheck::Block { plans, .. } => assert!(matches!(plans.as_slice(), [ReturnPlan::Accept(_)])),
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_dimension_is_unresolved_not_a_narrowing() {
        // A child value with Unknown trust: absorbing under combine, but never a narrowing.
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
            ReturnCheck::Unresolved(facts) => {
                assert_eq!(
                    facts,
                    vec![UnresolvedFact {
                        value: unknown_value,
                        dimension: Dimension::Trust,
                    }]
                );
            }
            other => panic!("expected Unresolved, got {other:?}"),
        }

        // Audience-Unknown and mixed report per dimension; a parent-side Unknown reports too.
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
            ReturnCheck::Unresolved(facts) => {
                assert_eq!(facts.len(), 3);
                assert!(facts.contains(&UnresolvedFact {
                    value: ValueId::new(1),
                    dimension: Dimension::Trust,
                }));
                assert!(facts.contains(&UnresolvedFact {
                    value: ValueId::new(1),
                    dimension: Dimension::Audience,
                }));
                assert!(facts.contains(&UnresolvedFact {
                    value: ValueId::new(2),
                    dimension: Dimension::Audience,
                }));
            }
            other => panic!("expected Unresolved, got {other:?}"),
        }
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

    /// Parent trusted+public, child read suspicious+internal: narrows both dimensions. The block
    /// offers Accept and declassify-composed (audience clears, trust residual).
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

    /// The full narrowing `blocked_family` offers Accept over.
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
        // One atomic batch: crossing, acceptance, admitted value, merge boundary.
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
        // Applying the batch narrows the parent to the accepted label.
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
        // The acceptance names exactly the residual, not the raw narrowing.
        match &batch.facts[1] {
            Fact::ChildReturnAcceptance { narrowing, .. } => {
                assert_eq!(narrowing.to, known(SUSPICIOUS, Audience::Public));
            }
            other => panic!("expected ChildReturnAcceptance, got {other:?}"),
        }
        log.extend(batch.facts);
        let projection = build(&log);
        // Parent keeps its public audience; only trust narrowed (the accepted residual).
        assert_eq!(
            projection.view(&parent()).current_label(),
            known(SUSPICIOUS, Audience::Public)
        );
        // The merged value's body is the derivation, and the crossing audits the sanitizer.
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
        // Parent suspicious+public: declassify fully clears the audience-only narrowing.
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
        // Fully cleared: the parent label is untouched.
        assert_eq!(
            projection.view(&parent()).current_label(),
            known(SUSPICIOUS, Audience::Public)
        );
    }

    #[test]
    fn an_unoffered_plan_is_refused() {
        // Standalone Sanitize is not offered for this block (trust residual remains).
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
        // A residual computed against a different parent state no longer matches any offer.
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
        // The parent narrowed itself since the offer: the raw return no longer narrows, so there
        // is no block to execute against.
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
        // A boundary that leaves every label untouched does not invalidate the offer: the block
        // re-derives identically, so executing it crosses exactly what a fresh offer would.
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
        // Execute Accept; the crossing consumes the child's one return, so the sibling composed
        // plan is refused outright — in either order.
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
            Err(BranchError::AlreadyReturned)
        );

        // Reverse order: the composed plan merges first; the stale Accept dies the same way.
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
            Err(BranchError::AlreadyReturned)
        );
    }

    #[test]
    fn a_second_return_from_one_child_is_refused() {
        // A silent (non-narrowing) crossing consumes the child's return: the next submission and
        // even the return check itself are refused, whatever they carry.
        let mut log = forked(known(SUSPICIOUS, internal()));
        let projection = build(&log);
        let ret = submit_child_return(&registry(), &projection.view(&parent()), &child(), raw("first")).unwrap();
        log.extend(ret.facts);
        let projection = build(&log);
        assert_eq!(
            submit_child_return(&registry(), &projection.view(&parent()), &child(), raw("second")),
            Err(BranchError::AlreadyReturned)
        );
        assert_eq!(
            check_child_return(&registry(), &projection.view(&parent()), &child()),
            Err(BranchError::AlreadyReturned)
        );
    }

    #[test]
    fn a_returned_child_cannot_become_a_fork_parent() {
        // A value return closes the errand: seeding a grandchild from the returned child is
        // refused inside the store's atomic seed, whichever mediator entry point asked.
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
            Err(BranchError::ParentReturned)
        );
    }

    #[test]
    fn a_submission_off_the_fork_policy_is_refused() {
        // A derived submission under a Raw binding: the binding, not the caller, names the path.
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
        // A raw submission under a Sanitized binding is likewise refused (fail-closed, never raw).
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
        // Under a Sanitized binding the model never chooses — no block flow exists.
        let log = forked_bound(known(TRUSTED, internal()), sanitized_policy());
        let projection = build(&log);
        assert_eq!(
            check_child_return(&registry(), &projection.view(&parent()), &child()),
            Err(BranchError::ReturnPolicyMismatch)
        );
    }

    #[test]
    fn return_facts_audit_their_derivation() {
        // Raw crossing audits Raw; sanitized crossing audits the transition and the raw digest.
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
                    from: internal(),
                    to: Audience::Public,
                }
            ),
            other => panic!("expected ChildReturn, got {other:?}"),
        }
    }

    #[test]
    fn sanitized_return_with_unmet_from_is_refused() {
        // Child fold audience is {finance}, which does not include internal → the declassifier's
        // `from` is unmet (a public fold would include internal and thus satisfy it, so we use a
        // restricted set that excludes internal).
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
        // Child seeded suspicious+internal declassifies to suspicious+public and returns it. The
        // admitted value's OWN label is the engine-derived returned label (what the value *is* —
        // for authority review and cast targeting), while the parent FOLD absorbs it like any
        // read: internal ∩ public = internal, so the fold never widens toward the returned label.
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
        // Fold invariance: combine is idempotent, so admitting under the returned label folds to
        // exactly what admitting under parent.combine(returned) folded to.
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
