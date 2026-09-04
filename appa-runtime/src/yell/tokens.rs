//! Report-local identifiers for the names a deployment chose.
//!
//! Under pseudonymization every policy-defined name — a tool, an authority, a group, a
//! selector — is replaced by a token like `tool-1`. The substitution is *within one report*:
//! two reports never agree on what `tool-1` means, so nothing here can be used to correlate
//! one deployment across reports.
//!
//! What survives the substitution is what a reader needs to reason about the trajectory:
//! equality (the same tool is the same token everywhere), cardinality (three tools stay
//! three), and order of first appearance. What does not survive is the spelling, which is the
//! only part that names the deployment.

use std::collections::BTreeMap;

/// What kind of name a token stands for. Tokens are numbered per class, so a report's
/// `tool-1` and `authority-1` are unrelated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Class {
    Trajectory,
    Reader,
    Authority,
    Tool,
    Effect,
    Sanitizer,
    Annotator,
    Mark,
    Group,
    Surface,
    Source,
    Selector,
    /// A trust rank's policy-given name. Ranks are numeric in a fact; only the chain names them.
    Trust,
    /// A custom identity implementation's name.
    Identity,
    /// A name the policy author chose for a field: a property in a return contract's schema, or
    /// the tool argument an `includes($arg)` placeholder reads.
    Field,
    /// A `const` or `enum` literal authored in a return contract's schema.
    Literal,
}

impl Class {
    /// The stem a token of this class is spelled with.
    fn stem(self) -> &'static str {
        match self {
            Class::Trajectory => "trajectory",
            Class::Reader => "reader",
            Class::Authority => "authority",
            Class::Tool => "tool",
            Class::Effect => "effect",
            Class::Sanitizer => "sanitizer",
            Class::Annotator => "annotator",
            Class::Mark => "mark",
            Class::Group => "group",
            Class::Surface => "surface",
            Class::Source => "source",
            Class::Selector => "selector",
            Class::Trust => "trust",
            Class::Identity => "identity",
            Class::Field => "field",
            Class::Literal => "literal",
        }
    }
}

/// One report's substitution table.
///
/// In [`Mode::Baseline`] this hands every name back unchanged; the walk is identical either
/// way, so a baseline and a pseudonymized report differ in exactly one place rather than
/// following two code paths that can drift apart.
#[derive(Debug, Default)]
pub(crate) struct Tokens {
    assigned: BTreeMap<(Class, String), u32>,
    counts: BTreeMap<Class, u32>,
}

/// Whether policy-defined names are carried as spelled or replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Names as the deployment spells them. Still excludes every value, argument, path and
    /// identity — the classification is the same; only the naming differs.
    Baseline,
    Pseudonymized,
}

impl Tokens {
    /// This name's token, minting one on first appearance.
    ///
    /// Numbering follows first appearance *in the walk*, and the walk's order is pinned
    /// (lexicographic keys, index order) precisely so that this numbering does not depend on
    /// how the JSON library happens to store its maps.
    pub(crate) fn token(&mut self, mode: Mode, class: Class, raw: &str) -> String {
        if mode == Mode::Baseline {
            return raw.to_string();
        }
        let key = (class, raw.to_string());
        let next = match self.assigned.get(&key) {
            Some(existing) => *existing,
            None => {
                let counter = self.counts.entry(class).or_insert(0);
                *counter += 1;
                let minted = *counter;
                self.assigned.insert(key, minted);
                minted
            }
        };
        format!("{}-{next}", class.stem())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_carries_names_as_spelled() {
        let mut tokens = Tokens::default();
        assert_eq!(tokens.token(Mode::Baseline, Class::Tool, "Read"), "Read");
    }

    /// Equality and cardinality are the whole point: a reader must be able to see that two
    /// steps used the same tool without being told which tool it was.
    #[test]
    fn one_name_keeps_one_token_and_distinct_names_get_distinct_ones() {
        let mut tokens = Tokens::default();
        let first = tokens.token(Mode::Pseudonymized, Class::Tool, "Read");
        let second = tokens.token(Mode::Pseudonymized, Class::Tool, "Write");
        let again = tokens.token(Mode::Pseudonymized, Class::Tool, "Read");
        assert_eq!(first, "tool-1");
        assert_eq!(second, "tool-2");
        assert_eq!(again, first);
    }

    #[test]
    fn classes_are_numbered_independently() {
        let mut tokens = Tokens::default();
        assert_eq!(tokens.token(Mode::Pseudonymized, Class::Tool, "Read"), "tool-1");
        assert_eq!(
            tokens.token(Mode::Pseudonymized, Class::Authority, "hitl"),
            "authority-1"
        );
    }

    /// The same spelling in two classes is two different things, and conflating them would
    /// invent an equality the trajectory never had.
    #[test]
    fn the_same_spelling_in_two_classes_is_two_tokens() {
        let mut tokens = Tokens::default();
        let tool = tokens.token(Mode::Pseudonymized, Class::Tool, "review");
        let authority = tokens.token(Mode::Pseudonymized, Class::Authority, "review");
        assert_eq!(tool, "tool-1");
        assert_eq!(authority, "authority-1");
    }
}
