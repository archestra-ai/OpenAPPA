//! Authorities, sanitizers, and casts — the declarations of who may cover what, and which
//! transforms produce new values.
//!
//! A [`Mandate`] declares what an authority's ruling may cover, each power naming the currency it
//! acts on (trust ceiling, reader ceiling, named waiver, attended marks); a [`Scope`] names the
//! tags it has jurisdiction over. A [`Sanitizer`] declares an **audience-only** transition — trust
//! is structurally not sanitizer territory (there is no trust field to raise). A [`Cast`] resolves
//! an Unknown dimension, constant XOR resolver-implemented.

use serde::{Deserialize, Serialize};

use crate::fact::EffectKind;
use crate::label::{Audience, DimValue, Trust};
use crate::names::{AuthorityName, CastName, MarkName, SanitizerName, TagName};

/// The state a cast may resolve an Unknown dimension to — trust OR audience, never both.
pub type CastTarget = DimValue;

/// What an authority's ruling may cover. Each power names its currency; a mandate covering nothing
/// is a loud load error (the empty-remedy proof depends on it — see [`Mandate::is_empty`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mandate {
    /// Cover an unmet trust floor up to this rank (the label never moves; the ceiling bounds it).
    pub trust_ceiling: Option<Trust>,
    /// Cover an unmet `includes` by vouching readers up to this set.
    pub reader_ceiling: Option<Audience>,
    /// Cover a failed `no_prior` for the admitted dispatch, naming the event kinds it may waive.
    pub waivers: Vec<EffectKind>,
    /// The attention marks whose demands this authority's ruling satisfies.
    pub attends: Vec<MarkName>,
}

impl Mandate {
    /// A mandate that grants no power at all — refused at load.
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
    /// Does this scope cover a call carrying `call_tags`? An empty scope covers everything.
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

/// Where a sanitizer may apply.
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

/// A registered sanitizer: an audience transition bound to its application points.
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

    /// Is `target` within this ceiling?
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
    /// Every Unknown on its dimension resolves to one declared state (the YOLO/paranoid knob).
    Constant(CastTarget),
    /// Decided per value by a registered resolver, bounded by `may_cast`.
    Resolver { may_cast: CastCeiling },
}

/// A registered cast that fills an Unknown label dimension in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cast {
    pub name: CastName,
    pub resolution: CastResolution,
}
