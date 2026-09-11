//! Tool annotations: what a call commits (`delta`, `emits`) and what it requires (`requires`),
//! and the declarations that produce them — statically from policy, or per call through a
//! registered Annotator.

use serde::{Deserialize, Serialize};

use crate::fact::{EffectKind, EffectSet};
use crate::label::{Audience, Clause, DeclaredAudience, GroupRef, Label, SymbolicAtom, Trust};
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
    pub audience: Option<DeltaAudience>,
}

impl Delta {
    pub const NONE: Delta = Delta {
        trust: None,
        audience: None,
    };

    /// The delta as a label — the output label a raw result carries, and the meet operand a
    /// successful call narrows the trajectory by. Absent dimensions fill with the fold identity,
    /// so they neither narrow the trajectory nor lower the value's own label. A symbolic
    /// audience enters the label as its clause, unresolved. A selector placeholder names no
    /// label until a call binds it — see [`ToolAnnotation::bound_to`].
    pub fn output_label(&self) -> Label {
        let audience = match &self.audience {
            Some(DeltaAudience::Static(audience)) => Audience::of_declared(audience),
            Some(DeltaAudience::Selector(placeholder)) => {
                unreachable!("delta placeholder {placeholder} is bound to its call before a label is read from it")
            }
            None => Audience::public(),
        };
        Label::new(self.trust.unwrap_or(Trust::new(u8::MAX)), audience)
    }

    pub fn is_none(&self) -> bool {
        self.trust.is_none() && self.audience.is_none()
    }

    /// The symbolic atoms this delta writes into the label as written; a placeholder writes
    /// the call's, unknown here.
    pub(crate) fn symbolic_atoms(&self) -> impl Iterator<Item = SymbolicAtom> + '_ {
        self.audience
            .iter()
            .filter_map(DeltaAudience::declared)
            .flat_map(DeclaredAudience::symbolic_atoms)
    }

    pub(crate) fn selector_placeholder(&self) -> Option<&SelectorPlaceholder> {
        match &self.audience {
            Some(DeltaAudience::Selector(placeholder)) => Some(placeholder),
            Some(DeltaAudience::Static(_)) | None => None,
        }
    }
}

/// An annotation the check can evaluate with no call at hand: nothing it reads comes from a
/// call — no placeholder recipients, no placeholder delta. This is the only shape a recovery route plans a
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
                AudienceRequirement::Includes(RecipientSpec::Placeholder(_) | RecipientSpec::Selector(_))
            )
        });
        if placeholder || annotation.delta.selector_placeholder().is_some() {
            return Err(NotStatic);
        }
        Ok(StaticAnnotation(annotation))
    }

    pub(crate) fn annotation(&self) -> &'a ToolAnnotation {
        self.0
    }
}

/// The recipients of an audience `includes` requirement — a static set, a placeholder resolved
/// from the call's arguments (`$recipient` → the value of argument `recipient`), or a source
/// collection the arguments key (`@slack:channel/$channel_id`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecipientSpec {
    Static(DeclaredAudience),
    Placeholder(String),
    Selector(SelectorPlaceholder),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudienceRequirement {
    Includes(RecipientSpec),
    Cap(DeclaredAudience),
}

impl AudienceRequirement {
    /// The audience this requirement writes: a static recipient set or a cap. A
    /// placeholder's audience is the call's, read when its argument is.
    pub(crate) fn declared(&self) -> Option<&DeclaredAudience> {
        match self {
            AudienceRequirement::Includes(RecipientSpec::Static(recipients)) => Some(recipients),
            AudienceRequirement::Cap(cap) => Some(cap),
            AudienceRequirement::Includes(RecipientSpec::Placeholder(_) | RecipientSpec::Selector(_)) => None,
        }
    }
}

/// One segment of a selector placeholder: text written as is, or the name of the call argument
/// whose value fills it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Segment {
    Literal(String),
    Argument(String),
}

/// A source collection keyed by the call: `@provider:selector` where at least one `/`-separated
/// segment is `$argument`. Instantiated per call into the ordinary `@provider:selector` mention
/// the arguments spell, whose members are then read exactly as for a static mention.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SelectorPlaceholder {
    provider: String,
    segments: Vec<Segment>,
}

/// Why a call's arguments do not fill a selector placeholder: a value must be one non-empty
/// selector segment a policy could write — no `/`, which would change the collection's shape,
/// and no leading `$`, which spells a placeholder.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "argument {argument:?} does not fill a selector segment: a non-empty string without `/` and not starting with `$`"
)]
pub struct UnfilledPlaceholder {
    pub argument: String,
}

impl SelectorPlaceholder {
    /// The text after the `@` mark, `provider:segment/segment/…`, when some segment starts with
    /// `$`. A bare `$`, an empty provider, selector, or segment reads as nothing; a spelling
    /// with no `$` segment is a static mention, not a placeholder.
    pub fn parse(after_at: &str) -> Option<SelectorPlaceholder> {
        let (provider, selector) = after_at.split_once(':')?;
        if provider.is_empty() || selector.is_empty() {
            return None;
        }
        let mut segments = Vec::new();
        for segment in selector.split('/') {
            let parsed = match segment.strip_prefix('$') {
                Some("") => return None,
                Some(argument) => Segment::Argument(argument.to_string()),
                None if segment.is_empty() => return None,
                None => Segment::Literal(segment.to_string()),
            };
            segments.push(parsed);
        }
        if !segments.iter().any(|segment| matches!(segment, Segment::Argument(_))) {
            return None;
        }
        Some(SelectorPlaceholder {
            provider: provider.to_string(),
            segments,
        })
    }

    pub(crate) fn provider(&self) -> &str {
        &self.provider
    }

    /// The call arguments this placeholder reads, in written order.
    pub(crate) fn arguments(&self) -> impl Iterator<Item = &str> {
        self.segments.iter().filter_map(|segment| match segment {
            Segment::Argument(argument) => Some(argument.as_str()),
            Segment::Literal(_) => None,
        })
    }

    /// Whether every instantiation of this placeholder matches the template: the same number
    /// of segments, a literal equal to the template's literal or filling a `<variable>`, and
    /// an argument only on a `<variable>`, since its value is unknown until the call.
    pub(crate) fn fits(&self, template: &crate::audience::SelectorTemplate) -> bool {
        let pattern: Vec<&str> = template.as_str().split('/').collect();
        pattern.len() == self.segments.len()
            && pattern.iter().zip(&self.segments).all(|(pattern, segment)| {
                let variable = pattern.starts_with('<') && pattern.ends_with('>');
                match segment {
                    Segment::Literal(literal) => variable || pattern == literal,
                    Segment::Argument(_) => variable,
                }
            })
    }

    /// The collection one call's arguments name: each argument segment replaced by the
    /// argument's value. Every argument is a required string of the tool's schema (the
    /// registry makes it one) and every value one writable segment (checked when the call is
    /// minted), so a minted call always instantiates into a mention the log can carry;
    /// anything else is refused rather than read as another collection.
    pub fn instantiate(&self, arguments: &serde_json::Value) -> Result<GroupRef, UnfilledPlaceholder> {
        let mut selector = Vec::with_capacity(self.segments.len());
        for segment in &self.segments {
            match segment {
                Segment::Literal(literal) => selector.push(literal.as_str()),
                Segment::Argument(argument) => match arguments.get(argument).and_then(serde_json::Value::as_str) {
                    Some(value) if !value.is_empty() && !value.contains('/') && !value.starts_with('$') => {
                        selector.push(value)
                    }
                    _ => {
                        return Err(UnfilledPlaceholder {
                            argument: argument.clone(),
                        });
                    }
                },
            }
        }
        Ok(GroupRef::Source {
            provider: self.provider.clone(),
            selector: selector.join("/"),
        })
    }
}

impl std::fmt::Display for SelectorPlaceholder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "@{}:", self.provider)?;
        for (index, segment) in self.segments.iter().enumerate() {
            if index > 0 {
                f.write_str("/")?;
            }
            match segment {
                Segment::Literal(literal) => f.write_str(literal)?,
                Segment::Argument(argument) => write!(f, "${argument}")?,
            }
        }
        Ok(())
    }
}

impl Serialize for SelectorPlaceholder {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for SelectorPlaceholder {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let spelled = String::deserialize(deserializer)?;
        spelled
            .strip_prefix('@')
            .and_then(SelectorPlaceholder::parse)
            .ok_or_else(|| serde::de::Error::custom(format!("{spelled:?} is not a selector placeholder")))
    }
}

/// What a delta writes into the audience: an audience as written, or a selector placeholder
/// the call's arguments fill.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DeltaAudience {
    Static(DeclaredAudience),
    Selector(SelectorPlaceholder),
}

impl DeltaAudience {
    /// The audience as written, when the delta does not read the call.
    pub fn declared(&self) -> Option<&DeclaredAudience> {
        match self {
            DeltaAudience::Static(audience) => Some(audience),
            DeltaAudience::Selector(_) => None,
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

    /// This annotation read for one call: a delta placeholder becomes the collection the
    /// call's arguments spell, so every label read from it downstream is static. `None` when
    /// the delta reads no placeholder — the annotation is already bound. A `contains`
    /// placeholder stays: it is a requirement, resolved against the call where the check reads
    /// recipients, as an argument placeholder is.
    pub(crate) fn bound_to(
        &self,
        arguments: &serde_json::Value,
    ) -> Result<Option<ToolAnnotation>, UnfilledPlaceholder> {
        let Some(DeltaAudience::Selector(placeholder)) = &self.delta.audience else {
            return Ok(None);
        };
        let group = placeholder.instantiate(arguments)?;
        let mut bound = self.clone();
        bound.delta.audience = Some(DeltaAudience::Static(DeclaredAudience::Union(
            Clause::new([], [group], []).expect("a group clause names no reader"),
        )));
        Ok(Some(bound))
    }

    /// Every audience-source provider this annotation names: by a `@provider:selector` mention
    /// in its delta or requirements, or by a selector placeholder.
    pub fn referenced_providers(&self) -> std::collections::BTreeSet<String> {
        let mentioned = self
            .delta
            .symbolic_atoms()
            .chain(self.requires.audience_requirements().iter().flat_map(|requirement| {
                let declared = match requirement {
                    AudienceRequirement::Includes(RecipientSpec::Static(declared))
                    | AudienceRequirement::Cap(declared) => Some(declared),
                    AudienceRequirement::Includes(RecipientSpec::Placeholder(_) | RecipientSpec::Selector(_)) => None,
                };
                declared.into_iter().flat_map(DeclaredAudience::symbolic_atoms)
            }))
            .filter_map(|atom| match atom {
                SymbolicAtom::Group(GroupRef::Source { provider, .. }) => Some(provider),
                SymbolicAtom::Group(GroupRef::Named(_)) | SymbolicAtom::Chain(_) | SymbolicAtom::Reader(_) => None,
            });
        mentioned
            .chain(
                self.selector_placeholders()
                    .map(|placeholder| placeholder.provider().to_string()),
            )
            .collect()
    }

    /// Every selector placeholder this annotation reads: its delta's and its `contains`'.
    pub fn selector_placeholders(&self) -> impl Iterator<Item = &SelectorPlaceholder> {
        self.delta
            .selector_placeholder()
            .into_iter()
            .chain(
                self.requires
                    .audience_requirements()
                    .iter()
                    .filter_map(|requirement| match requirement {
                        AudienceRequirement::Includes(RecipientSpec::Selector(placeholder)) => Some(placeholder),
                        AudienceRequirement::Includes(RecipientSpec::Static(_) | RecipientSpec::Placeholder(_))
                        | AudienceRequirement::Cap(_) => None,
                    }),
            )
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
                    .filter_map(DeltaAudience::declared)
                    .flat_map(move |audience| audience.needed_atoms(providers))
            })
            .into_iter()
            .flatten();
        delta.chain(
            requirements
                .iter()
                .filter_map(AudienceRequirement::declared)
                .flat_map(move |declared| declared.needed_atoms(providers)),
        )
    }
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

    /// A placeholder is spelled as the policy writes it, so the identity document and the log
    /// carry it as that one string, distinct from every static audience.
    #[test]
    fn a_delta_placeholder_serializes_as_its_spelling() {
        let placeholder = SelectorPlaceholder::parse("slack:channel/$channel_id").expect("a placeholder spelling");
        let delta = Delta {
            trust: None,
            audience: Some(DeltaAudience::Selector(placeholder.clone())),
        };
        let rendered = serde_json::to_value(&delta).expect("a delta serializes");
        assert_eq!(rendered["audience"], serde_json::json!("@slack:channel/$channel_id"));
        assert_eq!(
            serde_json::from_value::<Delta>(rendered).expect("the spelling reads back"),
            delta
        );
        let public = Delta {
            trust: None,
            audience: Some(DeltaAudience::Static(DeclaredAudience::Public)),
        };
        let rendered = serde_json::to_value(&public).expect("a delta serializes");
        assert_eq!(
            serde_json::from_value::<Delta>(rendered).expect("a static audience reads back"),
            public
        );
        assert!(serde_json::from_value::<Delta>(serde_json::json!({ "audience": "@slack:user-group/eng" })).is_err());
    }

    /// Every instantiation is a mention the policy could have written and the log can carry:
    /// one non-empty segment per argument, never a `/` or a placeholder mark.
    #[test]
    fn a_placeholder_instantiates_only_into_a_writable_mention() {
        let placeholder = SelectorPlaceholder::parse("slack:channel/$channel").expect("a placeholder spelling");
        assert_eq!(
            placeholder.instantiate(&serde_json::json!({ "channel": "C1" })),
            Ok(GroupRef::Source {
                provider: "slack".to_string(),
                selector: "channel/C1".to_string(),
            })
        );
        for value in [
            serde_json::json!(""),
            serde_json::json!("a/b"),
            serde_json::json!("$x"),
            serde_json::json!(7),
        ] {
            assert!(
                placeholder
                    .instantiate(&serde_json::json!({ "channel": value }))
                    .is_err(),
                "{value} fills no segment"
            );
        }
        assert!(placeholder.instantiate(&serde_json::json!({})).is_err());
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
                    audience: Some(DeltaAudience::Static(DeclaredAudience::Public)),
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
