//! Authorities and sanitizers — the declarations of who may cover what, and which
//! transforms produce new values.

use serde::{Deserialize, Serialize};

use crate::fact::EffectKind;
use crate::label::{
    Audience, DeclaredAudience, Evaluation, Label, MembershipContext, MembershipNeeded, SymbolicAtom, Trust,
};
use crate::names::{AuthorityName, MarkName, SanitizerName, TagName};

/// Trusted deployer prose for a registered component. OpenAPPA includes an Authority or
/// Sanitizer hint in remedy plans that reference the component. For model-backed components,
/// the hint is included in the consult's system prompt declaration to guide model evaluation.
/// Advisory only: a hint NEVER enters a check, enumeration, or ordering, and it cannot expand
/// a mandate. The load lint bounds its length ([`crate::registry::MAX_HINT_CHARS`]).
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
    /// Cover an unmet `includes` by vouching readers up to this set. A symbolic audience
    /// written here stays symbolic; the operation that validates the mandate resolves what
    /// the comparison needs.
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

    pub(crate) fn needed_atoms<'a>(
        &'a self,
        providers: &'a std::collections::BTreeSet<String>,
    ) -> impl Iterator<Item = SymbolicAtom> + 'a {
        self.reader_ceiling
            .iter()
            .flat_map(move |ceiling| ceiling.needed_atoms(providers))
    }

    /// The atoms a ruling covering `gaps` under this mandate reads: the
    /// reader ceiling's, only where an `includes` gap is among them — no other gap consults it.
    pub(crate) fn reads<'a>(
        &'a self,
        gaps: &[crate::check::Gap],
        providers: &'a std::collections::BTreeSet<String>,
    ) -> impl Iterator<Item = SymbolicAtom> + 'a {
        gaps.iter()
            .any(|gap| matches!(gap, crate::check::Gap::Includes { .. }))
            .then(|| self.needed_atoms(providers))
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
    /// The atoms this mandate writes: the application that reads it evaluates them
    /// together, because `from` and `to` are one declaration.
    pub(crate) fn needed_atoms<'a>(
        &'a self,
        providers: &'a std::collections::BTreeSet<String>,
    ) -> impl Iterator<Item = SymbolicAtom> + 'a {
        match self {
            DeclaredTransition::Audience { from_includes, to } => {
                Some(from_includes.needed_atoms(providers).chain(to.needed_atoms(providers)))
            }
            DeclaredTransition::Trust { .. } => None,
        }
        .into_iter()
        .flatten()
    }

    /// The transition as an application reads and a derivation record persists it: the
    /// declared `from` comparison as written, the `to` as the whole [`Audience`] the derived
    /// label carries. Nothing expands here — symbolic atoms survive into the derived label
    /// and the durable record.
    pub(crate) fn applied(&self) -> Transition {
        match self {
            DeclaredTransition::Audience { from_includes, to } => Transition::Audience {
                from_includes: from_includes.clone(),
                to: Audience::of_declared(to),
            },
            DeclaredTransition::Trust { from_floor, to } => Transition::Trust {
                from_floor: *from_floor,
                to: *to,
            },
        }
    }

    /// Could some directory answer make the declaration admit `raw`? Where the `from` names no
    /// symbolic audience this is the exact test; a symbolic audience may answer empty, so the
    /// widest admission requires `raw` to cover the literal readers alone, and an undecided
    /// comparison stays admitting. Load lints that size the planner read this; no decision does.
    pub(crate) fn may_admit(&self, raw: &Label, context: &MembershipContext<'_>) -> bool {
        match self {
            DeclaredTransition::Audience { from_includes, .. } => {
                let widest = match from_includes {
                    DeclaredAudience::Public => DeclaredAudience::Public,
                    DeclaredAudience::Union(clause) => DeclaredAudience::restricted(clause.readers().iter().cloned()),
                };
                !matches!(raw.covers(&widest, context), Evaluation::Fails)
            }
            DeclaredTransition::Trust { from_floor, .. } => raw.meets_floor(*from_floor),
        }
    }
}

/// A sanitizer's transition as an operation applied it: the declared transition's audiences
/// as whole symbolic [`Audience`] values. Derivation records persist this form; whatever a
/// comparison needed extensionally is pinned as primitive evidence beside it, so replay reads
/// what was applied and never the directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Transition {
    Audience {
        from_includes: DeclaredAudience,
        to: Audience,
    },
    Trust {
        from_floor: Trust,
        to: Trust,
    },
}

impl Transition {
    /// Does the raw value satisfy the `from` precondition?
    pub(crate) fn admits(&self, raw: &Label, context: &MembershipContext<'_>) -> Evaluation {
        match self {
            Transition::Audience { from_includes, .. } => raw.covers(from_includes, context),
            Transition::Trust { from_floor, .. } => Evaluation::of_exact(raw.meets_floor(*from_floor)),
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
        context: &MembershipContext<'_>,
    ) -> Result<Option<crate::label::Label>, MembershipNeeded> {
        if !(self.on.output && self.applies_to(tags)) {
            return Ok(None);
        }
        self.derives(raw, context)
    }

    /// The label this sanitizer's derivation of the call's argument bytes would carry, or `None`
    /// where it does not apply at tool input: registered for input, scope covering the
    /// callee, and the raw bytes satisfying its declared `from`.
    pub(crate) fn derive_input(
        &self,
        raw: &crate::label::Label,
        tags: &[TagName],
        context: &MembershipContext<'_>,
    ) -> Result<Option<crate::label::Label>, MembershipNeeded> {
        if !(self.on.input && self.applies_to(tags)) {
            return Ok(None);
        }
        self.derives(raw, context)
    }

    /// Does this sanitizer's jurisdiction reach a value originating from a contract carrying
    /// `tags`?
    pub(crate) fn applies_to(&self, tags: &[TagName]) -> bool {
        self.scope.covers(tags)
    }

    fn derives(
        &self,
        raw: &crate::label::Label,
        context: &MembershipContext<'_>,
    ) -> Result<Option<crate::label::Label>, MembershipNeeded> {
        let transition = self.transition.applied();
        match transition.admits(raw, context) {
            Evaluation::Holds => Ok(Some(transition.derive(raw))),
            Evaluation::Fails => Ok(None),
            Evaluation::Needs(needed) => Err(needed),
        }
    }

    pub(crate) fn needed_atoms<'a>(
        &'a self,
        providers: &'a std::collections::BTreeSet<String>,
    ) -> impl Iterator<Item = SymbolicAtom> + 'a {
        self.transition.needed_atoms(providers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::label::{Clause, Expansions, GroupRef, ReaderId, WithinAssertions};

    fn readers(ids: &[&str]) -> Audience {
        Audience::restricted(ids.iter().copied().map(ReaderId::new))
    }

    #[test]
    fn a_symbolic_from_may_admit_only_a_raw_covering_its_literal_readers() {
        let transition = DeclaredTransition::Audience {
            from_includes: DeclaredAudience::Union(
                Clause::new(
                    [],
                    [GroupRef::Named(crate::names::GroupName::new("team"))],
                    [ReaderId::new("alice")],
                )
                .expect("a literal reader and a group"),
            ),
            to: DeclaredAudience::Public,
        };
        let within = WithinAssertions::default();
        let providers = std::collections::BTreeSet::new();
        let expansions = Expansions::default();
        let context = MembershipContext::new(&within, &providers, &expansions);
        let raw = |audience: Audience| Label::new(Trust::new(1), audience);
        assert!(transition.may_admit(&raw(readers(&["alice", "carol"])), &context));
        assert!(!transition.may_admit(&raw(readers(&["bob"])), &context));
    }
}
