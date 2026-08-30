//! The two-outcome check: the pure evaluation of a proposed call against the trajectory.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::candidate::CallStage;
use crate::contract::{
    AudienceRequirement, HistoryRequirement, PinnedRequirementCast, RecipientSpec, RequirementSlot, StaticAnnotation,
    ToolAnnotation, ToolDeclaration,
};
use crate::fact::EffectKind;
use crate::groups::Expansions;
use crate::label::{Adequacy, Audience, Dimension, EstablishedLabel, PartialLabel, ReaderId, Trust};
use crate::names::{AnnotatorName, AudienceArgument, GroupName, MarkName};
use crate::projection::Views;
use crate::value::{ResolvedCall, ValueId};

/// A source whose consumed dimension no registered cast has established: named once,
/// with every dimension still unresolved on it — a whole-source cast establishes them all in one
/// fact. A missing fact, cleared by a cast landing, never by a ruling or a plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Requirement slots the policy left Unknown. Like `unestablished`, no plan clears them: a
    /// cast establishes the requirement, or the call stays undecidable.
    pub unknown_requirements: Vec<crate::contract::RequirementSlot>,
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
    /// Requirement slots the policy left Unknown and no pinned requirement cast answered: the
    /// check cannot judge the flow against them, so the call is not decidable until a cast
    /// establishes each one.
    pub(crate) unknown_requirements: Vec<crate::contract::RequirementSlot>,
}

/// The partial label the trajectory would hold after this call commits, on the check's clock:
/// the bound narrowed by the delta's established dimensions, the unresolved sets untouched. A
/// pending-cast dimension contributes identity here — its Unknown contribution folds only at
/// admission.
pub(crate) fn committed_label(
    annotation: &ToolAnnotation,
    current: &PartialLabel,
    expansions: &Expansions,
) -> PartialLabel {
    let mut committed = current.clone();
    committed.narrow_bound(&annotation.delta.established_narrowing(expansions));
    committed
}

/// What the check reads from the call it evaluates: the requirement cast pinned to it and the
/// arguments its placeholders spell. `Static` is the argument-independent case — a
/// [`StaticAnnotation`] evaluated with no call at hand, which by construction reads nothing.
#[derive(Clone, Copy)]
pub(crate) enum CallReads<'a> {
    Resolved(&'a ResolvedCall),
    Static,
}

impl<'a> CallReads<'a> {
    fn requirement_cast(self) -> Option<&'a PinnedRequirementCast> {
        match self {
            CallReads::Resolved(call) => call.requirement_cast(),
            CallReads::Static => None,
        }
    }
}

/// The state-only evaluation of an argument-independent annotation — the one path a recovery
/// route may check a tool over before any call to it exists (RMD-20). Same gap logic as
/// [`evaluate_state`], at the origin stage, reading no call.
pub(crate) fn evaluate_static(
    annotation: &StaticAnnotation<'_>,
    current: &PartialLabel,
    has_committed: &impl Fn(&EffectKind) -> bool,
    has_reserved: &impl Fn(&EffectKind) -> bool,
    expansions: &Expansions,
) -> StateEval {
    evaluate_state(
        annotation.annotation(),
        current,
        has_committed,
        has_reserved,
        CallReads::Static,
        &CallStage::default(),
        expansions,
    )
}

/// Evaluate one call against the branch views. Pure: a function of the annotation, the views, and
/// the resolved arguments. The block carries every slot at once: the evaluable gaps, the
/// narrowing, and the consumed-Unknown dimensions named per value.
pub(crate) fn evaluate(
    annotation: &ToolAnnotation,
    views: &Views,
    call: &ResolvedCall,
    stage: &CallStage,
    expansions: &Expansions,
) -> CheckOutcome {
    let current = views.current_label();
    let eval = evaluate_state(
        annotation,
        &current,
        &|kind| views.has_effect(kind),
        &|kind| views.has_reservation(kind),
        CallReads::Resolved(call),
        stage,
        expansions,
    );
    if eval.requirement_gaps.is_empty()
        && eval.narrowing.is_none()
        && eval.consumed.is_empty()
        && eval.unknown_requirements.is_empty()
    {
        return CheckOutcome::Allow;
    }
    let unestablished = unestablished_facts(&current, &eval.consumed);
    CheckOutcome::Block(RawBlock {
        requirement_gaps: eval.requirement_gaps,
        narrowing: eval.narrowing,
        unestablished,
        unknown_requirements: eval.unknown_requirements,
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
    annotation: &ToolAnnotation,
    current: &PartialLabel,
    has_committed: &impl Fn(&EffectKind) -> bool,
    has_reserved: &impl Fn(&EffectKind) -> bool,
    reads: CallReads<'_>,
    stage: &CallStage,
    expansions: &Expansions,
) -> StateEval {
    let committed = committed_label(annotation, current, expansions);
    let consumed = consumed_unknown(annotation, &committed, reads, stage, expansions);

    let narrowing = (committed.bound() != current.bound()).then(|| Narrowing {
        from: current.bound().clone(),
        to: committed.bound().clone(),
    });

    let mut gaps = Vec::new();
    label_gaps(annotation, &committed, reads, stage, expansions, &mut gaps);
    history_gaps(annotation, has_committed, has_reserved, &mut gaps);
    for mark in annotation.requires.attention_marks().iter().chain(
        reads
            .requirement_cast()
            .and_then(PinnedRequirementCast::attention)
            .into_iter()
            .flatten(),
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
        unknown_requirements: annotation
            .requires
            .unknown_slots()
            .filter(|slot| !reads.requirement_cast().is_some_and(|pinned| pinned.covers(*slot)))
            .collect(),
    }
}

fn consumed_unknown(
    annotation: &ToolAnnotation,
    committed: &PartialLabel,
    reads: CallReads<'_>,
    stage: &CallStage,
    expansions: &Expansions,
) -> Vec<Dimension> {
    let mut dims = Vec::new();
    if effective_trust_floors(annotation, reads).any(|floor| committed.meets_floor(floor) == Adequacy::Unresolved) {
        dims.push(Dimension::Trust);
    }
    let audience_unresolved = annotation
        .requires
        .audience_requirements()
        .iter()
        .any(|requirement| match requirement {
            AudienceRequirement::Includes(spec) => match resolve_recipients(spec, reads, expansions) {
                Some(recipients) => released_covers(stage, committed, &recipients) == Adequacy::Unresolved,
                None => !released_established(stage, committed),
            },
            AudienceRequirement::Cap(cap) => committed.within_cap(&cap.resolve(expansions)) == Adequacy::Unresolved,
        })
        || pinned_audience_requirements(reads).any(|required| {
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

/// Every trust floor this call must meet: the annotation's floor and the floor a requirement
/// cast answered for an Unknown slot — one stream, so the static and dynamic halves cannot
/// drift on how a floor is judged.
fn effective_trust_floors<'a>(
    annotation: &'a ToolAnnotation,
    reads: CallReads<'a>,
) -> impl Iterator<Item = Trust> + 'a {
    annotation
        .requires
        .trust_floor()
        .into_iter()
        .chain(reads.requirement_cast().and_then(PinnedRequirementCast::required_trust))
}

/// Every audience requirement pinned to the call by a requirement cast answering an Unknown slot.
fn pinned_audience_requirements<'a>(
    reads: CallReads<'a>,
) -> impl Iterator<Item = &'a crate::contract::RequiredAudience> {
    reads
        .requirement_cast()
        .and_then(PinnedRequirementCast::required_audience)
        .into_iter()
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
    annotation: &ToolAnnotation,
    committed: &PartialLabel,
    reads: CallReads<'_>,
    stage: &CallStage,
    expansions: &Expansions,
    gaps: &mut Vec<Gap>,
) {
    for floor in effective_trust_floors(annotation, reads) {
        if committed.meets_floor(floor) == Adequacy::Fails {
            gaps.push(Gap::TrustFloor {
                required: floor,
                actual: committed.bound().trust,
            });
        }
    }
    for requirement in annotation.requires.audience_requirements() {
        match requirement {
            AudienceRequirement::Includes(spec) => match resolve_recipients(spec, reads, expansions) {
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
    for required in pinned_audience_requirements(reads) {
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
    annotation: &ToolAnnotation,
    has_committed: &impl Fn(&EffectKind) -> bool,
    has_reserved: &impl Fn(&EffectKind) -> bool,
    gaps: &mut Vec<Gap>,
) {
    for requirement in &annotation.requires.history {
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

fn resolve_recipients(spec: &RecipientSpec, reads: CallReads<'_>, expansions: &Expansions) -> Option<Audience> {
    match (spec, reads) {
        (RecipientSpec::Static(audience), _) => Some(audience.resolve(expansions)),
        (RecipientSpec::Placeholder(_), CallReads::Static) => {
            unreachable!("`StaticAnnotation::of` refuses a placeholder, so a static read never meets one")
        }
        (RecipientSpec::Placeholder(key), CallReads::Resolved(call)) => match placeholder_argument(key, call)? {
            AudienceArgument::Public => Some(Audience::Public),
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
pub fn group_reads(annotation: &ToolAnnotation, call: &ResolvedCall) -> Vec<GroupRead> {
    annotation
        .requires
        .audience_requirements()
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

/// Why a call's annotation evidence is not admissible, or what it still owes.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum AnnotationRefusal {
    #[error("the call needs an annotation from annotator {}", .0.as_str())]
    Needed(AnnotatorName),
    #[error("the pinned annotation is not this call's under its declaration: {0}")]
    Foreign(String),
    #[error("the pinned annotation is outside its mandate: {0}")]
    OutsidePolicy(String),
}

/// Hold a call's annotation evidence to its declaration: a static declaration is its own
/// annotation and a pin, if one rides along, must restate it exactly; an Annotated declaration
/// requires a pin whose mandate names its annotator, whose operational metadata is the
/// declaration's, whose name is the call's, and whose every produced value is complete,
/// concrete, literal, and within the annotator's compiled mandate. The one validator the live
/// check and replay both consume.
pub(crate) fn validate_annotation(
    registry: &crate::registry::Registry,
    declaration: &ToolDeclaration,
    call: &ResolvedCall,
) -> Result<(), AnnotationRefusal> {
    let (annotator, pinned) = match (declaration.annotator(), call.annotation()) {
        (None, None) => return Ok(()),
        (None, Some(pinned)) => {
            // A static declaration's compiled annotation is the only admissible pin, and its
            // mandate is the policy's own declaration.
            let compiled = declaration
                .declared()
                .expect("a declaration without an annotator is static");
            let restates =
                pinned.mandate() == &crate::contract::AnnotationMandate::Declared && pinned.annotation() == compiled;
            return match restates {
                true => Ok(()),
                false => Err(AnnotationRefusal::Foreign(
                    "a static declaration is its own annotation".into(),
                )),
            };
        }
        (Some(annotator), None) => return Err(AnnotationRefusal::Needed(annotator.clone())),
        (Some(annotator), Some(pinned)) => (annotator, pinned),
    };
    let foreign = |what: &str| AnnotationRefusal::Foreign(what.to_string());
    if pinned.mandate() != &crate::contract::AnnotationMandate::Annotator(annotator.clone()) {
        return Err(foreign("the mandate is not this declaration's annotator"));
    }
    let annotation = pinned.annotation();
    if annotation.name != *call.tool() {
        return Err(foreign("the annotation names another tool"));
    }
    if !declaration.metadata_matches(annotation) {
        return Err(foreign("the annotation rewrites the declaration's metadata"));
    }
    let outside = |what: &str| AnnotationRefusal::OutsidePolicy(what.to_string());
    // Complete and concrete: an Annotator answers values, never the pending or Unknown states.
    if annotation.pending_cast_dim().is_some() {
        return Err(outside("a produced delta dimension is pending-cast"));
    }
    if annotation.requires.unknown_slots().next().is_some() {
        return Err(outside("a produced requirement slot is unknown"));
    }
    // Literal: a produced annotation pins exact reader sets — no groups, no placeholders.
    if annotation.groups().next().is_some() {
        return Err(outside("a produced annotation names a group"));
    }
    let placeholder = annotation.requires.audience_requirements().iter().any(|requirement| {
        matches!(
            requirement,
            AudienceRequirement::Includes(RecipientSpec::Placeholder(_))
        )
    });
    if placeholder {
        return Err(outside("a produced annotation reads a placeholder"));
    }
    let mandate = registry
        .annotator_mandate(annotator)
        .expect("declarations name only registered annotators");
    let permits_audience = |audience: &Audience| match audience {
        Audience::Public => true,
        Audience::Restricted(readers) => readers.iter().all(|reader| mandate.permits_reader(reader)),
    };
    let expansions = Expansions::empty_members(&[]);
    if let Some(crate::label::Dim::Known(trust)) = annotation.delta.trust
        && !mandate.permits_trust(trust)
    {
        return Err(outside("the produced delta trust is outside the mandate"));
    }
    if let crate::label::Dim::Known(audience) = annotation.output_label(&expansions).audience
        && !permits_audience(&audience)
    {
        return Err(outside("the produced delta audience is outside the mandate"));
    }
    if annotation
        .requires
        .trust_floor()
        .is_some_and(|floor| !mandate.permits_trust(floor))
    {
        return Err(outside("the produced trust floor is outside the mandate"));
    }
    let audience_within = annotation
        .requires
        .audience_requirements()
        .iter()
        .all(|requirement| match requirement {
            AudienceRequirement::Includes(RecipientSpec::Static(recipients)) => {
                permits_audience(&recipients.resolve(&expansions))
            }
            AudienceRequirement::Cap(cap) => permits_audience(&cap.resolve(&expansions)),
            AudienceRequirement::Includes(RecipientSpec::Placeholder(_)) => true,
        });
    if !audience_within {
        return Err(outside("a produced audience requirement is outside the mandate"));
    }
    if annotation
        .requires
        .attention_marks()
        .iter()
        .any(|mark| !mandate.permits_mark(mark))
    {
        return Err(outside("a produced attention mark is outside the mandate"));
    }
    let history_within = annotation.requires.history.iter().all(|requirement| match requirement {
        HistoryRequirement::Prior(kind) | HistoryRequirement::NoPrior(kind) => mandate.permits_effect(kind),
    });
    if !history_within {
        return Err(outside("a produced history requirement is outside the mandate"));
    }
    if annotation.emits.iter().any(|kind| !mandate.permits_effect(kind)) {
        return Err(outside("a produced effect is outside the mandate"));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum MembershipRefusal {
    #[error("the call names groups its check needs expansions for")]
    Needed(Vec<GroupRead>),
    #[error("the pinned membership answer for argument {0} is not bound to this call")]
    Foreign(String),
}

/// Why a call's requirement-cast pin is not admissible, or what it still owes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RequirementCastRefusal {
    /// The annotation leaves these slots Unknown and the call carries no answer.
    Needed(Vec<RequirementSlot>),
    /// The pin is not about this call: no such cast, a cast whose scope does not reach the
    /// annotation, an answer for other arguments or other slots, or a constant's groups the
    /// record does not expand.
    Foreign(String),
    /// The answer is not one the cast gives: a value outside the policy vocabulary, off the
    /// constant, or over `may_cast`.
    OutsidePolicy(String),
}

/// Hold a call's requirement-cast pin to the annotation's Unknown slots and the cast's
/// declaration: exactly the Unknown slots are answered, for exactly this canonical call, by a
/// registered cast reaching the annotation, with a constant's own values or within a resolver's
/// ceiling. The one validator the check, composition, and replay consume.
pub(crate) fn validate_requirement_cast(
    registry: &crate::registry::Registry,
    annotation: &ToolAnnotation,
    call: &ResolvedCall,
    expansions: &Expansions,
) -> Result<(), RequirementCastRefusal> {
    let slots: Vec<RequirementSlot> = annotation.requires.unknown_slots().collect();
    let Some(pinned) = call.requirement_cast() else {
        return match slots.is_empty() {
            true => Ok(()),
            false => Err(RequirementCastRefusal::Needed(slots)),
        };
    };
    let name = pinned.cast().as_str().to_string();
    let foreign = || RequirementCastRefusal::Foreign(name.clone());
    if slots.is_empty() || pinned.answered_for() != &call.digest() {
        return Err(foreign());
    }
    // The pin names a cast the engine consults for this annotation: covering it, able to answer
    // the slots, and before or at the first constant in registration order.
    let cast = registry
        .requirement_cast_order(&annotation.tags, &slots)
        .find(|cast| cast.name == *pinned.cast())
        .ok_or_else(foreign)?;
    if [
        RequirementSlot::Trust,
        RequirementSlot::Audience,
        RequirementSlot::Attention,
    ]
    .into_iter()
    .any(|slot| pinned.covers(slot) != slots.contains(&slot))
    {
        return Err(foreign());
    }
    if expansions.require(cast.resolution.groups()).is_err() {
        return Err(foreign());
    }
    let outside = || RequirementCastRefusal::OutsidePolicy(name.clone());
    if pinned
        .required_trust()
        .is_some_and(|trust| !registry.trust_chain().contains_rank(trust))
        || pinned
            .attention()
            .into_iter()
            .flatten()
            .any(|mark| !registry.knows_attention_mark(mark))
    {
        return Err(outside());
    }
    match cast.resolution.admits_requirement(pinned, expansions) {
        true => Ok(()),
        false => Err(outside()),
    }
}

/// The groups a call's pinned requirement cast reads: a constant's declared audience, a
/// resolver's ceiling. Composition requires them beside the annotation's own.
pub(crate) fn requirement_cast_groups<'a>(
    registry: &'a crate::registry::Registry,
    call: &ResolvedCall,
) -> impl Iterator<Item = &'a crate::names::GroupName> {
    call.requirement_cast()
        .and_then(|pinned| registry.cast(pinned.cast()))
        .into_iter()
        .flat_map(|cast| cast.resolution.groups())
}

/// The pinned answers a checked call may carry are exactly the ones its placeholders spell:
/// one per group-reading argument, nothing else, and one expansion per group
/// — two arguments spelling the same group share one resolution. The live boundary
/// and the replay validator both run this, so a log cannot hold pins the deciding path refused.
pub(crate) fn validate_memberships(annotation: &ToolAnnotation, call: &ResolvedCall) -> Result<(), MembershipRefusal> {
    let reads = group_reads(annotation, call);
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
    annotation: &ToolAnnotation,
    call: &ResolvedCall,
    expansions: &Expansions,
) -> Result<(), MembershipRefusal> {
    let reads = group_reads(annotation, call);
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
    use std::collections::BTreeSet;

    use super::*;
    use crate::contract::{
        AnnotationMandate, AudienceDelta, Delta, LabelRequirements, PinnedAnnotation, Requires, ToolAnnotation,
    };
    use crate::fact::{EffectKind, EffectSet};
    use crate::groups::DeclaredAudience;
    use crate::label::Dim;
    use crate::names::TagName;
    use crate::params::ToolParameters;
    use crate::registry::{AnnotatorDeclaration, Registry, RegistryConfig, TrustChain};
    use crate::value::ToolName;

    fn annotation(name: &str) -> ToolAnnotation {
        ToolAnnotation {
            name: ToolName::new(name),
            tags: vec![],
            description: None,
            parameters: ToolParameters::open(),
            delta: Delta::NONE,
            emits: EffectSet::default(),
            requires: Requires::default(),
        }
    }

    fn classifier() -> AnnotatorDeclaration {
        AnnotatorDeclaration {
            name: AnnotatorName::new("classifier"),
            trust: None,
            audiences: None,
            marks: None,
            effects: None,
        }
    }

    /// A policy with one static tool (whose declarations feed the vocabulary an omitted mandate
    /// bound resolves to) and one tool annotated per call by `classifier`.
    fn registry(annotators: Vec<AnnotatorDeclaration>) -> Registry {
        Registry::build_covered(RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![
                ToolDeclaration::Declared(ToolAnnotation {
                    description: Some("Reads one file.".to_string()),
                    delta: Delta {
                        trust: Some(Dim::Known(Trust::new(1))),
                        audience: Some(AudienceDelta::Static(DeclaredAudience::literal(Audience::restricted(
                            [ReaderId::new("support")],
                        )))),
                    },
                    emits: EffectSet::new([EffectKind::new("mail.sent")]).unwrap(),
                    requires: Requires {
                        attention: Dim::Known(vec![MarkName::new("reviewed")]),
                        ..Requires::default()
                    },
                    ..annotation("read")
                }),
                ToolDeclaration::Annotated {
                    name: ToolName::new("lookup"),
                    tags: vec![],
                    description: None,
                    parameters: ToolParameters::open(),
                    annotator: AnnotatorName::new("classifier"),
                },
            ],
            annotators,
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![],
            membership: None,
        })
        .expect("the fixture policy loads")
    }

    fn call(tool: &str) -> ResolvedCall {
        ResolvedCall::new(
            ToolName::new(tool),
            crate::params::test_arguments(&serde_json::json!({ "id": 7 })),
        )
    }

    fn pinned_by_classifier(produced: ToolAnnotation) -> ResolvedCall {
        call("lookup").with_annotation(Some(PinnedAnnotation::new(
            produced,
            AnnotationMandate::Annotator(AnnotatorName::new("classifier")),
        )))
    }

    #[test]
    fn a_static_declaration_is_its_own_annotation() {
        let registry = registry(vec![classifier()]);
        let declaration = registry.tool(&ToolName::new("read")).expect("read is registered");
        let compiled = declaration.declared().expect("read is static").clone();

        assert_eq!(validate_annotation(&registry, declaration, &call("read")), Ok(()));
        let restated = call("read").with_annotation(Some(PinnedAnnotation::new(
            compiled.clone(),
            AnnotationMandate::Declared,
        )));
        assert_eq!(validate_annotation(&registry, declaration, &restated), Ok(()));

        let mut edited = compiled.clone();
        edited.delta = Delta {
            trust: Some(Dim::Known(Trust::new(0))),
            audience: None,
        };
        let forged = call("read").with_annotation(Some(PinnedAnnotation::new(edited, AnnotationMandate::Declared)));
        assert!(matches!(
            validate_annotation(&registry, declaration, &forged),
            Err(AnnotationRefusal::Foreign(_))
        ));

        let wrong_mandate = call("read").with_annotation(Some(PinnedAnnotation::new(
            compiled,
            AnnotationMandate::Annotator(AnnotatorName::new("classifier")),
        )));
        assert!(matches!(
            validate_annotation(&registry, declaration, &wrong_mandate),
            Err(AnnotationRefusal::Foreign(_))
        ));
    }

    #[test]
    fn an_annotated_declaration_requires_its_annotators_pin() {
        let registry = registry(vec![classifier()]);
        let declaration = registry.tool(&ToolName::new("lookup")).expect("lookup is registered");

        assert!(matches!(
            validate_annotation(&registry, declaration, &call("lookup")),
            Err(AnnotationRefusal::Needed(name)) if name.as_str() == "classifier"
        ));

        assert_eq!(
            validate_annotation(&registry, declaration, &pinned_by_classifier(annotation("lookup"))),
            Ok(())
        );

        let declared_mandate = call("lookup").with_annotation(Some(PinnedAnnotation::new(
            annotation("lookup"),
            AnnotationMandate::Declared,
        )));
        assert!(matches!(
            validate_annotation(&registry, declaration, &declared_mandate),
            Err(AnnotationRefusal::Foreign(_))
        ));

        assert!(
            matches!(
                validate_annotation(&registry, declaration, &pinned_by_classifier(annotation("other"))),
                Err(AnnotationRefusal::Foreign(_))
            ),
            "the annotation must name the tool the actor called"
        );

        let mut retagged = annotation("lookup");
        retagged.tags = vec![TagName::new("shell")];
        assert!(
            matches!(
                validate_annotation(&registry, declaration, &pinned_by_classifier(retagged)),
                Err(AnnotationRefusal::Foreign(_))
            ),
            "an answer may not rewrite the declaration's metadata"
        );
    }

    #[test]
    fn a_produced_annotation_must_be_complete_concrete_and_literal() {
        let registry = registry(vec![classifier()]);
        let declaration = registry.tool(&ToolName::new("lookup")).expect("lookup is registered");

        let pending = ToolAnnotation {
            delta: Delta {
                trust: Some(Dim::Unknown),
                audience: None,
            },
            ..annotation("lookup")
        };
        let unknown_slot = ToolAnnotation {
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(Dim::Unknown),
                    audience: Dim::Known(vec![]),
                },
                ..Requires::default()
            },
            ..annotation("lookup")
        };
        let grouped = ToolAnnotation {
            delta: Delta {
                trust: None,
                audience: Some(AudienceDelta::Static(
                    DeclaredAudience::declared([], [GroupName::new("team")]).unwrap(),
                )),
            },
            ..annotation("lookup")
        };
        let placeholder = ToolAnnotation {
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: Dim::Known(vec![AudienceRequirement::Includes(RecipientSpec::Placeholder(
                        "to".into(),
                    ))]),
                },
                ..Requires::default()
            },
            ..annotation("lookup")
        };
        for produced in [pending, unknown_slot, grouped, placeholder] {
            assert!(matches!(
                validate_annotation(&registry, declaration, &pinned_by_classifier(produced.clone())),
                Err(AnnotationRefusal::OutsidePolicy(_))
            ));
        }
    }

    #[test]
    fn a_produced_annotation_stays_within_its_mandate() {
        let bounded = AnnotatorDeclaration {
            name: AnnotatorName::new("classifier"),
            trust: Some(BTreeSet::from([Trust::new(0)])),
            audiences: Some(BTreeSet::from([ReaderId::new("support")])),
            marks: Some(BTreeSet::from([MarkName::new("reviewed")])),
            effects: Some(BTreeSet::from([EffectKind::new("mail.sent")])),
        };
        let registry = registry(vec![bounded]);
        let declaration = registry.tool(&ToolName::new("lookup")).expect("lookup is registered");

        let within = ToolAnnotation {
            delta: Delta {
                trust: Some(Dim::Known(Trust::new(0))),
                audience: Some(AudienceDelta::Static(DeclaredAudience::literal(Audience::restricted(
                    [ReaderId::new("support")],
                )))),
            },
            emits: EffectSet::new([EffectKind::new("mail.sent")]).unwrap(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(Dim::Known(Trust::new(0))),
                    audience: Dim::Known(vec![AudienceRequirement::Cap(DeclaredAudience::literal(
                        Audience::restricted([ReaderId::new("support")]),
                    ))]),
                },
                history: vec![HistoryRequirement::NoPrior(EffectKind::new("mail.sent"))],
                attention: Dim::Known(vec![MarkName::new("reviewed")]),
            },
            ..annotation("lookup")
        };
        assert_eq!(
            validate_annotation(&registry, declaration, &pinned_by_classifier(within)),
            Ok(())
        );

        let outside = [
            ToolAnnotation {
                delta: Delta {
                    trust: Some(Dim::Known(Trust::new(1))),
                    audience: None,
                },
                ..annotation("lookup")
            },
            ToolAnnotation {
                delta: Delta {
                    trust: None,
                    audience: Some(AudienceDelta::Static(DeclaredAudience::literal(Audience::restricted(
                        [ReaderId::new("stranger")],
                    )))),
                },
                ..annotation("lookup")
            },
            ToolAnnotation {
                requires: Requires {
                    label: LabelRequirements {
                        trust_floor: Some(Dim::Known(Trust::new(1))),
                        audience: Dim::Known(vec![]),
                    },
                    ..Requires::default()
                },
                ..annotation("lookup")
            },
            ToolAnnotation {
                requires: Requires {
                    label: LabelRequirements {
                        trust_floor: None,
                        audience: Dim::Known(vec![AudienceRequirement::Includes(RecipientSpec::Static(
                            DeclaredAudience::literal(Audience::restricted([ReaderId::new("stranger")])),
                        ))]),
                    },
                    ..Requires::default()
                },
                ..annotation("lookup")
            },
            ToolAnnotation {
                requires: Requires {
                    attention: Dim::Known(vec![MarkName::new("invented")]),
                    ..Requires::default()
                },
                ..annotation("lookup")
            },
            ToolAnnotation {
                requires: Requires {
                    history: vec![HistoryRequirement::Prior(EffectKind::new("wire.sent"))],
                    ..Requires::default()
                },
                ..annotation("lookup")
            },
            ToolAnnotation {
                emits: EffectSet::new([EffectKind::new("wire.sent")]).unwrap(),
                ..annotation("lookup")
            },
        ];
        for produced in outside {
            assert!(matches!(
                validate_annotation(&registry, declaration, &pinned_by_classifier(produced.clone())),
                Err(AnnotationRefusal::OutsidePolicy(_))
            ));
        }
    }

    /// A produced annotation is public policy vocabulary; `public` itself is always an
    /// admissible audience state, whatever the reader bound says.
    #[test]
    fn public_is_always_within_an_audience_mandate() {
        let bounded = AnnotatorDeclaration {
            audiences: Some(BTreeSet::new()),
            ..classifier()
        };
        let registry = registry(vec![bounded]);
        let declaration = registry.tool(&ToolName::new("lookup")).expect("lookup is registered");
        let produced = ToolAnnotation {
            delta: Delta {
                trust: None,
                audience: Some(AudienceDelta::Static(DeclaredAudience::literal(Audience::Public))),
            },
            ..annotation("lookup")
        };
        assert_eq!(
            validate_annotation(&registry, declaration, &pinned_by_classifier(produced)),
            Ok(())
        );
        let restricted = ToolAnnotation {
            delta: Delta {
                trust: None,
                audience: Some(AudienceDelta::Static(DeclaredAudience::literal(Audience::restricted(
                    [ReaderId::new("support")],
                )))),
            },
            ..annotation("lookup")
        };
        assert!(matches!(
            validate_annotation(&registry, declaration, &pinned_by_classifier(restricted)),
            Err(AnnotationRefusal::OutsidePolicy(_))
        ));
    }
}
