//! The two-fold check: the pure evaluation of a proposed call against the trajectory.

use serde::{Deserialize, Serialize};

use crate::contract::{AudienceRequirement, HistoryRequirement, RecipientSpec, ToolContract};
use crate::fact::EffectKind;
use crate::label::{Adequacy, Audience, Dim, Dimension, Label, ReaderId, Trust};
use crate::names::MarkName;
use crate::projection::Views;
use crate::value::{ResolvedCall, ValueId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnresolvedFact {
    pub value: ValueId,
    pub dimension: Dimension,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Gap {
    TrustFloor { required: Trust, actual: Trust },
    Includes { recipients: Audience },
    Cap { cap: Audience },
    Prior(EffectKind),
    NoPrior(EffectKind),
    Attention(MarkName),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Narrowing {
    pub from: Label,
    pub to: Label,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawBlock {
    pub requirement_gaps: Vec<Gap>,
    pub narrowing: Option<Narrowing>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckOutcome {
    Allow,
    Block(RawBlock),
    Unresolved(Vec<UnresolvedFact>),
}

/// Evaluate one call against the branch views. Pure: a function of the contract, the views, and the
/// resolved arguments.
pub(crate) fn evaluate(contract: &ToolContract, views: &Views, call: &ResolvedCall) -> CheckOutcome {
    let current = views.current_label();
    let committed = contract.delta.apply(&current);

    let unresolved = unresolved_facts(views, &committed);
    if !unresolved.is_empty() {
        return CheckOutcome::Unresolved(unresolved);
    }

    evaluate_state(contract, &current, &|kind| views.has_effect(kind), call)
}

/// The gap logic on an abstract `(current label, effect predicate)` state — the one place the two
/// clocks live, shared by [`evaluate`] and the remedy reachability search (`plan`). A committed
/// label that is still `Unknown` yields [`CheckOutcome::Unresolved`] with no listed facts: the
/// caller that has the values (the view path) details them; the state-only search treats it as a
/// dead end (unresolved resolution is a cast path, outside the reachability subset).
pub(crate) fn evaluate_state(
    contract: &ToolContract,
    current: &Label,
    has_effect: &impl Fn(&EffectKind) -> bool,
    call: &ResolvedCall,
) -> CheckOutcome {
    let committed = contract.delta.apply(current);
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

fn unresolved_recipient(spec: &RecipientSpec) -> Audience {
    let key = match spec {
        RecipientSpec::Placeholder(key) => key.as_str(),
        RecipientSpec::Static(_) => "static",
    };
    Audience::restricted([ReaderId::new(format!("<unresolved:{key}>"))])
}
