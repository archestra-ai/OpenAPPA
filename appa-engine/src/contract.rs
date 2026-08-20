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
/// A contract may carry no delta at all ([`ToolContract::delta`] is `None`): the tool is
/// **unannotated** — the deployment never described its output, which is not the same as declaring
/// it neutral. An unannotated tool's result is admitted at `Unknown` in both dimensions
/// (fail-closed: the fold absorbs Unknown, and any later check whose requirement consumes the
/// dimension names the values a cast must resolve). The deliberate "this result carries nothing"
/// annotation is the empty declared delta ([`Delta::NONE`], `delta = {}` on the config surface).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delta {
    pub trust: Option<Dim<Trust>>,
    pub audience: Option<AudienceDelta>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DynamicAudienceBinding {
    pub resolver: DynamicResolverName,
    pub argument: String,
}

/// The declared audience contribution: a written reader set — literal readers and groups an
/// operation resolves when it reads them — a pending cast, or a dynamic binding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudienceDelta {
    Static(DeclaredAudience),
    PendingCast,
    Dynamic(DynamicAudienceBinding),
}

impl From<Dim<Audience>> for AudienceDelta {
    fn from(value: Dim<Audience>) -> Self {
        match value {
            Dim::Known(audience) => Self::Static(DeclaredAudience::literal(audience)),
            Dim::Unknown => Self::PendingCast,
        }
    }
}

/// One successful dynamic answer pinned to a proposed call: the literal reader set the
/// deployment's resolver returned for the argument this binding names. Only a successful answer
/// exists here — no answer is the absence of a pin, which the boundary refuses before any fact
/// ([`crate::check::validate_dynamic_resolutions`]), never a pin that carries none.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PinnedDynamicResolution {
    binding: DynamicAudienceBinding,
    audience: Audience,
}

impl PinnedDynamicResolution {
    /// Pin one resolver answer, or refuse it as malformed: `public` is not a literal
    /// reader set, and an `@group` must go through membership resolution. An empty reader set is a
    /// valid answer.
    pub fn from_answer(binding: DynamicAudienceBinding, audience: Audience) -> Option<Self> {
        match &audience {
            Audience::Public => None,
            Audience::Restricted(readers) if !readers.iter().all(ReaderId::is_literal) => None,
            Audience::Restricted(_) => Some(PinnedDynamicResolution { binding, audience }),
        }
    }

    pub fn binding(&self) -> &DynamicAudienceBinding {
        &self.binding
    }

    pub fn audience(&self) -> &Audience {
        &self.audience
    }
}

impl<'de> Deserialize<'de> for PinnedDynamicResolution {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireResolution {
            binding: DynamicAudienceBinding,
            audience: Audience,
        }

        let wire = WireResolution::deserialize(deserializer)?;
        PinnedDynamicResolution::from_answer(wire.binding, wire.audience).ok_or_else(|| {
            serde::de::Error::custom("a dynamic answer is a literal reader set, never public or a group")
        })
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
                Some(AudienceDelta::PendingCast | AudienceDelta::Dynamic(_)) => Dim::Unknown,
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
                Some(AudienceDelta::PendingCast | AudienceDelta::Dynamic(_)) | None => Audience::Public,
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
    Dynamic(DynamicAudienceBinding),
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
            AudienceRequirement::Includes(RecipientSpec::Placeholder(_) | RecipientSpec::Dynamic(_)) => None,
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
    /// The declared output contribution, or `None`: **unannotated** — results are admitted at
    /// `Unknown` in both dimensions (see [`Delta`]). `Some(Delta::NONE)` is the deliberate neutral
    /// annotation; the two are not interchangeable.
    #[serde(default)]
    pub delta: Option<Delta>,
    pub emits: EffectSet,
    pub requires: Requires,
}

impl ToolContract {
    /// The label a raw admitted result of this tool carries: the declared delta as a label, or —
    /// unannotated — `Unknown` in both dimensions. A written group reads as the operation resolved
    /// it.
    pub fn output_label(&self, expansions: &Expansions) -> Label {
        match &self.delta {
            Some(delta) => delta.output_label(expansions),
            None => Label::new(Dim::Unknown, Dim::Unknown),
        }
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

    /// The output label with a proposed call's dynamic audience answer pinned into it.
    /// Only a successful answer enters the engine, and every binding a contract spells carries one
    /// by the time a call is checked — [`crate::check::validate_dynamic_resolutions`] refuses the
    /// proposal otherwise, before any fact, and replay holds a persisted call to the same rule.
    pub(crate) fn output_label_for_call(&self, call: &crate::value::ResolvedCall, expansions: &Expansions) -> Label {
        let mut label = self.output_label(expansions);
        if let Some(AudienceDelta::Dynamic(binding)) = self.delta.as_ref().and_then(|delta| delta.audience.as_ref()) {
            let answer = call
                .dynamic_resolution(binding)
                .expect("a checked call carries an answer for every dynamic binding its contract spells");
            label.audience = Dim::Known(answer.clone());
        }
        label
    }

    /// The output label recovered from the dynamic answer persisted on a dispatch. Admission —
    /// and the runtime composing a whole-source pending-cast answer against it — uses
    /// this form, never the caller's in-memory resolution.
    pub fn output_label_for_resolutions(
        &self,
        resolutions: &[PinnedDynamicResolution],
        expansions: &Expansions,
    ) -> Label {
        let mut label = self.output_label(expansions);
        if let Some(AudienceDelta::Dynamic(binding)) = self.delta.as_ref().and_then(|delta| delta.audience.as_ref()) {
            let mut matching = resolutions.iter().filter(|resolution| resolution.binding() == binding);
            label.audience = match (matching.next(), matching.next()) {
                (Some(pinned), None) => Dim::Known(pinned.audience().clone()),
                _ => Dim::Unknown,
            };
        }
        label
    }

    /// The single dimension this contract declares pending-cast, if any. An unannotated tool
    /// declares none: its Unknown output is admitted as-is, not confined awaiting a cast.
    pub fn pending_cast_dim(&self) -> Option<Dimension> {
        self.delta.as_ref().and_then(Delta::pending_cast_dim)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::names::DynamicResolverName;

    fn binding() -> DynamicAudienceBinding {
        DynamicAudienceBinding {
            resolver: DynamicResolverName::new("crm-acl"),
            argument: "customer_id".to_string(),
        }
    }

    #[test]
    fn a_dynamic_answer_keeps_only_literal_reader_sets() {
        let pinned =
            |audience| PinnedDynamicResolution::from_answer(binding(), audience).map(|pin| pin.audience().clone());

        assert_eq!(pinned(Audience::Public), None);
        assert_eq!(pinned(Audience::restricted([ReaderId::new("public")])), None);
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
