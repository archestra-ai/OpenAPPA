//! Labels: the two-dimensional restrictive lattice APPA folds and checks.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// A rank in the deployment's finite trust chain, held as an index: higher rank = more trusted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Trust(u8);

impl Trust {
    pub const fn new(rank: u8) -> Self {
        Trust(rank)
    }

    pub const fn rank(self) -> u8 {
        self.0
    }

    fn combine(self, other: Self) -> Self {
        Trust(self.0.min(other.0))
    }
}

/// A symbolic reader identity — an opaque atom to the pure algebra. The current dialect's
/// restricted audiences are explicit id-lists (`public` is the one non-list state), so
/// intersection/subset are exact. Two spellings are reserved — `public` and a leading `@` —
/// which the algebra must never hold as a reader: the first is a label state, and a group is
/// expanded to literal IDs by the membership resolver before a reader set is built. The
/// constructor cannot enforce that, so the rule is [`is_literal`](ReaderId::is_literal),
/// applied on the ingresses that carry it: registry declarations at load, annotation answers,
/// and membership expansions. A `$recipient` argument never reaches the constructor with either
/// spelling: the check reads `public` and `@group` as what they are first.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ReaderId(String);

impl ReaderId {
    pub fn new(id: impl Into<String>) -> Self {
        ReaderId(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// A literal reader ID: `public` is a reserved label state — the whole audience — never a
    /// reader, and the `@` mark is reserved for group names, which only a membership resolver
    /// may expand. Every ingress that builds a reader set applies this rule: a declared
    /// audience at load, an annotation answer, and a membership expansion.
    pub fn is_literal(&self) -> bool {
        self.0 != "public" && !self.0.starts_with('@')
    }
}

/// A reader set: the whole universe ([`Audience::Public`]) or a concrete [`Audience::Restricted`]
/// set. Reading restricted data shrinks the set (intersection); it never grows. A tool's
/// requirement constrains it from either side — an `includes` ([`Audience::includes`]) or a cap
/// ([`Audience::within`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Audience {
    Public,
    Restricted(BTreeSet<ReaderId>),
}

impl Audience {
    pub fn restricted(readers: impl IntoIterator<Item = ReaderId>) -> Self {
        Audience::Restricted(readers.into_iter().collect())
    }

    fn combine(&self, other: &Self) -> Self {
        match (self, other) {
            (Audience::Public, x) | (x, Audience::Public) => x.clone(),
            (Audience::Restricted(a), Audience::Restricted(b)) => {
                Audience::Restricted(a.intersection(b).cloned().collect())
            }
        }
    }

    /// `self ⊇ recipients` — the trajectory's readers include every named recipient.
    /// Crate-visible because mandate-power comparison asks it of a substitution's
    /// declared `to`, which is a plain audience rather than one value's dimension.
    pub(crate) fn includes(&self, recipients: &Audience) -> bool {
        match (self, recipients) {
            (Audience::Public, _) => true,
            (Audience::Restricted(_), Audience::Public) => false,
            (Audience::Restricted(a), Audience::Restricted(r)) => r.is_subset(a),
        }
    }

    /// `self ⊆ cap` — the trajectory's readers stay within the tool's declared cap. Crate-visible
    /// because mandate-power comparison orders reader ceilings by this same inclusion.
    pub(crate) fn within(&self, cap: &Audience) -> bool {
        match (self, cap) {
            (_, Audience::Public) => true,
            (Audience::Public, Audience::Restricted(_)) => false,
            (Audience::Restricted(a), Audience::Restricted(c)) => a.is_subset(c),
        }
    }
}

/// The one label: both dimensions concrete, always. Every admitted value carries exactly one of
/// these, every trajectory fold is one, and every check reads one — no unestablished state is
/// representable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Label {
    pub trust: Trust,
    pub audience: Audience,
}

impl Label {
    pub fn new(trust: Trust, audience: Audience) -> Self {
        Label { trust, audience }
    }

    /// The fold identity: maximally permissive (top trust, public audience). Top trust is
    /// `u8::MAX`, an upper bound on any configured chain rank, so it clears every floor;
    /// audience `Public` includes every recipient.
    pub fn top() -> Self {
        Label::new(Trust::new(u8::MAX), Audience::Public)
    }

    /// The maximally restrictive label: the lowest rank and no readers at all. The fail-closed
    /// reading of a basis source whose record the log does not hold — folding it narrows the
    /// trajectory to the floor, and can never widen anything.
    pub(crate) const fn bottom() -> Self {
        Label {
            trust: Trust::new(0),
            audience: Audience::Restricted(BTreeSet::new()),
        }
    }

    /// The restrictive meet: minimum trust, intersect audience. Commutative, associative,
    /// idempotent, and it never widens either dimension.
    pub fn combine(&self, other: &Label) -> Label {
        Label {
            trust: self.trust.combine(other.trust),
            audience: self.audience.combine(&other.audience),
        }
    }

    /// Fold one contribution in place — the meet, assigned.
    pub(crate) fn fold(&mut self, other: &Label) {
        *self = self.combine(other);
    }

    /// Does this label's trust meet `floor`? Two-valued: a label is always concrete, so the
    /// check holds or fails and nothing else.
    pub fn meets_floor(&self, floor: Trust) -> bool {
        self.trust >= floor
    }

    /// Do this label's readers include every named recipient?
    pub fn covers(&self, recipients: &Audience) -> bool {
        self.audience.includes(recipients)
    }

    /// Do this label's readers stay within `cap`?
    pub fn within_cap(&self, cap: &Audience) -> bool {
        self.audience.within(cap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn trust_strategy() -> impl Strategy<Value = Trust> {
        (0u8..4).prop_map(Trust::new)
    }

    fn audience_strategy() -> impl Strategy<Value = Audience> {
        let readers =
            prop::collection::btree_set((b'a'..=b'e').prop_map(|c| ReaderId::new((c as char).to_string())), 0..5);
        prop_oneof![Just(Audience::Public), readers.prop_map(Audience::Restricted),]
    }

    fn label_strategy() -> impl Strategy<Value = Label> {
        (trust_strategy(), audience_strategy()).prop_map(|(t, a)| Label::new(t, a))
    }

    proptest! {
        #[test]
        fn combine_is_commutative(a in label_strategy(), b in label_strategy()) {
            prop_assert_eq!(a.combine(&b), b.combine(&a));
        }

        #[test]
        fn combine_is_associative(
            a in label_strategy(),
            b in label_strategy(),
            c in label_strategy(),
        ) {
            prop_assert_eq!(a.combine(&b).combine(&c), a.combine(&b.combine(&c)));
        }

        #[test]
        fn combine_is_idempotent(a in label_strategy()) {
            prop_assert_eq!(a.combine(&a), a.clone());
        }

        #[test]
        fn top_is_identity(a in label_strategy()) {
            let identity = Label::top();
            prop_assert_eq!(identity.combine(&a), a.clone());
            prop_assert_eq!(a.combine(&identity), a.clone());
        }

        #[test]
        fn combine_never_widens(a in label_strategy(), b in label_strategy()) {
            let folded = a.combine(&b);
            prop_assert!(folded.trust <= a.trust);
            prop_assert!(folded.trust <= b.trust);
            prop_assert!(folded.audience.within(&a.audience));
            prop_assert!(folded.audience.within(&b.audience));
        }

        /// The check is two-valued: on any folded label, each requirement test answers exactly
        /// the plain lattice comparison — a bool, with no third outcome representable.
        #[test]
        fn check_is_two_valued(
            start in label_strategy(),
            values in prop::collection::vec(label_strategy(), 0..6),
            floor in trust_strategy(),
            recipients in audience_strategy(),
            cap in audience_strategy(),
        ) {
            let mut fold = start;
            for value in &values {
                fold.fold(value);
            }
            prop_assert_eq!(fold.meets_floor(floor), fold.trust >= floor);
            prop_assert_eq!(fold.covers(&recipients), fold.audience.includes(&recipients));
            prop_assert_eq!(fold.within_cap(&cap), fold.audience.within(&cap));
        }

        /// Every admitted value contributes one concrete label, so the trajectory fold is the
        /// plain combine reduction — order-independent, one label, nothing pending alongside it.
        #[test]
        fn every_admitted_value_has_one_concrete_label(
            start in label_strategy(),
            values in prop::collection::vec(label_strategy(), 0..6),
        ) {
            let mut forward = start.clone();
            for value in &values {
                forward.fold(value);
            }
            let reduced = values.iter().fold(start.clone(), |fold, value| fold.combine(value));
            prop_assert_eq!(&forward, &reduced);
            let mut reversed = start;
            for value in values.iter().rev() {
                reversed.fold(value);
            }
            prop_assert_eq!(forward, reversed);
        }
    }

    #[test]
    fn floor_holds_at_or_above() {
        let floor = Trust::new(2);
        let at = Label::new(Trust::new(2), Audience::Public);
        let above = Label::new(Trust::new(3), Audience::Public);
        let below = Label::new(Trust::new(1), Audience::Public);
        assert!(at.meets_floor(floor));
        assert!(above.meets_floor(floor));
        assert!(!below.meets_floor(floor));
    }

    #[test]
    fn includes_and_cap_relations() {
        let internal = Audience::restricted([ReaderId::new("a"), ReaderId::new("b")]);
        let just_a = Audience::restricted([ReaderId::new("a")]);
        let with = |audience: Audience| Label::new(Trust::new(1), audience);

        assert!(with(internal.clone()).covers(&just_a));
        assert!(!with(just_a.clone()).covers(&internal));
        assert!(with(Audience::Public).covers(&internal));
        assert!(!with(internal.clone()).covers(&Audience::Public));
        assert!(with(just_a).within_cap(&internal));
        assert!(with(internal).within_cap(&Audience::Public));
    }

    #[test]
    fn a_label_round_trips_through_serde_verbatim() {
        let label = Label::new(
            Trust::new(1),
            Audience::restricted([ReaderId::new("internal"), ReaderId::new("audit")]),
        );
        let bytes = serde_json::to_string(&label).expect("a label serializes");
        let back: Label = serde_json::from_str(&bytes).expect("and deserializes");
        assert_eq!(back, label);
        assert_eq!(serde_json::to_string(&back).unwrap(), bytes);
    }

    #[test]
    fn intersect_shrinks_readers() {
        let ab = Audience::restricted([ReaderId::new("a"), ReaderId::new("b")]);
        let bc = Audience::restricted([ReaderId::new("b"), ReaderId::new("c")]);
        let mut label = Label::new(Trust::new(1), ab);
        label.fold(&Label::new(Trust::new(1), bc));
        assert_eq!(label.audience, Audience::restricted([ReaderId::new("b")]));
    }

    #[test]
    fn bottom_folds_fail_closed() {
        let held = Label::new(Trust::new(3), Audience::Public);
        let folded = held.combine(&Label::bottom());
        assert_eq!(folded.trust, Trust::new(0));
        assert_eq!(folded.audience, Audience::restricted([]));
        assert!(!folded.meets_floor(Trust::new(1)));
        assert!(!folded.covers(&Audience::restricted([ReaderId::new("a")])));
    }
}
