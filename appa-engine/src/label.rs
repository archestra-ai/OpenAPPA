//! Labels: the two-dimensional restrictive lattice APPA folds and checks.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::value::ValueId;

/// One dimension of one value's contribution: established ([`Dim::Known`]) or not
/// ([`Dim::Unknown`]).
///
/// `Unknown` is not a rank on any scale — `trusted < unknown < suspicious` does not exist. It
/// means this source's contribution has not been established yet. Under the fold it is identity
/// for the established bound and absorbing only for the source's identity, which joins the
/// dimension's unresolved set until a registered cast establishes the whole source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Dim<T> {
    Known(T),
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Dimension {
    Trust,
    Audience,
}

/// Three-valued outcome of one adequacy test.
///
/// A requirement is *checked*, never folded. [`Adequacy::Unresolved`] is returned when the
/// consumed dimension still has unresolved sources: the check cannot decide until a cast
/// resolves them. It is deliberately distinct from [`Adequacy::Fails`] — an unresolved
/// dimension is not a violation, it is a missing fact.
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
/// restricted audiences are explicit id-lists (`public` is the one non-list state), so
/// intersection/subset are exact. Three spellings are reserved — `public`, `unknown`, and a
/// leading `@` — which the algebra must never hold as a reader: the first two are label states,
/// and a group is expanded to literal IDs by the membership resolver before a reader set is
/// built. The constructor cannot enforce that, so the
/// rule is [`is_literal`](ReaderId::is_literal), applied on the ingresses that carry it:
/// registry declarations at load, cast answers against their ceiling, dynamic resolver answers,
/// and membership expansions. A `$recipient` argument never reaches the constructor with either
/// spelling: the check reads `public` and `@group` as what they are first.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ReaderId(String);

/// The spelling of the unestablished state on both dimensions. Reserved: it names neither a
/// trust rank nor a reader.
pub const UNKNOWN_STATE: &str = "unknown";

impl ReaderId {
    pub fn new(id: impl Into<String>) -> Self {
        ReaderId(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// A literal reader ID: `public` and `unknown` are reserved label states — the whole
    /// audience, and a contribution not yet established — never readers, and the `@` mark is
    /// reserved for group names, which only a membership resolver may expand. Every ingress
    /// that builds a reader set applies this rule: a declared audience at load, a cast answer
    /// against its ceiling, a dynamic resolver answer, and a membership expansion.
    pub fn is_literal(&self) -> bool {
        self.0 != "public" && self.0 != UNKNOWN_STATE && !self.0.starts_with('@')
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

fn bool_adequacy(holds: bool) -> Adequacy {
    if holds { Adequacy::Holds } else { Adequacy::Fails }
}

impl Dim<Trust> {
    /// Does this one value's trust meet `floor`? An unestablished contribution is
    /// [`Adequacy::Unresolved`]. Per-value checks (a sanitizer's `from`, a cast's exact-match)
    /// use this; trajectory-side checks go through [`PartialLabel`].
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

/// One value's contribution: the product of the two dimensions, each possibly `Unknown`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Label {
    pub trust: Dim<Trust>,
    pub audience: Dim<Audience>,
}

impl Label {
    pub fn new(trust: Dim<Trust>, audience: Dim<Audience>) -> Self {
        Label { trust, audience }
    }

    /// The identity contribution: maximally permissive (top trust, public audience). Folding it
    /// into any [`PartialLabel`] changes nothing. Top trust is `u8::MAX`, an upper bound on any
    /// configured chain rank, so it clears every floor; audience `Public` includes every
    /// recipient.
    pub fn top() -> Label {
        EstablishedLabel::top().into_label()
    }

    /// The wholly unestablished contribution: neither dimension established. Folding it moves no
    /// bound and records its source in both unresolved sets — the fail-closed reading of
    /// a basis source whose record the log does not hold.
    pub(crate) const fn unknown() -> Label {
        Label {
            trust: Dim::Unknown,
            audience: Dim::Unknown,
        }
    }

    /// This contribution as a meet operand on established bounds: a known dimension carries its
    /// value, an unknown one the meet identity (unknown is identity for the bound; its
    /// source identity is tracked separately by the fold).
    pub fn established_part(&self) -> EstablishedLabel {
        EstablishedLabel::new(
            match &self.trust {
                Dim::Known(t) => *t,
                Dim::Unknown => Trust::new(u8::MAX),
            },
            match &self.audience {
                Dim::Known(a) => a.clone(),
                Dim::Unknown => Audience::Public,
            },
        )
    }
}

/// A fully established label: both dimensions concrete, no `Unknown` representable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EstablishedLabel {
    pub trust: Trust,
    pub audience: Audience,
}

impl EstablishedLabel {
    pub fn new(trust: Trust, audience: Audience) -> Self {
        EstablishedLabel { trust, audience }
    }

    pub fn top() -> Self {
        EstablishedLabel::new(Trust::new(u8::MAX), Audience::Public)
    }

    /// The restrictive meet: minimum trust, intersect audience. Commutative, associative,
    /// idempotent, and it never widens either dimension.
    pub fn combine(&self, other: &EstablishedLabel) -> EstablishedLabel {
        EstablishedLabel {
            trust: self.trust.combine(other.trust),
            audience: self.audience.combine(&other.audience),
        }
    }

    pub fn into_label(self) -> Label {
        Label::new(Dim::Known(self.trust), Dim::Known(self.audience))
    }

    /// The established dimensions of `label`, when both are. A value whose label has any
    /// `Unknown` dimension has no established form.
    pub fn from_label(label: &Label) -> Option<EstablishedLabel> {
        match (&label.trust, &label.audience) {
            (Dim::Known(t), Dim::Known(a)) => Some(EstablishedLabel::new(*t, a.clone())),
            _ => None,
        }
    }
}

/// The trajectory projection's partial label: per dimension, the established bound
/// folded from every known contribution plus the set of source values whose contribution is
/// still `Unknown`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialLabel {
    bound: EstablishedLabel,
    unresolved_trust: BTreeSet<ValueId>,
    unresolved_audience: BTreeSet<ValueId>,
}

impl PartialLabel {
    /// A fully established state: `bound` with no unresolved sources. The monoid identity is
    /// `established(EstablishedLabel::top())`.
    pub fn established(bound: EstablishedLabel) -> Self {
        PartialLabel {
            bound,
            unresolved_trust: BTreeSet::new(),
            unresolved_audience: BTreeSet::new(),
        }
    }

    /// Fold a whole basis: the established base plus one contribution per source. The
    /// one derivation a trajectory's label and a fork snapshot's seed both go through, so a
    /// branch fold and the snapshot frozen from it cannot disagree.
    pub(crate) fn from_basis<'a>(
        base: EstablishedLabel,
        sources: impl IntoIterator<Item = (ValueId, &'a Label)>,
    ) -> Self {
        let mut fold = PartialLabel::established(base);
        for (source, label) in sources {
            fold.fold_value(source, label);
        }
        fold
    }

    /// Fold one admitted value's contribution: a known dimension meets into the
    /// bound; an unknown dimension records the source in that dimension's unresolved set.
    pub fn fold_value(&mut self, source: ValueId, label: &Label) {
        match &label.trust {
            Dim::Known(t) => self.bound.trust = self.bound.trust.combine(*t),
            Dim::Unknown => {
                self.unresolved_trust.insert(source);
            }
        }
        match &label.audience {
            Dim::Known(a) => self.bound.audience = self.bound.audience.combine(a),
            Dim::Unknown => {
                self.unresolved_audience.insert(source);
            }
        }
    }

    /// Narrow the established bound by `by`, leaving the unresolved sets untouched — the
    /// committed-label clock: a delta narrows what is known and adds no source.
    pub fn narrow_bound(&mut self, by: &EstablishedLabel) {
        self.bound = self.bound.combine(by);
    }

    pub fn combine(&self, other: &PartialLabel) -> PartialLabel {
        PartialLabel {
            bound: self.bound.combine(&other.bound),
            unresolved_trust: self.unresolved_trust.union(&other.unresolved_trust).copied().collect(),
            unresolved_audience: self
                .unresolved_audience
                .union(&other.unresolved_audience)
                .copied()
                .collect(),
        }
    }

    /// The established bound: every known restriction, readable even while sources
    /// stay unresolved. The narrowing check and sanitizer residual comparisons read
    /// exactly this.
    pub fn bound(&self) -> &EstablishedLabel {
        &self.bound
    }

    pub fn unresolved(&self, dim: Dimension) -> impl Iterator<Item = ValueId> + '_ {
        match dim {
            Dimension::Trust => self.unresolved_trust.iter().copied(),
            Dimension::Audience => self.unresolved_audience.iter().copied(),
        }
    }

    /// Is `source` still unresolved on `dim`? Set membership, for per-source reporting
    /// without rescanning the whole unresolved set.
    pub fn is_unresolved(&self, dim: Dimension, source: ValueId) -> bool {
        match dim {
            Dimension::Trust => self.unresolved_trust.contains(&source),
            Dimension::Audience => self.unresolved_audience.contains(&source),
        }
    }

    pub fn is_established(&self, dim: Dimension) -> bool {
        match dim {
            Dimension::Trust => self.unresolved_trust.is_empty(),
            Dimension::Audience => self.unresolved_audience.is_empty(),
        }
    }

    pub fn is_fully_established(&self) -> bool {
        self.unresolved_trust.is_empty() && self.unresolved_audience.is_empty()
    }

    /// Does the trajectory's trust meet `floor`? Consuming an unresolved dimension is
    /// [`Adequacy::Unresolved`]; otherwise the bound decides.
    pub fn meets_floor(&self, floor: Trust) -> Adequacy {
        if !self.unresolved_trust.is_empty() {
            return Adequacy::Unresolved;
        }
        bool_adequacy(self.bound.trust >= floor)
    }

    pub fn covers(&self, recipients: &Audience) -> Adequacy {
        if !self.unresolved_audience.is_empty() {
            return Adequacy::Unresolved;
        }
        bool_adequacy(self.bound.audience.includes(recipients))
    }

    pub fn within_cap(&self, cap: &Audience) -> Adequacy {
        if !self.unresolved_audience.is_empty() {
            return Adequacy::Unresolved;
        }
        bool_adequacy(self.bound.audience.within(cap))
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

    fn established_strategy() -> impl Strategy<Value = EstablishedLabel> {
        (trust_strategy(), audience_strategy()).prop_map(|(t, a)| EstablishedLabel::new(t, a))
    }

    fn partial_strategy() -> impl Strategy<Value = PartialLabel> {
        (established_strategy(), prop::collection::vec(label_strategy(), 0..6)).prop_map(|(start, values)| {
            let mut partial = PartialLabel::established(start);
            for (i, label) in values.iter().enumerate() {
                partial.fold_value(ValueId::new(i as u64), label);
            }
            partial
        })
    }

    proptest! {
        #[test]
        fn combine_is_commutative(a in partial_strategy(), b in partial_strategy()) {
            prop_assert_eq!(a.combine(&b), b.combine(&a));
        }

        #[test]
        fn combine_is_associative(a in partial_strategy(), b in partial_strategy(), c in partial_strategy()) {
            prop_assert_eq!(a.combine(&b).combine(&c), a.combine(&b.combine(&c)));
        }

        #[test]
        fn combine_is_idempotent(a in partial_strategy()) {
            prop_assert_eq!(a.combine(&a), a.clone());
        }

        #[test]
        fn established_top_is_identity(a in partial_strategy()) {
            let identity = PartialLabel::established(EstablishedLabel::top());
            prop_assert_eq!(identity.combine(&a), a.clone());
            prop_assert_eq!(a.combine(&identity), a.clone());
        }

        #[test]
        fn combine_never_widens(a in partial_strategy(), b in partial_strategy()) {
            let folded = a.combine(&b);
            prop_assert!(folded.bound().trust <= a.bound().trust);
            prop_assert!(folded.bound().trust <= b.bound().trust);
            prop_assert!(folded.bound().audience.within(&a.bound().audience));
            prop_assert!(folded.bound().audience.within(&b.bound().audience));
        }

        #[test]
        fn fold_keeps_known_restrictions_under_unknown(a in partial_strategy(), v in label_strategy()) {
            let before = a.clone();
            let source = ValueId::new(1000);
            let mut after = a;
            after.fold_value(source, &v);

            prop_assert!(after.bound().trust <= before.bound().trust);
            prop_assert!(after.bound().audience.within(&before.bound().audience));
            match &v.trust {
                Dim::Unknown => {
                    prop_assert_eq!(after.bound().trust, before.bound().trust);
                    prop_assert!(after.unresolved(Dimension::Trust).any(|id| id == source));
                }
                Dim::Known(_) => {
                    prop_assert!(!after.unresolved(Dimension::Trust).any(|id| id == source));
                }
            }
            match &v.audience {
                Dim::Unknown => {
                    prop_assert_eq!(&after.bound().audience, &before.bound().audience);
                    prop_assert!(after.unresolved(Dimension::Audience).any(|id| id == source));
                }
                Dim::Known(_) => {
                    prop_assert!(!after.unresolved(Dimension::Audience).any(|id| id == source));
                }
            }
        }

        #[test]
        fn fully_established_start_decides_every_test(start in established_strategy()) {
            let partial = PartialLabel::established(start.clone());
            prop_assert!(partial.is_fully_established());
            // The rank above the start is out of reach; `Adequacy::Unresolved` is what a
            // consumed dimension would answer, so both arms below prove the start decides.
            prop_assert_eq!(partial.meets_floor(start.trust), Adequacy::Holds);
            prop_assert_eq!(partial.meets_floor(Trust::new(start.trust.rank() + 1)), Adequacy::Fails);
            // A cap naming only a reader the alphabet never generates admits exactly the
            // empty restricted audience, so the expectation is decided by the start alone.
            let outside = Audience::restricted([ReaderId::new("z".to_string())]);
            let admitted = match &start.audience {
                Audience::Restricted(readers) if readers.is_empty() => Adequacy::Holds,
                Audience::Public | Audience::Restricted(_) => Adequacy::Fails,
            };
            prop_assert_eq!(partial.within_cap(&outside), admitted);
            // Every established audience is within a Public cap, the empty restricted one
            // and Public itself included; only a generated bound reaches those two.
            prop_assert_eq!(partial.within_cap(&Audience::Public), Adequacy::Holds);
        }
    }

    #[test]
    fn floor_holds_at_or_above() {
        let floor = Trust::new(2);
        let at = PartialLabel::established(EstablishedLabel::new(Trust::new(2), Audience::Public));
        let above = PartialLabel::established(EstablishedLabel::new(Trust::new(3), Audience::Public));
        let below = PartialLabel::established(EstablishedLabel::new(Trust::new(1), Audience::Public));
        assert_eq!(at.meets_floor(floor), Adequacy::Holds);
        assert_eq!(above.meets_floor(floor), Adequacy::Holds);
        assert_eq!(below.meets_floor(floor), Adequacy::Fails);

        let mut unresolved = above;
        unresolved.fold_value(ValueId::new(0), &Label::new(Dim::Unknown, Dim::Known(Audience::Public)));
        assert_eq!(unresolved.meets_floor(floor), Adequacy::Unresolved);
    }

    #[test]
    fn includes_and_cap_relations() {
        let internal = Audience::restricted([ReaderId::new("a"), ReaderId::new("b")]);
        let just_a = Audience::restricted([ReaderId::new("a")]);
        let with = |audience: Audience| PartialLabel::established(EstablishedLabel::new(Trust::new(1), audience));

        assert_eq!(with(internal.clone()).covers(&just_a), Adequacy::Holds);
        assert_eq!(with(just_a.clone()).covers(&internal), Adequacy::Fails);
        assert_eq!(with(Audience::Public).covers(&internal), Adequacy::Holds);
        assert_eq!(with(internal.clone()).covers(&Audience::Public), Adequacy::Fails);
        assert_eq!(with(just_a).within_cap(&internal), Adequacy::Holds);
        assert_eq!(with(internal.clone()).within_cap(&Audience::Public), Adequacy::Holds);

        let mut unresolved = with(internal);
        unresolved.fold_value(ValueId::new(0), &Label::new(Dim::Known(Trust::new(1)), Dim::Unknown));
        assert_eq!(unresolved.covers(&Audience::Public), Adequacy::Unresolved);
        assert_eq!(unresolved.within_cap(&Audience::Public), Adequacy::Unresolved);
        assert_eq!(unresolved.meets_floor(Trust::new(1)), Adequacy::Holds);
    }

    #[test]
    fn a_partial_label_round_trips_through_serde_verbatim() {
        let mut fold = PartialLabel::established(EstablishedLabel::new(
            Trust::new(1),
            Audience::restricted([ReaderId::new("internal"), ReaderId::new("audit")]),
        ));
        fold.fold_value(ValueId::new(3), &Label::new(Dim::Unknown, Dim::Known(Audience::Public)));
        fold.fold_value(ValueId::new(7), &Label::new(Dim::Known(Trust::new(0)), Dim::Unknown));

        let bytes = serde_json::to_string(&fold).expect("a partial label serializes");
        let back: PartialLabel = serde_json::from_str(&bytes).expect("and deserializes");
        assert_eq!(back, fold);
        assert_eq!(serde_json::to_string(&back).unwrap(), bytes);
    }

    #[test]
    fn intersect_shrinks_readers() {
        let ab = Audience::restricted([ReaderId::new("a"), ReaderId::new("b")]);
        let bc = Audience::restricted([ReaderId::new("b"), ReaderId::new("c")]);
        let mut partial = PartialLabel::established(EstablishedLabel::new(Trust::new(1), ab));
        partial.fold_value(ValueId::new(0), &Label::new(Dim::Known(Trust::new(1)), Dim::Known(bc)));
        assert_eq!(partial.bound().audience, Audience::restricted([ReaderId::new("b")]));
    }
}
