//! Authorities, sanitizers, and casts — the declarations of who may cover what, and which
//! transforms produce new values.

use serde::{Deserialize, Serialize};

use crate::fact::EffectKind;
use crate::label::{Adequacy, Audience, Dim, DimValue, Dimension, Label, Trust};
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

pub type CastTarget = DimValue;

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

/// The ceiling a resolver-implemented cast may not exceed: the admissible target states per
/// dimension. At least one dimension must be listed (a resolver that may cast to nothing is inert).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CastCeiling {
    pub trust: Vec<Trust>,
    pub audience: Vec<Audience>,
}

impl CastCeiling {
    pub fn is_empty(&self) -> bool {
        self.trust.is_empty() && self.audience.is_empty()
    }

    pub fn admits(&self, target: &CastTarget) -> bool {
        match target {
            DimValue::Trust(t) => self.trust.contains(t),
            DimValue::Audience(a) => self.audience.contains(a),
        }
    }
}

/// How a cast resolves — constant XOR resolver, never both (unrepresentable here by construction).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CastResolution {
    Constant(CastTarget),
    Resolver { may_cast: CastCeiling },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cast {
    pub name: CastName,
    pub resolution: CastResolution,
}
