//! Labels: the two-dimensional restrictive lattice APPA folds and checks.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// One dimension's value: established ([`Dim::Known`]) or not ([`Dim::Unknown`]).
///
/// `Unknown` is not a rank on any scale — `trusted < unknown < suspicious` does not exist. It
/// means the dimension has not been established yet. It is **absorbing** under the fold: folding
/// any value whose dimension is `Unknown` yields `Unknown`, so the fold never silently invents an
/// established value. The check layer turns a folded `Unknown` into an *unresolved* report naming
/// the offending values, to be filled in later by a registered cast.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Dim<T> {
    Known(T),
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Dimension {
    Trust,
    Audience,
}

/// A concrete state for one dimension — the target a cast resolves an Unknown to, or a resolved
/// override. Fills exactly one dimension (never both), preserving the known dimension and content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DimValue {
    Trust(Trust),
    Audience(Audience),
}

impl DimValue {
    pub fn dimension(&self) -> Dimension {
        match self {
            DimValue::Trust(_) => Dimension::Trust,
            DimValue::Audience(_) => Dimension::Audience,
        }
    }
}

/// Three-valued outcome of one adequacy test.
///
/// A requirement is *checked*, never folded. [`Adequacy::Unresolved`] is returned when the
/// trajectory side is [`Dim::Unknown`]: the check cannot decide until a cast resolves the
/// dimension. It is deliberately distinct from [`Adequacy::Fails`] — an unresolved dimension is
/// not a violation, it is a missing fact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Adequacy {
    Holds,
    Fails,
    Unresolved,
}

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
/// audiences are explicit id-lists: every audience that reaches the algebra is a concrete set, so
/// intersection/subset are exact. Named groups with resolver-backed membership (`john ∈ hr`
/// resolved fresh at every check) are design direction, not implemented (spec §Audience).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ReaderId(String);

impl ReaderId {
    pub fn new(id: impl Into<String>) -> Self {
        ReaderId(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
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

    fn includes(&self, recipients: &Audience) -> bool {
        match (self, recipients) {
            (Audience::Public, _) => true,
            (Audience::Restricted(_), Audience::Public) => false,
            (Audience::Restricted(a), Audience::Restricted(r)) => r.is_subset(a),
        }
    }

    fn within(&self, cap: &Audience) -> bool {
        match (self, cap) {
            (_, Audience::Public) => true,
            (Audience::Public, Audience::Restricted(_)) => false,
            (Audience::Restricted(a), Audience::Restricted(c)) => a.is_subset(c),
        }
    }
}

fn combine_dim<T>(a: &Dim<T>, b: &Dim<T>, f: impl Fn(&T, &T) -> T) -> Dim<T> {
    match (a, b) {
        (Dim::Known(x), Dim::Known(y)) => Dim::Known(f(x, y)),
        _ => Dim::Unknown,
    }
}

impl Dim<Trust> {
    /// Does the trajectory's trust meet `floor`? A tool requiring rank `floor` accepts anything at
    /// or above it; an unestablished dimension is [`Adequacy::Unresolved`].
    pub fn meets_floor(&self, floor: Trust) -> Adequacy {
        match self {
            Dim::Unknown => Adequacy::Unresolved,
            Dim::Known(t) => bool_adequacy(*t >= floor),
        }
    }
}

impl Dim<Audience> {
    pub fn covers(&self, recipients: &Audience) -> Adequacy {
        match self {
            Dim::Unknown => Adequacy::Unresolved,
            Dim::Known(a) => bool_adequacy(a.includes(recipients)),
        }
    }

    pub fn within_cap(&self, cap: &Audience) -> Adequacy {
        match self {
            Dim::Unknown => Adequacy::Unresolved,
            Dim::Known(a) => bool_adequacy(a.within(cap)),
        }
    }
}

fn bool_adequacy(holds: bool) -> Adequacy {
    if holds { Adequacy::Holds } else { Adequacy::Fails }
}

/// The full label: the product of the two dimensions.
///
/// [`Label::combine`] is the only way one label affects another, and it only ever narrows, so a
/// permissive delta — raising trust, adding readers — is unrepresentable by construction. Raising
/// a dimension is never a fold; it happens only on a *new* derived value through a registered cast
/// or authority (later slices).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Label {
    pub trust: Dim<Trust>,
    pub audience: Dim<Audience>,
}

impl Label {
    pub fn new(trust: Dim<Trust>, audience: Dim<Audience>) -> Self {
        Label { trust, audience }
    }

    /// The fold identity: maximally permissive (top trust, public audience) — the label of a
    /// trajectory before any value is folded in. `top().combine(x) == x` for every established
    /// `x`. Top trust is `u8::MAX`, an upper bound on any configured chain rank, so it clears
    /// every floor; audience `Public` includes every recipient.
    pub fn top() -> Label {
        Label::new(Dim::Known(Trust::new(u8::MAX)), Dim::Known(Audience::Public))
    }

    /// Restrictive fold: minimum trust, intersect audience. Commutative, associative, idempotent;
    /// `Unknown` absorbs in either dimension.
    pub fn combine(&self, other: &Label) -> Label {
        Label {
            trust: combine_dim(&self.trust, &other.trust, |a, b| a.combine(*b)),
            audience: combine_dim(&self.audience, &other.audience, Audience::combine),
        }
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

    fn dim_strategy<T: std::fmt::Debug + Clone>(inner: impl Strategy<Value = T>) -> impl Strategy<Value = Dim<T>> {
        prop_oneof![inner.prop_map(Dim::Known), Just(Dim::Unknown)]
    }

    fn label_strategy() -> impl Strategy<Value = Label> {
        (dim_strategy(trust_strategy()), dim_strategy(audience_strategy()))
            .prop_map(|(trust, audience)| Label::new(trust, audience))
    }

    proptest! {
        #[test]
        fn combine_is_commutative(a in label_strategy(), b in label_strategy()) {
            prop_assert_eq!(a.combine(&b), b.combine(&a));
        }

        #[test]
        fn combine_is_associative(a in label_strategy(), b in label_strategy(), c in label_strategy()) {
            prop_assert_eq!(a.combine(&b).combine(&c), a.combine(&b.combine(&c)));
        }

        #[test]
        fn combine_is_idempotent(a in label_strategy()) {
            prop_assert_eq!(a.combine(&a), a.clone());
        }

        #[test]
        fn combine_never_widens(a in label_strategy(), b in label_strategy()) {
            let folded = a.combine(&b);
            if let (Dim::Known(ft), Dim::Known(at), Dim::Known(bt)) =
                (&folded.trust, &a.trust, &b.trust)
            {
                prop_assert!(ft <= at && ft <= bt);
            }
            if let (Dim::Known(fa), Dim::Known(aa), Dim::Known(ba)) =
                (&folded.audience, &a.audience, &b.audience)
            {
                prop_assert_eq!(fa.within(aa), true);
                prop_assert_eq!(fa.within(ba), true);
            }
        }

        #[test]
        fn top_is_identity(a in label_strategy()) {
            prop_assert_eq!(Label::top().combine(&a), a.clone());
        }

        #[test]
        fn unknown_absorbs(a in label_strategy()) {
            let all_unknown = Label::new(Dim::Unknown, Dim::Unknown);
            let folded = a.combine(&all_unknown);
            prop_assert_eq!(folded.trust, Dim::Unknown);
            prop_assert_eq!(folded.audience, Dim::Unknown);
        }
    }

    #[test]
    fn floor_holds_at_or_above() {
        let floor = Trust::new(2);
        assert_eq!(Dim::Known(Trust::new(2)).meets_floor(floor), Adequacy::Holds);
        assert_eq!(Dim::Known(Trust::new(3)).meets_floor(floor), Adequacy::Holds);
        assert_eq!(Dim::Known(Trust::new(1)).meets_floor(floor), Adequacy::Fails);
        assert_eq!(Dim::<Trust>::Unknown.meets_floor(floor), Adequacy::Unresolved);
    }

    #[test]
    fn includes_and_cap_relations() {
        let internal = Audience::restricted([ReaderId::new("a"), ReaderId::new("b")]);
        let just_a = Audience::restricted([ReaderId::new("a")]);

        assert_eq!(Dim::Known(internal.clone()).covers(&just_a), Adequacy::Holds);
        assert_eq!(Dim::Known(just_a.clone()).covers(&internal), Adequacy::Fails);
        assert_eq!(Dim::Known(Audience::Public).covers(&internal), Adequacy::Holds);
        assert_eq!(Dim::Known(internal.clone()).covers(&Audience::Public), Adequacy::Fails);
        assert_eq!(Dim::Known(just_a).within_cap(&internal), Adequacy::Holds);
        assert_eq!(Dim::Known(internal).within_cap(&Audience::Public), Adequacy::Holds);
        assert_eq!(Dim::<Audience>::Unknown.covers(&Audience::Public), Adequacy::Unresolved);
    }

    #[test]
    fn intersect_shrinks_readers() {
        let ab = Audience::restricted([ReaderId::new("a"), ReaderId::new("b")]);
        let bc = Audience::restricted([ReaderId::new("b"), ReaderId::new("c")]);
        let folded = Label::new(Dim::Known(Trust::new(1)), Dim::Known(ab))
            .combine(&Label::new(Dim::Known(Trust::new(1)), Dim::Known(bc)));
        assert_eq!(folded.audience, Dim::Known(Audience::restricted([ReaderId::new("b")])));
    }
}
