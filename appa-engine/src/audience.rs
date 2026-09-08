//! Audience sources and the primitive evidence a decision pins.
//!
//! A symbolic audience resolves through registered **audience sources** (one per provider,
//! shipped by batteries). A source reports each member as a reader: the address the
//! provider verified for that account, or the provider-qualified id. The record pins the
//! PRIMITIVES — per-selector member answers and per-member lookups. Union, principal
//! substitution, and the `within` closure are recomputed deterministically from those
//! primitives at replay, so a live decision and its replay read the same answers, and
//! cross-audience invariants (`@finance ⊆ internal`) hold by construction.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::label::{ChainAudience, Expansions, GroupRef, ReaderId, SymbolicAtom, address_parts};
use crate::names::GroupName;

/// One selector's validated answer from its provider's source, as the record pins it. Each
/// member is the reader the source reports: an address or a `<provider>:<id>` under the
/// source's own provider — the one shape rule, applied by [`AudienceRegistry::expansions`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceClaims {
    pub provider: String,
    pub selector: String,
    pub members: Vec<ReaderId>,
}

/// One member lookup's pinned answer: the principal the answering entry reports for one
/// qualified reader, or `None` when it does not know the member — a definitive answer that
/// leaves the reader as written.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberLookup {
    /// The member's own provider — the pin key, whichever entry answered.
    pub provider: String,
    /// The provider-qualified reader that was looked up, e.g. `slack:U012345`.
    pub member: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<ReaderId>,
}

/// The primitive audience evidence one operation pins: everything its expansions are
/// recomputed from. Duplicate or malformed entries never validate, and after the act's
/// decision runs, every entry must be an inherited pin or answer an ask the operation
/// actually made ([`AudienceRegistry::only_requested`]) — evidence cannot be pre-loaded for
/// asks nobody made, live or at replay.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudienceEvidence {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<SourceClaims>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lookups: Vec<MemberLookup>,
}

impl AudienceEvidence {
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty() && self.lookups.is_empty()
    }

    /// Does this evidence carry every entry of `other`? An operation may extend what an
    /// earlier record pinned, never contradict or drop it.
    pub(crate) fn contains(&self, other: &AudienceEvidence) -> bool {
        other.sources.iter().all(|claims| self.sources.contains(claims))
            && other.lookups.iter().all(|lookup| self.lookups.contains(lookup))
    }

    /// This act's evidence read under an earlier record's pins: the pinned entries come
    /// first, an answer restating a pin is that one entry, and an answer for a key the pins
    /// hold with different content is a contradiction the operation refuses. Nothing here
    /// dedups the act's own entries: two answers for one key stay two, for validation to
    /// refuse. A chain of operations over one value reads each primitive under one answer.
    pub fn inheriting(&self, pinned: &AudienceEvidence) -> Result<AudienceEvidence, EvidenceRefusal> {
        let mut merged = pinned.clone();
        for claims in &self.sources {
            let held = pinned
                .sources
                .iter()
                .find(|entry| entry.provider == claims.provider && entry.selector == claims.selector);
            match held {
                None => merged.sources.push(claims.clone()),
                Some(entry) if entry == claims => {}
                Some(_) => return Err(EvidenceRefusal::ContradictedPin { entry: claims.entry() }),
            }
        }
        for lookup in &self.lookups {
            let held = pinned
                .lookups
                .iter()
                .find(|entry| entry.provider == lookup.provider && entry.member == lookup.member);
            match held {
                None => merged.lookups.push(lookup.clone()),
                Some(entry) if entry == lookup => {}
                Some(_) => return Err(EvidenceRefusal::ContradictedPin { entry: lookup.entry() }),
            }
        }
        Ok(merged)
    }
}

impl SourceClaims {
    /// The entry as a refusal names it.
    fn entry(&self) -> String {
        format!("source {}:{}", self.provider, self.selector)
    }
}

impl MemberLookup {
    /// The entry as a refusal names it.
    fn entry(&self) -> String {
        format!("lookup {}", self.member)
    }
}

/// Why pinned audience evidence is not admissible.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EvidenceRefusal {
    #[error("two answers for selector {provider}:{selector} in one operation")]
    DuplicateSelector { provider: String, selector: String },
    #[error("two lookups for member {member} in one operation")]
    DuplicateLookup { member: String },
    #[error(
        "selector {provider}:{selector} reports member {id:?}, which is neither an address nor a {provider}-qualified id"
    )]
    MalformedMember {
        provider: String,
        selector: String,
        id: String,
    },
    #[error("lookup under provider {provider} answers for member {member:?} outside that namespace")]
    ForeignLookup { provider: String, member: String },
    #[error(
        "the lookup of member {member:?} names principal {principal:?}, which is neither an address nor a {provider}-qualified id"
    )]
    MalformedPrincipal {
        provider: String,
        member: String,
        principal: String,
    },
    #[error("selector {provider}:{selector} reports member {id:?} twice in one answer")]
    DuplicateMember {
        provider: String,
        selector: String,
        id: String,
    },
    #[error("no registered audience source serves selector {provider}:{selector}")]
    UnroutableSelector { provider: String, selector: String },
    #[error("no registered audience provider {provider} serves the lookup of {member:?}")]
    UnroutableLookup { provider: String, member: String },
    #[error("evidence entry {entry} is neither an inherited pin nor requested by this operation")]
    UnrequestedEvidence { entry: String },
    #[error("evidence entry {entry} contradicts the answer an earlier record of this chain pinned")]
    ContradictedPin { entry: String },
}

/// The one shape rule on a reader a source reports under `provider`: a literal spelling
/// that is an address — the principal itself, the same reader a policy or a tool argument
/// names by writing it — or a `<provider>:<id>` in the reporting source's own namespace.
/// Deliberately shape-only: the source is trusted for what it says, and the rule only
/// keeps a source from seating a member on a reserved spelling, a bare name, or another
/// provider's namespace. Address normalization is [`ReaderId::new`]'s: the domain folds,
/// the local part does not.
pub fn well_formed_reader(provider: &str, reader: &ReaderId) -> bool {
    reader.is_literal() && (is_address(reader) || reader.provider_prefix() == Some(provider))
}

/// A reader written as one address in no provider's namespace: the principal itself. A
/// `<provider>:<id>` stays qualified whatever its id part spells.
fn is_address(reader: &ReaderId) -> bool {
    reader.provider_prefix().is_none() && address_parts(reader.as_str()).is_some()
}

/// Is this reader one a redirected provider must look up before it can seat it: a member
/// reported as a qualified id rather than an address.
fn needs_lookup(reader: &ReaderId) -> bool {
    !is_address(reader)
}

/// One selector as configuration spells it: `<provider>:<selector>`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SelectorSpec {
    pub provider: String,
    pub selector: String,
}

impl std::fmt::Display for SelectorSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.provider, self.selector)
    }
}

impl SelectorSpec {
    /// Parse `<provider>:<selector>`; both halves non-empty, split at the first `:`.
    pub fn parse(spelled: &str) -> Option<SelectorSpec> {
        let (provider, selector) = spelled.split_once(':')?;
        if provider.is_empty() || selector.is_empty() {
            return None;
        }
        Some(SelectorSpec {
            provider: provider.to_string(),
            selector: selector.to_string(),
        })
    }
}

/// One selector template a source advertises: literal segments and `<placeholder>` segments,
/// split on `/`. `group/<group-address>` matches `group/finance@corp.com` and nothing with
/// another segment count.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SelectorTemplate(String);

impl SelectorTemplate {
    pub fn new(template: impl Into<String>) -> SelectorTemplate {
        SelectorTemplate(template.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Segment-wise match: a `<placeholder>` segment matches exactly one non-empty segment.
    pub fn matches(&self, selector: &str) -> bool {
        let template: Vec<&str> = self.0.split('/').collect();
        let given: Vec<&str> = selector.split('/').collect();
        template.len() == given.len()
            && template.iter().zip(&given).all(|(pattern, segment)| {
                if pattern.starts_with('<') && pattern.ends_with('>') {
                    !segment.is_empty()
                } else {
                    pattern == segment
                }
            })
    }
}

/// One registered audience source: a provider name and the selector templates it serves.
/// Batteries register sources; one provider is registered exactly once per deployment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRegistration {
    pub provider: String,
    pub templates: Vec<SelectorTemplate>,
}

/// One configured named audience: `[audience.group.<name>] within / from`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedAudience {
    pub name: GroupName,
    /// The trusted policy assertion `@name ⊆ within-target`. `None` reads as `public` — no
    /// assertion.
    pub within: Option<ChainAudience>,
    pub from: Vec<SelectorSpec>,
}

/// The audience side of the registry: the registered sources, the chain mappings, and the
/// configured named audiences. All of it is policy meaning and enters the policy identity;
/// how a deployment *reaches* a source (URL, command, credentials) and where it sends a
/// provider's lookups never do.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudienceConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<SourceRegistration>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub self_from: Vec<SelectorSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub internal_from: Vec<SelectorSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<NamedAudience>,
    /// The entry that answers each redirected provider's member lookups: every qualified
    /// member such a provider reports is looked up there before it seats. Routing, not
    /// policy meaning — the deployment supplies it and it stays out of the policy identity.
    #[serde(skip)]
    pub lookup_targets: BTreeMap<String, String>,
}

/// The validated audience registry the engine reads: everything [`AudienceConfig`] declares,
/// indexed. Built once at load behind the registry's structural lints.
#[derive(Clone, Debug, Default)]
pub struct AudienceRegistry {
    providers: BTreeMap<String, Vec<SelectorTemplate>>,
    self_from: BTreeSet<SelectorSpec>,
    internal_from: BTreeSet<SelectorSpec>,
    groups: BTreeMap<GroupName, NamedAudience>,
    provider_names: BTreeSet<String>,
    lookup_targets: BTreeMap<String, String>,
    within: crate::label::WithinAssertions,
    /// The `@provider:selector` atoms declarations write directly; the registry build
    /// gathers them while routing each one.
    pub(crate) direct: BTreeSet<SelectorSpec>,
}

/// Why an atom cannot be routed to any registered source: the operational-failure side of
/// resolution. A statically written reference never gets here — load validation refuses it —
/// so this names only dynamically supplied references.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Unroutable {
    #[error("audience {0} is not a configured named audience")]
    UnknownGroup(GroupName),
    #[error("no registered audience source owns provider {0:?}")]
    UnknownProvider(String),
    #[error("selector {selector:?} matches no template of source {provider:?}")]
    UnknownSelector { provider: String, selector: String },
    #[error("the built-in audience {0} has no configured sources")]
    UnmappedChain(ChainAudience),
}

/// One member lookup to perform: the qualified member and the provider that owns it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LookupSpec {
    pub provider: String,
    pub member: String,
}

/// The primitive requests one round must answer: which selectors to consult and which member
/// lookups to perform. The lookups a redirected provider's members owe are derived from the
/// pinned answers instead ([`AudienceRegistry::member_lookups_owed`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NeededPrimitives {
    pub selectors: BTreeSet<SelectorSpec>,
    pub lookups: BTreeSet<LookupSpec>,
}

impl AudienceRegistry {
    pub(crate) fn build(config: &AudienceConfig) -> AudienceRegistry {
        AudienceRegistry {
            providers: config
                .sources
                .iter()
                .map(|source| (source.provider.clone(), source.templates.clone()))
                .collect(),
            self_from: config.self_from.iter().cloned().collect(),
            internal_from: config.internal_from.iter().cloned().collect(),
            groups: config
                .groups
                .iter()
                .map(|group| (group.name.clone(), group.clone()))
                .collect(),
            provider_names: config.sources.iter().map(|source| source.provider.clone()).collect(),
            lookup_targets: config.lookup_targets.clone(),
            within: crate::label::WithinAssertions::new(
                config
                    .groups
                    .iter()
                    .filter_map(|group| group.within.map(|target| (group.name.clone(), target))),
            ),
            direct: BTreeSet::new(),
        }
    }

    /// Every selector the policy can ask a source for: the chain mappings, every named
    /// audience's `from`, and the atoms declarations route directly.
    pub fn referenced_selectors(&self) -> BTreeSet<&SelectorSpec> {
        self.self_from
            .iter()
            .chain(self.internal_from.iter())
            .chain(self.groups.values().flat_map(|group| group.from.iter()))
            .chain(self.direct.iter())
            .collect()
    }

    /// The registered provider names — what decides which qualified readers canonicalize.
    pub fn providers(&self) -> &BTreeSet<String> {
        &self.provider_names
    }

    pub fn templates(&self, provider: &str) -> Option<&[SelectorTemplate]> {
        self.providers.get(provider).map(Vec::as_slice)
    }

    /// Does the deployment redirect this provider's lookups, so its qualified members are
    /// looked up before they seat?
    pub fn looks_up(&self, provider: &str) -> bool {
        self.lookup_targets.contains_key(provider)
    }

    /// The entry that answers this provider's member lookups: the one its routing names,
    /// else the provider's own source.
    pub fn lookup_target<'a>(&'a self, provider: &'a str) -> &'a str {
        self.lookup_targets.get(provider).map_or(provider, String::as_str)
    }

    pub fn groups(&self) -> impl Iterator<Item = &NamedAudience> {
        self.groups.values()
    }

    pub fn group(&self, name: &GroupName) -> Option<&NamedAudience> {
        self.groups.get(name)
    }

    pub fn chain_from(&self, level: ChainAudience) -> &BTreeSet<SelectorSpec> {
        match level {
            ChainAudience::Self_ => &self.self_from,
            ChainAudience::Internal => &self.internal_from,
        }
    }

    /// The declared `within` assertions, as the evaluation consumes them.
    pub(crate) fn within_assertions(&self) -> &crate::label::WithinAssertions {
        &self.within
    }

    /// Every group asserted within `level` — the members the symmetric closure folds in.
    fn groups_within(&self, level: ChainAudience) -> impl Iterator<Item = &NamedAudience> {
        self.groups.values().filter(move |group| group.within == Some(level))
    }

    /// The one routing rule for a selector: its provider is registered and one of that
    /// provider's templates matches it.
    pub(crate) fn route_selector(&self, provider: &str, selector: &str) -> Result<SelectorSpec, Unroutable> {
        let templates = self
            .providers
            .get(provider)
            .ok_or_else(|| Unroutable::UnknownProvider(provider.to_string()))?;
        if templates.iter().any(|template| template.matches(selector)) {
            Ok(SelectorSpec {
                provider: provider.to_string(),
                selector: selector.to_string(),
            })
        } else {
            Err(Unroutable::UnknownSelector {
                provider: provider.to_string(),
                selector: selector.to_string(),
            })
        }
    }

    /// Every selector whose answer the extensional closure of `level` reads: its own `from`,
    /// the groups asserted within it, and — for `internal` — the whole closure of `self`.
    fn chain_selectors(&self, level: ChainAudience) -> BTreeSet<SelectorSpec> {
        let mut selectors: BTreeSet<SelectorSpec> = self.chain_from(level).clone();
        for group in self.groups_within(level) {
            selectors.extend(group.from.iter().cloned());
        }
        if level == ChainAudience::Internal {
            selectors.extend(self.chain_selectors(ChainAudience::Self_));
        }
        selectors
    }

    /// Translate the atoms an evaluation still needs into the primitive requests that answer
    /// them. Deterministic: a pure function of the atoms and this registry. An atom no
    /// registered source can serve is an operational failure, never a policy state.
    pub fn needed_primitives(&self, atoms: &[SymbolicAtom]) -> Result<NeededPrimitives, Unroutable> {
        let mut needed = NeededPrimitives::default();
        for atom in atoms {
            match atom {
                SymbolicAtom::Chain(level) => {
                    let selectors = self.chain_selectors(*level);
                    if selectors.is_empty() {
                        return Err(Unroutable::UnmappedChain(*level));
                    }
                    needed.selectors.extend(selectors);
                }
                SymbolicAtom::Group(GroupRef::Named(name)) => {
                    let group = self
                        .groups
                        .get(name)
                        .ok_or_else(|| Unroutable::UnknownGroup(name.clone()))?;
                    needed.selectors.extend(group.from.iter().cloned());
                }
                SymbolicAtom::Group(GroupRef::Source { provider, selector }) => {
                    needed.selectors.insert(self.route_selector(provider, selector)?);
                }
                SymbolicAtom::Reader(reader) => {
                    let provider = reader
                        .provider_prefix()
                        .ok_or_else(|| Unroutable::UnknownProvider(String::new()))?;
                    if !self.providers.contains_key(provider) {
                        return Err(Unroutable::UnknownProvider(provider.to_string()));
                    }
                    needed.lookups.insert(LookupSpec {
                        provider: provider.to_string(),
                        member: reader.as_str().to_string(),
                    });
                }
            }
        }
        Ok(needed)
    }

    /// Build the operation's expansions from its pinned primitives: validate, seat every
    /// member as its reader (or, under a redirected provider, its pinned principal), union
    /// per selector, and close the chain levels symmetrically. Answers exist only for atoms
    /// whose primitives are all present — a selector one of whose qualified members still
    /// owes a lookup stays unanswered, so a check that reads it re-raises its ask. Duplicate,
    /// malformed, and unroutable entries refuse the evidence — the live act and its replay
    /// hold it to the same test.
    pub fn expansions(&self, evidence: &AudienceEvidence) -> Result<Expansions, EvidenceRefusal> {
        for claims in &evidence.sources {
            self.route_selector(&claims.provider, &claims.selector).map_err(|_| {
                EvidenceRefusal::UnroutableSelector {
                    provider: claims.provider.clone(),
                    selector: claims.selector.clone(),
                }
            })?;
        }
        for lookup in &evidence.lookups {
            if !self.providers().contains(&lookup.provider) {
                return Err(EvidenceRefusal::UnroutableLookup {
                    provider: lookup.provider.clone(),
                    member: lookup.member.clone(),
                });
            }
        }

        // Reader canonicalizations from lookups, validated: one answer per member, in the
        // member's own namespace, naming a well-formed principal. Not found is definitive:
        // the reader keeps its spelling.
        let mut principals: BTreeMap<ReaderId, ReaderId> = BTreeMap::new();
        for lookup in &evidence.lookups {
            let member = ReaderId::new(lookup.member.clone());
            if member.provider_prefix() != Some(lookup.provider.as_str()) {
                return Err(EvidenceRefusal::ForeignLookup {
                    provider: lookup.provider.clone(),
                    member: lookup.member.clone(),
                });
            }
            let principal = match &lookup.principal {
                None => member.clone(),
                Some(principal) if well_formed_reader(&lookup.provider, principal) => principal.clone(),
                Some(principal) => {
                    return Err(EvidenceRefusal::MalformedPrincipal {
                        provider: lookup.provider.clone(),
                        member: lookup.member.clone(),
                        principal: principal.as_str().to_string(),
                    });
                }
            };
            if principals.insert(member, principal).is_some() {
                return Err(EvidenceRefusal::DuplicateLookup {
                    member: lookup.member.clone(),
                });
            }
        }

        // Per-selector reader sets, validated. `None` marks a selector whose redirected
        // provider still owes a lookup for one of its qualified members.
        let mut selector_members: BTreeMap<SelectorSpec, Option<BTreeSet<ReaderId>>> = BTreeMap::new();
        for claims in &evidence.sources {
            let spec = SelectorSpec {
                provider: claims.provider.clone(),
                selector: claims.selector.clone(),
            };
            let mut members = Some(BTreeSet::new());
            let mut seen = BTreeSet::new();
            for member in &claims.members {
                if !well_formed_reader(&claims.provider, member) {
                    return Err(EvidenceRefusal::MalformedMember {
                        provider: claims.provider.clone(),
                        selector: claims.selector.clone(),
                        id: member.as_str().to_string(),
                    });
                }
                if !seen.insert(member.as_str()) {
                    return Err(EvidenceRefusal::DuplicateMember {
                        provider: claims.provider.clone(),
                        selector: claims.selector.clone(),
                        id: member.as_str().to_string(),
                    });
                }
                let seated = if self.looks_up(&claims.provider) && needs_lookup(member) {
                    principals.get(member).cloned()
                } else {
                    Some(member.clone())
                };
                match (&mut members, seated) {
                    (Some(set), Some(reader)) => {
                        set.insert(reader);
                    }
                    _ => members = None,
                }
            }
            if selector_members.insert(spec.clone(), members).is_some() {
                return Err(EvidenceRefusal::DuplicateSelector {
                    provider: spec.provider,
                    selector: spec.selector,
                });
            }
        }

        let mut answers: Vec<(SymbolicAtom, BTreeSet<ReaderId>)> = Vec::new();

        // Source-qualified selector atoms answer directly.
        for (spec, members) in &selector_members {
            if let Some(members) = members {
                answers.push((
                    SymbolicAtom::Group(GroupRef::Source {
                        provider: spec.provider.clone(),
                        selector: spec.selector.clone(),
                    }),
                    members.clone(),
                ));
            }
        }

        // Named audiences: the union of their selectors, when every one is answered.
        let union_of = |specs: &BTreeSet<SelectorSpec>| -> Option<BTreeSet<ReaderId>> {
            let mut union = BTreeSet::new();
            for spec in specs {
                union.extend(selector_members.get(spec)?.as_ref()?.iter().cloned());
            }
            Some(union)
        };
        for group in self.groups.values() {
            let specs: BTreeSet<SelectorSpec> = group.from.iter().cloned().collect();
            if let Some(members) = union_of(&specs) {
                answers.push((SymbolicAtom::Group(GroupRef::Named(group.name.clone())), members));
            }
        }

        // Chain levels: the symmetric closure over the same selector answers.
        for level in [ChainAudience::Self_, ChainAudience::Internal] {
            let specs = self.chain_selectors(level);
            if !specs.is_empty()
                && let Some(members) = union_of(&specs)
            {
                answers.push((SymbolicAtom::Chain(level), members));
            }
        }

        Ok(Expansions::new(answers, principals))
    }

    /// The lookups a redirected provider's pinned answers owe: every qualified member it
    /// reports, under the member's own provider. Evidence-derived, not atom-derived — the
    /// members are known only once the source has answered — and deterministic in the
    /// pinned answers, so live and replay derive the same set.
    fn owed_lookups(&self, evidence: &AudienceEvidence) -> BTreeSet<LookupSpec> {
        evidence
            .sources
            .iter()
            .filter(|claims| self.looks_up(&claims.provider))
            .flat_map(|claims| {
                claims
                    .members
                    .iter()
                    .filter(|member| needs_lookup(member))
                    .map(|member| LookupSpec {
                        provider: claims.provider.clone(),
                        member: member.as_str().to_string(),
                    })
            })
            .collect()
    }

    /// The owed lookups this evidence does not yet pin: what the next round must ask before
    /// the selectors that report those members can answer.
    pub fn member_lookups_owed(&self, evidence: &AudienceEvidence) -> BTreeSet<LookupSpec> {
        let pinned: BTreeSet<LookupSpec> = evidence
            .lookups
            .iter()
            .map(|lookup| LookupSpec {
                provider: lookup.provider.clone(),
                member: lookup.member.clone(),
            })
            .collect();
        let mut owed = self.owed_lookups(evidence);
        owed.retain(|spec| !pinned.contains(spec));
        owed
    }

    /// The operation-scope test on one act's pinned evidence: every entry is an inherited
    /// pin — pinned by a record this act continues — or answers a primitive the operation
    /// deterministically requested, read off the context's ask log after the decision ran.
    /// Anything else is refused: evidence cannot be pre-loaded for asks nobody made. The
    /// live act and its replay run this same test over the same reads.
    pub fn only_requested(
        &self,
        evidence: &AudienceEvidence,
        inherited: &AudienceEvidence,
        reads: &[SymbolicAtom],
    ) -> Result<(), EvidenceRefusal> {
        // Per-atom translation: a routable ask justifies its primitives; an unroutable ask
        // never answered, so it justifies nothing.
        let mut requested = NeededPrimitives::default();
        for atom in reads {
            if let Ok(primitives) = self.needed_primitives(std::slice::from_ref(atom)) {
                requested.selectors.extend(primitives.selectors);
                requested.lookups.extend(primitives.lookups);
            }
        }
        for claims in &evidence.sources {
            let spec = SelectorSpec {
                provider: claims.provider.clone(),
                selector: claims.selector.clone(),
            };
            if !requested.selectors.contains(&spec) && !inherited.sources.contains(claims) {
                return Err(EvidenceRefusal::UnrequestedEvidence { entry: claims.entry() });
            }
        }
        // A redirected provider's pinned answers justify the lookups their qualified members
        // owe, exactly as an atom justifies its selectors.
        let owed = self.owed_lookups(evidence);
        for lookup in &evidence.lookups {
            let spec = LookupSpec {
                provider: lookup.provider.clone(),
                member: lookup.member.clone(),
            };
            if !requested.lookups.contains(&spec) && !owed.contains(&spec) && !inherited.lookups.contains(lookup) {
                return Err(EvidenceRefusal::UnrequestedEvidence { entry: lookup.entry() });
            }
        }
        Ok(())
    }
}

/// One act's operation-scope ledger, live and at replay alike: the union of the evidence
/// the act's records pin, the pins of the records the act continues, and the atoms the
/// act's decision read — its contexts' asks plus the deterministic gate atoms it answered
/// before deciding. Settled when the act closes: every pinned entry is an inherited pin or
/// answers a collected ask ([`AudienceRegistry::only_requested`]), so a live act cannot
/// pre-load evidence and a log cannot smuggle evidence no operation requested.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ActLedger {
    evidence: AudienceEvidence,
    inherited: AudienceEvidence,
    reads: BTreeSet<SymbolicAtom>,
}

impl ActLedger {
    pub(crate) fn of(evidence: AudienceEvidence, inherited: AudienceEvidence) -> ActLedger {
        ActLedger {
            evidence,
            inherited,
            reads: BTreeSet::new(),
        }
    }

    /// The evidence the act's records pin so far.
    pub(crate) fn evidence(&self) -> &AudienceEvidence {
        &self.evidence
    }

    /// Union one more record's pinned evidence in: the records of one act pin one answer
    /// per key.
    pub(crate) fn pin(&mut self, evidence: &AudienceEvidence) -> Result<(), EvidenceRefusal> {
        self.evidence = evidence.inheriting(&self.evidence)?;
        Ok(())
    }

    /// Count `pins` — entries a record this act continues already pinned — as inherited.
    pub(crate) fn inherit(&mut self, pins: &AudienceEvidence) -> Result<(), EvidenceRefusal> {
        self.inherited = self.inherited.inheriting(pins)?;
        Ok(())
    }

    /// Log atoms the act read: a context's asks after a decision ran over it, or the gate
    /// atoms answered before one.
    pub(crate) fn read(&mut self, atoms: impl IntoIterator<Item = SymbolicAtom>) {
        self.reads.extend(atoms);
    }

    /// The operation-scope test over everything logged.
    pub(crate) fn settle(&self, audience: &AudienceRegistry) -> Result<(), EvidenceRefusal> {
        let reads: Vec<SymbolicAtom> = self.reads.iter().cloned().collect();
        audience.only_requested(&self.evidence, &self.inherited, &reads)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reader(id: &str) -> ReaderId {
        ReaderId::new(id)
    }

    fn spec(provider: &str, selector: &str) -> SelectorSpec {
        SelectorSpec {
            provider: provider.to_string(),
            selector: selector.to_string(),
        }
    }

    fn registry(config: AudienceConfig) -> AudienceRegistry {
        AudienceRegistry::build(&config)
    }

    fn corp_config() -> AudienceConfig {
        AudienceConfig {
            sources: vec![
                SourceRegistration {
                    provider: "google-workspace".into(),
                    templates: vec![
                        SelectorTemplate::new("viewer"),
                        SelectorTemplate::new("full-members"),
                        SelectorTemplate::new("group/<group-address>"),
                    ],
                },
                SourceRegistration {
                    provider: "slack".into(),
                    templates: vec![
                        SelectorTemplate::new("viewer"),
                        SelectorTemplate::new("full-members"),
                        SelectorTemplate::new("user-group/<handle>"),
                    ],
                },
            ],
            self_from: vec![spec("google-workspace", "viewer"), spec("slack", "viewer")],
            internal_from: vec![spec("google-workspace", "full-members"), spec("slack", "full-members")],
            groups: vec![NamedAudience {
                name: GroupName::new("finance"),
                within: Some(ChainAudience::Internal),
                from: vec![spec("google-workspace", "group/finance@corp.com")],
            }],
            lookup_targets: BTreeMap::new(),
        }
    }

    fn slack(selector: &str, members: &[&str]) -> SourceClaims {
        SourceClaims {
            provider: "slack".into(),
            selector: selector.into(),
            members: members.iter().map(|member| reader(member)).collect(),
        }
    }

    fn sources(sources: Vec<SourceClaims>) -> AudienceEvidence {
        AudienceEvidence {
            sources,
            ..AudienceEvidence::default()
        }
    }

    fn lookup(provider: &str, member: &str, principal: Option<&str>) -> MemberLookup {
        MemberLookup {
            provider: provider.into(),
            member: member.into(),
            principal: principal.map(reader),
        }
    }

    /// The ledger's verdict reads sets: pinned one act at a time as the live engine does,
    /// or one record and one ask at a time as replay does, the settlement is the same, and
    /// it is exactly "every pin is inherited or answers a read".
    mod ledger {
        use super::*;
        use proptest::prelude::*;

        /// The corp registry's six selectors: which atoms request each is the fixture's
        /// own statement of the configuration, independent of `needed_primitives`.
        const SELECTORS: [(&str, &str); 6] = [
            ("google-workspace", "viewer"),
            ("google-workspace", "full-members"),
            ("slack", "viewer"),
            ("slack", "full-members"),
            ("google-workspace", "group/finance@corp.com"),
            ("slack", "user-group/a"),
        ];

        fn claims(selector: usize) -> SourceClaims {
            let (provider, selector) = SELECTORS[selector];
            SourceClaims {
                provider: provider.to_string(),
                selector: selector.to_string(),
                members: vec![reader(&format!("{provider}:u1"))],
            }
        }

        fn evidence(selectors: impl IntoIterator<Item = usize>) -> AudienceEvidence {
            AudienceEvidence {
                sources: selectors.into_iter().map(claims).collect(),
                ..AudienceEvidence::default()
            }
        }

        fn atom(index: usize) -> (SymbolicAtom, &'static [usize]) {
            match index {
                0 => (SymbolicAtom::Chain(ChainAudience::Self_), &[0, 2]),
                // internal closes over self and every group asserted within it.
                1 => (SymbolicAtom::Chain(ChainAudience::Internal), &[0, 1, 2, 3, 4]),
                2 => (SymbolicAtom::Group(GroupRef::Named(GroupName::new("finance"))), &[4]),
                _ => (
                    SymbolicAtom::Group(GroupRef::Source {
                        provider: "slack".into(),
                        selector: "user-group/a".into(),
                    }),
                    &[5],
                ),
            }
        }

        proptest! {
            #[test]
            fn settlement_is_the_same_live_and_record_by_record(
                pins in prop::collection::btree_set(0usize..6, 0..5),
                inherited in prop::collection::btree_set(0usize..6, 0..4),
                reads in prop::collection::btree_set(0usize..4, 0..4),
            ) {
                let registry = registry(corp_config());
                let mut live = ActLedger::of(evidence(pins.clone()), evidence(inherited.clone()));
                live.read(reads.iter().map(|index| atom(*index).0));

                let mut replay = ActLedger::default();
                for pin in pins.iter().rev() {
                    replay.pin(&evidence([*pin])).expect("one act pins one answer per key");
                }
                for pin in inherited.iter().rev() {
                    replay.inherit(&evidence([*pin])).expect("one chain pins one answer per key");
                }
                for read in reads.iter().rev() {
                    replay.read([atom(*read).0]);
                }
                // The verdict is the invariant; which unrequested entry a refusal names
                // first follows pin order, which record-by-record replay reverses.
                let live = live.settle(&registry);
                let replay = replay.settle(&registry);
                prop_assert_eq!(live.is_ok(), replay.is_ok());

                let requested: BTreeSet<usize> = reads.iter().flat_map(|index| atom(*index).1.iter().copied()).collect();
                let justified = pins.iter().all(|pin| inherited.contains(pin) || requested.contains(pin));
                prop_assert_eq!(live.is_ok(), justified);
            }
        }
    }

    #[test]
    fn a_reported_reader_is_an_address_or_the_sources_own_qualified_id() {
        // An address is the principal itself: a reader written as one — by a policy, by a
        // tool argument, by a source — is one reader, with only the domain case folded.
        for good in [
            "alice@corp.com",
            "Alice@CORP.com",
            "a.lice@corp.com",
            "alice+x@corp.com",
            "slack:U012345",
            "slack:alice@corp.com",
        ] {
            assert!(well_formed_reader("slack", &reader(good)), "{good}");
        }
        assert_eq!(reader("Alice@CORP.com"), reader("Alice@corp.com"));
        assert_ne!(
            reader("alice@corp.com"),
            reader("Alice@corp.com"),
            "the local part keeps its case"
        );
        assert_eq!(reader("slack:U1").as_str(), "slack:U1");

        // Everything else: a bare name, a reserved spelling, a group mark, a malformed
        // address, another provider's namespace, a bare prefix.
        for bad in [
            "finance",
            "id63234",
            "public",
            "self",
            "internal",
            "@finance",
            "",
            "two@at@signs",
            "@corp.com",
            "alice@",
            "a b@corp.com",
            "github:alice",
            "github:alice@corp.com",
            "slack:",
        ] {
            assert!(!well_formed_reader("slack", &reader(bad)), "{bad:?}");
        }
    }

    #[test]
    fn templates_match_segment_wise() {
        let template = SelectorTemplate::new("org/<org>/team/<team>");
        assert!(template.matches("org/archestra-ai/team/finance"));
        assert!(!template.matches("org/archestra-ai/team"));
        assert!(!template.matches("org/archestra-ai/team/"));
        assert!(!template.matches("org/archestra-ai/members"));
        assert!(SelectorTemplate::new("viewer").matches("viewer"));
        assert!(!SelectorTemplate::new("viewer").matches("full-members"));
    }

    #[test]
    fn needed_primitives_follow_the_symmetric_closure() {
        let registry = registry(corp_config());
        let needed = registry
            .needed_primitives(&[SymbolicAtom::Chain(ChainAudience::Internal)])
            .unwrap();
        // internal reads its own sources, self's (symmetric closure), and finance's
        // (within = internal).
        assert!(needed.selectors.contains(&spec("google-workspace", "full-members")));
        assert!(needed.selectors.contains(&spec("slack", "full-members")));
        assert!(needed.selectors.contains(&spec("google-workspace", "viewer")));
        assert!(needed.selectors.contains(&spec("slack", "viewer")));
        assert!(
            needed
                .selectors
                .contains(&spec("google-workspace", "group/finance@corp.com"))
        );
        assert!(needed.lookups.is_empty());

        // A group's own atom reads only its selectors.
        let finance = registry
            .needed_primitives(&[SymbolicAtom::Group(GroupRef::Named(GroupName::new("finance")))])
            .unwrap();
        assert_eq!(
            finance.selectors,
            BTreeSet::from([spec("google-workspace", "group/finance@corp.com")])
        );

        // Only explicitly selected sources are consulted: nothing else appears. (No GitHub
        // source is configured here, so no GitHub org is ever read.)
        assert!(needed.selectors.iter().all(|s| s.provider != "github"));
    }

    #[test]
    fn unroutable_atoms_are_operational() {
        let registry = registry(corp_config());
        assert_eq!(
            registry.needed_primitives(&[SymbolicAtom::Group(GroupRef::Named(GroupName::new("finacne")))]),
            Err(Unroutable::UnknownGroup(GroupName::new("finacne")))
        );
        assert_eq!(
            registry.needed_primitives(&[SymbolicAtom::Group(GroupRef::Source {
                provider: "github".into(),
                selector: "org/x/members".into()
            })]),
            Err(Unroutable::UnknownProvider("github".into()))
        );
        assert_eq!(
            registry.needed_primitives(&[SymbolicAtom::Group(GroupRef::Source {
                provider: "slack".into(),
                selector: "channels/eng".into()
            })]),
            Err(Unroutable::UnknownSelector {
                provider: "slack".into(),
                selector: "channels/eng".into()
            })
        );
    }

    #[test]
    fn expansions_recompute_the_closure_from_primitives() {
        let registry = registry(corp_config());
        let google = |selector: &str, members: &[&str]| SourceClaims {
            provider: "google-workspace".into(),
            selector: selector.into(),
            members: members.iter().map(|member| reader(member)).collect(),
        };
        let evidence = sources(vec![
            google("viewer", &["me@corp.com"]),
            slack("viewer", &["me@corp.com"]),
            google("full-members", &["me@corp.com", "bob@corp.com"]),
            slack("full-members", &["bob@corp.com", "slack:UBOT"]),
            // An external auditor the finance source reports: within is a trusted
            // assertion, so this member is internal — no domain second-guessing.
            google("group/finance@corp.com", &["auditor@consulting.com"]),
        ]);
        let expansions = registry.expansions(&evidence).unwrap();

        // The two viewer accounts collapse to one reader: union dedups.
        assert_eq!(
            expansions.members(&SymbolicAtom::Chain(ChainAudience::Self_)),
            Some(&BTreeSet::from([reader("me@corp.com")]))
        );
        // internal ⊇ self ∪ own sources ∪ finance (within): the auditor is internal, and a
        // member without an address is its qualified id.
        assert_eq!(
            expansions.members(&SymbolicAtom::Chain(ChainAudience::Internal)),
            Some(&BTreeSet::from([
                reader("auditor@consulting.com"),
                reader("bob@corp.com"),
                reader("me@corp.com"),
                reader("slack:UBOT"),
            ]))
        );
        assert_eq!(
            expansions.members(&SymbolicAtom::Group(GroupRef::Named(GroupName::new("finance")))),
            Some(&BTreeSet::from([reader("auditor@consulting.com")]))
        );
    }

    /// Which refusal a case expects.
    type Refuses = fn(&EvidenceRefusal) -> bool;

    #[test]
    fn evidence_validation_refuses_duplicates_and_malformed_readers() {
        let registry = registry(corp_config());
        let refused: [(&str, AudienceEvidence, Refuses); 7] = [
            (
                "one selector answered twice",
                sources(vec![slack("viewer", &[]), slack("viewer", &["slack:U1"])]),
                |refusal| matches!(refusal, EvidenceRefusal::DuplicateSelector { .. }),
            ),
            (
                "a member outside the provider's namespace",
                sources(vec![slack("viewer", &["github:alice"])]),
                |refusal| matches!(refusal, EvidenceRefusal::MalformedMember { .. }),
            ),
            (
                "a bare name as a member",
                sources(vec![slack("viewer", &["finance"])]),
                |refusal| matches!(refusal, EvidenceRefusal::MalformedMember { .. }),
            ),
            (
                "a reserved spelling as a member",
                sources(vec![slack("viewer", &["internal"])]),
                |refusal| matches!(refusal, EvidenceRefusal::MalformedMember { .. }),
            ),
            (
                "one member twice under one selector",
                sources(vec![slack("viewer", &["a@corp.com", "a@corp.com"])]),
                |refusal| matches!(refusal, EvidenceRefusal::DuplicateMember { .. }),
            ),
            (
                "a lookup under a provider that does not own the member",
                AudienceEvidence {
                    lookups: vec![lookup("slack", "google-workspace:alice", Some("alice@corp.com"))],
                    ..AudienceEvidence::default()
                },
                |refusal| matches!(refusal, EvidenceRefusal::ForeignLookup { .. }),
            ),
            (
                "a lookup naming a principal outside both shapes",
                AudienceEvidence {
                    lookups: vec![lookup("slack", "slack:U1", Some("github:alice"))],
                    ..AudienceEvidence::default()
                },
                |refusal| matches!(refusal, EvidenceRefusal::MalformedPrincipal { .. }),
            ),
        ];
        for (case, evidence, expected) in refused {
            match registry.expansions(&evidence) {
                Err(refusal) => assert!(expected(&refusal), "{case}: {refusal:?}"),
                Ok(_) => panic!("{case}: admitted"),
            }
        }

        // One address under two domain cases is one reader.
        let two_cases = sources(vec![
            slack("viewer", &["Alice@CORP.com"]),
            slack("full-members", &["Alice@corp.com"]),
        ]);
        let expansions = registry.expansions(&two_cases).unwrap();
        let slack_viewer = SymbolicAtom::Group(GroupRef::Source {
            provider: "slack".into(),
            selector: "viewer".into(),
        });
        assert_eq!(
            expansions.members(&slack_viewer),
            Some(&BTreeSet::from([reader("Alice@corp.com")]))
        );
    }

    #[test]
    fn lookups_canonicalize_and_not_found_is_definitive() {
        let registry = registry(corp_config());
        let evidence = AudienceEvidence {
            lookups: vec![
                lookup("slack", "slack:U012345", Some("alice@corp.com")),
                lookup("slack", "slack:UGONE", None),
            ],
            ..AudienceEvidence::default()
        };
        let expansions = registry.expansions(&evidence).unwrap();
        assert_eq!(
            expansions.principal(&reader("slack:U012345")),
            Some(&reader("alice@corp.com"))
        );
        assert_eq!(
            expansions.principal(&reader("slack:UGONE")),
            Some(&reader("slack:UGONE")),
            "not found keeps the reader as written"
        );
    }

    #[test]
    fn a_redirected_provider_seats_its_qualified_members_through_pinned_lookups() {
        let mut config = corp_config();
        config.lookup_targets.insert("slack".into(), "people".into());
        let redirected = registry(config);
        let viewer = SymbolicAtom::Group(GroupRef::Source {
            provider: "slack".into(),
            selector: "viewer".into(),
        });

        // Until every qualified member is looked up, the selector is unanswered and the
        // owed lookups are exactly the qualified members without a pin.
        let unmapped = sources(vec![slack("viewer", &["slack:U1", "bob@corp.com"])]);
        assert_eq!(
            redirected.member_lookups_owed(&unmapped),
            BTreeSet::from([LookupSpec {
                provider: "slack".into(),
                member: "slack:U1".into()
            }])
        );
        let expansions = redirected.expansions(&unmapped).unwrap();
        assert_eq!(expansions.members(&viewer), None);
        assert_eq!(expansions.members(&SymbolicAtom::Chain(ChainAudience::Self_)), None);

        // A pinned principal seats the member; a pinned not-found leaves it as written.
        let mapped = AudienceEvidence {
            lookups: vec![lookup("slack", "slack:U1", Some("alice@corp.com"))],
            ..unmapped.clone()
        };
        assert!(redirected.member_lookups_owed(&mapped).is_empty());
        assert_eq!(
            redirected.expansions(&mapped).unwrap().members(&viewer),
            Some(&BTreeSet::from([reader("alice@corp.com"), reader("bob@corp.com")]))
        );
        let unknown = AudienceEvidence {
            lookups: vec![lookup("slack", "slack:U1", None)],
            ..unmapped.clone()
        };
        assert_eq!(
            redirected.expansions(&unknown).unwrap().members(&viewer),
            Some(&BTreeSet::from([reader("slack:U1"), reader("bob@corp.com")]))
        );

        // The owed lookup is justified by the pinned answer that reports the member, with
        // no atom naming it — and only under a redirected provider.
        redirected
            .only_requested(&mapped, &AudienceEvidence::default(), std::slice::from_ref(&viewer))
            .expect("an owed lookup answers the answer that owes it");
        assert!(
            matches!(
                redirected.only_requested(&mapped, &AudienceEvidence::default(), &[]),
                Err(EvidenceRefusal::UnrequestedEvidence { .. })
            ),
            "without the selector read, neither the answer nor its lookup is requested"
        );
        assert!(
            matches!(
                registry(corp_config()).only_requested(&mapped, &AudienceEvidence::default(), &[viewer]),
                Err(EvidenceRefusal::UnrequestedEvidence { .. })
            ),
            "an unredirected provider's members owe nothing"
        );
    }
}
