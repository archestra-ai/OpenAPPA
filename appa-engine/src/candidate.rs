//! The staged candidate pipeline's durable shapes.

use serde::{Deserialize, Serialize};

use crate::check::Narrowing;
use crate::label::Label;
use crate::names::SanitizerName;
use crate::value::{DispatchId, LabeledValue, OfferId, RawResultDigest, ResolvedCall};

/// The sanitizers a candidate's chain has already spent, in application order.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Vec<SanitizerName>", into = "Vec<SanitizerName>")]
pub struct SanitizerLineage(Vec<SanitizerName>);

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("sanitizer {} occurs more than once in the lineage", .0.as_str())]
pub struct RepeatedSanitizer(pub SanitizerName);

impl SanitizerLineage {
    pub(crate) fn extend(&self, name: SanitizerName) -> Option<SanitizerLineage> {
        if self.contains(&name) {
            return None;
        }
        let mut names = self.0.clone();
        names.push(name);
        Some(SanitizerLineage(names))
    }

    pub fn contains(&self, name: &SanitizerName) -> bool {
        self.0.contains(name)
    }

    pub fn names(&self) -> &[SanitizerName] {
        &self.0
    }
}

impl TryFrom<Vec<SanitizerName>> for SanitizerLineage {
    type Error = RepeatedSanitizer;

    fn try_from(names: Vec<SanitizerName>) -> Result<Self, Self::Error> {
        let mut lineage = SanitizerLineage::default();
        for name in names {
            lineage = lineage.extend(name.clone()).ok_or(RepeatedSanitizer(name))?;
        }
        Ok(lineage)
    }
}

impl From<SanitizerLineage> for Vec<SanitizerName> {
    fn from(lineage: SanitizerLineage) -> Vec<SanitizerName> {
        lineage.0
    }
}

/// What authorised one confined hop.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConfinedFrom {
    Bound,
    Offer(OfferId),
}

/// Where one call candidate stands, for the check that reads it and the stage that plans from it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CallStage {
    substituted: Option<Label>,
    lineage: SanitizerLineage,
}

impl CallStage {
    /// The stage a subject's live candidate leaves. A confined-result candidate is not a call
    /// stage and leaves the origin: no call ever stands on one.
    pub(crate) fn of(candidate: Option<&DerivedCandidate>, lineage: SanitizerLineage) -> CallStage {
        match candidate {
            Some(DerivedCandidate::Call { label, .. }) => CallStage::substituting(label.clone(), lineage),
            Some(DerivedCandidate::Result { .. }) | None => CallStage {
                substituted: None,
                lineage,
            },
        }
    }

    /// The stage one validated substitution leaves: its derivation's label, read by the `includes`
    /// check and nothing else, and its sanitizer spent.
    pub(crate) fn substituting(label: Label, lineage: SanitizerLineage) -> CallStage {
        CallStage {
            substituted: Some(label),
            lineage,
        }
    }

    pub(crate) fn substituted(&self) -> Option<&Label> {
        self.substituted.as_ref()
    }

    /// The label the bytes this call would release carry now — the source an input sanitizer's
    /// declared `from` is measured against. Model-authored arguments carry the
    /// trajectory's own established bound; a substitution carries its derivation's label instead.
    pub(crate) fn released(&self, current: &crate::label::PartialLabel) -> Label {
        match &self.substituted {
            Some(label) => label.clone(),
            None => current.bound().clone().into_label(),
        }
    }

    pub(crate) fn lineage(&self) -> &SanitizerLineage {
        &self.lineage
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DerivedCandidate {
    Call {
        source: RawResultDigest,
        from: OfferId,
        call: ResolvedCall,
        label: Label,
    },
    Result {
        dispatch: DispatchId,
        source: RawResultDigest,
        from: ConfinedFrom,
        value: LabeledValue,
        residual: Option<Narrowing>,
    },
}
