//! The two-fold check: the pure evaluation of a proposed call against the trajectory.

use serde::{Deserialize, Serialize};

use crate::contract::Delta;
use crate::contract::{AudienceRequirement, HistoryRequirement, RecipientSpec, ToolContract};
use crate::fact::EffectKind;
use crate::label::{Adequacy, Audience, Dim, Dimension, Label, ReaderId, Trust};
use crate::names::MarkName;
use crate::projection::Views;
use crate::registry::Registry;
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

/// How an `includes` placeholder that cannot resolve from the call's arguments enters the gap set.
/// The origin is carried structurally — never reconstructed from a gap's recipient value, which a
/// static contract could legally collide with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlaceholderGaps {
    FailClosed,
    Waived,
}

pub(crate) fn effective_delta(registry: &Registry, contract: &ToolContract) -> Option<Delta> {
    match (&contract.delta, &contract.output_sanitizer) {
        (delta, None) => delta.clone(),
        (Some(delta), Some(name)) => {
            let sanitizer = registry
                .sanitizer(name)
                .expect("load validation: bound output sanitizer is registered");
            Some(Delta {
                trust: delta.trust.clone(),
                audience: Some(Dim::Known(sanitizer.can_reduce.to.clone())),
            })
        }
        (None, Some(_)) => unreachable!("load validation: a sanitizer-bound tool declares a delta"),
    }
}

/// The label the trajectory would hold after this call commits, on the check's clock (see
/// [`effective_delta`] — an unannotated tool contributes identity here, Unknown at admission).
pub(crate) fn committed_label(registry: &Registry, contract: &ToolContract, current: &Label) -> Label {
    match effective_delta(registry, contract) {
        Some(delta) => delta.apply(current),
        None => current.clone(),
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
    match evaluate_state(
        registry,
        contract,
        &current,
        &|kind| views.has_effect(kind),
        call,
        PlaceholderGaps::FailClosed,
    ) {
        CheckOutcome::Unresolved(_) => {
            let committed = committed_label(registry, contract, &current);
            let dims = consumed_unresolved(contract, &committed, call);
            CheckOutcome::Unresolved(unresolved_facts(views, &dims))
        }
        outcome => outcome,
    }
}

/// The gap logic on an abstract `(current label, effect predicate)` state — the one place the two
/// clocks live, shared by [`evaluate`] and the remedy reachability search (`plan`). A label
/// requirement that consumes an `Unknown` dimension yields [`CheckOutcome::Unresolved`] with no
/// listed facts: the caller that has the values (the view path) details them; the state-only
/// search treats it as a dead end (unresolved resolution is a cast path, outside the reachability
/// subset). An Unknown dimension nothing requires blocks nothing.
pub(crate) fn evaluate_state(
    registry: &Registry,
    contract: &ToolContract,
    current: &Label,
    has_effect: &impl Fn(&EffectKind) -> bool,
    call: &ResolvedCall,
    placeholders: PlaceholderGaps,
) -> CheckOutcome {
    let committed = committed_label(registry, contract, current);
    if !consumed_unresolved(contract, &committed, call).is_empty() {
        return CheckOutcome::Unresolved(Vec::new());
    }

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
    let gaps = seen;

    if gaps.is_empty() && narrowing.is_none() {
        CheckOutcome::Allow
    } else {
        CheckOutcome::Block(RawBlock {
            requirement_gaps: gaps,
            narrowing,
        })
    }
}

fn consumed_unresolved(contract: &ToolContract, committed: &Label, call: &ResolvedCall) -> Vec<Dimension> {
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

fn unresolved_facts(views: &Views, dims: &[Dimension]) -> Vec<UnresolvedFact> {
    let mut facts = Vec::new();
    let trust_unknown = dims.contains(&Dimension::Trust);
    let audience_unknown = dims.contains(&Dimension::Audience);
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
                None => match placeholders {
                    PlaceholderGaps::FailClosed => gaps.push(Gap::Includes {
                        recipients: unresolved_recipient(spec),
                    }),
                    PlaceholderGaps::Waived => {}
                },
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
