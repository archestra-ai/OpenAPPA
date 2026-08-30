//! Tool annotations: what a call commits (`delta`, `emits`) and what it requires (`requires`),
//! and the declarations that produce them — statically from policy, or per call through a
//! registered Annotator.

use serde::{Deserialize, Serialize};

use crate::fact::{EffectKind, EffectSet};
use crate::groups::{DeclaredAudience, Expansions};
use crate::label::{Audience, Dim, Dimension, EstablishedLabel, Label, ReaderId, Trust};
use crate::names::{AnnotatorName, MarkName, TagName};
use crate::value::ToolName;

/// A **declared** restrictive label contribution: what a successful call folds into the trajectory.
/// Every delta only ever narrows — minimum trust, intersect audience — so a permissive delta is
/// unrepresentable. A dimension may also be declared [`Dim::Unknown`]: **pending-cast** — the
/// result's actual state is established by a registered cast at admission, so the raw result
/// is confined until then.
///
/// An omitted dimension is neutral, not unknown: annotating the call is what says the deployment
/// knows it, and a dimension the annotation does not describe restricts nothing ([`Delta::NONE`],
/// `delta = {}` on the config surface, is the same statement written out).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delta {
    pub trust: Option<Dim<Trust>>,
    pub audience: Option<AudienceDelta>,
}

/// The audience half of a `requires` answer: an `includes` floor, a `cap` ceiling, or both.
/// Dynamic answers may not contain groups: the exact literal audiences are pinned with the
/// call and replayed verbatim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequiredAudience {
    pub includes: Option<Audience>,
    pub cap: Option<Audience>,
}

/// One cast's answer to the requirement slots a contract leaves Unknown: the floor the call's
/// inputs must meet, the readers they must be disclosable to (read as `contains`), and the marks
/// the call carries. A slot is answered only where the contract leaves it Unknown; a slot the
/// contract writes is not a cast's to change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequirementAnswer {
    pub trust: Option<Trust>,
    pub audience: Option<Audience>,
    pub attention: Option<Vec<MarkName>>,
}

/// A cast's requirement answer pinned to the call it judged. The pin carries the digest of the
/// canonical call it was answered for, which binds it to that call alone;
/// [`crate::check::validate_requirement_cast`] re-derives the digest and holds the answer to the
/// contract's Unknown slots and the cast's declaration, at the check and again on replay.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PinnedRequirementCast {
    cast: crate::names::CastName,
    call: crate::value::CanonicalDigest,
    required_trust: Option<Trust>,
    required_audience: Option<RequiredAudience>,
    attention: Option<Vec<MarkName>>,
}

impl PinnedRequirementCast {
    /// An answer covering at least one slot, with literal readers only; the marks are
    /// canonicalized.
    pub fn from_answer(
        cast: crate::names::CastName,
        call: crate::value::CanonicalDigest,
        answer: RequirementAnswer,
    ) -> Option<Self> {
        let RequirementAnswer {
            trust,
            audience,
            attention,
        } = answer;
        if trust.is_none() && audience.is_none() && attention.is_none() {
            return None;
        }
        if matches!(&audience, Some(Audience::Restricted(readers)) if !readers.iter().all(ReaderId::is_literal)) {
            return None;
        }
        let attention = attention.map(|mut marks| {
            marks.sort();
            marks.dedup();
            marks
        });
        Some(PinnedRequirementCast {
            cast,
            call,
            required_trust: trust,
            required_audience: audience.map(|includes| RequiredAudience {
                includes: Some(includes),
                cap: None,
            }),
            attention,
        })
    }

    pub fn cast(&self) -> &crate::names::CastName {
        &self.cast
    }

    /// The digest of the canonical call this answer was given for.
    pub fn answered_for(&self) -> &crate::value::CanonicalDigest {
        &self.call
    }

    pub fn required_trust(&self) -> Option<Trust> {
        self.required_trust
    }

    pub fn required_audience(&self) -> Option<&RequiredAudience> {
        self.required_audience.as_ref()
    }

    pub fn attention(&self) -> Option<&[MarkName]> {
        self.attention.as_deref()
    }

    pub fn covers(&self, slot: RequirementSlot) -> bool {
        match slot {
            RequirementSlot::Trust => self.required_trust.is_some(),
            RequirementSlot::Audience => self.required_audience.is_some(),
            RequirementSlot::Attention => self.attention.is_some(),
        }
    }
}

impl<'de> Deserialize<'de> for PinnedRequirementCast {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            cast: crate::names::CastName,
            call: crate::value::CanonicalDigest,
            required_trust: Option<Trust>,
            required_audience: Option<RequiredAudience>,
            attention: Option<Vec<MarkName>>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let audience = match wire.required_audience {
            None => None,
            Some(RequiredAudience {
                includes: Some(includes),
                cap: None,
            }) => Some(includes),
            Some(_) => {
                return Err(serde::de::Error::custom(
                    "a pinned requirement cast answers the audience as a `contains` floor only",
                ));
            }
        };
        let answer = PinnedRequirementCast::from_answer(
            wire.cast,
            wire.call,
            RequirementAnswer {
                trust: wire.required_trust,
                audience,
                attention: wire.attention.clone(),
            },
        )
        .ok_or_else(|| serde::de::Error::custom("a pinned requirement cast must answer a slot with literal readers"))?;
        if answer.attention != wire.attention {
            return Err(serde::de::Error::custom(
                "pinned requirement-cast attention is not in canonical order",
            ));
        }
        Ok(answer)
    }
}

/// The declared audience contribution: a written reader set — literal readers and groups an
/// operation resolves when it reads them — or a pending cast.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudienceDelta {
    Static(DeclaredAudience),
    PendingCast,
}

impl From<Dim<Audience>> for AudienceDelta {
    fn from(value: Dim<Audience>) -> Self {
        match value {
            Dim::Known(audience) => Self::Static(DeclaredAudience::literal(audience)),
            Dim::Unknown => Self::PendingCast,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PinnedMembership {
    argument: String,
    readers: std::collections::BTreeSet<ReaderId>,
}

/// A membership answer that is not evidence: it named the reserved `public` state or an
/// unexpanded group as a reader.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("membership answer for argument {argument} names a non-literal reader {reader:?}")]
pub struct MalformedMembership {
    pub argument: String,
    pub reader: String,
}

impl PinnedMembership {
    pub fn new(
        argument: impl Into<String>,
        readers: impl IntoIterator<Item = ReaderId>,
    ) -> Result<Self, MalformedMembership> {
        let argument = argument.into();
        let readers: std::collections::BTreeSet<ReaderId> = readers.into_iter().collect();
        match readers.iter().find(|reader| !reader.is_literal()) {
            Some(reader) => Err(MalformedMembership {
                argument,
                reader: reader.as_str().to_string(),
            }),
            None => Ok(PinnedMembership { argument, readers }),
        }
    }

    pub fn argument(&self) -> &str {
        &self.argument
    }

    pub fn readers(&self) -> &std::collections::BTreeSet<ReaderId> {
        &self.readers
    }
}

impl<'de> Deserialize<'de> for PinnedMembership {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            argument: String,
            readers: std::collections::BTreeSet<ReaderId>,
        }

        let wire = Wire::deserialize(deserializer)?;
        PinnedMembership::new(wire.argument, wire.readers).map_err(serde::de::Error::custom)
    }
}

impl Delta {
    pub const NONE: Delta = Delta {
        trust: None,
        audience: None,
    };

    /// The delta as a label — the output label a raw result carries. Absent dimensions fill with
    /// the fold identity, so they neither narrow the trajectory nor lower the value's own label; a
    /// pending-cast dimension stays [`Dim::Unknown`] (admission refuses a raw value until a cast
    /// resolves it). A written group reads as the operation resolved it.
    pub fn output_label(&self, expansions: &Expansions) -> Label {
        Label::new(
            self.trust.clone().unwrap_or(Dim::Known(Trust::new(u8::MAX))),
            match &self.audience {
                Some(AudienceDelta::Static(a)) => Dim::Known(a.resolve(expansions)),
                Some(AudienceDelta::PendingCast) => Dim::Unknown,
                None => Dim::Known(Audience::Public),
            },
        )
    }

    /// The narrowing a successful call would commit, on the check's clock: the delta's
    /// **established** dimensions only, as a meet operand. A pending-cast dimension
    /// contributes identity here — its actual contribution folds at admission, at the resolved
    /// label, where every later call re-checks against it. (Sound because load validation
    /// refuses an annotation that pairs a pending-cast dimension with a `requires` on that same
    /// dimension, so no check this projection feeds can depend on the unestablished state.)
    pub fn established_narrowing(&self, expansions: &Expansions) -> EstablishedLabel {
        EstablishedLabel::new(
            match &self.trust {
                Some(Dim::Known(t)) => *t,
                Some(Dim::Unknown) | None => Trust::new(u8::MAX),
            },
            match &self.audience {
                Some(AudienceDelta::Static(a)) => a.resolve(expansions),
                Some(AudienceDelta::PendingCast) | None => Audience::Public,
            },
        )
    }

    pub fn pending_cast_dim(&self) -> Option<Dimension> {
        match (&self.trust, &self.audience) {
            (Some(Dim::Unknown), _) => Some(Dimension::Trust),
            (_, Some(AudienceDelta::PendingCast)) => Some(Dimension::Audience),
            _ => None,
        }
    }

    pub fn is_none(&self) -> bool {
        self.trust.is_none() && self.audience.is_none()
    }

    pub fn groups(&self) -> impl Iterator<Item = &crate::names::GroupName> {
        match &self.audience {
            Some(AudienceDelta::Static(audience)) => Some(audience.groups()),
            _ => None,
        }
        .into_iter()
        .flatten()
    }
}

/// An annotation the check can evaluate with no call at hand: nothing it reads comes from a call —
/// no placeholder recipients, every group it names already expanded — and its output contribution
/// is declared and established (no pending-cast dimension), so the label a successful call
/// commits is known before the call exists. This is the only shape a recovery route plans a
/// preceding tool over (RMD-20): its check and its successor state are argument-independent facts
/// of the registry. An Annotator-produced annotation never qualifies: it exists only per call.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StaticAnnotation<'a>(&'a ToolAnnotation);

/// Why an annotation is not [`StaticAnnotation`]: what a call under it would first have to supply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NotStatic {
    /// A placeholder recipient reads the call's arguments, or the annotation itself is produced
    /// per call by an Annotator.
    Arguments,
    /// The result label is established only at admission — pending-cast.
    Unestablished,
    /// Groups the annotation names that these expansions do not answer.
    Membership(Vec<crate::names::GroupName>),
}

impl<'a> StaticAnnotation<'a> {
    pub(crate) fn of(
        annotation: &'a ToolAnnotation,
        expansions: &Expansions,
    ) -> Result<StaticAnnotation<'a>, NotStatic> {
        let placeholder = annotation.requires.audience_requirements().iter().any(|requirement| {
            matches!(
                requirement,
                AudienceRequirement::Includes(RecipientSpec::Placeholder(_))
            )
        });
        if placeholder {
            return Err(NotStatic::Arguments);
        }
        if annotation.delta.pending_cast_dim().is_some() {
            return Err(NotStatic::Unestablished);
        }
        expansions
            .require(annotation.groups())
            .map_err(|needed| NotStatic::Membership(needed.needed))?;
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
    /// The groups this requirement writes: a static recipient set's and a cap's. A
    /// placeholder's group is the call's, pinned to it, and a dynamic form names none.
    pub fn groups(&self) -> impl Iterator<Item = &crate::names::GroupName> {
        match self {
            AudienceRequirement::Includes(RecipientSpec::Static(recipients)) => Some(recipients.groups()),
            AudienceRequirement::Cap(cap) => Some(cap.groups()),
            AudienceRequirement::Includes(RecipientSpec::Placeholder(_)) => None,
        }
        .into_iter()
        .flatten()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HistoryRequirement {
    Prior(EffectKind),
    NoPrior(EffectKind),
}

/// The label side of a requirement. Each slot is `(T | empty) | unknown`, like a delta
/// dimension: an omitted floor is no floor, an empty audience list demands nothing, and
/// [`Dim::Unknown`] is a requirement the policy did not state — the engine cannot check a
/// flow against it and asks a cast at the proposal, the point of need.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabelRequirements {
    pub trust_floor: Option<Dim<Trust>>,
    pub audience: Dim<Vec<AudienceRequirement>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requires {
    pub label: LabelRequirements,
    pub history: Vec<HistoryRequirement>,
    /// Marks the call must carry: the same monad as the label slots.
    pub attention: Dim<Vec<MarkName>>,
}

/// One requirement slot a cast can establish when the policy left it Unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RequirementSlot {
    Trust,
    Audience,
    Attention,
}

impl RequirementSlot {
    /// The slot as a policy author writes it.
    pub fn wire_name(self) -> &'static str {
        match self {
            RequirementSlot::Trust => "requires.trust",
            RequirementSlot::Audience => "requires.audience",
            RequirementSlot::Attention => "requires.attention",
        }
    }
}

impl Requires {
    /// The static trust floor, when the policy stated one.
    pub fn trust_floor(&self) -> Option<Trust> {
        match self.label.trust_floor {
            Some(Dim::Known(floor)) => Some(floor),
            Some(Dim::Unknown) | None => None,
        }
    }

    /// The static audience requirements; an Unknown slot contributes none, it is asked instead.
    pub fn audience_requirements(&self) -> &[AudienceRequirement] {
        match &self.label.audience {
            Dim::Known(requirements) => requirements,
            Dim::Unknown => &[],
        }
    }

    /// The static marks; an Unknown slot contributes none, it is asked instead.
    pub fn attention_marks(&self) -> &[MarkName] {
        match &self.attention {
            Dim::Known(marks) => marks,
            Dim::Unknown => &[],
        }
    }

    /// Whether the policy wrote anything into the slot — a value or `"unknown"`.
    pub(crate) fn declares(&self, slot: RequirementSlot) -> bool {
        match slot {
            RequirementSlot::Trust => self.label.trust_floor.is_some(),
            RequirementSlot::Audience => !matches!(&self.label.audience, Dim::Known(list) if list.is_empty()),
            RequirementSlot::Attention => !matches!(&self.attention, Dim::Known(marks) if marks.is_empty()),
        }
    }

    /// The slots the policy left Unknown, in slot order.
    pub fn unknown_slots(&self) -> impl Iterator<Item = RequirementSlot> + '_ {
        [
            (
                RequirementSlot::Trust,
                matches!(self.label.trust_floor, Some(Dim::Unknown)),
            ),
            (RequirementSlot::Audience, matches!(self.label.audience, Dim::Unknown)),
            (RequirementSlot::Attention, matches!(self.attention, Dim::Unknown)),
        ]
        .into_iter()
        .filter_map(|(slot, unknown)| unknown.then_some(slot))
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
    /// The complete call as a consult artifact — what an Annotator without an input mapping and a
    /// requirement cast both read: the proposed tool name, this annotation's description when the
    /// policy declares one, and the canonical arguments. A tool without a description sends no
    /// `description` key. The name is the one the actor proposed, not the annotation's own: a
    /// declaration selected by pattern answers for many names, and a classifier that saw the
    /// pattern would be judging a call it cannot identify.
    pub fn complete_call(&self, called: &ToolName, arguments: &serde_json::Value) -> serde_json::Value {
        let mut call = serde_json::Map::new();
        call.insert("name".into(), serde_json::Value::String(called.as_str().to_string()));
        if let Some(description) = &self.description {
            call.insert("description".into(), serde_json::Value::String(description.clone()));
        }
        call.insert("arguments".into(), arguments.clone());
        serde_json::Value::Object(call)
    }

    /// The output shape this annotation gives a raw result. Every dimension is exactly what the
    /// annotation describes; a pending-cast dimension stays Unknown until admission resolves it.
    pub fn output_label(&self, expansions: &Expansions) -> Label {
        self.delta.output_label(expansions)
    }

    /// The groups this annotation's check reads: its delta's, its static recipients' and
    /// its cap's. Required before the check runs; a placeholder's group rides the call instead.
    pub fn groups(&self) -> impl Iterator<Item = &crate::names::GroupName> {
        self.delta.groups().chain(
            self.requires
                .audience_requirements()
                .iter()
                .flat_map(AudienceRequirement::groups),
        )
    }

    /// The single dimension this annotation declares pending-cast, if any.
    pub fn pending_cast_dim(&self) -> Option<Dimension> {
        self.delta.pending_cast_dim()
    }
}

/// The semantic capability an annotation was produced under — what the engine validates the
/// annotation against and what a dispatch record binds. `Declared` is the policy's own static
/// declaration: the annotation must equal the compiled one. `Annotator` names the registered
/// Annotator whose compiled mandate bounds the answer's vocabulary. Backend routing — URL,
/// command, model, timeout — is the runtime's and never appears here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnnotationMandate {
    Declared,
    Annotator(AnnotatorName),
}

/// One complete annotation pinned to the call it was produced for, with the mandate that
/// authorized it. What a proposal carries for an Annotator-recipe tool, and what a dispatch
/// record persists for replay: replay validates the same binding and never re-runs an
/// implementation.
/// The parts live behind one box: a pin rides proposals, evidence, and dispatch records,
/// and a complete annotation inline would bloat every enum that carries them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PinnedAnnotation(Box<PinnedParts>);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PinnedParts {
    annotation: ToolAnnotation,
    mandate: AnnotationMandate,
}

impl PinnedAnnotation {
    pub fn new(annotation: ToolAnnotation, mandate: AnnotationMandate) -> Self {
        PinnedAnnotation(Box::new(PinnedParts { annotation, mandate }))
    }

    pub fn annotation(&self) -> &ToolAnnotation {
        &self.0.annotation
    }

    pub fn mandate(&self) -> &AnnotationMandate {
        &self.0.mandate
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

    /// The mandate a call under this declaration is annotated under.
    pub fn mandate(&self) -> AnnotationMandate {
        match self {
            ToolDeclaration::Declared(_) => AnnotationMandate::Declared,
            ToolDeclaration::Annotated { annotator, .. } => AnnotationMandate::Annotator(annotator.clone()),
        }
    }

    /// Whether a pinned annotation's operational metadata is this declaration's: name aside —
    /// the name is bound to the call — the tags, description, and parameter schema are the
    /// declaration's to state, and an answer may not rewrite them.
    pub(crate) fn metadata_matches(&self, annotation: &ToolAnnotation) -> bool {
        annotation.tags == *self.tags()
            && annotation.description.as_deref() == self.description()
            && annotation.parameters == *self.parameters()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::label::Audience;

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
        assert_eq!(declared.mandate(), AnnotationMandate::Declared);
        assert!(declared.annotator().is_none());
        assert_eq!(declared.declared().map(|a| a.name.as_str()), Some("Read"));

        let annotated = ToolDeclaration::Annotated {
            name: ToolName::new("Bash"),
            tags: vec![],
            description: None,
            parameters: crate::params::ToolParameters::open(),
            annotator: AnnotatorName::new("bash-classifier"),
        };
        assert_eq!(
            annotated.mandate(),
            AnnotationMandate::Annotator(AnnotatorName::new("bash-classifier"))
        );
        assert!(annotated.declared().is_none());
        assert_eq!(annotated.name().as_str(), "Bash");
    }

    #[test]
    fn an_answer_may_not_rewrite_the_declarations_metadata() {
        let declaration = ToolDeclaration::Annotated {
            name: ToolName::new("Bash"),
            tags: vec![TagName::new("shell")],
            description: Some("Runs one shell command.".to_string()),
            parameters: crate::params::ToolParameters::open(),
            annotator: AnnotatorName::new("bash-classifier"),
        };
        let mut produced = ToolAnnotation {
            name: ToolName::new("Bash"),
            tags: vec![TagName::new("shell")],
            description: Some("Runs one shell command.".to_string()),
            parameters: crate::params::ToolParameters::open(),
            delta: Delta::NONE,
            emits: EffectSet::default(),
            requires: Requires::default(),
        };
        assert!(declaration.metadata_matches(&produced));
        produced.tags = vec![];
        assert!(!declaration.metadata_matches(&produced));
    }

    #[test]
    fn a_membership_answer_pins_only_literal_reader_sets() {
        let pin = |readers: &[&str]| PinnedMembership::new("to", readers.iter().map(|reader| ReaderId::new(*reader)));
        assert!(pin(&["public"]).is_err());
        assert!(pin(&["@hr"]).is_err());
        assert!(
            pin(&["finance", "@hr"]).is_err(),
            "one group member spoils the whole answer"
        );
        assert!(pin(&[]).unwrap().readers().is_empty(), "no readers is a valid answer");
        assert_eq!(
            pin(&["ap@corp.example"]).unwrap().readers().len(),
            1,
            "`@` mid-ID is a reader"
        );
        let wire = serde_json::json!({ "argument": "to", "readers": ["public"] });
        assert!(serde_json::from_value::<PinnedMembership>(wire).is_err());
    }

    #[test]
    fn a_pinned_annotation_round_trips_and_binds_its_mandate() {
        let pinned = PinnedAnnotation::new(
            ToolAnnotation {
                delta: Delta {
                    trust: Some(Dim::Known(Trust::new(1))),
                    audience: Some(AudienceDelta::Static(DeclaredAudience::literal(Audience::Public))),
                },
                ..annotation("Bash")
            },
            AnnotationMandate::Annotator(AnnotatorName::new("bash-classifier")),
        );
        let wire = serde_json::to_value(&pinned).expect("a pinned annotation serializes");
        assert_eq!(
            serde_json::from_value::<PinnedAnnotation>(wire).expect("a pinned annotation reads back"),
            pinned
        );
    }
}
