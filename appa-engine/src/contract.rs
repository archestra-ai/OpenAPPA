//! Tool contracts: what a call commits (`delta`, `emits`) and what it requires (`requires`).

use serde::{Deserialize, Serialize};

use crate::fact::EffectKind;
use crate::label::{Audience, Dim, Dimension, Label, Trust};
use crate::names::{MarkName, SanitizerName, TagName};
use crate::value::ToolName;

/// A restrictive label contribution: what a successful call folds into the trajectory. Every delta
/// only ever narrows — minimum trust, intersect audience — so a permissive delta is unrepresentable.
/// A dimension may also be declared [`Dim::Unknown`]: **pending-cast** — the result's actual state
/// is established by a registered cast at admission (RP5), so the raw result is confined until then.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delta {
    pub trust: Option<Dim<Trust>>,
    pub audience: Option<Dim<Audience>>,
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
            self.audience.clone().unwrap_or(Dim::Known(Audience::Public)),
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
                Some(Dim::Known(a)) => Dim::Known(a.clone()),
                Some(Dim::Unknown) | None => Dim::Known(Audience::Public),
            },
        );
        label.combine(&established)
    }

    pub fn pending_cast_dim(&self) -> Option<Dimension> {
        match (&self.trust, &self.audience) {
            (Some(Dim::Unknown), _) => Some(Dimension::Trust),
            (_, Some(Dim::Unknown)) => Some(Dimension::Audience),
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolContract {
    pub name: ToolName,
    pub tags: Vec<TagName>,
    pub delta: Delta,
    pub emits: Vec<EffectKind>,
    pub requires: Requires,
    /// The policy-bound output sanitizer (RP4): every successful result of this tool is confined
    /// raw and admitted only as the named sanitizer's derivation, at its declared transition label.
    /// Load validation guarantees the name resolves to a registered `tool_output` sanitizer whose
    /// `from` the declared raw output satisfies; admission refuses a raw or differently-sanitized
    /// value for a bound tool, so the binding is engine-enforced, not runtime courtesy.
    #[serde(default)]
    pub output_sanitizer: Option<SanitizerName>,
}
