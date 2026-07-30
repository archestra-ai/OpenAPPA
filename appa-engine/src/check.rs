//! The two-outcome check: the pure evaluation of a proposed call against the trajectory.
//!
//! Ordered by the spec's clocks: **narrowing** first (on the label the dispatch would commit),
//! then **label requirements** (on that same committed label), then **history requirements** (on
//! the log as it stands — a call's own `emits` never trips its own precondition). Attention demands
//! are per-call gaps, never satisfied by history. The outcome is `allow`, or `block` carrying
//! everything that stopped the call at once (`CHK-1`): the unmet requirements, the narrowing where
//! one fired, and — where a label requirement **consumes** an `Unknown` dimension — the values
//! whose dimension no cast has established yet, named per value in the block's `unestablished`
//! slot (`UNK-3`), never as a blanket Unknown. A requirement that consumes an Unknown is reported
//! there and only there: its gap evaluation is masked, so one missing fact is never double-billed
//! as a coverable gap. A call with no requirement on an Unknown dimension proceeds: an Unknown
//! trajectory does not brick unannotated flows, it fails closed exactly at the sinks whose
//! requirements consume it (the gradual-annotation story).
//!
//! This module is pure and has no ad-hoc judgment: every branch is label arithmetic or a log query.
//! Resolution is the runtime's job (`CHK-16`): it attempts the registered casts on the
//! unestablished values and re-checks; what lands here is only the residual.

use serde::{Deserialize, Serialize};

use crate::contract::{AudienceRequirement, HistoryRequirement, RecipientSpec, ToolContract};
use crate::fact::EffectKind;
use crate::label::{Adequacy, Audience, Dim, Dimension, Label, ReaderId, Trust};
use crate::names::MarkName;
use crate::projection::Views;
use crate::value::{ResolvedCall, ValueId};

/// A value whose consumed dimension no registered cast has established — a missing fact, cleared
/// by a cast landing (`CHK-16`), never by a ruling or a plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnestablishedFact {
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

/// The block as the check finds it — gaps, a narrowing, and/or unestablished values — before
/// remedy planning. The slots are independent and may coexist (`CHK-1`); `unestablished` entries
/// offer no plan by design (`RMD-10`), since a fact rather than a plan clears them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawBlock {
    pub requirement_gaps: Vec<Gap>,
    pub narrowing: Option<Narrowing>,
    pub unestablished: Vec<UnestablishedFact>,
}

/// The check's verdict: two outcomes (`CHK-1`).
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
    /// A malformed placeholder fails closed as an unsatisfiable sentinel gap — the real-dispatch
    /// path: a call that cannot name its recipient releases to no one.
    FailClosed,
    /// An unresolvable placeholder is waived — the planner's synthetic no-argument prerequisite
    /// call cannot know the recipient the agent supplies at real dispatch, so the requirement is
    /// not a gap there at all. Static `includes` requirements are untouched by this mode.
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
    // Allow demands `consumed` empty, not merely no enumerated facts: a consumed dimension with no
    // nameable value (unreachable while Unknown enters only through admitted values) still refuses.
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
    let committed = committed_label(contract, current);
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
    // Canonical: a duplicated requirement entry (the same mark or effect listed twice) is one gap —
    // a repeat adds no obligation, and downstream plan enumeration would otherwise mint
    // order-permuted duplicate assignments from it.
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

/// The dimensions whose Unknown state a label requirement of this call consumes — the ones only a
/// cast can establish. Requirement-scoped by design; a malformed `includes` placeholder consumes
/// the dimension only when the audience is Unknown (`label_gaps` masks its sentinel gap for
/// exactly that case), and on a Known audience stays the hard fail-closed gap `label_gaps`
/// reports.
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
                // A malformed placeholder on an Unknown audience still consumes the dimension:
                // downgrading it to an ordinary gap would let an authority with a reader ceiling
                // cover the fail-closed sentinel and open the dispatch with the Unknown never
                // resolved. On a Known audience it stays the unwaivable-by-trajectory hard gap.
                None => matches!(committed.audience, Dim::Unknown),
            },
            AudienceRequirement::Cap(cap) => committed.audience.within_cap(cap) == Adequacy::Unresolved,
        });
    if audience_unresolved {
        dims.push(Dimension::Audience);
    }
    dims
}

/// The branch values with an Unknown in a consumed dimension — the block's `unestablished` slot.
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
                // A placeholder that cannot resolve: on the real-dispatch path it fails closed
                // even on a public trajectory (a call that cannot name its recipient releases to
                // no one); on the planner's synthetic prerequisite path it is waived — the agent
                // supplies the recipient at real dispatch (only a Placeholder spec can reach this
                // arm, so waiving never drops a static requirement). On an Unknown audience the
                // requirement consumes the dimension instead (see `consumed_unknown`) and the
                // sentinel gap is masked: reporting it too would let a reader-ceiling authority
                // cover it and open the dispatch with the Unknown never resolved.
                None => match placeholders {
                    PlaceholderGaps::FailClosed if !matches!(committed.audience, Dim::Unknown) => {
                        gaps.push(Gap::Includes {
                            recipients: unresolved_recipient(spec),
                        })
                    }
                    PlaceholderGaps::FailClosed | PlaceholderGaps::Waived => {}
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

/// Resolve an `includes` requirement's recipients. A placeholder reads the named argument's string
/// value as a reader identity; a missing or non-string argument yields `None` — [`label_gaps`]
/// then fails it closed or waives it per its [`PlaceholderGaps`] mode.
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
