//! The check: the pure evaluation of a proposed call against the trajectory. Its outcome is
//! allow or block — or, before either, a membership ask: the symbolic atoms the audience
//! comparisons need extensional answers for. The ask is not a decision and appends nothing;
//! the operation gathers the pinned evidence and the same check runs again, decided.

use serde::{Deserialize, Serialize};

use crate::candidate::CallStage;
use crate::contract::{
    AudienceRequirement, HistoryRequirement, RecipientSpec, StaticAnnotation, ToolAnnotation, ToolDeclaration,
};
use crate::fact::EffectKind;
use crate::label::{
    Audience, Clause, DeclaredAudience, Evaluation, Label, MembershipContext, MembershipNeeded, ReaderId, SymbolicAtom,
    Trust,
};
use crate::names::{AnnotatorName, AudienceArgument, MarkName};
use crate::projection::Views;
use crate::value::ResolvedCall;

/// One requirement the committed label misses. The audience payloads are the declared,
/// symbolic recipient sets — feedback renders `internal` or `@finance` as written, never a
/// membership snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Gap {
    TrustFloor { required: Trust, actual: Trust },
    Includes { recipients: DeclaredAudience },
    Cap { cap: DeclaredAudience },
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
/// current fold narrowed by the delta. Expansion-free — symbolic audience atoms fold as
/// themselves.
pub(crate) fn committed_label(annotation: &ToolAnnotation, current: &Label) -> Label {
    current.combine(&annotation.delta.output_label())
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
    context: &MembershipContext<'_>,
) -> Result<RawBlock, MembershipNeeded> {
    evaluate_state(
        annotation.annotation(),
        current,
        has_committed,
        has_reserved,
        CallReads::Static,
        &CallStage::default(),
        context,
    )
}

/// Evaluate one call against the branch views. Pure: a function of the annotation, the views,
/// the resolved arguments, and the membership context. The block carries every slot at once:
/// the gaps and the narrowing. `Err` is the membership ask — the union of every atom the
/// audience comparisons still need, deterministic in the call and the context.
pub(crate) fn evaluate(
    annotation: &ToolAnnotation,
    views: &Views,
    call: &ResolvedCall,
    stage: &CallStage,
    context: &MembershipContext<'_>,
) -> Result<CheckOutcome, MembershipNeeded> {
    let current = views.current_label();
    let eval = evaluate_state(
        annotation,
        &current,
        &|kind| views.has_effect(kind),
        &|kind| views.has_reservation(kind),
        CallReads::Resolved(call),
        stage,
        context,
    )?;
    if eval.requirement_gaps.is_empty() && eval.narrowing.is_none() {
        return Ok(CheckOutcome::Allow);
    }
    Ok(CheckOutcome::Block(eval))
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
    context: &MembershipContext<'_>,
) -> Result<RawBlock, MembershipNeeded> {
    let committed = committed_label(annotation, current);

    let narrowing = (&committed != current).then(|| Narrowing {
        from: current.clone(),
        to: committed.clone(),
    });

    let mut gaps = Vec::new();
    let mut needed = Vec::new();
    label_gaps(annotation, &committed, reads, stage, context, &mut gaps, &mut needed);
    if !needed.is_empty() {
        // An undecided comparison keeps the whole check open: gaps drive remedy planning,
        // and a requirement that is neither held nor refuted cannot be planned over yet.
        needed.sort();
        needed.dedup();
        return Err(MembershipNeeded { needed });
    }
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

    Ok(RawBlock {
        requirement_gaps: seen,
        narrowing,
    })
}

fn released_covers(
    stage: &CallStage,
    committed: &Label,
    recipients: &DeclaredAudience,
    context: &MembershipContext<'_>,
) -> Evaluation {
    match stage.substituted() {
        None => committed.covers(recipients, context),
        Some(label) => label.covers(recipients, context),
    }
}

fn label_gaps(
    annotation: &ToolAnnotation,
    committed: &Label,
    reads: CallReads<'_>,
    stage: &CallStage,
    context: &MembershipContext<'_>,
    gaps: &mut Vec<Gap>,
    needed: &mut Vec<SymbolicAtom>,
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
            AudienceRequirement::Includes(spec) => match resolve_recipients(spec, reads) {
                Some(recipients) => match released_covers(stage, committed, &recipients, context) {
                    Evaluation::Holds => {}
                    Evaluation::Fails => gaps.push(Gap::Includes { recipients }),
                    Evaluation::Needs(mut ask) => needed.append(&mut ask.needed),
                },
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
            AudienceRequirement::Cap(cap) => match committed.within_cap(cap, context) {
                Evaluation::Holds => {}
                Evaluation::Fails => gaps.push(Gap::Cap { cap: cap.clone() }),
                Evaluation::Needs(mut ask) => needed.append(&mut ask.needed),
            },
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

/// The declared recipient set one includes-requirement compares against: the static
/// declaration itself, or the symbolic audience its placeholder argument spells. A chain
/// word, a group reference, and a literal reader all stay symbolic here — membership, if the
/// comparison needs it, is the evaluation's question, never this parse's.
fn resolve_recipients(spec: &RecipientSpec, reads: CallReads<'_>) -> Option<DeclaredAudience> {
    match (spec, reads) {
        (RecipientSpec::Static(declared), _) => Some(declared.clone()),
        (RecipientSpec::Placeholder(_), CallReads::Static) => {
            unreachable!("`StaticAnnotation::of` refuses a placeholder, so a static read never meets one")
        }
        (RecipientSpec::Placeholder(key), CallReads::Resolved(call)) => {
            placeholder_argument(key, call).map(|argument| match argument {
                AudienceArgument::Public => DeclaredAudience::Public,
                AudienceArgument::Chain(chain) => {
                    DeclaredAudience::Union(Clause::new([chain], [], []).expect("a chain clause names no reader"))
                }
                AudienceArgument::Group(group) => {
                    DeclaredAudience::Union(Clause::new([], [group], []).expect("a group clause names no reader"))
                }
                AudienceArgument::Reader(reader) => DeclaredAudience::restricted([reader]),
            })
        }
    }
}

fn placeholder_argument(key: &str, call: &ResolvedCall) -> Option<AudienceArgument> {
    call.arguments()
        .get(key)
        .and_then(|value| value.as_str())
        .and_then(AudienceArgument::parse)
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
    // Literal: a produced annotation pins exact reader sets — no chain words, no groups,
    // no placeholders. Symbolic audiences are the policy author's vocabulary, not an
    // annotator's.
    if annotation.symbolic_atoms().next().is_some() {
        return Err(outside("a produced annotation names a symbolic audience"));
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
    let permits_readers =
        |readers: &std::collections::BTreeSet<ReaderId>| readers.iter().all(|reader| mandate.permits_reader(reader));
    // The literal check above holds here, so every clause is a plain reader list.
    let permits_audience = |audience: &Audience| audience.clauses().all(|clause| permits_readers(clause.readers()));
    let permits_declared = |declared: &DeclaredAudience| match declared {
        DeclaredAudience::Public => true,
        DeclaredAudience::Union(clause) => permits_readers(clause.readers()),
    };
    if let Some(trust) = annotation.delta.trust
        && !mandate.permits_trust(trust)
    {
        return Err(outside("the produced delta trust is outside the mandate"));
    }
    if !permits_audience(&annotation.delta.output_label().audience) {
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
            AudienceRequirement::Includes(RecipientSpec::Static(recipients)) => permits_declared(recipients),
            AudienceRequirement::Cap(cap) => permits_declared(cap),
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

fn unresolved_recipient(key: &str) -> DeclaredAudience {
    DeclaredAudience::restricted([ReaderId::new(format!("<unresolved:{key}>"))])
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::contract::{Delta, LabelRequirements, PinnedAnnotation, ProducedAnnotation, Requires, ToolAnnotation};
    use crate::fact::{EffectKind, EffectSet};
    use crate::label::GroupRef;
    use crate::names::GroupName;
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
                        audience: Some(DeclaredAudience::restricted([ReaderId::new("support")])),
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
            audience: crate::audience::AudienceConfig::default(),
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
                audience: Some(DeclaredAudience::Union(
                    Clause::new([], [GroupRef::Named(GroupName::new("team"))], []).unwrap(),
                )),
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
                audience: Some(DeclaredAudience::restricted([ReaderId::new("support")])),
            },
            emits: EffectSet::new([EffectKind::new("mail.sent")]).unwrap(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(Trust::new(0)),
                    audience: vec![AudienceRequirement::Cap(DeclaredAudience::restricted([ReaderId::new(
                        "support",
                    )]))],
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
                    audience: Some(DeclaredAudience::restricted([ReaderId::new("stranger")])),
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
                            DeclaredAudience::restricted([ReaderId::new("stranger")]),
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
                audience: Some(DeclaredAudience::Public),
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
                audience: Some(DeclaredAudience::restricted([ReaderId::new("support")])),
            },
            ..annotation("lookup")
        };
        assert!(matches!(
            validate_annotation(&registry, declaration, &pinned_by_classifier(restricted)),
            Err(AnnotationRefusal::OutsidePolicy(_))
        ));
    }
}
