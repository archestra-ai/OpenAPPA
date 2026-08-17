//! The two-outcome check: the pure evaluation of a proposed call against the trajectory.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::candidate::CallStage;
use crate::contract::{AudienceRequirement, HistoryRequirement, RecipientSpec, ToolContract};
use crate::fact::EffectKind;
use crate::label::{Adequacy, Audience, Dimension, EstablishedLabel, PartialLabel, ReaderId, Trust};
use crate::names::{AudienceArgument, DynamicResolverName, GroupName, MarkName};
use crate::projection::Views;
use crate::value::{ResolvedCall, ValueId};

/// A source whose consumed dimension no registered cast has established: named once,
/// with every dimension still unresolved on it — a whole-source cast establishes them all in one
/// fact. A missing fact, cleared by a cast landing, never by a ruling or a plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnestablishedFact {
    pub value: ValueId,
    pub dimensions: BTreeSet<Dimension>,
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

/// A voluntary narrowing of the release frontier: committing this call moves the established
/// bound down. The comparison and the recorded acceptance read bounds —
/// unresolved sources remain alongside and are neither narrowed nor erased.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Narrowing {
    pub from: EstablishedLabel,
    pub to: EstablishedLabel,
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

/// The state-only evaluation shared by [`evaluate`] and executable-plan enumeration (`plan`):
/// the gaps and narrowing as the clocks find them, plus the dimensions whose Unknown a label
/// requirement consumes. The state path cannot name values — the views path ([`evaluate`])
/// enumerates them into the block's `unestablished` slot; enumeration reads only the gaps and
/// narrowing, because plans are gap-scoped.
pub(crate) struct StateEval {
    pub(crate) requirement_gaps: Vec<Gap>,
    pub(crate) narrowing: Option<Narrowing>,
    pub(crate) consumed: Vec<Dimension>,
}

/// The partial label the trajectory would hold after this call commits, on the check's clock:
/// the bound narrowed by the delta's established dimensions, the unresolved sets untouched. An
/// unannotated tool contributes identity here — like a pending-cast dimension, its Unknown
/// contribution folds only at admission.
pub(crate) fn committed_label(contract: &ToolContract, current: &PartialLabel) -> PartialLabel {
    let mut committed = current.clone();
    if let Some(delta) = &contract.delta {
        committed.narrow_bound(&delta.established_narrowing());
    }
    committed
}

pub(crate) fn committed_label_for_call(
    contract: &ToolContract,
    current: &PartialLabel,
    call: &ResolvedCall,
) -> PartialLabel {
    let mut committed = committed_label(contract, current);
    if let Some(crate::contract::Delta {
        audience: Some(crate::contract::AudienceDelta::Dynamic(binding)),
        ..
    }) = &contract.delta
        && let Some(audience) = call.dynamic_resolution(binding)
    {
        committed.narrow_bound(&EstablishedLabel::new(Trust::new(u8::MAX), audience.clone()));
    }
    committed
}

/// Evaluate one call against the branch views. Pure: a function of the contract, the views, and
/// the resolved arguments. The block carries every slot at once: the evaluable gaps, the
/// narrowing, and the consumed-Unknown dimensions named per value.
pub(crate) fn evaluate(contract: &ToolContract, views: &Views, call: &ResolvedCall, stage: &CallStage) -> CheckOutcome {
    let current = views.current_label();
    let eval = evaluate_state(
        contract,
        &current,
        &|kind| views.has_effect(kind),
        &|kind| views.has_reservation(kind),
        call,
        stage,
    );
    if eval.requirement_gaps.is_empty() && eval.narrowing.is_none() && eval.consumed.is_empty() {
        return CheckOutcome::Allow;
    }
    let unestablished = unestablished_facts(&current, &eval.consumed);
    CheckOutcome::Block(RawBlock {
        requirement_gaps: eval.requirement_gaps,
        narrowing: eval.narrowing,
        unestablished,
    })
}

/// The gap logic on an abstract `(current label, history predicates)` state — the one place the
/// two clocks live, shared by [`evaluate`] and remedy enumeration (`plan`). History
/// reads two predicates: `has_committed` answers for appended effects, `has_reserved`
/// for unsettled reservations — `prior(k)` consults only the first, `no_prior(k)`
/// fails on either, and the two are never merged. A label requirement that consumes an `Unknown`
/// dimension lands in `consumed`, never in the gaps (masked — one missing fact is not also a
/// coverable gap); requirements on established dimensions evaluate as always. An Unknown
/// dimension nothing requires blocks nothing.
pub(crate) fn evaluate_state(
    contract: &ToolContract,
    current: &PartialLabel,
    has_committed: &impl Fn(&EffectKind) -> bool,
    has_reserved: &impl Fn(&EffectKind) -> bool,
    call: &ResolvedCall,
    stage: &CallStage,
) -> StateEval {
    let committed = committed_label_for_call(contract, current, call);
    let consumed = consumed_unknown(contract, &committed, call, stage);

    let narrowing = (committed.bound() != current.bound()).then(|| Narrowing {
        from: current.bound().clone(),
        to: committed.bound().clone(),
    });

    let mut gaps = Vec::new();
    label_gaps(contract, &committed, call, stage, &mut gaps);
    history_gaps(contract, has_committed, has_reserved, &mut gaps);
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

fn consumed_unknown(
    contract: &ToolContract,
    committed: &PartialLabel,
    call: &ResolvedCall,
    stage: &CallStage,
) -> Vec<Dimension> {
    let mut dims = Vec::new();
    if let Some(floor) = contract.requires.label.trust_floor
        && committed.meets_floor(floor) == Adequacy::Unresolved
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
                Some(recipients) => released_covers(stage, committed, &recipients) == Adequacy::Unresolved,
                None => !released_established(stage, committed),
            },
            AudienceRequirement::Cap(cap) => committed.within_cap(cap) == Adequacy::Unresolved,
        });
    if audience_unresolved {
        dims.push(Dimension::Audience);
    }
    dims
}

/// The block's `unestablished` slot: every source unresolved on a consumed dimension,
/// named once with all of its unresolved dimensions — a whole-source cast clears them together.
/// The branch return check builds its report through this same function.
pub(crate) fn unestablished_facts(current: &PartialLabel, dims: &[Dimension]) -> Vec<UnestablishedFact> {
    let mut sources: BTreeSet<ValueId> = BTreeSet::new();
    for dim in dims {
        sources.extend(current.unresolved(*dim));
    }
    sources
        .into_iter()
        .map(|value| UnestablishedFact {
            value,
            dimensions: [Dimension::Trust, Dimension::Audience]
                .into_iter()
                .filter(|dim| current.is_unresolved(*dim, value))
                .collect(),
        })
        .collect()
}

fn released_covers(stage: &CallStage, committed: &PartialLabel, recipients: &Audience) -> Adequacy {
    match stage.substituted() {
        None => committed.covers(recipients),
        Some(label) => label.audience.covers(recipients),
    }
}

fn released_established(stage: &CallStage, committed: &PartialLabel) -> bool {
    stage.substituted().is_some() || committed.is_established(Dimension::Audience)
}

fn label_gaps(
    contract: &ToolContract,
    committed: &PartialLabel,
    call: &ResolvedCall,
    stage: &CallStage,
    gaps: &mut Vec<Gap>,
) {
    if let Some(floor) = contract.requires.label.trust_floor
        && committed.meets_floor(floor) == Adequacy::Fails
    {
        gaps.push(Gap::TrustFloor {
            required: floor,
            actual: committed.bound().trust,
        });
    }
    for requirement in &contract.requires.label.audience {
        match requirement {
            AudienceRequirement::Includes(spec) => match resolve_recipients(spec, call) {
                Some(recipients) => {
                    if released_covers(stage, committed, &recipients) == Adequacy::Fails {
                        gaps.push(Gap::Includes { recipients });
                    }
                }
                None => match spec {
                    RecipientSpec::Dynamic(binding) => gaps.push(Gap::UnresolvedDynamicRecipient {
                        resolver: binding.resolver.clone(),
                        argument: binding.argument.clone(),
                    }),
                    RecipientSpec::Placeholder(key) => {
                        if released_established(stage, committed) {
                            gaps.push(Gap::Includes {
                                recipients: unresolved_recipient(key),
                            });
                        }
                    }
                    RecipientSpec::Static(_) => {
                        unreachable!("a static includes spec always resolves to its declared audience")
                    }
                },
            },
            AudienceRequirement::Cap(cap) => {
                if committed.within_cap(cap) == Adequacy::Fails {
                    gaps.push(Gap::Cap { cap: cap.clone() });
                }
            }
        }
    }
}

fn history_gaps(
    contract: &ToolContract,
    has_committed: &impl Fn(&EffectKind) -> bool,
    has_reserved: &impl Fn(&EffectKind) -> bool,
    gaps: &mut Vec<Gap>,
) {
    for requirement in &contract.requires.history {
        match requirement {
            HistoryRequirement::Prior(kind) => {
                if !has_committed(kind) {
                    gaps.push(Gap::Prior(kind.clone()));
                }
            }
            HistoryRequirement::NoPrior(kind) => {
                if has_committed(kind) || has_reserved(kind) {
                    gaps.push(Gap::NoPrior(kind.clone()));
                }
            }
        }
    }
}

fn resolve_recipients(spec: &RecipientSpec, call: &ResolvedCall) -> Option<Audience> {
    match spec {
        RecipientSpec::Static(audience) => Some(audience.clone()),
        RecipientSpec::Placeholder(key) => match placeholder_argument(key, call)? {
            AudienceArgument::Public => Some(Audience::Public),
            AudienceArgument::Reader(reader) => Some(Audience::restricted([reader])),
            AudienceArgument::Group(_) => call
                .membership(key)
                .map(|membership| Audience::restricted(membership.readers().iter().cloned())),
        },
        RecipientSpec::Dynamic(binding) => call.dynamic_resolution(binding).cloned(),
    }
}

fn placeholder_argument(key: &str, call: &ResolvedCall) -> Option<AudienceArgument> {
    call.arguments()
        .get(key)
        .and_then(|value| value.as_str())
        .and_then(AudienceArgument::parse)
}

/// One group a proposed call reads through a placeholder: the argument that spells it
/// and the group it spells. What the runtime resolves and what the engine requires a pin for.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GroupRead {
    pub argument: String,
    pub group: GroupName,
}

/// Every group this call's placeholders spell, in argument order. These are
/// the expansions the call must carry before it can be checked.
pub fn group_reads(contract: &ToolContract, call: &ResolvedCall) -> Vec<GroupRead> {
    contract
        .requires
        .label
        .audience
        .iter()
        .filter_map(|requirement| match requirement {
            AudienceRequirement::Includes(RecipientSpec::Placeholder(key)) => match placeholder_argument(key, call) {
                Some(AudienceArgument::Group(group)) => Some(GroupRead {
                    argument: key.clone(),
                    group,
                }),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum MembershipRefusal {
    #[error("the call names groups its check needs expansions for")]
    Needed(Vec<GroupRead>),
    #[error("the pinned membership answer for argument {0} is not bound to this call")]
    Foreign(String),
}

/// The pinned answers a checked call may carry are exactly the ones its placeholders spell:
/// one per group-reading argument, nothing else, and one expansion per group
/// — two arguments spelling the same group share one resolution. The live boundary
/// and the replay validator both run this, so a log cannot hold pins the deciding path refused.
pub(crate) fn validate_memberships(contract: &ToolContract, call: &ResolvedCall) -> Result<(), MembershipRefusal> {
    let reads = group_reads(contract, call);
    let mut expansions: Vec<(&GroupName, &BTreeSet<ReaderId>)> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for membership in call.memberships() {
        let argument = membership.argument();
        let Some(read) = reads.iter().find(|read| read.argument == argument) else {
            return Err(MembershipRefusal::Foreign(argument.to_string()));
        };
        if seen.contains(&argument) {
            return Err(MembershipRefusal::Foreign(argument.to_string()));
        }
        seen.push(argument);
        match expansions.iter().find(|(group, _)| **group == read.group) {
            Some((_, readers)) if *readers != membership.readers() => {
                return Err(MembershipRefusal::Foreign(argument.to_string()));
            }
            Some(_) => {}
            None => expansions.push((&read.group, membership.readers())),
        }
    }
    let missing: Vec<GroupRead> = reads
        .into_iter()
        .filter(|read| !seen.contains(&read.argument.as_str()))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(MembershipRefusal::Needed(missing))
    }
}

fn unresolved_recipient(key: &str) -> Audience {
    Audience::restricted([ReaderId::new(format!("<unresolved:{key}>"))])
}
