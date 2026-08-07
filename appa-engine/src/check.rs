//! The two-outcome check: the pure evaluation of a proposed call against the trajectory.

use serde::{Deserialize, Serialize};

use crate::contract::{AudienceRequirement, HistoryRequirement, RecipientSpec, ToolContract};
use crate::fact::EffectKind;
use crate::label::{Adequacy, Audience, Dim, Dimension, Label, ReaderId, Trust};
use crate::names::{DynamicResolverName, MarkName};
use crate::projection::Views;
use crate::value::{ResolvedCall, ValueId};

/// A value whose consumed dimension no registered cast has established — a missing fact, cleared
/// by a cast landing, never by a ruling or a plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnestablishedFact {
    pub value: ValueId,
    pub dimension: Dimension,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Gap {
    TrustFloor { required: Trust, actual: Trust },
    Includes { recipients: Audience },
    UnresolvedDynamicRecipient {
        resolver: DynamicResolverName,
        argument: String,
    },
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

/// The block as the check finds it — gaps, a narrowing, and/or unestablished values — before
/// remedy planning. The slots are independent and may coexist; `unestablished` entries
/// offer no plan by design, since a fact rather than a plan clears them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawBlock {
    pub requirement_gaps: Vec<Gap>,
    pub narrowing: Option<Narrowing>,
    pub unestablished: Vec<UnestablishedFact>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckOutcome {
    Allow,
    Block(RawBlock),
}

/// The state-only evaluation shared by [`evaluate`] and the remedy reachability search: the gaps
/// and narrowing as the clocks find them, plus the dimensions whose Unknown a label requirement
/// consumes. The state path cannot name values — the views path ([`evaluate`]) enumerates them
/// into the block's `unestablished` slot. Plans are gap-scoped, so the search reads only the
/// gaps and narrowing for the target; `consumed` matters where a call must actually *run* — a
/// redispatch prerequisite whose own requirements consume an Unknown is not runnable.
pub(crate) struct StateEval {
    pub(crate) requirement_gaps: Vec<Gap>,
    pub(crate) narrowing: Option<Narrowing>,
    pub(crate) consumed: Vec<Dimension>,
}

/// How an `includes` placeholder that cannot resolve from the call's arguments enters the gap set.
/// The origin is carried structurally — never reconstructed from a gap's recipient value, which a
/// static contract could legally collide with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlaceholderGaps {
    FailClosed,
    Waived,
}

/// The label the trajectory would hold after this call commits, on the check's clock. An
/// unannotated tool contributes identity here — like a pending-cast dimension, its Unknown
/// contribution folds only at admission.
pub(crate) fn committed_label(contract: &ToolContract, current: &Label) -> Label {
    match &contract.delta {
        Some(delta) => delta.apply(current),
        None => current.clone(),
    }
}

pub(crate) fn committed_label_for_call(contract: &ToolContract, current: &Label, call: &ResolvedCall) -> Label {
    let mut label = committed_label(contract, current);
    if let Some(crate::contract::Delta {
        audience: Some(crate::contract::AudienceDelta::Dynamic(binding)),
        ..
    }) = &contract.delta
        && let Some(audience) = call.dynamic_resolution(binding)
    {
        label = label.combine(&Label::new(
            Dim::Known(Trust::new(u8::MAX)),
            Dim::Known(audience.clone()),
        ));
    }
    label
}

/// Evaluate one call against the branch views. Pure: a function of the contract, the views, and
/// the resolved arguments. The block carries every slot at once: the evaluable gaps, the
/// narrowing, and the consumed-Unknown dimensions named per value.
pub(crate) fn evaluate(contract: &ToolContract, views: &Views, call: &ResolvedCall) -> CheckOutcome {
    let current = views.current_label();
    let eval = evaluate_state(
        contract,
        &current,
        &|kind| views.has_effect(kind),
        call,
        PlaceholderGaps::FailClosed,
    );
    if eval.requirement_gaps.is_empty() && eval.narrowing.is_none() && eval.consumed.is_empty() {
        return CheckOutcome::Allow;
    }
    let unestablished = unestablished_facts(views, &eval.consumed);
    CheckOutcome::Block(RawBlock {
        requirement_gaps: eval.requirement_gaps,
        narrowing: eval.narrowing,
        unestablished,
    })
}

/// The gap logic on an abstract `(current label, effect predicate)` state — the one place the two
/// clocks live, shared by [`evaluate`] and the remedy reachability search (`plan`). A label
/// requirement that consumes an `Unknown` dimension lands in `consumed`, never in the gaps
/// (masked — one missing fact is not also a coverable gap); requirements on established
/// dimensions evaluate as always. An Unknown dimension nothing requires blocks nothing.
pub(crate) fn evaluate_state(
    contract: &ToolContract,
    current: &Label,
    has_effect: &impl Fn(&EffectKind) -> bool,
    call: &ResolvedCall,
    placeholders: PlaceholderGaps,
) -> StateEval {
    let committed = committed_label_for_call(contract, current, call);
    let consumed = consumed_unknown(contract, &committed, call);

    // Clock 1: narrowing, on the committed label.
    let narrowing = (&committed != current).then(|| Narrowing {
        from: current.clone(),
        to: committed.clone(),
    });

    // Clocks 2 and 3: label requirements on the committed label, history on the log as it stands.
    let mut gaps = Vec::new();
    label_gaps(contract, &committed, call, placeholders, &mut gaps);
    history_gaps(contract, has_effect, &mut gaps);
    for mark in &contract.requires.attention {
        gaps.push(Gap::Attention(mark.clone()));
    }
    let mut seen = Vec::with_capacity(gaps.len());
    for gap in gaps {
        if !seen.contains(&gap) {
            seen.push(gap);
        }
    }

    StateEval {
        requirement_gaps: seen,
        narrowing,
        consumed,
    }
}

fn consumed_unknown(contract: &ToolContract, committed: &Label, call: &ResolvedCall) -> Vec<Dimension> {
    let mut dims = Vec::new();
    if let Some(floor) = contract.requires.label.trust_floor
        && committed.trust.meets_floor(floor) == Adequacy::Unresolved
    {
        dims.push(Dimension::Trust);
    }
    let audience_unresolved = contract
        .requires
        .label
        .audience
        .iter()
        .any(|requirement| match requirement {
            AudienceRequirement::Includes(spec) => match resolve_recipients(spec, call) {
                Some(recipients) => committed.audience.covers(&recipients) == Adequacy::Unresolved,
                None => matches!(committed.audience, Dim::Unknown),
            },
            AudienceRequirement::Cap(cap) => committed.audience.within_cap(cap) == Adequacy::Unresolved,
        });
    if audience_unresolved {
        dims.push(Dimension::Audience);
    }
    dims
}

fn unestablished_facts(views: &Views, dims: &[Dimension]) -> Vec<UnestablishedFact> {
    let mut facts = Vec::new();
    let trust_unknown = dims.contains(&Dimension::Trust);
    let audience_unknown = dims.contains(&Dimension::Audience);
    if !trust_unknown && !audience_unknown {
        return facts;
    }
    for (id, label) in views.branch_values() {
        if trust_unknown && matches!(label.trust, Dim::Unknown) {
            facts.push(UnestablishedFact {
                value: id,
                dimension: Dimension::Trust,
            });
        }
        if audience_unknown && matches!(label.audience, Dim::Unknown) {
            facts.push(UnestablishedFact {
                value: id,
                dimension: Dimension::Audience,
            });
        }
    }
    facts
}

fn label_gaps(
    contract: &ToolContract,
    committed: &Label,
    call: &ResolvedCall,
    placeholders: PlaceholderGaps,
    gaps: &mut Vec<Gap>,
) {
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
                None => {
                    if let RecipientSpec::Dynamic(binding) = spec {
                        if placeholders == PlaceholderGaps::FailClosed {
                            gaps.push(Gap::UnresolvedDynamicRecipient {
                                resolver: binding.resolver.clone(),
                                argument: binding.argument.clone(),
                            });
                        }
                    } else {
                        match placeholders {
                            PlaceholderGaps::FailClosed if !matches!(committed.audience, Dim::Unknown) => {
                                gaps.push(Gap::Includes {
                                    recipients: unresolved_recipient(spec),
                                })
                            }
                            PlaceholderGaps::FailClosed | PlaceholderGaps::Waived => {}
                        }
                    }
                }
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
        RecipientSpec::Dynamic(binding) => call.dynamic_resolution(binding).cloned(),
    }
}

fn unresolved_recipient(spec: &RecipientSpec) -> Audience {
    let key = match spec {
        RecipientSpec::Placeholder(key) => key.as_str(),
        RecipientSpec::Static(_) => "static",
        RecipientSpec::Dynamic(_) => "dynamic",
    };
    Audience::restricted([ReaderId::new(format!("<unresolved:{key}>"))])
}
