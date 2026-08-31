//! Authorities and sanitizers — the declarations of who may cover what, and which
//! transforms produce new values.

use serde::{Deserialize, Serialize};

use crate::fact::EffectKind;
use crate::groups::{DeclaredAudience, Expansions};
use crate::label::{Audience, Label, Trust};
use crate::names::{AuthorityName, GroupName, MarkName, SanitizerName, TagName};

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
    /// Cover an unmet `includes` by vouching readers up to this set. A group written here
    /// resolves at the operation that validates the mandate.
    pub reader_ceiling: Option<DeclaredAudience>,
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

    pub fn groups(&self) -> impl Iterator<Item = &GroupName> {
        self.reader_ceiling.iter().flat_map(DeclaredAudience::groups)
    }

    /// The groups a ruling covering `gaps` under this mandate reads: the
    /// reader ceiling's, only where an `includes` gap is among them — no other gap consults it.
    pub(crate) fn reads<'a>(&'a self, gaps: &[crate::check::Gap]) -> impl Iterator<Item = &'a GroupName> {
        gaps.iter()
            .any(|gap| matches!(gap, crate::check::Gap::Includes { .. }))
            .then(|| self.groups())
            .into_iter()
            .flatten()
    }
}

/// A component's jurisdiction: the tags it covers. Empty = everything (small configs stay
/// small). Authorities and sanitizers route by this one shape.
/// Attention gaps ignore scope — they route by their own currency (the attended mark).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    pub tags: Vec<TagName>,
}

impl Scope {
    pub fn covers(&self, call_tags: &[TagName]) -> bool {
        self.tags.is_empty() || self.tags.iter().any(|t| call_tags.contains(t))
    }

    /// Does this scope cover every value `other` covers? Unscoped covers every
    /// scope; otherwise coverage is tag-set superset — a scoped component never covers an
    /// unscoped one.
    pub fn covers_scope(&self, other: &Scope) -> bool {
        self.tags.is_empty() || (!other.tags.is_empty() && other.tags.iter().all(|t| self.tags.contains(t)))
    }

    pub fn is_unscoped(&self) -> bool {
        self.tags.is_empty()
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
/// registration, so the declared `to` is the transition ceiling. The
/// undeclared dimension is untouched: the derivation carries the raw value's label on it unchanged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeclaredTransition {
    Audience {
        from_includes: DeclaredAudience,
        to: DeclaredAudience,
    },
    Trust {
        from_floor: Trust,
        to: Trust,
    },
}

impl DeclaredTransition {
    /// The groups this mandate writes: the application that reads it requires them
    /// together, because `from` and `to` are one declaration.
    pub fn groups(&self) -> impl Iterator<Item = &GroupName> {
        match self {
            DeclaredTransition::Audience { from_includes, to } => Some(from_includes.groups().chain(to.groups())),
            DeclaredTransition::Trust { .. } => None,
        }
        .into_iter()
        .flatten()
    }

    /// The transition as the operation reads it: every written group replaced by the operation's
    /// answer. This literal form is what a derivation record persists.
    pub(crate) fn resolve(&self, expansions: &Expansions) -> Transition {
        match self {
            DeclaredTransition::Audience { from_includes, to } => Transition::Audience {
                from_includes: from_includes.resolve(expansions),
                to: to.resolve(expansions),
            },
            DeclaredTransition::Trust { from_floor, to } => Transition::Trust {
                from_floor: *from_floor,
                to: *to,
            },
        }
    }

    /// Does some group answer make the declaration admit `raw`? Where it names no group this is
    /// the exact `from` test; where it does, an answer only adds readers `raw` must include, so
    /// the empty answer is the widest admission: `raw` must cover the literal readers alone. Load
    /// lints that size the planner read this; no decision does.
    pub(crate) fn may_admit(&self, raw: &Label) -> bool {
        match self {
            DeclaredTransition::Audience { from_includes, .. } => {
                let widest = match from_includes {
                    DeclaredAudience::Public => Audience::Public,
                    DeclaredAudience::Restricted { readers, .. } => Audience::restricted(readers.iter().cloned()),
                };
                raw.covers(&widest)
            }
            DeclaredTransition::Trust { from_floor, .. } => raw.meets_floor(*from_floor),
        }
    }
}

/// A sanitizer's transition as an operation applied it: the declared transition with
/// every written group resolved to the operation's answer. Derivation records persist
/// this literal form, so replay reads what was applied and never the directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Transition {
    Audience { from_includes: Audience, to: Audience },
    Trust { from_floor: Trust, to: Trust },
}

impl Transition {
    /// Does the raw value satisfy the `from` precondition?
    pub fn admits(&self, raw: &Label) -> bool {
        match self {
            Transition::Audience { from_includes, .. } => raw.covers(from_includes),
            Transition::Trust { from_floor, .. } => raw.meets_floor(*from_floor),
        }
    }

    /// The derivation's label: the raw value's label with this dimension replaced by the declared
    /// `to`. The other dimension rides through untouched.
    pub fn derive(&self, raw: &Label) -> Label {
        match self {
            Transition::Audience { to, .. } => Label::new(raw.trust, to.clone()),
            Transition::Trust { to, .. } => Label::new(*to, raw.audience.clone()),
        }
    }
}

/// A registered sanitizer: one transition bound to its application points and the tags it has
/// jurisdiction over, plus the operator's own account of what it is for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sanitizer {
    pub name: SanitizerName,
    pub on: SanitizerPoints,
    pub transition: DeclaredTransition,
    /// Where this sanitizer is offered: the called tool's tags for a result, the
    /// callee's for a `tool_input` substitution. Scope narrows where a component is offered; it
    /// never changes what the mandate claims. An unscoped sanitizer applies everywhere,
    /// a child return included — nothing else does, because a return originates from no tool.
    #[serde(default)]
    pub scope: Scope,
    pub hint: Option<Hint>,
}

impl Sanitizer {
    /// The label this sanitizer's derivation of `raw` would carry at an output point, or `None`
    /// where it does not apply there: it must be registered for output, its scope must
    /// cover the tags of the contract the value originates from, and the source must
    /// satisfy its declared `from`. The one predicate live admission, the child crossing
    /// and the transition validator share, so none of them can drift from the others.
    pub(crate) fn derive_output(
        &self,
        raw: &crate::label::Label,
        tags: &[TagName],
        expansions: &Expansions,
    ) -> Option<crate::label::Label> {
        (self.on.output && self.applies_to(tags)).then_some(())?;
        self.derives(raw, expansions)
    }

    /// The label this sanitizer's derivation of the call's argument bytes would carry, or `None`
    /// where it does not apply at tool input: registered for input, scope covering the
    /// callee, and the raw bytes satisfying its declared `from`.
    pub(crate) fn derive_input(
        &self,
        raw: &crate::label::Label,
        tags: &[TagName],
        expansions: &Expansions,
    ) -> Option<crate::label::Label> {
        (self.on.input && self.applies_to(tags)).then_some(())?;
        self.derives(raw, expansions)
    }

    /// Does this sanitizer's jurisdiction reach a value originating from a contract carrying
    /// `tags`?
    pub(crate) fn applies_to(&self, tags: &[TagName]) -> bool {
        self.scope.covers(tags)
    }

    fn derives(&self, raw: &crate::label::Label, expansions: &Expansions) -> Option<crate::label::Label> {
        let transition = self.transition.resolve(expansions);
        transition.admits(raw).then(|| transition.derive(raw))
    }

    pub fn groups(&self) -> impl Iterator<Item = &GroupName> {
        self.transition.groups()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::label::ReaderId;

    fn readers(ids: &[&str]) -> Audience {
        Audience::restricted(ids.iter().copied().map(ReaderId::new))
    }

    #[test]
    fn a_grouped_from_may_admit_only_a_raw_covering_its_literal_readers() {
        let transition = DeclaredTransition::Audience {
            from_includes: DeclaredAudience::declared([ReaderId::new("alice")], [crate::names::GroupName::new("team")])
                .expect("a literal reader and a group"),
            to: DeclaredAudience::literal(Audience::Public),
        };
        let raw = |audience: Audience| Label::new(Trust::new(1), audience);
        assert!(transition.may_admit(&raw(readers(&["alice", "carol"]))));
        assert!(!transition.may_admit(&raw(readers(&["bob"]))));
    }
}
