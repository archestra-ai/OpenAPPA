//! The two-fold check: the pure evaluation of a proposed call against the trajectory.
//!
//! Ordered by the spec's clocks: **narrowing** first (on the label the dispatch would commit),
//! then **label requirements** (on that same committed label), then **history requirements** (on
//! the log as it stands — a call's own `emits` never trips its own precondition). Attention demands
//! are per-call gaps, never satisfied by history. If any consumed label dimension is `Unknown`, the
//! check is [`CheckOutcome::Unresolved`] — it names the values to cast, never a blanket Unknown.
//!
//! This module is pure and has no ad-hoc judgment: every branch is label arithmetic or a log query.

use serde::{Deserialize, Serialize};

use crate::contract::Delta;
use crate::contract::{AudienceRequirement, HistoryRequirement, RecipientSpec, ToolContract};
use crate::fact::EffectKind;
use crate::label::{Adequacy, Audience, Dim, Dimension, Label, ReaderId, Trust};
use crate::names::MarkName;
use crate::projection::Views;
use crate::registry::Registry;
use crate::value::{ResolvedCall, ValueId};

/// A value whose dimension is Unknown and must be cast before the check can decide.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnresolvedFact {
    pub value: ValueId,
    pub dimension: Dimension,
}

/// One requirement the trajectory does not satisfy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Gap {
    /// Trust below the required floor.
    TrustFloor { required: Trust, actual: Trust },
    /// The trajectory's readers do not include these recipients.
    Includes { recipients: Audience },
    /// The committed reader set exceeds this cap.
    Cap { cap: Audience },
    /// A required prior effect is missing.
    Prior(EffectKind),
    /// A forbidden effect is already present.
    NoPrior(EffectKind),
    /// A per-call attention demand.
    Attention(MarkName),
}

/// A voluntary narrowing of the release frontier: committing this call moves the label down.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Narrowing {
    pub from: Label,
    pub to: Label,
}

/// The block as the check finds it — gaps and/or a narrowing — before remedy planning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawBlock {
    pub requirement_gaps: Vec<Gap>,
    pub narrowing: Option<Narrowing>,
}

/// The check's verdict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckOutcome {
    Allow,
    Block(RawBlock),
    Unresolved(Vec<UnresolvedFact>),
}

/// The contribution a successful call would actually fold. For an unbound tool that is its raw
/// `delta`; a sanitizer-bound tool (RP4) folds the **bound derivation** instead, so its audience
/// contribution is the sanitizer's declared `to` (trust untouched — never sanitizer territory).
/// The distinction is load-bearing for the narrowing clock: a bound tool whose raw output is
/// internal but whose sanitizer declassifies to public narrows nothing, and must not soft-block a
/// narrowing that never enters the trajectory.
pub(crate) fn effective_delta(registry: &Registry, contract: &ToolContract) -> Delta {
    match &contract.output_sanitizer {
        None => contract.delta.clone(),
        Some(name) => {
            let sanitizer = registry
                .sanitizer(name)
                .expect("load validation: bound output sanitizer is registered");
            Delta {
                trust: contract.delta.trust.clone(),
                audience: Some(Dim::Known(sanitizer.can_reduce.to.clone())),
            }
        }
    }
}

/// Evaluate one call against the branch views. Pure: a function of the registry, the contract, the
/// views, and the resolved arguments.
pub(crate) fn evaluate(
    registry: &Registry,
    contract: &ToolContract,
    views: &Views,
    call: &ResolvedCall,
) -> CheckOutcome {
    let current = views.current_label();
    let committed = effective_delta(registry, contract).apply(&current);

    // Any Unknown dimension the check would consume must be resolved first — reported with the
    // offending branch values, which only the views can enumerate.
    let unresolved = unresolved_facts(views, &committed);
    if !unresolved.is_empty() {
        return CheckOutcome::Unresolved(unresolved);
    }

    evaluate_state(registry, contract, &current, &|kind| views.has_effect(kind), call)
}

/// The gap logic on an abstract `(current label, effect predicate)` state — the one place the two
/// clocks live, shared by [`evaluate`] and the remedy reachability search (`plan`). A committed
/// label that is still `Unknown` yields [`CheckOutcome::Unresolved`] with no listed facts: the
/// caller that has the values (the view path) details them; the state-only search treats it as a
/// dead end (unresolved resolution is a cast path, outside the reachability subset).
pub(crate) fn evaluate_state(
    registry: &Registry,
    contract: &ToolContract,
    current: &Label,
    has_effect: &impl Fn(&EffectKind) -> bool,
    call: &ResolvedCall,
) -> CheckOutcome {
    let committed = effective_delta(registry, contract).apply(current);
    if matches!(committed.trust, Dim::Unknown) || matches!(committed.audience, Dim::Unknown) {
        return CheckOutcome::Unresolved(Vec::new());
    }

    // Clock 1: narrowing, on the committed label.
    let narrowing = (&committed != current).then(|| Narrowing {
        from: current.clone(),
        to: committed.clone(),
    });

    // Clocks 2 and 3: label requirements on the committed label, history on the log as it stands.
    let mut gaps = Vec::new();
    label_gaps(contract, &committed, call, &mut gaps);
    history_gaps(contract, has_effect, &mut gaps);
    for mark in &contract.requires.attention {
        gaps.push(Gap::Attention(mark.clone()));
    }

    if gaps.is_empty() && narrowing.is_none() {
        CheckOutcome::Allow
    } else {
        CheckOutcome::Block(RawBlock {
            requirement_gaps: gaps,
            narrowing,
        })
    }
}

/// The branch values with an Unknown in a dimension the committed label leaves Unknown.
fn unresolved_facts(views: &Views, committed: &Label) -> Vec<UnresolvedFact> {
    let mut facts = Vec::new();
    let trust_unknown = matches!(committed.trust, Dim::Unknown);
    let audience_unknown = matches!(committed.audience, Dim::Unknown);
    if !trust_unknown && !audience_unknown {
        return facts;
    }
    for (id, label) in views.branch_values() {
        if trust_unknown && matches!(label.trust, Dim::Unknown) {
            facts.push(UnresolvedFact {
                value: id,
                dimension: Dimension::Trust,
            });
        }
        if audience_unknown && matches!(label.audience, Dim::Unknown) {
            facts.push(UnresolvedFact {
                value: id,
                dimension: Dimension::Audience,
            });
        }
    }
    facts
}

fn label_gaps(contract: &ToolContract, committed: &Label, call: &ResolvedCall, gaps: &mut Vec<Gap>) {
    if let Some(floor) = contract.requires.label.trust_floor
        && committed.trust.meets_floor(floor) == Adequacy::Fails
        && let Dim::Known(actual) = committed.trust
    {
        gaps.push(Gap::TrustFloor {
            required: floor,
            actual,
        });
    }
    for requirement in &contract.requires.label.audience {
        match requirement {
            AudienceRequirement::Includes(spec) => match resolve_recipients(spec, call) {
                Some(recipients) => {
                    if committed.audience.covers(&recipients) == Adequacy::Fails {
                        gaps.push(Gap::Includes { recipients });
                    }
                }
                // A malformed placeholder (missing or non-string arg) fails closed even on a public
                // trajectory: a call that cannot name its recipient releases to no one.
                None => gaps.push(Gap::Includes {
                    recipients: unresolved_recipient(spec),
                }),
            },
            AudienceRequirement::Cap(cap) => {
                if committed.audience.within_cap(cap) == Adequacy::Fails {
                    gaps.push(Gap::Cap { cap: cap.clone() });
                }
            }
        }
    }
}

fn history_gaps(contract: &ToolContract, has_effect: &impl Fn(&EffectKind) -> bool, gaps: &mut Vec<Gap>) {
    for requirement in &contract.requires.history {
        match requirement {
            HistoryRequirement::Prior(kind) => {
                if !has_effect(kind) {
                    gaps.push(Gap::Prior(kind.clone()));
                }
            }
            HistoryRequirement::NoPrior(kind) => {
                if has_effect(kind) {
                    gaps.push(Gap::NoPrior(kind.clone()));
                }
            }
        }
    }
}

/// Resolve an `includes` requirement's recipients. A placeholder reads the named argument's string
/// value as a reader identity; a missing or non-string argument yields `None` (the call is malformed
/// — the caller decides how to fail, and [`label_gaps`] fails it closed).
fn resolve_recipients(spec: &RecipientSpec, call: &ResolvedCall) -> Option<Audience> {
    match spec {
        RecipientSpec::Static(audience) => Some(audience.clone()),
        RecipientSpec::Placeholder(key) => call
            .arguments()
            .get(key)
            .and_then(|value| value.as_str())
            .map(|value| Audience::restricted([ReaderId::new(value)])),
    }
}

/// The unsatisfiable recipient a malformed `includes` gap names — a reader no trajectory holds, so
/// the gap can never be spuriously cleared.
fn unresolved_recipient(spec: &RecipientSpec) -> Audience {
    let key = match spec {
        RecipientSpec::Placeholder(key) => key.as_str(),
        RecipientSpec::Static(_) => "static",
    };
    Audience::restricted([ReaderId::new(format!("<unresolved:{key}>"))])
}
