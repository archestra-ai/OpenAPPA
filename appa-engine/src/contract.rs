//! Tool contracts: what a call commits (`delta`, `emits`) and what it requires (`requires`).

use std::collections::BTreeSet;

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
/// A contract may carry no delta and no resolver-owned label field: the tool is **unannotated** —
/// the deployment never described its output, which is not the same as declaring it neutral. An
/// unannotated tool's result is admitted at `Unknown` in both dimensions
/// (fail-closed: the fold absorbs Unknown, and any later check whose requirement consumes the
/// dimension names the values a cast must resolve). The deliberate "this result carries nothing"
/// annotation is the empty declared delta ([`Delta::NONE`], `delta = {}` on the config surface).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delta {
    pub trust: Option<Dim<Trust>>,
    pub audience: Option<AudienceDelta>,
}

/// One part of a tool contract a tool-level dynamic resolver establishes from the complete
/// canonical call arguments. Label dimensions have one owner; attention is additive.
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

/// The wire scope one returned field lives in: the output `delta` or the call-time `requires`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReturnScope {
    Delta,
    Requires,
}

impl ReturnScope {
    pub fn wire_name(self) -> &'static str {
        match self {
            ReturnScope::Delta => "delta",
            ReturnScope::Requires => "requires",
        }
    }
}

impl ResolverReturn {
    pub const ALL: [ResolverReturn; 5] = [
        ResolverReturn::Trust,
        ResolverReturn::Audience,
        ResolverReturn::RequiredTrust,
        ResolverReturn::RequiredAudience,
        ResolverReturn::Attention,
    ];

    /// The one scope/name map every wire surface reads — the policy's `returns` table, the
    /// resolver request, and the response shape all agree through it.
    pub fn scope(self) -> ReturnScope {
        match self {
            ResolverReturn::Trust | ResolverReturn::Audience => ReturnScope::Delta,
            ResolverReturn::RequiredTrust | ResolverReturn::RequiredAudience | ResolverReturn::Attention => {
                ReturnScope::Requires
            }
        }
    }

    pub fn wire_name(self) -> &'static str {
        match self {
            ResolverReturn::Trust | ResolverReturn::RequiredTrust => "trust",
            ResolverReturn::Audience | ResolverReturn::RequiredAudience => "audience",
            ResolverReturn::Attention => "attention",
        }
    }
}

/// One resolver attached to a tool: which registered resolver, what it reads, and the exact
/// scoped fields it must return. A binding without `argument` shows the resolver the complete
/// canonical argument object; with `argument = "field"` it shows exactly that declared required
/// top-level string value, and its pin survives argument substitution while that field's value
/// is unchanged.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ToolResolverBinding {
    pub resolver: DynamicResolverName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argument: Option<String>,
    pub returns: BTreeSet<ResolverReturn>,
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
/// accepts exactly the fields the binding declares and canonicalizes additive attention marks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PinnedToolResolution {
    binding: ToolResolverBinding,
    trust: Option<Trust>,
    audience: Option<Audience>,
    required_trust: Option<Trust>,
    required_audience: Option<RequiredAudience>,
    attention: Vec<MarkName>,
}

impl PinnedToolResolution {
    pub fn from_answer(
        binding: ToolResolverBinding,
        trust: Option<Trust>,
        audience: Option<Audience>,
        required_trust: Option<Trust>,
        required_audience: Option<RequiredAudience>,
        attention: Option<Vec<MarkName>>,
    ) -> Option<Self> {
        if binding.returns.is_empty()
            || binding.returns.contains(&ResolverReturn::Trust) != trust.is_some()
            || binding.returns.contains(&ResolverReturn::Audience) != audience.is_some()
            || binding.returns.contains(&ResolverReturn::RequiredTrust) != required_trust.is_some()
            || binding.returns.contains(&ResolverReturn::RequiredAudience) != required_audience.is_some()
            || binding.returns.contains(&ResolverReturn::Attention) != attention.is_some()
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
            binding,
            trust,
            audience,
            required_trust,
            required_audience,
            attention,
        })
    }

    pub fn binding(&self) -> &ToolResolverBinding {
        &self.binding
    }

    pub fn trust(&self) -> Option<Trust> {
        self.trust
    }

    pub fn audience(&self) -> Option<&Audience> {
        self.audience.as_ref()
    }

    pub fn required_trust(&self) -> Option<Trust> {
        self.required_trust
    }

    pub fn required_audience(&self) -> Option<&RequiredAudience> {
        self.required_audience.as_ref()
    }

    pub fn attention(&self) -> &[MarkName] {
        &self.attention
    }
}

impl<'de> Deserialize<'de> for PinnedToolResolution {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            binding: ToolResolverBinding,
            trust: Option<Trust>,
            audience: Option<Audience>,
            required_trust: Option<Trust>,
            required_audience: Option<RequiredAudience>,
            attention: Vec<MarkName>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let original_attention = wire.attention.clone();
        let returned_attention = wire
            .binding
            .returns
            .contains(&ResolverReturn::Attention)
            .then_some(wire.attention);
        let answer = PinnedToolResolution::from_answer(
            wire.binding,
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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabelRequirements {
    pub trust_floor: Option<Trust>,
    pub audience: Vec<AudienceRequirement>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requires {
    pub label: LabelRequirements,
    pub history: Vec<HistoryRequirement>,
    pub attention: Vec<MarkName>,
}

/// A tool contract: name, routing tags, the compiled input schema, and the three
/// algebraic slots.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolContract {
    pub name: ToolName,
    pub tags: Vec<TagName>,
    /// The compiled, normalized `APPA Tool Parameters v1` schema — part of policy identity.
    /// Omitted `parameters` normalizes to the permissive open object.
    #[serde(default = "crate::params::ToolParameters::open")]
    pub parameters: crate::params::ToolParameters,
    /// Dynamic parts of this tool's contract, resolved from its complete canonical arguments
    /// before the call is checked.
    #[serde(default)]
    pub resolvers: Vec<ToolResolverBinding>,
    /// The static output contribution. `None` is unannotated only when no tool-level resolver owns
    /// trust or audience. `Some(Delta::NONE)` is the deliberate neutral annotation.
    #[serde(default)]
    pub delta: Option<Delta>,
    pub emits: EffectSet,
    pub requires: Requires,
}

impl ToolContract {
    /// Whether a tool-level resolver binding owns this returned field — the one definition of
    /// "resolver-owned" every load check and label derivation reads.
    pub(crate) fn resolver_owns(&self, field: ResolverReturn) -> bool {
        self.resolvers.iter().any(|binding| binding.returns.contains(&field))
    }

    /// The unresolved output shape before call-pinned answers are applied. Each dimension is
    /// derived independently: resolver-owned fields are `Unknown` until their pin applies, and
    /// every other dimension is exactly what the static contract describes — its declared delta
    /// value, or `Unknown` on an unannotated tool. Resolver ownership of one dimension never
    /// establishes the other.
    pub fn output_label(&self, expansions: &Expansions) -> Label {
        let mut label = match &self.delta {
            Some(delta) => delta.output_label(expansions),
            None => Label::new(Dim::Unknown, Dim::Unknown),
        };
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
        self.delta.iter().flat_map(Delta::groups).chain(
            self.requires
                .label
                .audience
                .iter()
                .flat_map(AudienceRequirement::groups),
        )
    }

    /// The output label with a proposed call's pinned resolver answers applied.
    /// Only a successful answer enters the engine, and every binding a contract spells carries one
    /// by the time a call is checked — [`crate::check::validate_tool_resolutions`] refuses the
    /// proposal otherwise, before any fact, and replay holds a persisted call to the same rule.
    pub(crate) fn output_label_for_call(&self, call: &crate::value::ResolvedCall, expansions: &Expansions) -> Label {
        let mut label = self.output_label(expansions);
        apply_tool_resolutions(&mut label, call.tool_resolutions());
        label
    }

    /// The output label recovered from the resolver answers persisted on a dispatch. Admission —
    /// and the runtime composing a whole-source pending-cast answer against it — uses
    /// this form, never the caller's in-memory resolution.
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
        self.delta.as_ref().and_then(Delta::pending_cast_dim)
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

    fn binding() -> ToolResolverBinding {
        ToolResolverBinding {
            resolver: DynamicResolverName::new("crm-acl"),
            argument: Some("customer_id".to_string()),
            returns: BTreeSet::from([ResolverReturn::Audience]),
        }
    }

    #[test]
    fn resolver_ownership_leaves_the_other_dimension_fail_closed() {
        use crate::groups::Expansions;

        let owner = |field: ResolverReturn| ToolContract {
            name: ToolName::new("lookup"),
            tags: vec![],
            parameters: crate::params::ToolParameters::open(),
            resolvers: vec![ToolResolverBinding {
                resolver: DynamicResolverName::new("classifier"),
                argument: None,
                returns: BTreeSet::from([field]),
            }],
            delta: None,
            emits: crate::fact::EffectSet::default(),
            requires: Requires::default(),
        };

        // An unannotated dimension stays Unknown whatever the resolver owns: ownership of
        // the audience never promotes trust off the fail-closed floor, and vice versa.
        let label = owner(ResolverReturn::Audience).output_label(&Expansions::default());
        assert_eq!(label.trust, Dim::Unknown, "trust was never described");
        assert_eq!(label.audience, Dim::Unknown, "the pin has not applied yet");

        let label = owner(ResolverReturn::Trust).output_label(&Expansions::default());
        assert_eq!(label.trust, Dim::Unknown, "the pin has not applied yet");
        assert_eq!(label.audience, Dim::Unknown, "audience was never described");

        // With the pin applied, exactly the owned dimension resolves.
        let binding = ToolResolverBinding {
            resolver: DynamicResolverName::new("classifier"),
            argument: None,
            returns: BTreeSet::from([ResolverReturn::Audience]),
        };
        let pin = PinnedToolResolution::from_answer(
            binding,
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
        assert_eq!(label.trust, Dim::Unknown, "the undescribed dimension stays fail-closed");
    }

    #[test]
    fn a_resolver_audience_answer_keeps_only_literal_reader_sets() {
        let pinned = |audience| {
            PinnedToolResolution::from_answer(binding(), None, Some(audience), None, None, None)
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
