//! Report-local identifiers for the strings a report may not carry as spelled.
//!
//! # Two reasons a name is replaced, and only one of them is a mode
//!
//! Some strings are the *deployment's* vocabulary: a tool, an authority, a trust rank. Those
//! are diagnostic in a report a team sends about its own deployment, so [`Mode::Baseline`]
//! carries them as spelled and [`Mode::Pseudonymized`] replaces them — that is the whole of
//! what the person is agreeing to at the first prompt.
//!
//! Others are not the deployment's vocabulary at all: a harness session id, a person, a key
//! the *model* chose in a tool call, a field name in a schema the *agent* authored. No mode
//! carries those, because no mode was ever offered for them. [`Class::always_tokenized`] is
//! that distinction, and it is a property of the class rather than of the caller, so a table
//! entry cannot get it wrong by choosing the mode-governed spelling for a person.
//!
//! # What a token preserves
//!
//! Equality (the same tool is the same token everywhere), cardinality (three tools stay
//! three), and order of first appearance. What it does not preserve is the spelling.
//!
//! Tokens are report-*local*: nothing here is a stable identifier, and `tool-1` in one report
//! carries no relation to `tool-1` in another. They are not randomized aliases, so two reports
//! of similar sessions may well number similarly — the guarantee is that the spelling does not
//! leave, not that an observer holding one report learns nothing about another.

use std::collections::BTreeMap;

/// What kind of name a token stands for. Tokens are numbered per class, so a report's
/// `tool-1` and `authority-1` are unrelated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Class {
    /// The harness's own session id. It names the machine and often the person, so no mode
    /// carries it.
    Trajectory,
    /// A person: an email address, an account id, a provider-qualified member.
    Reader,
    /// A key the model chose in a tool call. Tool parameters are not confined to
    /// policy-declared names — an open-parameter tool takes any object — so an argument *key*
    /// is the caller's data exactly as its value is.
    Argument,
    /// A content digest. It correlates a call with its offer inside one report, and outside
    /// one it is an unsalted hash of the very arguments and outputs the report promises not
    /// to carry: a recipient who guesses them can confirm the guess. A token keeps the
    /// correlation and removes the oracle.
    Digest,
    Authority,
    Tool,
    Effect,
    Sanitizer,
    Annotator,
    Mark,
    Group,
    Surface,
    Source,
    /// A selector as a source *answered* it, never as the policy templates it.
    ///
    /// A template like `group/<group-address>` is the deployment's; the instantiated
    /// `group/finance@corp.example` that reaches a fact is not. `includes($argument)` fills
    /// the placeholder from a tool call's argument, so the spelling is the model's, and no
    /// template ever reaches a fact for Baseline to spell instead.
    Selector,
    /// A trust rank's policy-given name. Ranks are numeric in a fact; only the chain names them.
    Trust,
    /// A custom identity implementation's name.
    Identity,
    /// A property name in a return contract's schema. The schema arrives in a remedy plan's
    /// arguments, so it is the agent's text, not the policy's.
    Field,
    /// A `const` or `enum` literal in that same agent-authored schema.
    Literal,
    /// An endpoint a binding points at. Not the deployment's vocabulary but its address: no
    /// mode spells one. The token still carries the one thing a reader needs — that two
    /// bindings reach the same service, or that they do not.
    Url,
}

impl Class {
    /// Whether this class is replaced in *both* modes.
    ///
    /// True for everything that is not the deployment's own vocabulary. Baseline offers the
    /// person one thing — the names their policy spells — and a session id, a colleague's
    /// email, a key the model invented and a digest of the arguments are none of them.
    fn always_tokenized(self) -> bool {
        match self {
            Class::Trajectory
            | Class::Reader
            | Class::Argument
            | Class::Digest
            | Class::Field
            | Class::Literal
            | Class::Selector
            | Class::Url => true,
            Class::Authority
            | Class::Tool
            | Class::Effect
            | Class::Sanitizer
            | Class::Annotator
            | Class::Mark
            | Class::Group
            | Class::Surface
            | Class::Source
            | Class::Trust
            | Class::Identity => false,
        }
    }

    /// Every class, so that one can be checked against all of them.
    const ALL: [Class; 19] = [
        Class::Trajectory,
        Class::Reader,
        Class::Argument,
        Class::Digest,
        Class::Authority,
        Class::Tool,
        Class::Effect,
        Class::Sanitizer,
        Class::Annotator,
        Class::Mark,
        Class::Group,
        Class::Surface,
        Class::Source,
        Class::Selector,
        Class::Trust,
        Class::Identity,
        Class::Field,
        Class::Literal,
        Class::Url,
    ];

    /// Whether a name is spelled the way this report spells its own tokens.
    ///
    /// Tokens and Baseline's spelled names share one namespace, so a deployment that writes a
    /// tool called `tool-1` would be indistinguishable in a Baseline report from the first
    /// tool the same report had to tokenize — one name reading as two, or two as one. Such a
    /// name is tokenized instead, which costs the reader that one spelling and keeps every
    /// token in the report meaning exactly one thing.
    fn spelled_like_a_token(raw: &str) -> bool {
        let Some((stem, ordinal)) = raw.rsplit_once('-') else {
            return false;
        };
        !ordinal.is_empty()
            && ordinal.chars().all(|digit| digit.is_ascii_digit())
            && Class::ALL.iter().any(|class| class.stem() == stem)
    }

    /// The stem a token of this class is spelled with.
    fn stem(self) -> &'static str {
        match self {
            Class::Trajectory => "trajectory",
            Class::Reader => "reader",
            Class::Argument => "argument",
            Class::Digest => "digest",
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
            Class::Url => "url",
        }
    }
}

/// One report's substitution table.
///
/// In [`Mode::Baseline`] this hands the deployment's own names back unchanged; the walk is
/// identical either way, so a baseline and a pseudonymized report differ in exactly one place
/// rather than following two code paths that can drift apart.
#[derive(Debug, Default)]
pub(crate) struct Tokens {
    assigned: BTreeMap<(Class, String), u32>,
    counts: BTreeMap<Class, u32>,
}

/// Whether policy-defined names are carried as spelled or replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// The deployment's own names as it spells them: tools, authorities, sanitizers, effects,
    /// trust ranks, groups. Everything [`Class::always_tokenized`] covers is replaced here
    /// too, so this mode still carries no session id, no person, no argument key and no
    /// content digest. The classification is the same in both modes; only the naming differs.
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
        if mode == Mode::Baseline && !class.always_tokenized() && !Class::spelled_like_a_token(raw) {
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

    /// Tokens and spelled names share one namespace, so a name shaped like a token cannot be
    /// carried as spelled: one name would read as two, or two as one.
    #[test]
    fn a_name_shaped_like_a_token_is_tokenized_even_in_baseline() {
        let mut tokens = Tokens::default();
        let collides = tokens.token(Mode::Baseline, Class::Tool, "tool-1");
        let invented = tokens.token(Mode::Baseline, Class::Tool, "Bash");
        assert_ne!(collides, invented);
        assert_eq!(invented, "Bash", "an ordinary name is still spelled");
        // Across classes too: the stem is what collides, not the class it was minted in.
        assert_ne!(tokens.token(Mode::Baseline, Class::Authority, "tool-1"), "tool-1");
        // A name that merely ends in a number is not a token.
        assert_eq!(tokens.token(Mode::Baseline, Class::Tool, "web-2"), "web-2");
    }

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
