//! Authorities, sanitizers, and casts — the declarations of who may cover what, and which
//! transforms produce new values.

use serde::{Deserialize, Serialize};

use crate::fact::EffectKind;
use crate::groups::{DeclaredAudience, Expansions};
use crate::label::{Adequacy, Audience, Dim, Dimension, EstablishedLabel, Label, ReaderId, Trust};
use crate::names::{AuthorityName, CastName, GroupName, MarkName, SanitizerName, TagName};

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
/// small). Authorities, casts, and sanitizers all route by this one shape.
/// Attention gaps ignore scope — they route by their own currency (the attended mark).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    pub tags: Vec<TagName>,
}

impl Scope {
    pub fn covers(&self, call_tags: &[TagName]) -> bool {
        self.tags.is_empty() || self.tags.iter().any(|t| call_tags.contains(t))
    }

    /// The routing gate every cast consumer shares — planning, admission, and the
    /// transition validator — so none of them can drift: does this scope reach `value`? A tool
    /// result routes by its originating contract's tags, a provider-run result by its own
    /// contract's, and a child return originates from no tool, so only an unscoped
    /// component reaches it. `None` names a missing routing record; the caller
    /// decides whether that skips the source, breaks a proven invariant, or refuses the record.
    pub(crate) fn reaches(
        &self,
        registry: &crate::registry::Registry,
        views: &crate::projection::Views<'_>,
        value: crate::value::ValueId,
    ) -> Option<bool> {
        Some(match views.value_provenance(value)? {
            crate::value::Provenance::ToolResult { dispatch } => {
                let tool = views.dispatch_tool(dispatch)?;
                self.covers(&registry.tool(tool)?.tags)
            }
            crate::value::Provenance::ProviderRun { tool, .. } => {
                self.covers(&registry.provider_run_contract(tool)?.tags)
            }
            crate::value::Provenance::ChildReturn { .. } => self.is_unscoped(),
        })
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
/// registration — a sanitizer does not decide its derivation label per value, as a
/// resolver-implemented [`Cast`] does — so the declared `to` is the transition ceiling. The
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
    pub fn dimension(&self) -> Dimension {
        match self {
            DeclaredTransition::Audience { .. } => Dimension::Audience,
            DeclaredTransition::Trust { .. } => Dimension::Trust,
        }
    }

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
                raw.audience.covers(&widest) == Adequacy::Holds
            }
            DeclaredTransition::Trust { from_floor, .. } => raw.trust.meets_floor(*from_floor) == Adequacy::Holds,
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

/// The complete product ceiling a resolver-implemented cast may not exceed: the
/// admissible target trust ranks and a cap the resolved audience must stay within. A `public`
/// cap is the open gate that admits a Public resolution. An empty trust list means
/// the cast can never fill an unresolved trust dimension — it still serves sources whose
/// trust is already established, so only the catalogue-aware reachability pass
/// can prove a ceiling dead at load.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CastCeiling {
    pub trust: Vec<Trust>,
    pub audience: DeclaredAudience,
}

impl CastCeiling {
    fn admits_unresolved(&self, answer: &EstablishedLabel, dim: Dimension, expansions: &Expansions) -> bool {
        match dim {
            Dimension::Trust => self.trust.contains(&answer.trust),
            Dimension::Audience => answer.audience.within(&self.audience.resolve(expansions)),
        }
    }
}

/// A constant cast's declared complete label: a trust rank and a written audience whose
/// groups resolve at the validation that reads it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclaredLabel {
    pub trust: Trust,
    pub audience: DeclaredAudience,
}

impl DeclaredLabel {
    pub fn literal(label: EstablishedLabel) -> DeclaredLabel {
        DeclaredLabel {
            trust: label.trust,
            audience: DeclaredAudience::literal(label.audience),
        }
    }

    pub(crate) fn resolve(&self, expansions: &Expansions) -> EstablishedLabel {
        EstablishedLabel::new(self.trust, self.audience.resolve(expansions))
    }
}

/// How a cast resolves — constant XOR resolver, never both (unrepresentable here by construction).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CastResolution {
    Constant(DeclaredLabel),
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
    pub fn validate(
        &self,
        prior: &Label,
        answer: &EstablishedLabel,
        expansions: &Expansions,
    ) -> Result<(), CastRefusal> {
        let literal_ids = answer
            .audience
            .readers()
            .is_none_or(|readers| readers.iter().all(ReaderId::is_literal));
        if !literal_ids {
            return Err(CastRefusal::NonLiteralReader);
        }
        if let CastResolution::Constant(constant) = self
            && *answer != constant.resolve(expansions)
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
                && !may_cast.admits_unresolved(answer, dim, expansions)
            {
                return Err(CastRefusal::CeilingExceeded(dim));
            }
        }
        Ok(())
    }

    /// Does any complete answer exist that [`CastResolution::validate`] would admit for a source
    /// whose per-value label is `prior`? Selection reads this before requesting: a
    /// request against a resolution no answer can satisfy — a constant disagreeing with an
    /// established dimension, a resolver whose trust ceiling is empty while trust is unresolved —
    /// would redrive forever without reaching a capable cast registered after it.
    pub(crate) fn can_establish(&self, prior: &crate::label::Label, expansions: &Expansions) -> bool {
        match self {
            CastResolution::Constant(constant) => {
                let trust_agrees = match prior.trust {
                    Dim::Known(trust) => trust == constant.trust,
                    Dim::Unknown => true,
                };
                let audience_agrees = match &prior.audience {
                    Dim::Known(audience) => *audience == constant.audience.resolve(expansions),
                    Dim::Unknown => true,
                };
                trust_agrees && audience_agrees
            }
            CastResolution::Resolver { may_cast } => !matches!(prior.trust, Dim::Unknown) || !may_cast.trust.is_empty(),
        }
    }

    pub fn groups(&self) -> impl Iterator<Item = &GroupName> {
        match self {
            CastResolution::Constant(constant) => constant.audience.groups(),
            CastResolution::Resolver { may_cast } => may_cast.audience.groups(),
        }
    }

    /// The groups validating one answer for a source at `prior` reads: a
    /// constant is read whole; a resolver's audience ceiling only where the source's audience is
    /// unresolved — an established dimension constrains the answer, never the ceiling.
    pub fn reads(&self, prior: &crate::label::Label) -> impl Iterator<Item = &GroupName> {
        match self {
            CastResolution::Constant(constant) => Some(constant.audience.groups()),
            CastResolution::Resolver { may_cast } => {
                matches!(prior.audience, Dim::Unknown).then(|| may_cast.audience.groups())
            }
        }
        .into_iter()
        .flatten()
    }
}

/// A registered cast that establishes an Unknown value's complete label. It applies to values
/// whose originating tool contract carries a covered tag; a child return or user turn
/// originates from no tool, so only unscoped casts apply there. Among applicable
/// casts, registration order decides and the first complete valid answer stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cast {
    pub name: CastName,
    pub resolution: CastResolution,
    #[serde(default)]
    pub scope: Scope,
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
        (transition.admits(raw) == crate::label::Adequacy::Holds).then(|| transition.derive(raw))
    }

    pub fn groups(&self) -> impl Iterator<Item = &GroupName> {
        self.transition.groups()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let raw = |audience: Dim<Audience>| Label::new(Dim::Known(Trust::new(1)), audience);
        assert!(transition.may_admit(&raw(Dim::Known(readers(&["alice", "carol"])))));
        assert!(!transition.may_admit(&raw(Dim::Known(readers(&["bob"])))));
        assert!(!transition.may_admit(&raw(Dim::Unknown)));
    }

    fn resolver(trust: &[u8], cap: Audience) -> CastResolution {
        CastResolution::Resolver {
            may_cast: CastCeiling {
                trust: trust.iter().copied().map(Trust::new).collect(),
                audience: DeclaredAudience::literal(cap),
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
            cast.validate(
                &both_unknown(),
                &answer(0, readers(&["finance"])),
                &Expansions::default()
            ),
            Ok(())
        );
        assert_eq!(
            cast.validate(
                &both_unknown(),
                &answer(0, readers(&["finance", "audit"])),
                &Expansions::default()
            ),
            Ok(())
        );
        assert_eq!(
            cast.validate(
                &both_unknown(),
                &answer(0, readers(&["finance", "stranger"])),
                &Expansions::default()
            ),
            Err(CastRefusal::CeilingExceeded(Dimension::Audience))
        );
    }

    #[test]
    fn a_public_resolution_is_admitted_only_under_a_public_cap() {
        let wide = resolver(&[0], Audience::Public);
        assert_eq!(
            wide.validate(&both_unknown(), &answer(0, Audience::Public), &Expansions::default()),
            Ok(())
        );
        assert_eq!(
            wide.validate(
                &both_unknown(),
                &answer(0, readers(&["anyone"])),
                &Expansions::default()
            ),
            Ok(())
        );
        let narrow = resolver(&[0], readers(&["finance"]));
        assert_eq!(
            narrow.validate(&both_unknown(), &answer(0, Audience::Public), &Expansions::default()),
            Err(CastRefusal::CeilingExceeded(Dimension::Audience))
        );
    }

    #[test]
    fn a_resolution_must_hold_literal_reader_ids() {
        let wide = resolver(&[0], Audience::Public);
        for bad in [readers(&["@hr"]), readers(&["public"]), readers(&["ap@corp", "@hr"])] {
            assert_eq!(
                wide.validate(&both_unknown(), &answer(0, bad), &Expansions::default()),
                Err(CastRefusal::NonLiteralReader)
            );
        }
        assert_eq!(
            wide.validate(
                &both_unknown(),
                &answer(0, readers(&["ap@corp"])),
                &Expansions::default()
            ),
            Ok(())
        );
    }

    #[test]
    fn both_ceilings_bound_the_answer_independently() {
        let cast = resolver(&[1], readers(&["finance"]));
        assert_eq!(
            cast.validate(
                &both_unknown(),
                &answer(1, readers(&["finance"])),
                &Expansions::default()
            ),
            Ok(())
        );
        assert_eq!(
            cast.validate(
                &both_unknown(),
                &answer(2, readers(&["finance"])),
                &Expansions::default()
            ),
            Err(CastRefusal::CeilingExceeded(Dimension::Trust))
        );
        assert_eq!(
            cast.validate(
                &both_unknown(),
                &answer(1, readers(&["stranger"])),
                &Expansions::default()
            ),
            Err(CastRefusal::CeilingExceeded(Dimension::Audience))
        );
    }

    #[test]
    fn an_established_dimension_must_match_exactly() {
        let cast = resolver(&[1], readers(&["finance"]));
        let trust_known = Label::new(Dim::Known(Trust::new(3)), Dim::Unknown);
        assert_eq!(
            cast.validate(&trust_known, &answer(3, readers(&["finance"])), &Expansions::default()),
            Ok(())
        );
        assert_eq!(
            cast.validate(&trust_known, &answer(1, readers(&["finance"])), &Expansions::default()),
            Err(CastRefusal::EstablishedMismatch(Dimension::Trust))
        );
        let audience_known = Label::new(Dim::Unknown, Dim::Known(Audience::Public));
        assert_eq!(
            cast.validate(&audience_known, &answer(1, Audience::Public), &Expansions::default()),
            Ok(())
        );
        assert_eq!(
            cast.validate(
                &audience_known,
                &answer(1, readers(&["finance"])),
                &Expansions::default()
            ),
            Err(CastRefusal::EstablishedMismatch(Dimension::Audience))
        );
    }

    #[test]
    fn a_constant_admits_exactly_its_declared_label_where_it_agrees() {
        let constant = CastResolution::Constant(DeclaredLabel::literal(answer(0, Audience::Public)));
        assert_eq!(
            constant.validate(&both_unknown(), &answer(0, Audience::Public), &Expansions::default()),
            Ok(())
        );
        assert_eq!(
            constant.validate(&both_unknown(), &answer(1, Audience::Public), &Expansions::default()),
            Err(CastRefusal::ConstantMismatch)
        );
        let agreeing = Label::new(Dim::Known(Trust::new(0)), Dim::Unknown);
        assert_eq!(
            constant.validate(&agreeing, &answer(0, Audience::Public), &Expansions::default()),
            Ok(())
        );
        let disagreeing = Label::new(Dim::Known(Trust::new(2)), Dim::Unknown);
        assert_eq!(
            constant.validate(&disagreeing, &answer(0, Audience::Public), &Expansions::default()),
            Err(CastRefusal::EstablishedMismatch(Dimension::Trust))
        );
    }
}
