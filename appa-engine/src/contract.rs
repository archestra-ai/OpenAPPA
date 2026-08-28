//! Tool contracts: what a call commits (`delta`, `emits`) and what it requires (`requires`).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::fact::{EffectKind, EffectSet};
use crate::groups::{DeclaredAudience, Expansions};
use crate::label::{Audience, Dim, Dimension, EstablishedLabel, Label, ReaderId, Trust};
use crate::names::{DynamicResolverName, MarkName, TagName};
use crate::value::ToolName;

/// A **declared** restrictive label contribution: what a successful call folds into the trajectory.
/// Every delta only ever narrows — minimum trust, intersect audience — so a permissive delta is
/// unrepresentable. A dimension may also be declared [`Dim::Unknown`]: **pending-cast** — the
/// result's actual state is established by a registered cast at admission, so the raw result
/// is confined until then.
///
/// An omitted dimension is neutral, not unknown: declaring the tool is what says the deployment
/// knows it, and a dimension it does not describe restricts nothing ([`Delta::NONE`], `delta = {}`
/// on the config surface, is the same statement written out). Unknown enters a declared contract
/// only where the policy asks for it — `"unknown"` for a pending cast, or a resolver owning the
/// dimension until its answer pins. A tool the policy never declares is checked as the reserved
/// undeclared contract: its output and every requirement slot Unknown, so only a cast covering
/// undeclared tools can decide a call to it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delta {
    pub trust: Option<Dim<Trust>>,
    pub audience: Option<AudienceDelta>,
}

/// One result a dynamic resolver returns, named by the single contract field it establishes.
/// A resolver declares its results and returns every one of them; a tool reads the ones its
/// fields reference. The output and the requirement on one dimension are separate results, so a
/// resolver can classify a call's output and demand a floor for it in the same answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolverReturn {
    /// Output-label trust (`delta.trust`).
    Trust,
    /// Output-label audience (`delta.audience`).
    Audience,
    /// A call-time trust floor (`requires.trust`).
    RequiredTrust,
    /// Call-time audience constraints (`requires.audience`).
    RequiredAudience,
    /// Fresh call-time review marks (`requires.attention`).
    Attention,
}

impl ResolverReturn {
    pub const ALL: [ResolverReturn; 5] = [
        ResolverReturn::Trust,
        ResolverReturn::Audience,
        ResolverReturn::RequiredTrust,
        ResolverReturn::RequiredAudience,
        ResolverReturn::Attention,
    ];

    /// The name a resolver declares and answers under — the `returns` list and the response's
    /// `result` object agree through it. A result has exactly one destination, so the name is
    /// that destination's path.
    pub fn wire_name(self) -> &'static str {
        match self {
            ResolverReturn::Trust => "delta.trust",
            ResolverReturn::Audience => "delta.audience",
            ResolverReturn::RequiredTrust => "requires.trust",
            ResolverReturn::RequiredAudience => "requires.audience",
            ResolverReturn::Attention => "requires.attention",
        }
    }
}

/// The root of the one special source a `uses` entry may read.
pub const TOOL_CALL_ROOT: &str = "$tool_call";

/// Where a `uses` entry reads one input value. `$tool_call` is the only source, and these are
/// its five forms.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum ToolCallSource {
    /// `$tool_call` — the complete call: its name, its description when the tool declares one,
    /// and its arguments.
    Call,
    /// `$tool_call.name`
    Name,
    /// `$tool_call.description`
    Description,
    /// `$tool_call.arguments` — the complete argument object.
    Arguments,
    /// `$tool_call.arguments.<name>` — one top-level argument.
    Argument(ArgumentName),
}

/// A top-level argument name a source can spell: non-empty and dot-free, so a source always
/// serializes into a spelling that parses back. The inner string is private, so an argument
/// source that would strand a persisted trajectory cannot be built at all.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ArgumentName(String);

impl ArgumentName {
    pub fn new(name: &str) -> Option<ArgumentName> {
        match name.is_empty() || name.contains('.') {
            true => None,
            false => Some(ArgumentName(name.to_string())),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A source spelling outside the five supported forms.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "{spelling:?} is not a tool-call value: a `uses` input reads `$tool_call`, `$tool_call.name`, \
     `$tool_call.description`, `$tool_call.arguments`, or `$tool_call.arguments.<name>`"
)]
pub struct UnknownCallSource {
    pub spelling: String,
}

impl ToolCallSource {
    pub fn parse(spelling: &str) -> Option<ToolCallSource> {
        match spelling {
            "$tool_call" => Some(ToolCallSource::Call),
            "$tool_call.name" => Some(ToolCallSource::Name),
            "$tool_call.description" => Some(ToolCallSource::Description),
            "$tool_call.arguments" => Some(ToolCallSource::Arguments),
            // One top-level argument only: an empty name and a nested path are both outside the
            // five forms, and neither has a value the schema can pin.
            _ => spelling
                .strip_prefix("$tool_call.arguments.")
                .and_then(ArgumentName::new)
                .map(ToolCallSource::Argument),
        }
    }

    /// The source reading one top-level argument, or `None` when the name is not one a
    /// spelling can carry.
    pub fn argument(name: &str) -> Option<ToolCallSource> {
        ArgumentName::new(name).map(ToolCallSource::Argument)
    }

    pub fn spelling(&self) -> String {
        match self {
            ToolCallSource::Call => TOOL_CALL_ROOT.to_string(),
            ToolCallSource::Name => format!("{TOOL_CALL_ROOT}.name"),
            ToolCallSource::Description => format!("{TOOL_CALL_ROOT}.description"),
            ToolCallSource::Arguments => format!("{TOOL_CALL_ROOT}.arguments"),
            ToolCallSource::Argument(argument) => format!("{TOOL_CALL_ROOT}.arguments.{}", argument.as_str()),
        }
    }

    /// Whether this source needs a declared tool description. `$tool_call` carries the description
    /// when there is one and omits it otherwise; only `$tool_call.description` insists on it.
    pub fn requires_declared_description(&self) -> bool {
        matches!(self, ToolCallSource::Description)
    }
}

impl TryFrom<String> for ToolCallSource {
    type Error = UnknownCallSource;

    fn try_from(spelling: String) -> Result<Self, Self::Error> {
        ToolCallSource::parse(&spelling).ok_or(UnknownCallSource { spelling })
    }
}

impl From<ToolCallSource> for String {
    fn from(source: ToolCallSource) -> String {
        source.spelling()
    }
}

/// A digest of the canonical `args` one consult was given. The pin carries this rather than the
/// value itself: the arguments and the `uses` entry are both already on the record, so the value
/// is re-derivable, and a tool with several resolvers would otherwise persist a copy per resolver.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ResolverArgsDigest(#[serde(with = "crate::hex32")] [u8; 32]);

impl ResolverArgsDigest {
    pub fn of(canonical: &[u8]) -> Self {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(b"appa.resolver-args.v1");
        hasher.update(canonical);
        ResolverArgsDigest(hasher.finalize().into())
    }

    pub fn bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// One resolver a tool uses: which registered resolver, the call value it reads for each input
/// that resolver declares, and the contract destinations it owns. An empty `inputs` map sends the
/// complete call.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ToolResolverUse {
    pub resolver: DynamicResolverName,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, ToolCallSource>,
    /// Every destination the resolver owns. An answer must carry exactly these.
    pub returns: BTreeSet<ResolverReturn>,
}

impl ToolResolverUse {
    /// Whether this use needs a declared tool description.
    pub fn requires_declared_description(&self) -> bool {
        self.inputs.values().any(ToolCallSource::requires_declared_description)
    }
}

/// The audience half of a `requires` answer: an `includes` floor, a `cap` ceiling, or both.
/// Dynamic answers may not contain groups: the exact literal audiences are pinned with the
/// call and replayed verbatim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequiredAudience {
    pub includes: Option<Audience>,
    pub cap: Option<Audience>,
}

impl RequiredAudience {
    fn is_literal(&self) -> bool {
        self.includes.iter().chain(self.cap.iter()).all(
            |audience| !matches!(audience, Audience::Restricted(readers) if !readers.iter().all(ReaderId::is_literal)),
        )
    }
}

/// One successful tool-level resolver answer pinned to the call it classified. The constructor
/// accepts exactly the results the resolver declares and canonicalizes additive attention marks.
///
/// The pin carries the canonical `args` it was answered for. That is what binds it to one call:
/// an answer given for other arguments is not evidence here, and
/// [`crate::check::validate_tool_resolutions`] rebuilds the value and compares it, at the check
/// and again on replay.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PinnedToolResolution {
    uses: ToolResolverUse,
    args: ResolverArgsDigest,
    trust: Option<Trust>,
    audience: Option<Audience>,
    required_trust: Option<Trust>,
    required_audience: Option<RequiredAudience>,
    attention: Vec<MarkName>,
}

impl PinnedToolResolution {
    pub fn from_answer(
        uses: ToolResolverUse,
        args: ResolverArgsDigest,
        trust: Option<Trust>,
        audience: Option<Audience>,
        required_trust: Option<Trust>,
        required_audience: Option<RequiredAudience>,
        attention: Option<Vec<MarkName>>,
    ) -> Option<Self> {
        if uses.returns.is_empty()
            || uses.returns.contains(&ResolverReturn::Trust) != trust.is_some()
            || uses.returns.contains(&ResolverReturn::Audience) != audience.is_some()
            || uses.returns.contains(&ResolverReturn::RequiredTrust) != required_trust.is_some()
            || uses.returns.contains(&ResolverReturn::RequiredAudience) != required_audience.is_some()
            || uses.returns.contains(&ResolverReturn::Attention) != attention.is_some()
        {
            return None;
        }
        if matches!(&audience, Some(Audience::Restricted(readers)) if !readers.iter().all(ReaderId::is_literal)) {
            return None;
        }
        if let Some(required) = &required_audience
            && (!required.is_literal() || (required.includes.is_none() && required.cap.is_none()))
        {
            return None;
        }
        let mut attention = attention.unwrap_or_default();
        attention.sort();
        attention.dedup();
        Some(PinnedToolResolution {
            uses,
            args,
            trust,
            audience,
            required_trust,
            required_audience,
            attention,
        })
    }

    pub fn uses(&self) -> &ToolResolverUse {
        &self.uses
    }

    /// The digest of the canonical `args` this answer was given for.
    pub fn args(&self) -> ResolverArgsDigest {
        self.args
    }

    /// Whether this answer owns a contract destination.
    fn owns(&self, result: ResolverReturn) -> bool {
        self.uses.returns.contains(&result)
    }

    /// Every trust rank the answer carries.
    pub(crate) fn every_trust(&self) -> impl Iterator<Item = Trust> + '_ {
        self.trust.into_iter().chain(self.required_trust)
    }

    /// Every attention mark the answer carries.
    pub(crate) fn every_mark(&self) -> &[MarkName] {
        &self.attention
    }

    pub fn trust(&self) -> Option<Trust> {
        self.owns(ResolverReturn::Trust).then_some(self.trust).flatten()
    }

    pub fn audience(&self) -> Option<&Audience> {
        self.owns(ResolverReturn::Audience)
            .then_some(self.audience.as_ref())
            .flatten()
    }

    pub fn required_trust(&self) -> Option<Trust> {
        self.owns(ResolverReturn::RequiredTrust)
            .then_some(self.required_trust)
            .flatten()
    }

    pub fn required_audience(&self) -> Option<&RequiredAudience> {
        self.owns(ResolverReturn::RequiredAudience)
            .then_some(self.required_audience.as_ref())
            .flatten()
    }

    pub fn attention(&self) -> &[MarkName] {
        match self.owns(ResolverReturn::Attention) {
            true => &self.attention,
            false => &[],
        }
    }
}

impl<'de> Deserialize<'de> for PinnedToolResolution {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            uses: ToolResolverUse,
            args: ResolverArgsDigest,
            trust: Option<Trust>,
            audience: Option<Audience>,
            required_trust: Option<Trust>,
            required_audience: Option<RequiredAudience>,
            attention: Vec<MarkName>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let original_attention = wire.attention.clone();
        let returned_attention = wire
            .uses
            .returns
            .contains(&ResolverReturn::Attention)
            .then_some(wire.attention);
        let answer = PinnedToolResolution::from_answer(
            wire.uses,
            wire.args,
            wire.trust,
            wire.audience,
            wire.required_trust,
            wire.required_audience,
            returned_attention,
        )
        .ok_or_else(|| serde::de::Error::custom("a pinned tool resolution must match its declared returns"))?;
        if answer.attention != original_attention {
            return Err(serde::de::Error::custom(
                "pinned tool-resolution attention is not in canonical order",
            ));
        }
        Ok(answer)
    }
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

    pub(crate) fn answer(&self) -> RequirementAnswer {
        RequirementAnswer {
            trust: self.required_trust,
            audience: self
                .required_audience
                .as_ref()
                .and_then(|required| required.includes.clone()),
            attention: self.attention.clone(),
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
    readers: BTreeSet<ReaderId>,
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
        let readers: BTreeSet<ReaderId> = readers.into_iter().collect();
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

    pub fn readers(&self) -> &BTreeSet<ReaderId> {
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
            readers: BTreeSet<ReaderId>,
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
    /// **established** dimensions only, as a meet operand. A pending-cast or dynamic dimension
    /// contributes identity here — its actual contribution folds at admission, at the resolved
    /// label, where every later call re-checks against it. (Sound because load validation
    /// refuses a contract that pairs a pending-cast dimension with a `requires` on that same
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

/// A contract the check can evaluate with no call at hand: nothing it reads comes from a call —
/// no resolver answers (`uses`), no placeholder recipients, every group it names already
/// expanded — and its output contribution is declared and established (not unannotated, no
/// pending-cast dimension), so the label a successful call commits is known before the call
/// exists. This is the only shape a recovery route plans a preceding tool over (RMD-20): its
/// check and its successor state are argument-independent facts of the registry.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StaticContract<'a>(&'a ToolContract);

/// Why a contract is not [`StaticContract`]: what a call to it would first have to supply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NotStatic {
    /// A resolver answer or a placeholder recipient reads the call's arguments.
    Arguments,
    /// The result label is established only at admission — unannotated or pending-cast.
    Unestablished,
    /// Groups the contract names that these expansions do not answer.
    Membership(Vec<crate::names::GroupName>),
}

impl<'a> StaticContract<'a> {
    pub(crate) fn of(contract: &'a ToolContract, expansions: &Expansions) -> Result<StaticContract<'a>, NotStatic> {
        let placeholder = contract.requires.audience_requirements().iter().any(|requirement| {
            matches!(
                requirement,
                AudienceRequirement::Includes(RecipientSpec::Placeholder(_))
            )
        });
        if !contract.uses.is_empty() || placeholder {
            return Err(NotStatic::Arguments);
        }
        if contract.delta.pending_cast_dim().is_some() {
            return Err(NotStatic::Unestablished);
        }
        expansions
            .require(contract.groups())
            .map_err(|needed| NotStatic::Membership(needed.needed))?;
        Ok(StaticContract(contract))
    }

    pub(crate) fn contract(&self) -> &'a ToolContract {
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
        self.resolver_return().wire_name()
    }

    /// The resolver destination that answers this slot: what a requirement cast is asked for,
    /// and what a written slot forbids a dynamic resolver from also owning.
    pub fn resolver_return(self) -> ResolverReturn {
        match self {
            RequirementSlot::Trust => ResolverReturn::RequiredTrust,
            RequirementSlot::Audience => ResolverReturn::RequiredAudience,
            RequirementSlot::Attention => ResolverReturn::Attention,
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

    /// Whether the policy wrote anything into the slot — a value or `"unknown"`. A written slot
    /// has an owner, so a resolver may not own the same destination.
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

/// A tool contract: name, routing tags, the compiled input schema, and the three
/// algebraic slots.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolContract {
    pub name: ToolName,
    pub tags: Vec<TagName>,
    /// What this tool does, in the policy's words. Part of policy identity, because a resolver
    /// may read it: load validation requires it wherever a `uses` entry does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The compiled, normalized `APPA Tool Parameters v1` schema — part of policy identity.
    /// Omitted `parameters` normalizes to the permissive open object.
    #[serde(default = "crate::params::ToolParameters::open")]
    pub parameters: crate::params::ToolParameters,
    /// The resolvers this tool uses, each classifying a proposed call before the call is checked.
    #[serde(default)]
    pub uses: Vec<ToolResolverUse>,
    /// The static output contribution. An omitted `delta` is [`Delta::NONE`]: the tool is declared,
    /// so its unwritten dimensions restrict nothing. A resolver owning a dimension still holds it
    /// Unknown until its answer pins.
    #[serde(default)]
    pub delta: Delta,
    pub emits: EffectSet,
    pub requires: Requires,
}

impl ToolContract {
    /// Whether one of this tool's resolvers owns this destination.
    pub(crate) fn resolver_owns(&self, field: ResolverReturn) -> bool {
        self.uses.iter().any(|uses| uses.returns.contains(&field))
    }

    /// The description a `$tool_call.description` input reads, which load validation guarantees
    /// is present wherever one is read.
    fn described(&self) -> &str {
        self.description
            .as_deref()
            .expect("load validation refuses a `uses` entry that reads a description the tool does not declare")
    }

    /// The complete call as a consult artifact — what a resolver without an input mapping and a
    /// requirement cast both read: the proposed tool name, this contract's description when the
    /// policy declares one, and the canonical arguments. A tool without a description sends no
    /// `description` key. The name is the one the actor proposed, not the contract's own: a
    /// contract selected by pattern answers for many names, and a classifier that saw the pattern
    /// would be judging a call it cannot identify.
    pub fn complete_call(&self, called: &ToolName, arguments: &serde_json::Value) -> serde_json::Value {
        let mut call = serde_json::Map::new();
        call.insert("name".into(), serde_json::Value::String(called.as_str().to_string()));
        if let Some(description) = &self.description {
            call.insert("description".into(), serde_json::Value::String(description.clone()));
        }
        call.insert("arguments".into(), arguments.clone());
        serde_json::Value::Object(call)
    }

    fn source_value(
        &self,
        called: &ToolName,
        source: &ToolCallSource,
        arguments: &serde_json::Value,
    ) -> serde_json::Value {
        match source {
            ToolCallSource::Call => self.complete_call(called, arguments),
            ToolCallSource::Name => serde_json::Value::String(called.as_str().to_string()),
            ToolCallSource::Description => serde_json::Value::String(self.described().to_string()),
            ToolCallSource::Arguments => arguments.clone(),
            ToolCallSource::Argument(argument) => arguments
                .get(argument.as_str())
                .cloned()
                .expect("load validation pins a mapped argument to a required top-level property, and every proposal is schema-validated before resolution"),
        }
    }

    /// The `args` value one use sends: the complete call when the resolver declares no inputs,
    /// otherwise one entry per declared input. The single definition — the runtime builds the
    /// request through it, and the check rebuilds the pinned value through it — so the tool name a
    /// no-input resolver sees is part of what its answer is pinned to.
    pub fn resolver_args(
        &self,
        uses: &ToolResolverUse,
        called: &ToolName,
        arguments: &serde_json::Value,
    ) -> serde_json::Value {
        match uses.inputs.is_empty() {
            true => self.complete_call(called, arguments),
            false => serde_json::Value::Object(
                uses.inputs
                    .iter()
                    .map(|(input, source)| (input.clone(), self.source_value(called, source, arguments)))
                    .collect(),
            ),
        }
    }

    /// [`Self::resolver_args`] in its canonical spelling — the exact bytes a consult carries.
    pub fn canonical_resolver_args(
        &self,
        uses: &ToolResolverUse,
        called: &ToolName,
        arguments: &serde_json::Value,
    ) -> Vec<u8> {
        crate::params::canonical_bytes(&self.resolver_args(uses, called, arguments))
    }

    /// What a pin stores, and what a re-derivation is compared against.
    pub fn resolver_args_digest(
        &self,
        uses: &ToolResolverUse,
        called: &ToolName,
        arguments: &serde_json::Value,
    ) -> ResolverArgsDigest {
        ResolverArgsDigest::of(&self.canonical_resolver_args(uses, called, arguments))
    }

    /// The unresolved output shape before call-pinned answers are applied. Each dimension is
    /// derived independently: resolver-owned fields are `Unknown` until their pin applies, and
    /// every other dimension is exactly what the static contract describes. Resolver ownership of
    /// one dimension never establishes the other.
    pub fn output_label(&self, expansions: &Expansions) -> Label {
        let mut label = self.delta.output_label(expansions);
        if self.resolver_owns(ResolverReturn::Trust) {
            label.trust = Dim::Unknown;
        }
        if self.resolver_owns(ResolverReturn::Audience) {
            label.audience = Dim::Unknown;
        }
        label
    }

    /// The groups this contract's check reads: its delta's, its static recipients' and
    /// its cap's. Required before the check runs; a placeholder's group rides the call instead.
    pub fn groups(&self) -> impl Iterator<Item = &crate::names::GroupName> {
        self.delta.groups().chain(
            self.requires
                .audience_requirements()
                .iter()
                .flat_map(AudienceRequirement::groups),
        )
    }

    /// The output label with a set of pinned resolver answers applied. Admission — and the runtime
    /// composing a whole-source pending-cast answer against it — passes the answers persisted on
    /// the dispatch, never the caller's in-memory resolution. Plan construction passes the proposed
    /// call's own: only a successful answer enters the engine, and every binding a contract spells
    /// carries one by the time a call is checked — [`crate::check::validate_tool_resolutions`]
    /// refuses the proposal otherwise, before any fact, and replay holds a persisted call to the
    /// same rule.
    pub fn output_label_for_resolutions(
        &self,
        tool_resolutions: &[PinnedToolResolution],
        expansions: &Expansions,
    ) -> Label {
        let mut label = self.output_label(expansions);
        apply_tool_resolutions(&mut label, tool_resolutions);
        label
    }

    /// The single dimension this contract declares pending-cast, if any. An unannotated tool
    /// declares none: its Unknown output is admitted as-is, not confined awaiting a cast.
    pub fn pending_cast_dim(&self) -> Option<Dimension> {
        self.delta.pending_cast_dim()
    }
}

/// Pin a call's tool-level resolver answers into a label: each answered field becomes `Known`.
fn apply_tool_resolutions(label: &mut Label, resolutions: &[PinnedToolResolution]) {
    for resolution in resolutions {
        if let Some(trust) = resolution.trust() {
            label.trust = Dim::Known(trust);
        }
        if let Some(audience) = resolution.audience() {
            label.audience = Dim::Known(audience.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::names::DynamicResolverName;

    fn binding() -> ToolResolverUse {
        ToolResolverUse {
            resolver: DynamicResolverName::new("crm-acl"),
            inputs: BTreeMap::from([(
                "customer_id".to_string(),
                ToolCallSource::argument("customer_id").expect("a plain name is a source"),
            )]),
            returns: BTreeSet::from([ResolverReturn::Audience]),
        }
    }

    #[test]
    fn a_resolver_owns_its_dimension_and_leaves_the_rest_of_the_delta_alone() {
        use crate::groups::Expansions;

        let owner = |field: ResolverReturn| ToolContract {
            name: ToolName::new("lookup"),
            tags: vec![],
            parameters: crate::params::ToolParameters::open(),
            description: Some("A test tool.".to_string()),
            uses: vec![ToolResolverUse {
                resolver: DynamicResolverName::new("classifier"),
                inputs: std::collections::BTreeMap::new(),
                returns: BTreeSet::from([field]),
            }],
            delta: crate::contract::Delta::NONE,
            emits: crate::fact::EffectSet::default(),
            requires: Requires::default(),
        };

        // A resolver holds the dimension it owns Unknown until its answer pins it. The
        // dimension the contract describes neither statically nor through a resolver is the
        // neutral one the declaration asked for.
        let label = owner(ResolverReturn::Audience).output_label(&Expansions::default());
        assert_eq!(
            label.trust,
            Dim::Known(Trust::new(u8::MAX)),
            "trust is described by neither"
        );
        assert_eq!(label.audience, Dim::Unknown, "the pin has not applied yet");

        let label = owner(ResolverReturn::Trust).output_label(&Expansions::default());
        assert_eq!(label.trust, Dim::Unknown, "the pin has not applied yet");
        assert_eq!(
            label.audience,
            Dim::Known(crate::label::Audience::Public),
            "audience is described by neither"
        );

        // With the pin applied, exactly the owned dimension resolves.
        let binding = ToolResolverUse {
            resolver: DynamicResolverName::new("classifier"),
            inputs: std::collections::BTreeMap::new(),
            returns: BTreeSet::from([ResolverReturn::Audience]),
        };
        let pin = PinnedToolResolution::from_answer(
            binding,
            crate::contract::ResolverArgsDigest::of(b""),
            None,
            Some(Audience::restricted([ReaderId::new("alice")])),
            None,
            None,
            None,
        )
        .expect("a literal reader set pins");
        let label = owner(ResolverReturn::Audience)
            .output_label_for_resolutions(std::slice::from_ref(&pin), &Expansions::default());
        assert_eq!(
            label.audience,
            Dim::Known(Audience::restricted([ReaderId::new("alice")]))
        );
        assert_eq!(
            label.trust,
            Dim::Known(Trust::new(u8::MAX)),
            "pinning the owned dimension leaves the undescribed one alone"
        );
    }

    #[test]
    fn a_resolver_audience_answer_keeps_only_literal_reader_sets() {
        let pinned = |audience| {
            PinnedToolResolution::from_answer(
                binding(),
                crate::contract::ResolverArgsDigest::of(b""),
                None,
                Some(audience),
                None,
                None,
                None,
            )
            .map(|pin| pin.audience().cloned().expect("audience is the declared return"))
        };

        assert_eq!(pinned(Audience::restricted([ReaderId::new("@hr")])), None);
        assert_eq!(
            pinned(Audience::restricted([ReaderId::new("finance"), ReaderId::new("@hr")])),
            None,
            "one group member spoils the whole answer"
        );

        let empty = Audience::restricted([]);
        assert_eq!(pinned(empty.clone()), Some(empty), "no readers is a valid answer");
        let email = Audience::restricted([ReaderId::new("ap@corp.example")]);
        assert_eq!(pinned(email.clone()), Some(email), "`@` mid-ID is an ordinary reader");
        assert_eq!(
            pinned(Audience::Public),
            Some(Audience::Public),
            "an owned output dimension may resolve to the public state"
        );
    }

    fn described(uses: Vec<ToolResolverUse>) -> ToolContract {
        ToolContract {
            name: ToolName::new("Bash"),
            tags: vec![],
            description: Some("Runs one shell command and returns its output.".to_string()),
            parameters: crate::params::ToolParameters::open(),
            uses,
            delta: Delta::NONE,
            emits: EffectSet::default(),
            requires: Requires::default(),
        }
    }

    #[test]
    fn every_source_a_policy_can_write_survives_its_own_spelling() {
        // A source rides policy identity and every persisted pin as its spelling, so the two
        // directions have to agree or a stored trajectory stops replaying.
        let sources = [
            ToolCallSource::Call,
            ToolCallSource::Name,
            ToolCallSource::Description,
            ToolCallSource::Arguments,
            ToolCallSource::argument("customer_id").expect("a plain name is a source"),
        ];
        for source in sources {
            assert_eq!(ToolCallSource::parse(&source.spelling()), Some(source.clone()));
        }
        // The names a spelling cannot carry are refused at construction, not silently kept.
        for name in ["", "a.b"] {
            assert_eq!(ToolCallSource::argument(name), None, "{name:?} is not an argument name");
        }
    }

    #[test]
    fn each_call_value_selects_exactly_what_the_wire_carries() {
        let arguments = serde_json::json!({ "command": "git push origin main", "timeout": 60_000 });
        let complete = serde_json::json!({
            "name": "Bash",
            "description": "Runs one shell command and returns its output.",
            "arguments": { "command": "git push origin main", "timeout": 60_000 },
        });

        let mapped = ToolResolverUse {
            resolver: DynamicResolverName::new("classify"),
            inputs: BTreeMap::from([
                ("whole".to_string(), ToolCallSource::Call),
                ("tool".to_string(), ToolCallSource::Name),
                ("purpose".to_string(), ToolCallSource::Description),
                ("every".to_string(), ToolCallSource::Arguments),
                (
                    "one".to_string(),
                    ToolCallSource::argument("timeout").expect("a plain name is a source"),
                ),
            ]),
            returns: BTreeSet::from([ResolverReturn::Trust]),
        };
        let contract = described(vec![mapped.clone()]);
        assert_eq!(
            contract.resolver_args(&mapped, &ToolName::new("Bash"), &arguments),
            serde_json::json!({
                "whole": complete,
                "tool": "Bash",
                "purpose": "Runs one shell command and returns its output.",
                "every": { "command": "git push origin main", "timeout": 60_000 },
                // A mapped argument reaches the resolver as the JSON value the call carries,
                // not as text.
                "one": 60_000,
            })
        );

        // A resolver declaring no inputs receives the complete call.
        let whole = ToolResolverUse {
            inputs: BTreeMap::new(),
            ..mapped
        };
        assert!(!whole.requires_declared_description());
        assert_eq!(
            described(vec![whole.clone()]).resolver_args(&whole, &ToolName::new("Bash"), &arguments),
            complete
        );

        // A tool without a description sends `name` and `arguments` only, whether the resolver
        // declares no inputs or maps `$tool_call`; only `$tool_call.description` insists on one.
        let undescribed = ToolContract {
            description: None,
            ..described(vec![])
        };
        let bare = serde_json::json!({
            "name": "Bash",
            "arguments": { "command": "git push origin main", "timeout": 60_000 },
        });
        assert_eq!(
            undescribed.resolver_args(&whole, &ToolName::new("Bash"), &arguments),
            bare
        );
        let call_source = ToolResolverUse {
            inputs: BTreeMap::from([("whole".to_string(), ToolCallSource::Call)]),
            ..whole.clone()
        };
        assert!(!call_source.requires_declared_description());
        assert_eq!(
            undescribed.resolver_args(&call_source, &ToolName::new("Bash"), &arguments),
            serde_json::json!({ "whole": bare })
        );
        let description_source = ToolResolverUse {
            inputs: BTreeMap::from([("purpose".to_string(), ToolCallSource::Description)]),
            ..whole
        };
        assert!(description_source.requires_declared_description());
    }

    #[test]
    fn two_tools_sharing_a_resolver_and_arguments_are_different_classification_subjects() {
        // The tool name rides inside the digested `args`, so an answer given for one tool never
        // stands in for another tool's call with identical arguments.
        let uses = ToolResolverUse {
            resolver: DynamicResolverName::new("classify"),
            inputs: BTreeMap::new(),
            returns: BTreeSet::from([ResolverReturn::Trust]),
        };
        let arguments = serde_json::json!({ "path": "notes.txt" });
        let read = described(vec![uses.clone()]);
        let glob = ToolContract {
            name: ToolName::new("Glob"),
            ..described(vec![uses.clone()])
        };
        assert_ne!(
            read.resolver_args_digest(&uses, &read.name, &arguments),
            glob.resolver_args_digest(&uses, &glob.name, &arguments)
        );
    }

    #[test]
    fn a_pin_round_trips_through_its_persisted_form() {
        let uses = binding();
        let contract = ToolContract {
            parameters: crate::params::ToolParameters::open(),
            ..described(vec![uses.clone()])
        };
        let arguments = serde_json::json!({ "customer_id": "cust-7" });
        let pin = PinnedToolResolution::from_answer(
            uses.clone(),
            contract.resolver_args_digest(&uses, &contract.name, &arguments),
            None,
            Some(Audience::restricted([ReaderId::new("support")])),
            None,
            None,
            None,
        )
        .expect("the declared audience answer pins");
        let bytes = serde_json::to_vec(&pin).expect("a pin serializes");
        let read: PinnedToolResolution = serde_json::from_slice(&bytes).expect("a pin reads back");
        assert_eq!(read, pin);
        assert_eq!(
            read.args(),
            crate::contract::ResolverArgsDigest::of(br#"{"customer_id":"cust-7"}"#),
            "the pin binds to the exact value its resolver was sent"
        );
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
}
