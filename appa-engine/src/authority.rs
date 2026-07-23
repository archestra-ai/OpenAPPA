//! Authorities, sanitizers, and casts — the declarations of who may cover what, and which
//! transforms produce new values.

use serde::{Deserialize, Serialize};

use crate::fact::EffectKind;
use crate::label::{Audience, DimValue, Trust};
use crate::names::{AuthorityName, CastName, MarkName, SanitizerName, TagName};

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

    /// A mandate with a cover ceiling (trust or readers) — the one thing a self-granted in-process
    /// `approve` builtin may not carry (it may clear only what it can fully see).
    pub fn has_cover_ceiling(&self) -> bool {
        self.trust_ceiling.is_some() || self.reader_ceiling.is_some()
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SanitizerPoints {
    pub input: bool,
    pub output: bool,
}

/// A sanitizer's declared audience transition — **audience only, by construction**. It applies only
/// when the source audience satisfies `from_includes` (`audience ⊇ from_includes`), and produces
/// the exact output audience `to`. Trust is preserved: there is no field here to raise it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudienceTransition {
    pub from_includes: Audience,
    pub to: Audience,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sanitizer {
    pub name: SanitizerName,
    pub on: SanitizerPoints,
    pub can_reduce: AudienceTransition,
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
