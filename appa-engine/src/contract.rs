//! Tool annotations: what a call commits (`delta`, `emits`) and what it requires (`requires`),
//! and the declarations that produce them — statically from policy, or per call through a
//! registered Annotator.

use serde::{Deserialize, Serialize};

use crate::fact::{EffectKind, EffectSet};
use crate::label::{Audience, DeclaredAudience, Label, SymbolicAtom, Trust};
use crate::names::{AnnotatorName, MarkName, TagName};
use crate::value::ToolName;

/// A **declared** restrictive label contribution: what a successful call folds into the trajectory.
/// Every delta only ever narrows — minimum trust, intersect audience — so a permissive delta is
/// unrepresentable. A symbolic audience folds symbolically: the label keeps the clause, and
/// no membership answer is consulted to apply a delta.
///
/// An omitted dimension is neutral: annotating the call is what says the deployment
/// knows it, and a dimension the annotation does not describe restricts nothing ([`Delta::NONE`],
/// `delta = {}` on the config surface, is the same statement written out).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delta {
    pub trust: Option<Trust>,
    pub audience: Option<DeclaredAudience>,
}

impl Delta {
    pub const NONE: Delta = Delta {
        trust: None,
        audience: None,
    };

    /// The delta as a label — the output label a raw result carries, and the meet operand a
    /// successful call narrows the trajectory by. Absent dimensions fill with the fold identity,
    /// so they neither narrow the trajectory nor lower the value's own label. A symbolic
    /// audience enters the label as its clause, unresolved.
    pub fn output_label(&self) -> Label {
        Label::new(
            self.trust.unwrap_or(Trust::new(u8::MAX)),
            match &self.audience {
                Some(audience) => Audience::of_declared(audience),
                None => Audience::public(),
            },
        )
    }

    pub fn is_none(&self) -> bool {
        self.trust.is_none() && self.audience.is_none()
    }

    /// The symbolic atoms this delta writes into the label.
    pub fn symbolic_atoms(&self) -> Box<dyn Iterator<Item = SymbolicAtom> + '_> {
        match &self.audience {
            Some(audience) => audience.symbolic_atoms(),
            None => Box::new(std::iter::empty()),
        }
    }
}

/// An annotation the check can evaluate with no call at hand: nothing it reads comes from a
/// call — no placeholder recipients. This is the only shape a recovery route plans a
/// preceding tool over (RMD-20): its check and its successor state are argument-independent
/// facts of the registry (symbolic audiences included — they ride the label and resolve at
/// the check, not before it). An Annotator-produced annotation never qualifies: it exists
/// only per call.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StaticAnnotation<'a>(&'a ToolAnnotation);

/// Why an annotation is not [`StaticAnnotation`]: a placeholder recipient reads the call's
/// arguments, or the annotation itself is produced per call by an Annotator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the annotation reads its call's arguments")]
pub(crate) struct NotStatic;

impl<'a> StaticAnnotation<'a> {
    pub(crate) fn of(annotation: &'a ToolAnnotation) -> Result<StaticAnnotation<'a>, NotStatic> {
        let placeholder = annotation.requires.audience_requirements().iter().any(|requirement| {
            matches!(
                requirement,
                AudienceRequirement::Includes(RecipientSpec::Placeholder(_))
            )
        });
        if placeholder {
            return Err(NotStatic);
        }
        Ok(StaticAnnotation(annotation))
    }

    pub(crate) fn annotation(&self) -> &'a ToolAnnotation {
        self.0
    }
}

/// The recipients of an audience `includes` requirement — a static set, or a placeholder resolved
/// from the call's arguments (`$recipient` → the value of argument `recipient`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecipientSpec {
    Static(DeclaredAudience),
    Placeholder(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudienceRequirement {
    Includes(RecipientSpec),
    Cap(DeclaredAudience),
}

impl AudienceRequirement {
    /// The symbolic atoms this requirement writes: a static recipient set's and a cap's. A
    /// placeholder's atoms are the call's, read when its argument is.
    pub fn symbolic_atoms(&self) -> Box<dyn Iterator<Item = SymbolicAtom> + '_> {
        match self {
            AudienceRequirement::Includes(RecipientSpec::Static(recipients)) => recipients.symbolic_atoms(),
            AudienceRequirement::Cap(cap) => cap.symbolic_atoms(),
            AudienceRequirement::Includes(RecipientSpec::Placeholder(_)) => Box::new(std::iter::empty()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HistoryRequirement {
    Prior(EffectKind),
    NoPrior(EffectKind),
}

/// The label side of a requirement: an omitted floor is no floor, and an empty audience list
/// demands nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabelRequirements {
    pub trust_floor: Option<Trust>,
    pub audience: Vec<AudienceRequirement>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requires {
    pub label: LabelRequirements,
    pub history: Vec<HistoryRequirement>,
    /// Marks the call must carry; an empty list demands none.
    pub attention: Vec<MarkName>,
}

impl Requires {
    /// The trust floor, when the policy stated one.
    pub fn trust_floor(&self) -> Option<Trust> {
        self.label.trust_floor
    }

    pub fn audience_requirements(&self) -> &[AudienceRequirement] {
        &self.label.audience
    }

    pub fn attention_marks(&self) -> &[MarkName] {
        &self.attention
    }
}

/// One complete tool annotation: the call's operational identity — name, routing tags, what it
/// does, its compiled input schema — and the three algebraic slots. Every call the engine
/// releases is checked against exactly one of these, whether policy declared it statically or a
/// registered Annotator produced it for the exact call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolAnnotation {
    pub name: ToolName,
    pub tags: Vec<TagName>,
    /// What this tool does, in the policy's words. Part of policy identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The compiled, normalized `APPA Tool Parameters v1` schema — part of policy identity.
    /// Omitted `parameters` normalizes to the permissive open object.
    #[serde(default = "crate::params::ToolParameters::open")]
    pub parameters: crate::params::ToolParameters,
    /// The output contribution. An omitted `delta` is [`Delta::NONE`]: the call is annotated,
    /// so its unwritten dimensions restrict nothing.
    #[serde(default)]
    pub delta: Delta,
    pub emits: EffectSet,
    pub requires: Requires,
}

impl ToolAnnotation {
    /// The output shape this annotation gives a raw result: exactly what the annotation
    /// describes, with omitted dimensions at the fold identity.
    pub fn output_label(&self) -> Label {
        self.delta.output_label()
    }

    /// The symbolic atoms this annotation writes: its delta's, its static recipients' and
    /// its cap's. Load validation routes each one; a placeholder's atoms ride the call.
    pub fn symbolic_atoms(&self) -> impl Iterator<Item = SymbolicAtom> + '_ {
        annotation_atoms(&self.delta, &self.requires)
    }

    /// Every atom evaluating this annotation may ask for beyond a placeholder's: the atoms of
    /// its audience requirements, and — only where a requirement compares the committed label
    /// extensionally — its delta's, plus each written reader whose provider prefix names a
    /// registered source (see [`crate::label::Clause::needed_atoms`]). A delta with no
    /// `requires` over it narrows symbolically and reads no membership, so its atoms are not
    /// demanded. Operation gathering reads this.
    pub(crate) fn needed_atoms<'a>(
        &'a self,
        providers: &'a std::collections::BTreeSet<String>,
    ) -> impl Iterator<Item = SymbolicAtom> + 'a {
        let requirements = self.requires.audience_requirements();
        let delta = (!requirements.is_empty())
            .then(|| {
                self.delta
                    .audience
                    .iter()
                    .flat_map(move |audience| audience.needed_atoms(providers))
            })
            .into_iter()
            .flatten();
        delta.chain(requirements.iter().flat_map(move |requirement| match requirement {
            AudienceRequirement::Includes(RecipientSpec::Static(recipients)) => recipients.needed_atoms(providers),
            AudienceRequirement::Cap(cap) => cap.needed_atoms(providers),
            AudienceRequirement::Includes(RecipientSpec::Placeholder(_)) => Box::new(std::iter::empty()),
        }))
    }
}

/// The symbolic atoms a delta and its requirements name, whether the annotation is a
/// compiled declaration or a produced answer.
fn annotation_atoms<'a>(delta: &'a Delta, requires: &'a Requires) -> impl Iterator<Item = SymbolicAtom> + 'a {
    delta.symbolic_atoms().chain(
        requires
            .audience_requirements()
            .iter()
            .flat_map(AudienceRequirement::symbolic_atoms),
    )
}

/// The semantic fields an Annotator produces for one call: the annotation minus the
/// operational metadata, which is the declaration's to state and is never repeated in an
/// answer or a record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProducedAnnotation {
    /// An omitted `delta` is [`Delta::NONE`]: the call is annotated, so its unwritten
    /// dimensions restrict nothing.
    #[serde(default)]
    pub delta: Delta,
    pub emits: EffectSet,
    pub requires: Requires,
}

impl ProducedAnnotation {
    /// The symbolic atoms this produced annotation names. A valid produced annotation is
    /// literal and names none; the validator asks to enforce exactly that.
    pub(crate) fn symbolic_atoms(&self) -> impl Iterator<Item = SymbolicAtom> + '_ {
        annotation_atoms(&self.delta, &self.requires)
    }
}

/// An Annotator's produced answer pinned to the exact call it judged: the annotator that
/// produced it, the canonical digest of the rendered call it saw, and the produced semantic
/// fields. What a proposal carries for an Annotator-routed tool, and what a dispatch record
/// persists for replay: replay validates the same binding — annotator, digest, mandate
/// vocabulary — and never re-runs an implementation. The declaration's operational metadata
/// is not here: replay reads it from the registry, fixed byte-for-byte by the opening's
/// policy identity.
/// The parts live behind one box: a pin rides proposals, evidence, and dispatch records,
/// and the parts inline would bloat every enum that carries them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PinnedAnnotation(Box<PinnedParts>);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PinnedParts {
    annotator: AnnotatorName,
    call: crate::value::CanonicalDigest,
    produced: ProducedAnnotation,
}

impl PinnedAnnotation {
    pub fn new(annotator: AnnotatorName, call: crate::value::CanonicalDigest, produced: ProducedAnnotation) -> Self {
        PinnedAnnotation(Box::new(PinnedParts {
            annotator,
            call,
            produced,
        }))
    }

    /// The Annotator that produced this pin.
    pub fn annotator(&self) -> &AnnotatorName {
        &self.0.annotator
    }

    /// The canonical digest of the exact rendered call the Annotator judged.
    pub fn call(&self) -> &crate::value::CanonicalDigest {
        &self.0.call
    }

    pub fn produced(&self) -> &ProducedAnnotation {
        &self.0.produced
    }

    /// The one complete annotation this pin gives the call under its declaration: the
    /// declaration's operational metadata, the proposed name, and the produced semantic
    /// fields. Derived on demand — never stored — so a record cannot disagree with its
    /// own registry.
    pub(crate) fn tool_annotation(&self, declaration: &ToolDeclaration, called: &ToolName) -> ToolAnnotation {
        ToolAnnotation {
            name: called.clone(),
            tags: declaration.tags().to_vec(),
            description: declaration.description().map(str::to_string),
            parameters: declaration.parameters().clone(),
            delta: self.0.produced.delta.clone(),
            emits: self.0.produced.emits.clone(),
            requires: self.0.produced.requires.clone(),
        }
    }
}

/// The registry's entry for one tool declaration: how calls selected by it are annotated.
/// `Declared` carries the complete static annotation — the declaration *is* the annotation.
/// `Annotated` carries the operational metadata and names the Annotator that produces the
/// semantic fields per call; a declaration never carries both.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolDeclaration {
    Declared(ToolAnnotation),
    Annotated {
        name: ToolName,
        tags: Vec<TagName>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default = "crate::params::ToolParameters::open")]
        parameters: crate::params::ToolParameters,
        annotator: AnnotatorName,
    },
}

impl ToolDeclaration {
    pub fn name(&self) -> &ToolName {
        match self {
            ToolDeclaration::Declared(annotation) => &annotation.name,
            ToolDeclaration::Annotated { name, .. } => name,
        }
    }

    pub(crate) fn set_name(&mut self, renamed: ToolName) {
        match self {
            ToolDeclaration::Declared(annotation) => annotation.name = renamed,
            ToolDeclaration::Annotated { name, .. } => *name = renamed,
        }
    }

    pub fn tags(&self) -> &[TagName] {
        match self {
            ToolDeclaration::Declared(annotation) => &annotation.tags,
            ToolDeclaration::Annotated { tags, .. } => tags,
        }
    }

    pub fn description(&self) -> Option<&str> {
        match self {
            ToolDeclaration::Declared(annotation) => annotation.description.as_deref(),
            ToolDeclaration::Annotated { description, .. } => description.as_deref(),
        }
    }

    pub fn parameters(&self) -> &crate::params::ToolParameters {
        match self {
            ToolDeclaration::Declared(annotation) => &annotation.parameters,
            ToolDeclaration::Annotated { parameters, .. } => parameters,
        }
    }

    /// The Annotator this declaration routes annotation through, when it is not static.
    pub fn annotator(&self) -> Option<&AnnotatorName> {
        match self {
            ToolDeclaration::Declared(_) => None,
            ToolDeclaration::Annotated { annotator, .. } => Some(annotator),
        }
    }

    /// The static annotation, when the declaration is one.
    pub fn declared(&self) -> Option<&ToolAnnotation> {
        match self {
            ToolDeclaration::Declared(annotation) => Some(annotation),
            ToolDeclaration::Annotated { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn annotation(name: &str) -> ToolAnnotation {
        ToolAnnotation {
            name: ToolName::new(name),
            tags: vec![],
            description: Some("A test tool.".to_string()),
            parameters: crate::params::ToolParameters::open(),
            delta: Delta::NONE,
            emits: EffectSet::default(),
            requires: Requires::default(),
        }
    }

    #[test]
    fn a_declaration_dispatches_between_its_static_and_annotated_forms() {
        let declared = ToolDeclaration::Declared(annotation("Read"));
        assert!(declared.annotator().is_none());
        assert_eq!(declared.declared().map(|a| a.name.as_str()), Some("Read"));

        let annotated = ToolDeclaration::Annotated {
            name: ToolName::new("Bash"),
            tags: vec![],
            description: None,
            parameters: crate::params::ToolParameters::open(),
            annotator: AnnotatorName::new("bash-classifier"),
        };
        assert_eq!(annotated.annotator().map(|name| name.as_str()), Some("bash-classifier"));
        assert!(annotated.declared().is_none());
        assert_eq!(annotated.name().as_str(), "Bash");
    }

    #[test]
    fn a_pin_materializes_the_declarations_metadata_and_its_own_semantics() {
        let declaration = ToolDeclaration::Annotated {
            name: ToolName::new("*"),
            tags: vec![TagName::new("shell")],
            description: Some("Runs one shell command.".to_string()),
            parameters: crate::params::ToolParameters::open(),
            annotator: AnnotatorName::new("bash-classifier"),
        };
        let produced = ProducedAnnotation {
            delta: Delta {
                trust: Some(Trust::new(1)),
                audience: None,
            },
            emits: EffectSet::default(),
            requires: Requires::default(),
        };
        let digest = crate::value::CanonicalDigest::of_call(
            &ToolName::new("Bash"),
            &crate::params::test_arguments(&serde_json::json!({ "command": "ls" })),
        );
        let pinned = PinnedAnnotation::new(AnnotatorName::new("bash-classifier"), digest, produced.clone());
        let materialized = pinned.tool_annotation(&declaration, &ToolName::new("Bash"));
        assert_eq!(
            materialized.name.as_str(),
            "Bash",
            "the name is the call's, not the pattern"
        );
        assert_eq!(materialized.tags, declaration.tags());
        assert_eq!(materialized.description.as_deref(), declaration.description());
        assert_eq!(materialized.delta, produced.delta);
        assert_eq!(materialized.emits, produced.emits);
        assert_eq!(materialized.requires, produced.requires);
    }

    #[test]
    fn a_pinned_annotation_round_trips_and_binds_its_call() {
        let digest = crate::value::CanonicalDigest::of_call(
            &ToolName::new("Bash"),
            &crate::params::test_arguments(&serde_json::json!({ "command": "ls" })),
        );
        let pinned = PinnedAnnotation::new(
            AnnotatorName::new("bash-classifier"),
            digest,
            ProducedAnnotation {
                delta: Delta {
                    trust: Some(Trust::new(1)),
                    audience: Some(DeclaredAudience::Public),
                },
                emits: EffectSet::default(),
                requires: Requires::default(),
            },
        );
        let wire = serde_json::to_value(&pinned).expect("a pinned annotation serializes");
        assert_eq!(
            serde_json::from_value::<PinnedAnnotation>(wire).expect("a pinned annotation reads back"),
            pinned
        );
        assert_eq!(pinned.call(), &digest);
        assert_eq!(pinned.annotator().as_str(), "bash-classifier");
    }
}
