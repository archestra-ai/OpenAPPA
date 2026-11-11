//! A file's label as the plugin keeps it: a trust rank and an audience, in the policy's own
//! spellings. The plugin never decides a flow. It meets labels — what a read of several
//! files carries is the meet of their labels — and hands the result back to the engine.

use serde::{Deserialize, Serialize};

/// A trust rank and an audience. `trust: None` is the top of the chain, the identity of the
/// meet. The audience is an intersection of clauses, each the union of its entries; no clause
/// is `public`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Label {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audience: Vec<Vec<String>>,
}

/// The policy's trust ranks, lowest first, as an annotation consult declares them. A rank the
/// chain does not name reads as the lowest: a label written under another revision never
/// rises.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ranks(pub Vec<String>);

impl Ranks {
    fn position(&self, rank: &str) -> usize {
        self.0.iter().position(|known| known == rank).unwrap_or(0)
    }

    pub fn lowest(&self) -> Option<&str> {
        self.0.first().map(String::as_str)
    }

    /// The higher of two required ranks: the stricter trust floor.
    pub fn higher<'a>(&self, left: Option<&'a str>, right: Option<&'a str>) -> Option<&'a str> {
        match (left, right) {
            (Some(left), Some(right)) => Some(match self.position(left) >= self.position(right) {
                true => left,
                false => right,
            }),
            (left, right) => left.or(right),
        }
    }
}

impl Label {
    /// The label the runtime publishes to a recorder: `{"trust": rank, "audience":
    /// "public" | [[entry, ...], ...]}`.
    pub fn from_published(value: &serde_json::Value) -> Result<Label, String> {
        let trust = value["trust"]
            .as_str()
            .ok_or("a published label names its trust rank")?
            .to_string();
        let audience = match &value["audience"] {
            serde_json::Value::String(public) if public == "public" => Vec::new(),
            clauses => serde_json::from_value(clauses.clone())
                .map_err(|_| "a published audience is \"public\" or a list of clauses".to_string())?,
        };
        Ok(Label {
            trust: Some(trust),
            audience,
        }
        .normalized())
    }

    /// The meet: the lower trust, and both audiences' clauses.
    pub fn meet(&self, other: &Label, ranks: &Ranks) -> Label {
        let trust = match (&self.trust, &other.trust) {
            (Some(left), Some(right)) => Some(match ranks.position(left) <= ranks.position(right) {
                true => left.clone(),
                false => right.clone(),
            }),
            (left, right) => left.clone().or_else(|| right.clone()),
        };
        let mut audience = self.audience.clone();
        audience.extend(other.audience.iter().cloned());
        Label { trust, audience }.normalized()
    }

    /// One canonical spelling: each clause sorted and free of an entry another subsumes, and no
    /// clause a narrower one already implies. The built-in chain is `self ⊆ internal ⊆ public`.
    fn normalized(mut self) -> Label {
        for clause in &mut self.audience {
            if clause.iter().any(|entry| entry == "internal") {
                clause.retain(|entry| entry != "self");
            }
            clause.sort();
            clause.dedup();
        }
        self.audience.sort();
        self.audience.dedup();
        let clauses = self.audience.clone();
        self.audience
            .retain(|clause| !clauses.iter().any(|other| other != clause && narrower(other, clause)));
        self
    }
}

/// Whether every reader of `narrow` is a reader of `wide`.
fn narrower(narrow: &[String], wide: &[String]) -> bool {
    narrow
        .iter()
        .all(|entry| wide.contains(entry) || (entry == "self" && wide.iter().any(|reader| reader == "internal")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranks() -> Ranks {
        Ranks(vec!["suspicious".into(), "trusted".into()])
    }

    fn label(trust: Option<&str>, audience: &[&[&str]]) -> Label {
        Label {
            trust: trust.map(str::to_string),
            audience: audience
                .iter()
                .map(|clause| clause.iter().map(|entry| entry.to_string()).collect())
                .collect(),
        }
    }

    #[test]
    fn the_meet_takes_the_lower_rank_and_the_narrower_audience() {
        let secret = label(Some("trusted"), &[&["self"]]);
        let web = label(Some("suspicious"), &[]);
        let team = label(None, &[&["internal"]]);
        assert_eq!(secret.meet(&web, &ranks()), label(Some("suspicious"), &[&["self"]]));
        assert_eq!(team.meet(&secret, &ranks()), label(Some("trusted"), &[&["self"]]));
        // Two unrelated readers keep both clauses: only someone in both may read.
        let both = label(None, &[&["alice"]]).meet(&label(None, &[&["bob"]]), &ranks());
        assert_eq!(both.audience.len(), 2);
    }

    #[test]
    fn a_rank_the_chain_does_not_name_reads_as_the_lowest() {
        let stale = label(Some("vetted"), &[]);
        let trusted = label(Some("trusted"), &[]);
        assert_eq!(trusted.meet(&stale, &ranks()).trust.as_deref(), Some("vetted"));
        assert_eq!(ranks().higher(Some("vetted"), Some("trusted")), Some("trusted"));
    }

    #[test]
    fn a_published_label_reads_back_in_canonical_form() {
        let published = serde_json::json!({ "trust": "trusted", "audience": [["internal", "self"]] });
        assert_eq!(
            Label::from_published(&published).unwrap(),
            label(Some("trusted"), &[&["internal"]])
        );
        let public = serde_json::json!({ "trust": "suspicious", "audience": "public" });
        assert_eq!(Label::from_published(&public).unwrap(), label(Some("suspicious"), &[]));
    }
}
