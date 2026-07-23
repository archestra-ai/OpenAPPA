//! Tool contracts: what a call commits (`delta`, `emits`) and what it requires (`requires`).
//!
//! A contract declares one contribution per piece of state and its requirements in three kinds:
//! label requirements (trust floor, audience includes/cap), history requirements
//! (`prior`/`no_prior`), and per-call attention demands. The two commit slots stay distinct from
//! the requirement slots — a call may carry either, both, or neither.

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

    /// The label a successful call would commit: the current label folded with this delta.
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

/// An audience-side label requirement, from either direction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudienceRequirement {
    /// `audience ⊇ recipients` — the trajectory's readers include the call's recipients.
    Includes(RecipientSpec),
    /// `audience ⊆ cap` — the committed reader set stays within the tool's declared cap.
    Cap(Audience),
}

/// A history requirement, checked against the log, in two species.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HistoryRequirement {
    /// A matching effect exists ("delete only after backup ran"). Remedy: make the effect happen.
    Prior(EffectKind),
    /// No matching effect in the log. Waivable for one dispatch by a competent ruling.
    NoPrior(EffectKind),
}

/// The label requirements: an optional trust floor and any number of audience constraints.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabelRequirements {
    pub trust_floor: Option<Trust>,
    pub audience: Vec<AudienceRequirement>,
}

/// A tool's full requirement set across the three kinds.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requires {
    pub label: LabelRequirements,
    pub history: Vec<HistoryRequirement>,
    pub attention: Vec<MarkName>,
}

/// A tool contract: name, routing tags, and the three algebraic slots.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolContract {
    pub name: ToolName,
    pub tags: Vec<TagName>,
    pub delta: Delta,
    pub emits: Vec<EffectKind>,
    pub requires: Requires,
}
