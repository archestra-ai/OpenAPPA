//! Tool contracts: what a call commits (`delta`, `emits`) and what it requires (`requires`).

use serde::{Deserialize, Serialize};

use crate::fact::{EffectKind, EffectSet};
use crate::label::{Audience, Dim, Dimension, Label, ReaderId, Trust};
use crate::names::{DynamicResolverName, MarkName, TagName};
use crate::value::ToolName;

/// A **declared** restrictive label contribution: what a successful call folds into the trajectory.
/// Every delta only ever narrows — minimum trust, intersect audience — so a permissive delta is
/// unrepresentable. A dimension may also be declared [`Dim::Unknown`]: **pending-cast** — the
/// result's actual state is established by a registered cast at admission (RP5), so the raw result
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudienceDelta {
    Static(Audience),
    PendingCast,
    Dynamic(DynamicAudienceBinding),
}

impl From<Dim<Audience>> for AudienceDelta {
    fn from(value: Dim<Audience>) -> Self {
        match value {
            Dim::Known(audience) => Self::Static(audience),
            Dim::Unknown => Self::PendingCast,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PinnedDynamicResolution {
    binding: DynamicAudienceBinding,
    audience: Option<Audience>,
}

impl PinnedDynamicResolution {
    /// Pin one resolver answer. A malformed dynamic answer contributes no audience:
    /// `public` is not a literal reader set, and an `@group` must go through membership resolution.
    pub fn from_answer(binding: DynamicAudienceBinding, audience: Option<Audience>) -> Self {
        let audience = audience.filter(|answer| match answer {
            Audience::Public => false,
            Audience::Restricted(readers) => readers.iter().all(ReaderId::is_literal),
        });
        PinnedDynamicResolution { binding, audience }
    }

    pub fn binding(&self) -> &DynamicAudienceBinding {
        &self.binding
    }

    pub fn audience(&self) -> Option<&Audience> {
        self.audience.as_ref()
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
            audience: Option<Audience>,
        }

        let wire = WireResolution::deserialize(deserializer)?;
        Ok(PinnedDynamicResolution::from_answer(wire.binding, wire.audience))
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
    /// resolves it).
    pub fn output_label(&self) -> Label {
        Label::new(
            self.trust.clone().unwrap_or(Dim::Known(Trust::new(u8::MAX))),
            match &self.audience {
                Some(AudienceDelta::Static(a)) => Dim::Known(a.clone()),
                Some(AudienceDelta::PendingCast | AudienceDelta::Dynamic(_)) => Dim::Unknown,
                None => Dim::Known(Audience::Public),
            },
        )
    }

    /// The label a successful call would commit, on the check's clock: the current label folded
    /// with the delta's **established** dimensions only. A pending-cast dimension contributes
    /// identity here — its actual contribution folds at admission, at the resolved label, where
    /// every later call re-checks against it. (Sound because load validation refuses a contract
    /// that pairs a pending-cast dimension with a `requires` on that same dimension, so no check
    /// this projection feeds can depend on the unestablished state.)
    pub fn apply(&self, label: &Label) -> Label {
        let established = Label::new(
            match &self.trust {
                Some(Dim::Known(t)) => Dim::Known(*t),
                Some(Dim::Unknown) | None => Dim::Known(Trust::new(u8::MAX)),
            },
            match &self.audience {
                Some(AudienceDelta::Static(a)) => Dim::Known(a.clone()),
                Some(AudienceDelta::PendingCast | AudienceDelta::Dynamic(_)) | None => Dim::Known(Audience::Public),
            },
        );
        label.combine(&established)
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
}

/// The recipients of an audience `includes` requirement — a static set, or a placeholder resolved
/// from the call's arguments (`$recipient` → the value of argument `recipient`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecipientSpec {
    Static(Audience),
    Placeholder(String),
    Dynamic(DynamicAudienceBinding),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudienceRequirement {
    Includes(RecipientSpec),
    Cap(Audience),
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
    /// unannotated — `Unknown` in both dimensions.
    pub fn output_label(&self) -> Label {
        match &self.delta {
            Some(delta) => delta.output_label(),
            None => Label::new(Dim::Unknown, Dim::Unknown),
        }
    }

    /// The output label with a proposed call's dynamic audience answer pinned into it. A missing
    /// or failed answer leaves that dimension Unknown.
    pub(crate) fn output_label_for_call(&self, call: &crate::value::ResolvedCall) -> Label {
        let mut label = self.output_label();
        if let Some(AudienceDelta::Dynamic(binding)) = self.delta.as_ref().and_then(|delta| delta.audience.as_ref()) {
            label.audience = call
                .dynamic_resolution(binding)
                .cloned()
                .map(Dim::Known)
                .unwrap_or(Dim::Unknown);
        }
        label
    }

    /// The output label recovered from the dynamic answer persisted on a dispatch. Admission uses
    /// this form, never the caller's in-memory resolution.
    pub(crate) fn output_label_for_resolutions(&self, resolutions: &[PinnedDynamicResolution]) -> Label {
        let mut label = self.output_label();
        if let Some(AudienceDelta::Dynamic(binding)) = self.delta.as_ref().and_then(|delta| delta.audience.as_ref()) {
            let mut matching = resolutions.iter().filter(|resolution| resolution.binding() == binding);
            let answer = matching.next().and_then(PinnedDynamicResolution::audience);
            label.audience = match (answer, matching.next()) {
                (Some(audience), None) => Dim::Known(audience.clone()),
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
        let pinned = |audience| {
            PinnedDynamicResolution::from_answer(binding(), Some(audience))
                .audience()
                .cloned()
        };

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
}
