//! Labels this crate keeps outside the engine's facts: held and embedded peer messages, and the
//! label a peer send record names.
//!
//! A label's trust is an ordinal into the trust chain of the policy that wrote it, and a later
//! policy revision may rename, reorder or drop ranks. A stored label therefore also records the
//! name of its rank, and a reader resolves the name in its own chain: a rank the reader's chain
//! lacks drops to the lowest, as a value does when it crosses between revisions. A label stored
//! without the name, by an earlier version, keeps its ordinal. The name is an additive key inside
//! the label's JSON object, which earlier versions ignore.

use appa_engine::label::{Label, Trust};
use appa_engine::registry::TrustChain;

/// A label as it is stored outside the log: its fields, and the name its trust rank has in the
/// chain it was written under.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StoredLabel {
    #[serde(flatten)]
    label: Label,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    trust_name: Option<String>,
}

impl StoredLabel {
    /// `label` as written under `chain`. Top trust, above every chain, is stored without a name.
    pub fn named(label: &Label, chain: &TrustChain) -> Self {
        let trust_name = (label.trust != Trust::new(u8::MAX))
            .then(|| chain.name_of(label.trust).map(str::to_owned))
            .flatten();
        StoredLabel {
            label: label.clone(),
            trust_name,
        }
    }

    /// The label a reader under `chain` takes: its rank by name, the lowest when `chain` lacks
    /// the name, and the stored ordinal when no name was stored.
    pub fn under(&self, chain: &TrustChain) -> Label {
        let mut label = self.label.clone();
        if let Some(name) = &self.trust_name {
            label.trust = chain.rank_of(name).unwrap_or(Trust::new(0));
        }
        label
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use appa_engine::label::Audience;

    fn chain(ranks: &[&str]) -> TrustChain {
        TrustChain::new(ranks.iter().map(|rank| rank.to_string()).collect())
    }

    fn label(rank: u8) -> Label {
        Label::new(Trust::new(rank), Audience::public())
    }

    #[test]
    fn a_stored_label_keeps_its_rank_by_name_across_a_reordered_chain() {
        let written = StoredLabel::named(&label(1), &chain(&["suspicious", "trusted"]));
        assert_eq!(
            written.under(&chain(&["untrusted", "suspicious", "verified", "trusted"])),
            label(3)
        );
        assert_eq!(written.under(&chain(&["suspicious", "trusted"])), label(1));
    }

    #[test]
    fn a_rank_the_reading_chain_lacks_drops_to_its_lowest() {
        let written = StoredLabel::named(&label(1), &chain(&["low", "mid", "high"]));
        assert_eq!(written.under(&chain(&["low", "high"])), label(0));
    }

    #[test]
    fn top_trust_and_unnamed_labels_keep_their_ordinal() {
        let top = StoredLabel::named(&Label::top(), &chain(&["suspicious", "trusted"]));
        assert_eq!(top.under(&chain(&["other"])), Label::top());
        let earlier: StoredLabel = serde_json::from_value(serde_json::to_value(label(1)).unwrap()).unwrap();
        assert_eq!(earlier.under(&chain(&["trusted", "suspicious"])), label(1));
    }

    #[test]
    fn the_name_is_an_additive_key_an_earlier_reader_ignores() {
        let written = StoredLabel::named(&label(1), &chain(&["suspicious", "trusted"]));
        let json = serde_json::to_value(&written).unwrap();
        let mut bare = serde_json::to_value(label(1)).unwrap();
        bare.as_object_mut()
            .unwrap()
            .insert("trust_name".into(), serde_json::json!("trusted"));
        assert_eq!(json, bare);
        assert_eq!(serde_json::from_value::<Label>(json).unwrap(), label(1));
    }
}
