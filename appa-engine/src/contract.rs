//! Tool contracts: what a call commits (`delta`, `emits`) and what it requires (`requires`).

use serde::{Deserialize, Serialize};

use crate::fact::EffectKind;
use crate::label::{Audience, Dim, Label, Trust};
use crate::names::{MarkName, TagName};
use crate::value::ToolName;

/// A restrictive label contribution: what a successful call folds into the trajectory. Every delta
/// only ever narrows — minimum trust, intersect audience — so a permissive delta is unrepresentable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delta {
    pub trust: Option<Trust>,
    pub audience: Option<Audience>,
}

impl Delta {
    pub const NONE: Delta = Delta {
        trust: None,
        audience: None,
    };

    /// The delta as a label — the output label a raw result carries. Absent dimensions fill with
    /// the fold identity, so they neither narrow the trajectory nor lower the value's own label.
    pub fn output_label(&self) -> Label {
        Label::new(
            self.trust.map_or(Dim::Known(Trust::new(u8::MAX)), Dim::Known),
            self.audience.clone().map_or(Dim::Known(Audience::Public), Dim::Known),
        )
    }

    pub fn apply(&self, label: &Label) -> Label {
        label.combine(&self.output_label())
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
}
