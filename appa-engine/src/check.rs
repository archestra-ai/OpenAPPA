//! The two-outcome check: the pure evaluation of a proposed call against the trajectory.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::candidate::CallStage;
use crate::contract::{AudienceRequirement, HistoryRequirement, RecipientSpec, ToolContract, ToolResolverUse};
use crate::fact::EffectKind;
use crate::groups::Expansions;
use crate::label::{Adequacy, Audience, Dimension, EstablishedLabel, PartialLabel, ReaderId, Trust};
use crate::names::{AudienceArgument, GroupName, MarkName};
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
pub(crate) fn committed_label(
    contract: &ToolContract,
    current: &PartialLabel,
    expansions: &Expansions,
) -> PartialLabel {
    let mut committed = current.clone();
    if let Some(delta) = &contract.delta {
        committed.narrow_bound(&delta.established_narrowing(expansions));
    }
    committed
}

pub(crate) fn committed_label_for_call(
    contract: &ToolContract,
    current: &PartialLabel,
    call: &ResolvedCall,
    expansions: &Expansions,
) -> PartialLabel {
    let mut committed = committed_label(contract, current, expansions);
    for resolution in call.tool_resolutions() {
        committed.narrow_bound(&EstablishedLabel::new(
            resolution.trust().unwrap_or(Trust::new(u8::MAX)),
            resolution.audience().cloned().unwrap_or(Audience::Public),
        ));
    }
    committed
}

/// Evaluate one call against the branch views. Pure: a function of the contract, the views, and
/// the resolved arguments. The block carries every slot at once: the evaluable gaps, the
/// narrowing, and the consumed-Unknown dimensions named per value.
pub(crate) fn evaluate(
    contract: &ToolContract,
    views: &Views,
    call: &ResolvedCall,
    stage: &CallStage,
    expansions: &Expansions,
) -> CheckOutcome {
    let current = views.current_label();
    let eval = evaluate_state(
        contract,
        &current,
        &|kind| views.has_effect(kind),
        &|kind| views.has_reservation(kind),
        call,
        stage,
        expansions,
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
    expansions: &Expansions,
) -> StateEval {
    let committed = committed_label_for_call(contract, current, call, expansions);
    let consumed = consumed_unknown(contract, &committed, call, stage, expansions);

    let narrowing = (committed.bound() != current.bound()).then(|| Narrowing {
        from: current.bound().clone(),
        to: committed.bound().clone(),
    });

    let mut gaps = Vec::new();
    label_gaps(contract, &committed, call, stage, expansions, &mut gaps);
    history_gaps(contract, has_committed, has_reserved, &mut gaps);
    for mark in contract.requires.attention.iter().chain(
        call.tool_resolutions()
            .iter()
            .flat_map(|resolution| resolution.attention()),
    ) {
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
    expansions: &Expansions,
) -> Vec<Dimension> {
    let mut dims = Vec::new();
    if effective_trust_floors(contract, call).any(|floor| committed.meets_floor(floor) == Adequacy::Unresolved) {
        dims.push(Dimension::Trust);
    }
    let audience_unresolved = contract
        .requires
        .label
        .audience
        .iter()
        .any(|requirement| match requirement {
            AudienceRequirement::Includes(spec) => match resolve_recipients(spec, call, expansions) {
                Some(recipients) => released_covers(stage, committed, &recipients) == Adequacy::Unresolved,
                None => !released_established(stage, committed),
            },
            AudienceRequirement::Cap(cap) => committed.within_cap(&cap.resolve(expansions)) == Adequacy::Unresolved,
        })
        || pinned_audience_requirements(call).any(|required| {
            required
                .includes
                .as_ref()
                .is_some_and(|recipients| released_covers(stage, committed, recipients) == Adequacy::Unresolved)
                || required
                    .cap
                    .as_ref()
                    .is_some_and(|cap| committed.within_cap(cap) == Adequacy::Unresolved)
        });
    if audience_unresolved {
        dims.push(Dimension::Audience);
    }
    dims
}

/// Every trust floor this call must meet: the contract's static floor, then each floor a
/// tool-level resolver pinned to the call — one stream, so the static and dynamic halves
/// cannot drift on how a floor is judged.
fn effective_trust_floors<'a>(contract: &'a ToolContract, call: &'a ResolvedCall) -> impl Iterator<Item = Trust> + 'a {
    contract.requires.label.trust_floor.into_iter().chain(
        call.tool_resolutions()
            .iter()
            .filter_map(|resolution| resolution.required_trust()),
    )
}

/// Every audience requirement pinned to the call by a tool-level resolver.
fn pinned_audience_requirements(call: &ResolvedCall) -> impl Iterator<Item = &crate::contract::RequiredAudience> {
    call.tool_resolutions()
        .iter()
        .filter_map(|resolution| resolution.required_audience())
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
    expansions: &Expansions,
    gaps: &mut Vec<Gap>,
) {
    for floor in effective_trust_floors(contract, call) {
        if committed.meets_floor(floor) == Adequacy::Fails {
            gaps.push(Gap::TrustFloor {
                required: floor,
                actual: committed.bound().trust,
            });
        }
    }
    for requirement in &contract.requires.label.audience {
        match requirement {
            AudienceRequirement::Includes(spec) => match resolve_recipients(spec, call, expansions) {
                Some(recipients) => {
                    if released_covers(stage, committed, &recipients) == Adequacy::Fails {
                        gaps.push(Gap::Includes { recipients });
                    }
                }
                None => match spec {
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
                let cap = cap.resolve(expansions);
                if committed.within_cap(&cap) == Adequacy::Fails {
                    gaps.push(Gap::Cap { cap });
                }
            }
        }
    }
    for required in pinned_audience_requirements(call) {
        if let Some(recipients) = &required.includes
            && released_covers(stage, committed, recipients) == Adequacy::Fails
        {
            gaps.push(Gap::Includes {
                recipients: recipients.clone(),
            });
        }
        if let Some(cap) = &required.cap
            && committed.within_cap(cap) == Adequacy::Fails
        {
            gaps.push(Gap::Cap { cap: cap.clone() });
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

fn resolve_recipients(spec: &RecipientSpec, call: &ResolvedCall, expansions: &Expansions) -> Option<Audience> {
    match spec {
        RecipientSpec::Static(audience) => Some(audience.resolve(expansions)),
        RecipientSpec::Placeholder(key) => match placeholder_argument(key, call)? {
            AudienceArgument::Public => Some(Audience::Public),
            AudienceArgument::Private => Some(Audience::Private),
            AudienceArgument::Reader(reader) => Some(Audience::restricted([reader])),
            AudienceArgument::Group(_) => call
                .membership(key)
                .map(|membership| Audience::restricted(membership.readers().iter().cloned())),
        },
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
pub(crate) enum ToolResolutionRefusal {
    #[error("the call needs tool-level resolver answers")]
    Needed(Vec<ToolResolverUse>),
    #[error("the pinned tool-level answer from resolver {0} is not bound to this call")]
    Foreign(String),
    #[error("the pinned tool-level answer from resolver {0} contains a value outside policy")]
    OutsidePolicy(String),
}

pub(crate) fn validate_tool_resolutions(
    registry: &crate::registry::Registry,
    contract: &ToolContract,
    call: &ResolvedCall,
) -> Result<(), ToolResolutionRefusal> {
    let declared = &contract.uses;
    let mut seen: Vec<&ToolResolverUse> = Vec::new();
    for pinned in call.tool_resolutions() {
        let uses = pinned.uses();
        // An answer belongs to this call only if the tool declares that use *and* the answer was
        // given for the exact value this call would send it. Rebuilding the payload is what keeps
        // one call's answer from standing in for another's.
        if !declared.contains(uses)
            || seen.contains(&uses)
            || pinned.args() != contract.resolver_args_digest(uses, call.canonical_arguments().value())
        {
            return Err(ToolResolutionRefusal::Foreign(uses.resolver.as_str().to_string()));
        }
        // Every value the pin carries, not only the ones this tool reads: an unread result
        // establishes nothing, but it is persisted, so it answers to the policy's vocabulary too.
        if pinned
            .every_trust()
            .any(|trust| !registry.trust_chain().contains_rank(trust))
            || pinned.every_mark().iter().any(|mark| !registry.attends(mark))
        {
            return Err(ToolResolutionRefusal::OutsidePolicy(uses.resolver.as_str().to_string()));
        }
        seen.push(uses);
    }
    let missing: Vec<ToolResolverUse> = declared.iter().filter(|uses| !seen.contains(uses)).cloned().collect();
    match missing.is_empty() {
        true => Ok(()),
        false => Err(ToolResolutionRefusal::Needed(missing)),
    }
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

/// One operation reads one answer per group: a pin whose readers differ from the
/// expansion the operation reads the same group under — a group a declaration also writes — is
/// not evidence for this operation. Pins no read spells are [`validate_memberships`]'s.
pub(crate) fn pins_agree(
    contract: &ToolContract,
    call: &ResolvedCall,
    expansions: &Expansions,
) -> Result<(), MembershipRefusal> {
    let reads = group_reads(contract, call);
    for membership in call.memberships() {
        let disagrees = reads
            .iter()
            .find(|read| read.argument == membership.argument())
            .and_then(|read| expansions.readers(&read.group))
            .is_some_and(|readers| readers != membership.readers());
        if disagrees {
            return Err(MembershipRefusal::Foreign(membership.argument().to_string()));
        }
    }
    Ok(())
}

fn unresolved_recipient(key: &str) -> Audience {
    Audience::restricted([ReaderId::new(format!("<unresolved:{key}>"))])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::{Authority, Mandate, Scope};
    use crate::candidate::CallStage;
    use crate::contract::{PinnedToolResolution, RequiredAudience, ResolverReturn, ToolResolverUse};
    use crate::fact::EffectSet;
    use crate::label::{Dim, Label};
    use crate::names::{AuthorityName, DynamicResolverName, MarkName};
    use crate::params::ToolParameters;
    use crate::registry::{Registry, RegistryConfig, TrustChain};
    use crate::value::ToolName;

    #[test]
    fn tool_resolution_narrows_both_dimensions_and_adds_fresh_attention() {
        let binding = ToolResolverUse {
            resolver: DynamicResolverName::new("classifier"),
            inputs: std::collections::BTreeMap::new(),
            returns: [
                ResolverReturn::Trust,
                ResolverReturn::Audience,
                ResolverReturn::Attention,
            ]
            .into_iter()
            .collect(),
            reads: [
                ResolverReturn::Trust,
                ResolverReturn::Audience,
                ResolverReturn::Attention,
            ]
            .into_iter()
            .collect(),
        };
        let contract = ToolContract {
            name: ToolName::new("lookup"),
            tags: vec![],
            parameters: ToolParameters::open(),
            description: Some("A test tool.".to_string()),
            uses: vec![binding.clone()],
            delta: None,
            emits: EffectSet::default(),
            requires: crate::contract::Requires {
                attention: vec![MarkName::new("static-review")],
                ..Default::default()
            },
        };
        assert_eq!(
            contract.output_label(&Expansions::default()),
            Label::new(Dim::Unknown, Dim::Unknown)
        );
        let call = ResolvedCall::new(
            ToolName::new("lookup"),
            crate::params::CanonicalArguments::from_value(&serde_json::json!({"id": 7}), &ToolParameters::open())
                .expect("arguments compile"),
        )
        .with_tool_resolutions(vec![
            PinnedToolResolution::from_answer(
                binding,
                crate::contract::ResolverArgsDigest::of(b""),
                Some(Trust::new(0)),
                Some(Audience::restricted([ReaderId::new("support")])),
                None,
                None,
                Some(vec![MarkName::new("dynamic-review")]),
            )
            .expect("the declared fields pin"),
        ]);
        let current = PartialLabel::established(EstablishedLabel::top());
        let outcome = evaluate_state(
            &contract,
            &current,
            &|_| false,
            &|_| false,
            &call,
            &CallStage::default(),
            &Expansions::default(),
        );
        assert_eq!(
            outcome.narrowing.expect("both resolved fields narrow").to,
            EstablishedLabel::new(Trust::new(0), Audience::restricted([ReaderId::new("support")]))
        );
        assert_eq!(outcome.requirement_gaps.len(), 2);
        assert!(
            outcome
                .requirement_gaps
                .contains(&Gap::Attention(MarkName::new("dynamic-review")))
        );
        assert!(
            outcome
                .requirement_gaps
                .contains(&Gap::Attention(MarkName::new("static-review")))
        );
    }

    #[test]
    fn tool_resolution_adds_dynamic_trust_audience_and_attention_requirements() {
        let binding = ToolResolverUse {
            resolver: DynamicResolverName::new("classifier"),
            inputs: std::collections::BTreeMap::new(),
            returns: [
                ResolverReturn::Trust,
                ResolverReturn::Audience,
                ResolverReturn::RequiredTrust,
                ResolverReturn::RequiredAudience,
                ResolverReturn::Attention,
            ]
            .into_iter()
            .collect(),
            reads: [
                ResolverReturn::Trust,
                ResolverReturn::Audience,
                ResolverReturn::RequiredTrust,
                ResolverReturn::RequiredAudience,
                ResolverReturn::Attention,
            ]
            .into_iter()
            .collect(),
        };
        let contract = ToolContract {
            name: ToolName::new("lookup"),
            tags: vec![],
            parameters: ToolParameters::open(),
            description: Some("A test tool.".to_string()),
            uses: vec![binding.clone()],
            delta: None,
            emits: EffectSet::default(),
            requires: Default::default(),
        };
        let call = ResolvedCall::new(
            ToolName::new("lookup"),
            crate::params::CanonicalArguments::from_value(&serde_json::json!({"id": 7}), &ToolParameters::open())
                .expect("arguments compile"),
        )
        .with_tool_resolutions(vec![
            PinnedToolResolution::from_answer(
                binding,
                crate::contract::ResolverArgsDigest::of(b""),
                Some(Trust::new(0)),
                Some(Audience::restricted([ReaderId::new("support")])),
                Some(Trust::new(1)),
                Some(RequiredAudience {
                    includes: Some(Audience::restricted([ReaderId::new("audit")])),
                    cap: Some(Audience::restricted([ReaderId::new("audit")])),
                }),
                Some(vec![MarkName::new("operator-signoff")]),
            )
            .expect("the scoped answer pins"),
        ]);
        let outcome = evaluate_state(
            &contract,
            &PartialLabel::established(EstablishedLabel::top()),
            &|_| false,
            &|_| false,
            &call,
            &CallStage::default(),
            &Expansions::default(),
        );
        assert!(outcome.requirement_gaps.contains(&Gap::TrustFloor {
            required: Trust::new(1),
            actual: Trust::new(0),
        }));
        assert!(outcome.requirement_gaps.contains(&Gap::Includes {
            recipients: Audience::restricted([ReaderId::new("audit")]),
        }));
        assert!(outcome.requirement_gaps.contains(&Gap::Cap {
            cap: Audience::restricted([ReaderId::new("audit")]),
        }));
        assert!(
            outcome
                .requirement_gaps
                .contains(&Gap::Attention(MarkName::new("operator-signoff")))
        );
    }

    #[test]
    fn tool_resolution_values_must_come_from_the_policy() {
        let binding = ToolResolverUse {
            resolver: DynamicResolverName::new("classifier"),
            inputs: std::collections::BTreeMap::new(),
            returns: [
                ResolverReturn::Trust,
                ResolverReturn::RequiredTrust,
                ResolverReturn::Attention,
            ]
            .into_iter()
            .collect(),
            reads: [
                ResolverReturn::Trust,
                ResolverReturn::RequiredTrust,
                ResolverReturn::Attention,
            ]
            .into_iter()
            .collect(),
        };
        let contract = ToolContract {
            name: ToolName::new("lookup"),
            tags: vec![],
            parameters: ToolParameters::open(),
            description: Some("A test tool.".to_string()),
            uses: vec![binding.clone()],
            delta: None,
            emits: EffectSet::default(),
            requires: Default::default(),
        };
        let registry = Registry::build_covered(RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![contract],
            authorities: vec![Authority {
                name: AuthorityName::new("operator"),
                mandate: Mandate {
                    trust_ceiling: Some(Trust::new(1)),
                    attends: vec![MarkName::new("operator-signoff")],
                    ..Mandate::default()
                },
                scope: Scope::default(),
                hint: None,
            }],
            sanitizers: vec![],
            casts: vec![],
            membership: None,
        })
        .expect("the policy loads");
        let contract = registry.tool(&ToolName::new("lookup")).expect("the tool is registered");
        let arguments = serde_json::json!({});
        let args = contract.resolver_args_digest(&binding, &arguments);
        let call = |trust, required_trust, attention: &str| {
            ResolvedCall::new(
                ToolName::new("lookup"),
                crate::params::CanonicalArguments::from_value(&arguments, &ToolParameters::open())
                    .expect("arguments compile"),
            )
            .with_tool_resolutions(vec![
                PinnedToolResolution::from_answer(
                    binding.clone(),
                    args,
                    Some(trust),
                    None,
                    Some(required_trust),
                    None,
                    Some(vec![MarkName::new(attention)]),
                )
                .expect("the declared fields pin"),
            ])
        };

        assert!(
            validate_tool_resolutions(
                &registry,
                contract,
                &call(Trust::new(0), Trust::new(1), "operator-signoff"),
            )
            .is_ok()
        );
        for invalid in [
            call(Trust::new(0), Trust::new(1), "invented-review"),
            call(Trust::new(2), Trust::new(1), "operator-signoff"),
            call(Trust::new(0), Trust::new(2), "operator-signoff"),
        ] {
            assert!(matches!(
                validate_tool_resolutions(&registry, contract, &invalid),
                Err(ToolResolutionRefusal::OutsidePolicy(resolver)) if resolver == "classifier"
            ));
        }
    }

    #[test]
    fn a_result_the_tool_never_reads_still_answers_to_the_policy_vocabulary() {
        use crate::names::DynamicResolverName;
        use crate::registry::{Registry, RegistryConfig};
        use crate::value::ToolName;

        // The resolver returns both trusts; the tool reads only the output one.
        let uses = ToolResolverUse {
            resolver: DynamicResolverName::new("classifier"),
            inputs: std::collections::BTreeMap::new(),
            returns: [ResolverReturn::Trust, ResolverReturn::RequiredTrust]
                .into_iter()
                .collect(),
            reads: [ResolverReturn::Trust].into_iter().collect(),
        };
        let registry = Registry::build_covered(RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![ToolContract {
                name: ToolName::new("lookup"),
                tags: vec![],
                description: Some("A test tool.".to_string()),
                parameters: ToolParameters::open(),
                uses: vec![uses.clone()],
                delta: None,
                emits: EffectSet::default(),
                requires: Default::default(),
            }],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![],
            membership: None,
        })
        .expect("the policy loads");
        let contract = registry.tool(&ToolName::new("lookup")).expect("lookup is registered");
        let arguments = serde_json::json!({});
        let call = |required_trust| {
            ResolvedCall::new(
                ToolName::new("lookup"),
                crate::params::CanonicalArguments::from_value(&arguments, &ToolParameters::open())
                    .expect("arguments compile"),
            )
            .with_tool_resolutions(vec![
                PinnedToolResolution::from_answer(
                    uses.clone(),
                    contract.resolver_args_digest(&uses, &arguments),
                    Some(Trust::new(0)),
                    None,
                    Some(required_trust),
                    None,
                    None,
                )
                .expect("the declared fields pin"),
            ])
        };

        assert!(validate_tool_resolutions(&registry, contract, &call(Trust::new(1))).is_ok());
        assert!(
            matches!(
                validate_tool_resolutions(&registry, contract, &call(Trust::new(9))),
                Err(ToolResolutionRefusal::OutsidePolicy(resolver)) if resolver == "classifier"
            ),
            "a rank outside the chain is refused even where no field reads it"
        );
    }

    #[test]
    fn an_answer_given_for_other_arguments_is_not_evidence_for_this_call() {
        use crate::names::DynamicResolverName;
        use crate::registry::{Registry, RegistryConfig};
        use crate::value::ToolName;

        let uses = ToolResolverUse {
            resolver: DynamicResolverName::new("acl"),
            inputs: std::collections::BTreeMap::from([(
                "subject".to_string(),
                crate::contract::ToolCallSource::argument("file").expect("a plain name is a source"),
            )]),
            returns: [ResolverReturn::Audience].into_iter().collect(),
            reads: [ResolverReturn::Audience].into_iter().collect(),
        };
        let registry = Registry::build_covered(RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![ToolContract {
                name: ToolName::new("read"),
                tags: vec![],
                description: Some("Reads one file.".to_string()),
                parameters: ToolParameters::compile(&serde_json::json!({
                    "type": "object",
                    "properties": { "file": { "type": "string" } },
                    "required": ["file"],
                }))
                .expect("the schema compiles"),
                uses: vec![uses.clone()],
                delta: Some(crate::contract::Delta::NONE),
                emits: EffectSet::default(),
                requires: Default::default(),
            }],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![],
            membership: None,
        })
        .expect("the policy loads");
        let contract = registry.tool(&ToolName::new("read")).expect("read is registered");

        let call = |file: &str| {
            ResolvedCall::new(
                ToolName::new("read"),
                crate::params::CanonicalArguments::from_value(
                    &serde_json::json!({ "file": file }),
                    &contract.parameters,
                )
                .expect("arguments compile"),
            )
        };
        let answer_for = |file: &str| {
            PinnedToolResolution::from_answer(
                uses.clone(),
                contract.resolver_args_digest(&uses, &serde_json::json!({ "file": file })),
                None,
                Some(Audience::restricted([ReaderId::new("hr-lead")])),
                None,
                None,
                None,
            )
            .expect("the declared audience answer pins")
        };

        assert!(
            validate_tool_resolutions(
                &registry,
                contract,
                &call("payroll.md").with_tool_resolutions(vec![answer_for("payroll.md")]),
            )
            .is_ok()
        );
        // The same resolver, the same tool, the same declared use — and an answer given about a
        // different file. It is not evidence here.
        assert!(matches!(
            validate_tool_resolutions(
                &registry,
                contract,
                &call("packet.md").with_tool_resolutions(vec![answer_for("payroll.md")]),
            ),
            Err(ToolResolutionRefusal::Foreign(resolver)) if resolver == "acl"
        ));
    }
}
