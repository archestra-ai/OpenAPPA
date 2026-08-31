//! The two-outcome check: the pure evaluation of a proposed call against the trajectory.

use serde::{Deserialize, Serialize};

use crate::candidate::CallStage;
use crate::contract::{
    AudienceRequirement, HistoryRequirement, RecipientSpec, StaticAnnotation, ToolAnnotation, ToolDeclaration,
};
use crate::fact::EffectKind;
use crate::groups::Expansions;
use crate::label::{Audience, Label, ReaderId, Trust};
use crate::names::{AnnotatorName, AudienceArgument, GroupName, MarkName};
use crate::projection::Views;
use crate::value::ResolvedCall;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Gap {
    TrustFloor { required: Trust, actual: Trust },
    Includes { recipients: Audience },
    Cap { cap: Audience },
    Prior(EffectKind),
    NoPrior(EffectKind),
    Attention(MarkName),
}

/// A voluntary narrowing of the release frontier: committing this call moves the trajectory's
/// label down. The comparison and the recorded acceptance read exactly these two labels.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Narrowing {
    pub from: Label,
    pub to: Label,
}

/// The block as the check finds it — the gaps and/or a narrowing — before remedy planning.
/// The slots are independent and may coexist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawBlock {
    pub requirement_gaps: Vec<Gap>,
    pub narrowing: Option<Narrowing>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckOutcome {
    Allow,
    Block(RawBlock),
}

/// The label the trajectory would hold after this call commits, on the check's clock: the
/// current fold narrowed by the delta.
pub(crate) fn committed_label(annotation: &ToolAnnotation, current: &Label, expansions: &Expansions) -> Label {
    current.combine(&annotation.delta.output_label(expansions))
}

/// What the check reads from the call it evaluates: the arguments its placeholders spell.
/// `Static` is the argument-independent case — a [`StaticAnnotation`] evaluated with no call at
/// hand, which by construction reads nothing.
#[derive(Clone, Copy)]
pub(crate) enum CallReads<'a> {
    Resolved(&'a ResolvedCall),
    Static,
}

/// The state-only evaluation of an argument-independent annotation — the one path a recovery
/// route may check a tool over before any call to it exists (RMD-20). Same gap logic as
/// [`evaluate_state`], at the origin stage, reading no call.
pub(crate) fn evaluate_static(
    annotation: &StaticAnnotation<'_>,
    current: &Label,
    has_committed: &impl Fn(&EffectKind) -> bool,
    has_reserved: &impl Fn(&EffectKind) -> bool,
    expansions: &Expansions,
) -> RawBlock {
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
/// the resolved arguments. The block carries every slot at once: the gaps and the narrowing.
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
    if eval.requirement_gaps.is_empty() && eval.narrowing.is_none() {
        return CheckOutcome::Allow;
    }
    CheckOutcome::Block(eval)
}

/// The gap logic on an abstract `(current label, history predicates)` state — the one place the
/// two clocks live, shared by [`evaluate`] and remedy enumeration (`plan`). History
/// reads two predicates: `has_committed` answers for appended effects, `has_reserved`
/// for unsettled reservations — `prior(k)` consults only the first, `no_prior(k)`
/// fails on either, and the two are never merged.
pub(crate) fn evaluate_state(
    annotation: &ToolAnnotation,
    current: &Label,
    has_committed: &impl Fn(&EffectKind) -> bool,
    has_reserved: &impl Fn(&EffectKind) -> bool,
    reads: CallReads<'_>,
    stage: &CallStage,
    expansions: &Expansions,
) -> RawBlock {
    let committed = committed_label(annotation, current, expansions);

    let narrowing = (&committed != current).then(|| Narrowing {
        from: current.clone(),
        to: committed.clone(),
    });

    let mut gaps = Vec::new();
    label_gaps(annotation, &committed, reads, stage, expansions, &mut gaps);
    history_gaps(annotation, has_committed, has_reserved, &mut gaps);
    for mark in annotation.requires.attention_marks() {
        gaps.push(Gap::Attention(mark.clone()));
    }
    let mut seen = Vec::with_capacity(gaps.len());
    for gap in gaps {
        if !seen.contains(&gap) {
            seen.push(gap);
        }
    }

    RawBlock {
        requirement_gaps: seen,
        narrowing,
    }
}

fn released_covers(stage: &CallStage, committed: &Label, recipients: &Audience) -> bool {
    match stage.substituted() {
        None => committed.covers(recipients),
        Some(label) => label.covers(recipients),
    }
}

fn label_gaps(
    annotation: &ToolAnnotation,
    committed: &Label,
    reads: CallReads<'_>,
    stage: &CallStage,
    expansions: &Expansions,
    gaps: &mut Vec<Gap>,
) {
    if let Some(floor) = annotation.requires.trust_floor()
        && !committed.meets_floor(floor)
    {
        gaps.push(Gap::TrustFloor {
            required: floor,
            actual: committed.trust,
        });
    }
    for requirement in annotation.requires.audience_requirements() {
        match requirement {
            AudienceRequirement::Includes(spec) => match resolve_recipients(spec, reads, expansions) {
                Some(recipients) => {
                    if !released_covers(stage, committed, &recipients) {
                        gaps.push(Gap::Includes { recipients });
                    }
                }
                None => match spec {
                    // An argument that spells no recipients resolves to nothing a released
                    // value could cover — the gap names the unresolved placeholder, closed.
                    RecipientSpec::Placeholder(key) => {
                        gaps.push(Gap::Includes {
                            recipients: unresolved_recipient(key),
                        });
                    }
                    RecipientSpec::Static(_) => {
                        unreachable!("a static includes spec always resolves to its declared audience")
                    }
                },
            },
            AudienceRequirement::Cap(cap) => {
                let cap = cap.resolve(expansions);
                if !committed.within_cap(&cap) {
                    gaps.push(Gap::Cap { cap });
                }
            }
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
/// annotation and takes no pin; an Annotated declaration requires a pin its annotator
/// produced for this exact rendered call — the pin binds the call's canonical digest —
/// whose every produced value is complete, literal, and within the annotator's compiled
/// mandate. The one validator the live check and replay both consume.
pub(crate) fn validate_annotation(
    registry: &crate::registry::Registry,
    declaration: &ToolDeclaration,
    call: &ResolvedCall,
) -> Result<(), AnnotationRefusal> {
    let foreign = |what: &str| AnnotationRefusal::Foreign(what.to_string());
    let (annotator, pinned) = match (declaration.annotator(), call.annotation()) {
        (None, None) => return Ok(()),
        (None, Some(_)) => {
            return Err(foreign("a static declaration is its own annotation and takes no pin"));
        }
        (Some(annotator), None) => return Err(AnnotationRefusal::Needed(annotator.clone())),
        (Some(annotator), Some(pinned)) => (annotator, pinned),
    };
    if pinned.annotator() != annotator {
        return Err(foreign("the pin is not this declaration's annotator's"));
    }
    if pinned.call() != &call.digest() {
        return Err(foreign("the pin binds another call"));
    }
    let annotation = pinned.produced();
    let outside = |what: &str| AnnotationRefusal::OutsidePolicy(what.to_string());
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
    if let Some(trust) = annotation.delta.trust
        && !mandate.permits_trust(trust)
    {
        return Err(outside("the produced delta trust is outside the mandate"));
    }
    if !permits_audience(&annotation.delta.output_label(&expansions).audience) {
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

/// The pinned answers a checked call may carry are exactly the ones its placeholders spell:
/// one per group-reading argument, nothing else, and one expansion per group
/// — two arguments spelling the same group share one resolution. The live boundary
/// and the replay validator both run this, so a log cannot hold pins the deciding path refused.
pub(crate) fn validate_memberships(annotation: &ToolAnnotation, call: &ResolvedCall) -> Result<(), MembershipRefusal> {
    let reads = group_reads(annotation, call);
    let mut expansions: Vec<(&GroupName, &std::collections::BTreeSet<ReaderId>)> = Vec::new();
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
    use crate::contract::{Delta, LabelRequirements, PinnedAnnotation, ProducedAnnotation, Requires, ToolAnnotation};
    use crate::fact::{EffectKind, EffectSet};
    use crate::groups::DeclaredAudience;
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
                        trust: Some(Trust::new(1)),
                        audience: Some(DeclaredAudience::literal(Audience::restricted([ReaderId::new(
                            "support",
                        )]))),
                    },
                    emits: EffectSet::new([EffectKind::new("mail.sent")]).unwrap(),
                    requires: Requires {
                        attention: vec![MarkName::new("reviewed")],
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

    fn produced_of(annotation: ToolAnnotation) -> ProducedAnnotation {
        ProducedAnnotation {
            delta: annotation.delta,
            emits: annotation.emits,
            requires: annotation.requires,
        }
    }

    fn pinned_by_classifier(produced: ToolAnnotation) -> ResolvedCall {
        let unpinned = call("lookup");
        let pin = PinnedAnnotation::new(
            AnnotatorName::new("classifier"),
            unpinned.digest(),
            produced_of(produced),
        );
        unpinned.with_annotation(Some(pin))
    }

    #[test]
    fn a_static_declaration_is_its_own_annotation_and_takes_no_pin() {
        let registry = registry(vec![classifier()]);
        let declaration = registry.tool(&ToolName::new("read")).expect("read is registered");
        let compiled = declaration.declared().expect("read is static").clone();

        assert_eq!(validate_annotation(&registry, declaration, &call("read")), Ok(()));

        let unpinned = call("read");
        let restated = unpinned.clone().with_annotation(Some(PinnedAnnotation::new(
            AnnotatorName::new("classifier"),
            unpinned.digest(),
            produced_of(compiled),
        )));
        assert!(
            matches!(
                validate_annotation(&registry, declaration, &restated),
                Err(AnnotationRefusal::Foreign(_))
            ),
            "a static declaration is its own annotation: even a faithful restatement is a foreign pin"
        );
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

        let unpinned = call("lookup");
        let foreign = unpinned.clone().with_annotation(Some(PinnedAnnotation::new(
            AnnotatorName::new("other"),
            unpinned.digest(),
            produced_of(annotation("lookup")),
        )));
        assert!(
            matches!(
                validate_annotation(&registry, declaration, &foreign),
                Err(AnnotationRefusal::Foreign(_))
            ),
            "the pin must be the declaration's own annotator's"
        );

        let sibling = ResolvedCall::new(
            ToolName::new("lookup"),
            crate::params::test_arguments(&serde_json::json!({ "id": 8 })),
        );
        let reused = call("lookup").with_annotation(Some(PinnedAnnotation::new(
            AnnotatorName::new("classifier"),
            sibling.digest(),
            produced_of(annotation("lookup")),
        )));
        assert!(
            matches!(
                validate_annotation(&registry, declaration, &reused),
                Err(AnnotationRefusal::Foreign(_))
            ),
            "a pin produced for one call cannot ride a sibling call under the same declaration"
        );
    }

    #[test]
    fn a_produced_annotation_must_be_literal() {
        let registry = registry(vec![classifier()]);
        let declaration = registry.tool(&ToolName::new("lookup")).expect("lookup is registered");

        let grouped = ToolAnnotation {
            delta: Delta {
                trust: None,
                audience: Some(DeclaredAudience::declared([], [GroupName::new("team")]).unwrap()),
            },
            ..annotation("lookup")
        };
        let placeholder = ToolAnnotation {
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Includes(RecipientSpec::Placeholder("to".into()))],
                },
                ..Requires::default()
            },
            ..annotation("lookup")
        };
        for produced in [grouped, placeholder] {
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
                trust: Some(Trust::new(0)),
                audience: Some(DeclaredAudience::literal(Audience::restricted([ReaderId::new(
                    "support",
                )]))),
            },
            emits: EffectSet::new([EffectKind::new("mail.sent")]).unwrap(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(Trust::new(0)),
                    audience: vec![AudienceRequirement::Cap(DeclaredAudience::literal(
                        Audience::restricted([ReaderId::new("support")]),
                    ))],
                },
                history: vec![HistoryRequirement::NoPrior(EffectKind::new("mail.sent"))],
                attention: vec![MarkName::new("reviewed")],
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
                    trust: Some(Trust::new(1)),
                    audience: None,
                },
                ..annotation("lookup")
            },
            ToolAnnotation {
                delta: Delta {
                    trust: None,
                    audience: Some(DeclaredAudience::literal(Audience::restricted([ReaderId::new(
                        "stranger",
                    )]))),
                },
                ..annotation("lookup")
            },
            ToolAnnotation {
                requires: Requires {
                    label: LabelRequirements {
                        trust_floor: Some(Trust::new(1)),
                        audience: vec![],
                    },
                    ..Requires::default()
                },
                ..annotation("lookup")
            },
            ToolAnnotation {
                requires: Requires {
                    label: LabelRequirements {
                        trust_floor: None,
                        audience: vec![AudienceRequirement::Includes(RecipientSpec::Static(
                            DeclaredAudience::literal(Audience::restricted([ReaderId::new("stranger")])),
                        ))],
                    },
                    ..Requires::default()
                },
                ..annotation("lookup")
            },
            ToolAnnotation {
                requires: Requires {
                    attention: vec![MarkName::new("invented")],
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
                audience: Some(DeclaredAudience::literal(Audience::Public)),
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
                audience: Some(DeclaredAudience::literal(Audience::restricted([ReaderId::new(
                    "support",
                )]))),
            },
            ..annotation("lookup")
        };
        assert!(matches!(
            validate_annotation(&registry, declaration, &pinned_by_classifier(restricted)),
            Err(AnnotationRefusal::OutsidePolicy(_))
        ));
    }
}
