//! Authorities, sanitizers, and casts — the declarations of who may cover what, and which
//! transforms produce new values.

use serde::{Deserialize, Serialize};

use crate::fact::EffectKind;
use crate::label::{Adequacy, Audience, Dim, Dimension, EstablishedLabel, Label, ReaderId, Trust};
use crate::names::{AuthorityName, CastName, MarkName, SanitizerName, TagName};

/// Operator prose on a registered authority or sanitizer: why this entry exists, in the
/// deployer's own words. It travels with every remedy plan naming the entity, so an agent chooses
/// among plans on stated purpose rather than on a bare name, and a reviewer reads the intent beside
/// the mandate. Advisory only: a hint NEVER enters a check, an enumeration, or an ordering, and it
/// widens no mandate. The load lint bounds its length ([`crate::registry::MAX_HINT_CHARS`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hint(String);

impl Hint {
    pub fn new(text: impl Into<String>) -> Self {
        Hint(text.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What an authority's ruling may cover. Each power names its currency; a mandate covering nothing
/// is a loud load error (the empty-remedy proof depends on it — see [`Mandate::is_empty`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mandate {
    pub trust_ceiling: Option<Trust>,
    pub reader_ceiling: Option<Audience>,
    pub waivers: Vec<EffectKind>,
    pub attends: Vec<MarkName>,
}

impl Mandate {
    pub fn is_empty(&self) -> bool {
        self.trust_ceiling.is_none()
            && self.reader_ceiling.is_none()
            && self.waivers.is_empty()
            && self.attends.is_empty()
    }
}

/// An authority's jurisdiction: the tags it covers. Empty = every call (small configs stay small).
/// Attention gaps ignore scope — they route by their own currency (the attended mark).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    pub tags: Vec<TagName>,
}

impl Scope {
    pub fn covers(&self, call_tags: &[TagName]) -> bool {
        self.tags.is_empty() || self.tags.iter().any(|t| call_tags.contains(t))
    }
}

/// An authority declaration: its name, what it may cover, and where. The implementation (inline fn
/// or external resolver) lives in the runtime, keyed by name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Authority {
    pub name: AuthorityName,
    pub mandate: Mandate,
    pub scope: Scope,
    pub hint: Option<Hint>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SanitizerPoints {
    pub input: bool,
    pub output: bool,
}

/// A sanitizer's declared transition: the one transition its mandate MAY claim, on one
/// dimension, as a `from` and a `to`. Trust and audience are bound on the same terms, and the enum
/// keeps a mandate claiming both dimensions at once unrepresentable. The `to` is fixed at
/// registration — a sanitizer does not decide its derivation label per value, as a
/// resolver-implemented [`Cast`] does — so the declared `to` is the transition ceiling. The
/// undeclared dimension is untouched: the derivation carries the raw value's label on it unchanged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Transition {
    Audience { from_includes: Audience, to: Audience },
    Trust { from_floor: Trust, to: Trust },
}

impl Transition {
    pub fn dimension(&self) -> Dimension {
        match self {
            Transition::Audience { .. } => Dimension::Audience,
            Transition::Trust { .. } => Dimension::Trust,
        }
    }

    /// Does the raw value satisfy the `from` precondition? An Unknown on the transitioned
    /// dimension is [`Adequacy::Unresolved`] — a sanitizer moves an established state, never an
    /// unestablished one.
    pub fn admits(&self, raw: &Label) -> Adequacy {
        match self {
            Transition::Audience { from_includes, .. } => raw.audience.covers(from_includes),
            Transition::Trust { from_floor, .. } => raw.trust.meets_floor(*from_floor),
        }
    }

    /// The derivation's label: the raw value's label with this dimension replaced by the declared
    /// `to`. The other dimension rides through untouched.
    pub fn derive(&self, raw: &Label) -> Label {
        match self {
            Transition::Audience { to, .. } => Label::new(raw.trust.clone(), Dim::Known(to.clone())),
            Transition::Trust { to, .. } => Label::new(Dim::Known(*to), raw.audience.clone()),
        }
    }
}

/// A registered sanitizer: one transition bound to its application points, plus the operator's own
/// account of what it is for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sanitizer {
    pub name: SanitizerName,
    pub on: SanitizerPoints,
    pub transition: Transition,
    pub hint: Option<Hint>,
}

/// The complete product ceiling a resolver-implemented cast may not exceed: the
/// admissible target trust ranks and a cap the resolved audience must stay within. A `public`
/// cap is the open gate that admits a Public resolution. An empty trust list means
/// the cast can never fill an unresolved trust dimension — it still serves sources whose
/// trust is already established, so only the catalogue-aware reachability pass
/// can prove a ceiling dead at load.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CastCeiling {
    pub trust: Vec<Trust>,
    pub audience: Audience,
}

impl CastCeiling {
    fn admits_unresolved(&self, answer: &EstablishedLabel, dim: Dimension) -> bool {
        match dim {
            Dimension::Trust => self.trust.contains(&answer.trust),
            Dimension::Audience => answer.audience.within(&self.audience),
        }
    }
}

/// How a cast resolves — constant XOR resolver, never both (unrepresentable here by construction).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CastResolution {
    Constant(EstablishedLabel),
    Resolver { may_cast: CastCeiling },
}

/// Why a complete cast answer is not admissible for its source. The one
/// validator both admission paths and replay consume — no caller re-derives these rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CastRefusal {
    NonLiteralReader,
    ConstantMismatch,
    EstablishedMismatch(Dimension),
    CeilingExceeded(Dimension),
}

impl CastResolution {
    /// Validate `answer` as the complete resolution of a source whose
    /// per-value label is `prior`. Every dimension `prior` already establishes must match
    /// exactly; every formerly unresolved dimension must clear the declared ceiling (resolver)
    /// or carry the declared constant, which admits only the constant label itself. A
    /// restricted resolved audience must hold literal reader IDs: never the reserved `public`
    /// spelling, never a group.
    pub fn validate(&self, prior: &Label, answer: &EstablishedLabel) -> Result<(), CastRefusal> {
        let literal_ids = match &answer.audience {
            Audience::Public => true,
            Audience::Restricted(readers) => readers.iter().all(ReaderId::is_literal),
        };
        if !literal_ids {
            return Err(CastRefusal::NonLiteralReader);
        }
        if let CastResolution::Constant(constant) = self
            && answer != constant
        {
            return Err(CastRefusal::ConstantMismatch);
        }
        for dim in [Dimension::Trust, Dimension::Audience] {
            let established = match dim {
                Dimension::Trust => matches!(prior.trust, Dim::Known(_)),
                Dimension::Audience => matches!(prior.audience, Dim::Known(_)),
            };
            if established {
                let matches_prior = match dim {
                    Dimension::Trust => matches!(prior.trust, Dim::Known(t) if t == answer.trust),
                    Dimension::Audience => {
                        matches!(&prior.audience, Dim::Known(a) if *a == answer.audience)
                    }
                };
                if !matches_prior {
                    return Err(CastRefusal::EstablishedMismatch(dim));
                }
                continue;
            }
            if let CastResolution::Resolver { may_cast } = self
                && !may_cast.admits_unresolved(answer, dim)
            {
                return Err(CastRefusal::CeilingExceeded(dim));
            }
        }
        Ok(())
    }
}

/// A registered cast that establishes an Unknown value's complete label. Tag scope and
/// registration-order request routing are `T05`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cast {
    pub name: CastName,
    pub resolution: CastResolution,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn readers(ids: &[&str]) -> Audience {
        Audience::restricted(ids.iter().copied().map(ReaderId::new))
    }

    fn resolver(trust: &[u8], cap: Audience) -> CastResolution {
        CastResolution::Resolver {
            may_cast: CastCeiling {
                trust: trust.iter().copied().map(Trust::new).collect(),
                audience: cap,
            },
        }
    }

    fn both_unknown() -> Label {
        Label::new(Dim::Unknown, Dim::Unknown)
    }

    fn answer(trust: u8, audience: Audience) -> EstablishedLabel {
        EstablishedLabel::new(Trust::new(trust), audience)
    }

    #[test]
    fn an_audience_ceiling_is_a_cap_not_an_enumeration() {
        let cast = resolver(&[0], readers(&["finance", "audit"]));
        assert_eq!(
            cast.validate(&both_unknown(), &answer(0, readers(&["finance"]))),
            Ok(())
        );
        assert_eq!(
            cast.validate(&both_unknown(), &answer(0, readers(&["finance", "audit"]))),
            Ok(())
        );
        assert_eq!(
            cast.validate(&both_unknown(), &answer(0, readers(&["finance", "stranger"]))),
            Err(CastRefusal::CeilingExceeded(Dimension::Audience))
        );
    }

    #[test]
    fn a_public_resolution_is_admitted_only_under_a_public_cap() {
        let wide = resolver(&[0], Audience::Public);
        assert_eq!(wide.validate(&both_unknown(), &answer(0, Audience::Public)), Ok(()));
        assert_eq!(wide.validate(&both_unknown(), &answer(0, readers(&["anyone"]))), Ok(()));
        let narrow = resolver(&[0], readers(&["finance"]));
        assert_eq!(
            narrow.validate(&both_unknown(), &answer(0, Audience::Public)),
            Err(CastRefusal::CeilingExceeded(Dimension::Audience))
        );
    }

    #[test]
    fn a_resolution_must_hold_literal_reader_ids() {
        let wide = resolver(&[0], Audience::Public);
        for bad in [readers(&["@hr"]), readers(&["public"]), readers(&["ap@corp", "@hr"])] {
            assert_eq!(
                wide.validate(&both_unknown(), &answer(0, bad)),
                Err(CastRefusal::NonLiteralReader)
            );
        }
        assert_eq!(
            wide.validate(&both_unknown(), &answer(0, readers(&["ap@corp"]))),
            Ok(())
        );
    }

    #[test]
    fn both_ceilings_bound_the_answer_independently() {
        let cast = resolver(&[1], readers(&["finance"]));
        assert_eq!(
            cast.validate(&both_unknown(), &answer(1, readers(&["finance"]))),
            Ok(())
        );
        assert_eq!(
            cast.validate(&both_unknown(), &answer(2, readers(&["finance"]))),
            Err(CastRefusal::CeilingExceeded(Dimension::Trust))
        );
        assert_eq!(
            cast.validate(&both_unknown(), &answer(1, readers(&["stranger"]))),
            Err(CastRefusal::CeilingExceeded(Dimension::Audience))
        );
    }

    #[test]
    fn an_established_dimension_must_match_exactly() {
        let cast = resolver(&[1], readers(&["finance"]));
        let trust_known = Label::new(Dim::Known(Trust::new(3)), Dim::Unknown);
        assert_eq!(cast.validate(&trust_known, &answer(3, readers(&["finance"]))), Ok(()));
        assert_eq!(
            cast.validate(&trust_known, &answer(1, readers(&["finance"]))),
            Err(CastRefusal::EstablishedMismatch(Dimension::Trust))
        );
        let audience_known = Label::new(Dim::Unknown, Dim::Known(Audience::Public));
        assert_eq!(cast.validate(&audience_known, &answer(1, Audience::Public)), Ok(()));
        assert_eq!(
            cast.validate(&audience_known, &answer(1, readers(&["finance"]))),
            Err(CastRefusal::EstablishedMismatch(Dimension::Audience))
        );
    }

    #[test]
    fn a_constant_admits_exactly_its_declared_label_where_it_agrees() {
        let constant = CastResolution::Constant(answer(0, Audience::Public));
        assert_eq!(constant.validate(&both_unknown(), &answer(0, Audience::Public)), Ok(()));
        assert_eq!(
            constant.validate(&both_unknown(), &answer(1, Audience::Public)),
            Err(CastRefusal::ConstantMismatch)
        );
        let agreeing = Label::new(Dim::Known(Trust::new(0)), Dim::Unknown);
        assert_eq!(constant.validate(&agreeing, &answer(0, Audience::Public)), Ok(()));
        let disagreeing = Label::new(Dim::Known(Trust::new(2)), Dim::Unknown);
        assert_eq!(
            constant.validate(&disagreeing, &answer(0, Audience::Public)),
            Err(CastRefusal::EstablishedMismatch(Dimension::Trust))
        );
    }
}
