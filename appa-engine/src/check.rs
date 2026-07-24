//! The two-fold check: the pure evaluation of a proposed call against the trajectory.
//!
//! Ordered by the spec's clocks: **narrowing** first (on the label the dispatch would commit),
//! then **label requirements** (on that same committed label), then **history requirements** (on
//! the log as it stands — a call's own `emits` never trips its own precondition). Attention demands
//! are per-call gaps, never satisfied by history. If a label requirement **consumes** an `Unknown`
//! dimension, the check is [`CheckOutcome::Unresolved`] — it names the values to cast, never a
//! blanket Unknown. A call with no requirement on an Unknown dimension proceeds: an Unknown
//! trajectory does not brick unannotated flows, it fails closed exactly at the sinks whose
//! requirements consume it (the gradual-annotation story).
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

/// The contribution a successful call would actually fold, on the check's clock. For an unbound
/// tool that is its declared `delta`; a sanitizer-bound tool (RP4) folds the **bound derivation**
/// instead, so its audience contribution is the sanitizer's declared `to` (trust untouched — never
/// sanitizer territory). The distinction is load-bearing for the narrowing clock: a bound tool
/// whose raw output is internal but whose sanitizer declassifies to public narrows nothing, and
/// must not soft-block a narrowing that never enters the trajectory. `None` is the unannotated
/// tool: like a pending-cast dimension, its contribution (Unknown) folds only at admission, so it
/// is identity here.
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
        // The state evaluation only signals that a requirement consumed an Unknown dimension; the
        // offending branch values are named here, where the views can enumerate them.
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
    // Canonical: a duplicated requirement entry (the same mark or effect listed twice) is one gap —
    // a repeat adds no obligation, and downstream plan enumeration would otherwise mint
    // order-permuted duplicate assignments from it.
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

/// The dimensions whose Unknown state a label requirement of this call consumes — the ones a cast
/// must resolve before the check can decide. Requirement-scoped by design; a malformed `includes`
/// placeholder consumes the dimension only when the audience is Unknown (unresolved-first), and
/// on a Known audience stays the hard fail-closed gap `label_gaps` reports.
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

/// The branch values with an Unknown in a consumed-unresolved dimension.
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
                // A placeholder that cannot resolve: on the real-dispatch path it fails closed
                // even on a public trajectory (a call that cannot name its recipient releases to
                // no one); on the planner's synthetic prerequisite path it is waived — the agent
                // supplies the recipient at real dispatch (only a Placeholder spec can reach this
                // arm, so waiving never drops a static requirement).
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
