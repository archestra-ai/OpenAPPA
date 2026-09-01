//! Audience sources, identity, and the primitive evidence a decision pins.
//!
//! A symbolic audience resolves through registered **audience sources** (one per provider,
//! shipped by batteries) and the deployment's **identity implementation**, which
//! canonicalizes each provider member to one principal. The record pins the PRIMITIVES —
//! per-selector member claims, per-member lookups, and (for a custom identity
//! implementation) id→principal mappings. Identity application, union, and the `within`
//! closure are recomputed deterministically from those primitives at replay, so a live
//! decision and its replay read the same answers, and cross-audience invariants
//! (`@finance ⊆ internal`) hold by construction.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::label::{ChainAudience, Expansions, GroupRef, ReaderId, SymbolicAtom};
use crate::names::{GroupName, IdentityImplementationName};

/// One provider member as its source reports it: the provider-qualified id and, when the
/// provider explicitly verifies one, the member's preferred verified email. Nothing else —
/// display names and usernames are never identity evidence.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberClaims {
    /// Provider-qualified id, e.g. `slack:U012345`. Must carry the prefix of the source that
    /// reported it; evidence validation refuses a cross-provider claim.
    pub id: String,
    /// The provider-verified preferred email, exactly as claimed. Absent is a definitive
    /// state, not an error: the member keeps its qualified identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_email: Option<String>,
}

/// One selector's validated answer from its provider's source, as the record pins it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceClaims {
    pub provider: String,
    pub selector: String,
    pub members: Vec<MemberClaims>,
}

/// One member lookup's pinned answer: the claims the provider reports for one qualified
/// reader, or `None` when the provider does not know it — a definitive answer that leaves
/// the reader its qualified identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberLookup {
    pub provider: String,
    /// The provider-qualified reader that was looked up, e.g. `slack:U012345`.
    pub member: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claims: Option<MemberClaims>,
}

/// One custom identity answer: the principal one provider-qualified id canonicalizes to.
/// Pinned only when the deployment runs a custom identity implementation; the shipped
/// `verified-email` implementation is deterministic and recomputed at replay instead.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct IdentityMapping {
    pub id: String,
    pub principal: ReaderId,
}

impl<'de> Deserialize<'de> for IdentityMapping {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            id: String,
            principal: ReaderId,
        }
        let wire = Wire::deserialize(deserializer)?;
        if !wire.principal.is_literal() {
            return Err(serde::de::Error::custom(format!(
                "identity mapping for {:?} names reserved output {:?}",
                wire.id,
                wire.principal.as_str()
            )));
        }
        Ok(IdentityMapping {
            id: wire.id,
            principal: wire.principal,
        })
    }
}

/// The primitive audience evidence one operation pins: everything its expansions are
/// recomputed from. Duplicate or conflicting entries never validate. Routable answers beyond
/// what an act asks are admissible — they only pre-answer later asks — and replay holds every
/// entry, surplus included, to the same validation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudienceEvidence {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<SourceClaims>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lookups: Vec<MemberLookup>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identity: Vec<IdentityMapping>,
}

impl AudienceEvidence {
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty() && self.lookups.is_empty() && self.identity.is_empty()
    }

    /// Does this evidence carry every entry of `other`? An operation may extend what an
    /// earlier record pinned, never contradict or drop it.
    pub fn contains(&self, other: &AudienceEvidence) -> bool {
        other.sources.iter().all(|claims| self.sources.contains(claims))
            && other.lookups.iter().all(|lookup| self.lookups.contains(lookup))
            && other.identity.iter().all(|mapping| self.identity.contains(mapping))
    }

    /// This act's evidence read under an earlier record's pins: the pinned entries come
    /// first and win, and only answers for keys the pins do not hold are added. A chain of
    /// operations over one value reads each primitive under one answer.
    pub fn inheriting(&self, pinned: &AudienceEvidence) -> AudienceEvidence {
        let mut merged = pinned.clone();
        for claims in &self.sources {
            let held = merged
                .sources
                .iter()
                .any(|entry| entry.provider == claims.provider && entry.selector == claims.selector);
            if !held {
                merged.sources.push(claims.clone());
            }
        }
        for lookup in &self.lookups {
            let held = merged
                .lookups
                .iter()
                .any(|entry| entry.provider == lookup.provider && entry.member == lookup.member);
            if !held {
                merged.lookups.push(lookup.clone());
            }
        }
        for mapping in &self.identity {
            if !merged.identity.iter().any(|entry| entry.id == mapping.id) {
                merged.identity.push(mapping.clone());
            }
        }
        merged
    }
}

/// Why pinned audience evidence is not admissible.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EvidenceRefusal {
    #[error("two answers for selector {provider}:{selector} in one operation")]
    DuplicateSelector { provider: String, selector: String },
    #[error("two lookups for member {member} in one operation")]
    DuplicateLookup { member: String },
    #[error("two identity mappings for {id} in one operation")]
    DuplicateIdentity { id: String },
    #[error("selector {provider}:{selector} reports member {id:?} outside its own provider namespace")]
    ForeignMember {
        provider: String,
        selector: String,
        id: String,
    },
    #[error("lookup under provider {provider} answers for member {member:?} outside that namespace")]
    ForeignLookup { provider: String, member: String },
    #[error("the lookup of member {member:?} under provider {provider} carries claims for a different id")]
    ForeignLookupClaims { provider: String, member: String },
    #[error("selector {provider}:{selector} reports member {id:?} twice in one answer")]
    DuplicateMember {
        provider: String,
        selector: String,
        id: String,
    },
    #[error("member {id:?} carries conflicting verified-email claims in one operation")]
    ConflictingClaims { id: String },
    #[error("identity mapping for {id:?} names a reserved principal")]
    ReservedPrincipal { id: String },
    #[error("member {id:?} claims verified email {email:?}, which does not parse as one address")]
    MalformedEmail { id: String, email: String },
    #[error("identity implementation returned no mapping for {id:?}")]
    UnmappedIdentity { id: String },
    #[error("no registered audience source serves selector {provider}:{selector}")]
    UnroutableSelector { provider: String, selector: String },
    #[error("no registered audience provider {provider} serves the lookup of {member:?}")]
    UnroutableLookup { provider: String, member: String },
}

/// The conservative `verified-email` normalization, shipped and deterministic: a member with
/// a well-formed verified email becomes `email:<local>@<lowercased-domain>`; a member
/// without one keeps its provider-qualified id. Nothing merges identities beyond exact
/// verified-email equality: no dot folding, no `+suffix` stripping, no alias folding, and
/// the local part keeps its case. A malformed claimed email is an invalid answer, never a
/// silent fallback.
pub fn verified_email_principal(claims: &MemberClaims) -> Result<ReaderId, EvidenceRefusal> {
    match &claims.verified_email {
        None => Ok(ReaderId::new(claims.id.clone())),
        Some(email) => {
            let malformed = || EvidenceRefusal::MalformedEmail {
                id: claims.id.clone(),
                email: email.clone(),
            };
            let (local, domain) = email.split_once('@').ok_or_else(malformed)?;
            if local.is_empty()
                || domain.is_empty()
                || domain.contains('@')
                || email.chars().any(|c| c.is_whitespace() || c.is_control())
            {
                return Err(malformed());
            }
            Ok(ReaderId::new(format!("email:{}@{}", local, domain.to_lowercase())))
        }
    }
}

/// The deployment's identity implementation, as the registry holds it. `VerifiedEmail` is
/// the shipped default — deterministic and network-free, recomputed at replay. A custom
/// implementation answers through the external-binding pattern and its mappings are pinned.
#[derive(Default, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum IdentityImplementation {
    #[default]
    VerifiedEmail,
    Custom(IdentityImplementationName),
}

impl IdentityImplementation {
    pub const VERIFIED_EMAIL: &'static str = "verified-email";

    pub fn name(&self) -> &str {
        match self {
            IdentityImplementation::VerifiedEmail => Self::VERIFIED_EMAIL,
            IdentityImplementation::Custom(name) => name.as_str(),
        }
    }
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

/// One configured named audience: `[[audience.group]] name / within / from`.
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
/// how a deployment *reaches* a source (URL, command, credentials) never does.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<IdentityImplementation>,
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
    identity: IdentityImplementation,
    within: crate::label::WithinAssertions,
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

/// The primitive requests one round must answer: which selectors to consult and which member
/// lookups to perform. Identity inputs are derived from the claims those answers carry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NeededPrimitives {
    pub selectors: BTreeSet<SelectorSpec>,
    pub lookups: BTreeSet<SelectorSpec>,
}

impl NeededPrimitives {
    pub fn is_empty(&self) -> bool {
        self.selectors.is_empty() && self.lookups.is_empty()
    }
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
            identity: config.identity.clone().unwrap_or_default(),
            within: crate::label::WithinAssertions::new(
                config
                    .groups
                    .iter()
                    .filter_map(|group| group.within.map(|target| (group.name.clone(), target))),
            ),
        }
    }

    /// The registered provider names — what decides which qualified readers canonicalize.
    pub fn providers(&self) -> &BTreeSet<String> {
        &self.provider_names
    }

    pub fn templates(&self, provider: &str) -> Option<&[SelectorTemplate]> {
        self.providers.get(provider).map(Vec::as_slice)
    }

    pub fn identity(&self) -> &IdentityImplementation {
        &self.identity
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
    pub fn within_assertions(&self) -> &crate::label::WithinAssertions {
        &self.within
    }

    /// Every group asserted within `level` — the members the symmetric closure folds in.
    fn groups_within(&self, level: ChainAudience) -> impl Iterator<Item = &NamedAudience> {
        self.groups.values().filter(move |group| group.within == Some(level))
    }

    fn route_selector(&self, provider: &str, selector: &str) -> Result<SelectorSpec, Unroutable> {
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
                    needed.lookups.insert(SelectorSpec {
                        provider: provider.to_string(),
                        selector: reader.as_str().to_string(),
                    });
                }
            }
        }
        Ok(needed)
    }

    /// Build the operation's expansions from its pinned primitives: validate, canonicalize
    /// every member to its principal, union per selector, and close the chain levels
    /// symmetrically. Answers exist only for atoms whose primitives are all present — a
    /// check that still misses one re-raises its ask. Duplicates, cross-provider claims,
    /// unroutable entries, malformed emails, and (under a custom identity) unmapped members
    /// refuse the evidence — the live act and its replay hold it to the same test.
    pub fn expansions(&self, evidence: &AudienceEvidence) -> Result<Expansions, EvidenceRefusal> {
        for claims in &evidence.sources {
            let routable = self
                .templates(&claims.provider)
                .is_some_and(|templates| templates.iter().any(|template| template.matches(&claims.selector)));
            if !routable {
                return Err(EvidenceRefusal::UnroutableSelector {
                    provider: claims.provider.clone(),
                    selector: claims.selector.clone(),
                });
            }
        }
        for lookup in &evidence.lookups {
            if !self.providers().contains(&lookup.provider) {
                return Err(EvidenceRefusal::UnroutableLookup {
                    provider: lookup.provider.clone(),
                    member: lookup.member.clone(),
                });
            }
        }
        let identity = IdentityTable::new(&self.identity, &evidence.identity)?;

        // One operation-wide claim set per provider id: every occurrence of an id — across
        // selectors and lookups — must carry the same verified email, so one id resolves to
        // exactly one principal in this operation.
        let mut claimed: BTreeMap<&str, &Option<String>> = BTreeMap::new();
        let occurrences = evidence
            .sources
            .iter()
            .flat_map(|claims| claims.members.iter())
            .chain(evidence.lookups.iter().filter_map(|lookup| lookup.claims.as_ref()));
        for claims in occurrences {
            match claimed.insert(claims.id.as_str(), &claims.verified_email) {
                Some(held) if held != &claims.verified_email => {
                    return Err(EvidenceRefusal::ConflictingClaims { id: claims.id.clone() });
                }
                _ => {}
            }
        }

        // Per-selector principal sets, validated.
        let mut selector_members: BTreeMap<SelectorSpec, BTreeSet<ReaderId>> = BTreeMap::new();
        for claims in &evidence.sources {
            let spec = SelectorSpec {
                provider: claims.provider.clone(),
                selector: claims.selector.clone(),
            };
            let mut members = BTreeSet::new();
            let mut ids = BTreeSet::new();
            for member in &claims.members {
                if ReaderId::new(member.id.clone()).provider_prefix() != Some(claims.provider.as_str()) {
                    return Err(EvidenceRefusal::ForeignMember {
                        provider: claims.provider.clone(),
                        selector: claims.selector.clone(),
                        id: member.id.clone(),
                    });
                }
                // One id may not appear twice under one selector: the second entry could
                // carry a conflicting verified email and seat a second principal.
                if !ids.insert(member.id.as_str()) {
                    return Err(EvidenceRefusal::DuplicateMember {
                        provider: claims.provider.clone(),
                        selector: claims.selector.clone(),
                        id: member.id.clone(),
                    });
                }
                members.insert(identity.principal(member)?);
            }
            if selector_members.insert(spec.clone(), members).is_some() {
                return Err(EvidenceRefusal::DuplicateSelector {
                    provider: spec.provider,
                    selector: spec.selector,
                });
            }
        }

        let mut answers: Vec<(SymbolicAtom, BTreeSet<ReaderId>)> = Vec::new();

        // Reader canonicalizations from lookups.
        let mut seen_lookups = BTreeSet::new();
        for lookup in &evidence.lookups {
            if !seen_lookups.insert(lookup.member.clone()) {
                return Err(EvidenceRefusal::DuplicateLookup {
                    member: lookup.member.clone(),
                });
            }
            if ReaderId::new(lookup.member.clone()).provider_prefix() != Some(lookup.provider.as_str()) {
                return Err(EvidenceRefusal::ForeignLookup {
                    provider: lookup.provider.clone(),
                    member: lookup.member.clone(),
                });
            }
            let principal = match &lookup.claims {
                // Not found is definitive: the reader keeps its qualified identity.
                None => ReaderId::new(lookup.member.clone()),
                // Claims for another id would let a source canonicalize a member it does
                // not own, or pre-seat an identity mapping for it.
                Some(claims) if claims.id != lookup.member => {
                    return Err(EvidenceRefusal::ForeignLookupClaims {
                        provider: lookup.provider.clone(),
                        member: lookup.member.clone(),
                    });
                }
                Some(claims) => identity.principal(claims)?,
            };
            answers.push((
                SymbolicAtom::Reader(ReaderId::new(lookup.member.clone())),
                BTreeSet::from([principal]),
            ));
        }

        // Source-qualified selector atoms answer directly.
        for (spec, members) in &selector_members {
            answers.push((
                SymbolicAtom::Group(GroupRef::Source {
                    provider: spec.provider.clone(),
                    selector: spec.selector.clone(),
                }),
                members.clone(),
            ));
        }

        // Named audiences: the union of their selectors, when every one is answered.
        let union_of = |specs: &BTreeSet<SelectorSpec>| -> Option<BTreeSet<ReaderId>> {
            let mut union = BTreeSet::new();
            for spec in specs {
                union.extend(selector_members.get(spec)?.iter().cloned());
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

        Ok(Expansions::new(answers))
    }
}

/// The identity implementation applied to one operation's claims: the shipped normalization
/// recomputed, or a custom implementation's pinned mappings looked up.
struct IdentityTable<'a> {
    implementation: &'a IdentityImplementation,
    mappings: BTreeMap<&'a str, &'a ReaderId>,
}

impl<'a> IdentityTable<'a> {
    fn new(
        implementation: &'a IdentityImplementation,
        pinned: &'a [IdentityMapping],
    ) -> Result<IdentityTable<'a>, EvidenceRefusal> {
        let mut mappings: BTreeMap<&str, &ReaderId> = BTreeMap::new();
        for mapping in pinned {
            // Deserialization refuses reserved principals, but a mapping built in process
            // (a buggy custom implementation) must fail here, not when the record it was
            // pinned into refuses to decode: the live act and its replay hold one test.
            if !mapping.principal.is_literal() {
                return Err(EvidenceRefusal::ReservedPrincipal { id: mapping.id.clone() });
            }
            if mappings.insert(mapping.id.as_str(), &mapping.principal).is_some() {
                return Err(EvidenceRefusal::DuplicateIdentity { id: mapping.id.clone() });
            }
        }
        Ok(IdentityTable {
            implementation,
            mappings,
        })
    }

    fn principal(&self, claims: &MemberClaims) -> Result<ReaderId, EvidenceRefusal> {
        match self.implementation {
            IdentityImplementation::VerifiedEmail => verified_email_principal(claims),
            IdentityImplementation::Custom(_) => self
                .mappings
                .get(claims.id.as_str())
                .map(|principal| (*principal).clone())
                .ok_or_else(|| EvidenceRefusal::UnmappedIdentity { id: claims.id.clone() }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(id: &str, email: Option<&str>) -> MemberClaims {
        MemberClaims {
            id: id.to_string(),
            verified_email: email.map(str::to_string),
        }
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
            identity: None,
        }
    }

    #[test]
    fn verified_email_is_conservative() {
        // Same verified corporate email from two providers: one principal.
        let gw = member("google-workspace:alice@corp.com", Some("alice@corp.com"));
        let slack = member("slack:U012345", Some("alice@corp.com"));
        assert_eq!(
            verified_email_principal(&gw).unwrap(),
            verified_email_principal(&slack).unwrap()
        );
        assert_eq!(verified_email_principal(&gw).unwrap().as_str(), "email:alice@corp.com");

        // A personal email stays a different principal — no corporate guessing.
        let github = member("github:alice", Some("alice@gmail.com"));
        assert_ne!(
            verified_email_principal(&github).unwrap(),
            verified_email_principal(&gw).unwrap()
        );

        // No verified email: the qualified identity stands.
        assert_eq!(
            verified_email_principal(&member("github:alice", None))
                .unwrap()
                .as_str(),
            "github:alice"
        );

        // Domain case folds; the local part never does, and no dots or +suffixes fold.
        assert_eq!(
            verified_email_principal(&member("x:1", Some("Alice@CORP.com")))
                .unwrap()
                .as_str(),
            "email:Alice@corp.com"
        );
        assert_ne!(
            verified_email_principal(&member("x:1", Some("a.lice@corp.com"))).unwrap(),
            verified_email_principal(&member("x:2", Some("alice@corp.com"))).unwrap()
        );
        assert_ne!(
            verified_email_principal(&member("x:1", Some("alice+x@corp.com"))).unwrap(),
            verified_email_principal(&member("x:2", Some("alice@corp.com"))).unwrap()
        );

        // A malformed claimed email is an invalid answer, not a fallback.
        for bad in ["nodomain", "two@at@signs", "@corp.com", "alice@", "a b@corp.com"] {
            assert!(verified_email_principal(&member("x:1", Some(bad))).is_err(), "{bad}");
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
        let evidence = AudienceEvidence {
            sources: vec![
                SourceClaims {
                    provider: "google-workspace".into(),
                    selector: "viewer".into(),
                    members: vec![member("google-workspace:me@corp.com", Some("me@corp.com"))],
                },
                SourceClaims {
                    provider: "slack".into(),
                    selector: "viewer".into(),
                    members: vec![member("slack:U0ME", Some("me@corp.com"))],
                },
                SourceClaims {
                    provider: "google-workspace".into(),
                    selector: "full-members".into(),
                    members: vec![
                        member("google-workspace:me@corp.com", Some("me@corp.com")),
                        member("google-workspace:bob@corp.com", Some("bob@corp.com")),
                    ],
                },
                SourceClaims {
                    provider: "slack".into(),
                    selector: "full-members".into(),
                    members: vec![member("slack:U0BOB", Some("bob@corp.com"))],
                },
                SourceClaims {
                    provider: "google-workspace".into(),
                    selector: "group/finance@corp.com".into(),
                    members: vec![
                        // An external auditor the finance source reports: within is a trusted
                        // assertion, so this member is internal — no domain second-guessing.
                        member(
                            "google-workspace:auditor@consulting.com",
                            Some("auditor@consulting.com"),
                        ),
                    ],
                },
            ],
            lookups: vec![],
            identity: vec![],
        };
        let expansions = registry.expansions(&evidence).unwrap();
        let reader = |s: &str| ReaderId::new(s);

        // The two viewer accounts collapse to one principal: union dedups.
        assert_eq!(
            expansions.members(&SymbolicAtom::Chain(ChainAudience::Self_)),
            Some(&BTreeSet::from([reader("email:me@corp.com")]))
        );
        // internal ⊇ self ∪ own sources ∪ finance (within): the auditor is internal.
        assert_eq!(
            expansions.members(&SymbolicAtom::Chain(ChainAudience::Internal)),
            Some(&BTreeSet::from([
                reader("email:auditor@consulting.com"),
                reader("email:bob@corp.com"),
                reader("email:me@corp.com"),
            ]))
        );
        assert_eq!(
            expansions.members(&SymbolicAtom::Group(GroupRef::Named(GroupName::new("finance")))),
            Some(&BTreeSet::from([reader("email:auditor@consulting.com")]))
        );
    }

    #[test]
    fn evidence_validation_refuses_duplicates_and_foreign_claims() {
        let registry = registry(corp_config());
        let twice = AudienceEvidence {
            sources: vec![
                SourceClaims {
                    provider: "slack".into(),
                    selector: "viewer".into(),
                    members: vec![],
                },
                SourceClaims {
                    provider: "slack".into(),
                    selector: "viewer".into(),
                    members: vec![member("slack:U1", None)],
                },
            ],
            ..AudienceEvidence::default()
        };
        assert!(matches!(
            registry.expansions(&twice),
            Err(EvidenceRefusal::DuplicateSelector { .. })
        ));

        let foreign = AudienceEvidence {
            sources: vec![SourceClaims {
                provider: "slack".into(),
                selector: "viewer".into(),
                members: vec![member("github:alice", None)],
            }],
            ..AudienceEvidence::default()
        };
        assert!(matches!(
            registry.expansions(&foreign),
            Err(EvidenceRefusal::ForeignMember { .. })
        ));

        let malformed = AudienceEvidence {
            sources: vec![SourceClaims {
                provider: "slack".into(),
                selector: "viewer".into(),
                members: vec![member("slack:U1", Some("not-an-email"))],
            }],
            ..AudienceEvidence::default()
        };
        assert!(matches!(
            registry.expansions(&malformed),
            Err(EvidenceRefusal::MalformedEmail { .. })
        ));

        // One member twice under one selector could seat two principals for one id.
        let doubled = AudienceEvidence {
            sources: vec![SourceClaims {
                provider: "slack".into(),
                selector: "viewer".into(),
                members: vec![
                    member("slack:U1", Some("a@corp.com")),
                    member("slack:U1", Some("a@corp.com")),
                ],
            }],
            ..AudienceEvidence::default()
        };
        assert!(matches!(
            registry.expansions(&doubled),
            Err(EvidenceRefusal::DuplicateMember { .. })
        ));

        // A bare provider prefix names no member: it is not inside the provider's namespace.
        let bare = AudienceEvidence {
            sources: vec![SourceClaims {
                provider: "slack".into(),
                selector: "viewer".into(),
                members: vec![member("slack:", None)],
            }],
            ..AudienceEvidence::default()
        };
        assert!(matches!(
            registry.expansions(&bare),
            Err(EvidenceRefusal::ForeignMember { .. })
        ));

        // One id, one operation-wide claim set: the same member reported with different
        // verified emails across two selectors would resolve to two principals.
        let conflicting = AudienceEvidence {
            sources: vec![
                SourceClaims {
                    provider: "slack".into(),
                    selector: "viewer".into(),
                    members: vec![member("slack:U1", Some("a@corp.com"))],
                },
                SourceClaims {
                    provider: "slack".into(),
                    selector: "full-members".into(),
                    members: vec![member("slack:U1", Some("b@corp.com"))],
                },
            ],
            ..AudienceEvidence::default()
        };
        assert!(matches!(
            registry.expansions(&conflicting),
            Err(EvidenceRefusal::ConflictingClaims { .. })
        ));
        // The same claims twice are consistent, not conflicting.
        let agreeing = AudienceEvidence {
            sources: vec![
                SourceClaims {
                    provider: "slack".into(),
                    selector: "viewer".into(),
                    members: vec![member("slack:U1", Some("a@corp.com"))],
                },
                SourceClaims {
                    provider: "slack".into(),
                    selector: "full-members".into(),
                    members: vec![member("slack:U1", Some("a@corp.com"))],
                },
            ],
            ..AudienceEvidence::default()
        };
        assert!(registry.expansions(&agreeing).is_ok());

        // A lookup's claims must be for the member asked — a source may not canonicalize a
        // member it does not own.
        let hijack = AudienceEvidence {
            lookups: vec![MemberLookup {
                provider: "slack".into(),
                member: "slack:U1".into(),
                claims: Some(member("google-workspace:alice", Some("alice@corp.com"))),
            }],
            ..AudienceEvidence::default()
        };
        assert!(matches!(
            registry.expansions(&hijack),
            Err(EvidenceRefusal::ForeignLookupClaims { .. })
        ));

        // A Rust-constructed identity mapping with a reserved principal fails validation,
        // not the eventual decode of the record it was pinned into.
        let mut custom = corp_config();
        custom.identity = Some(IdentityImplementation::Custom(IdentityImplementationName::new(
            "corp-identity",
        )));
        let reserved = AudienceEvidence {
            identity: vec![IdentityMapping {
                id: "slack:U1".into(),
                principal: ReaderId::new("internal"),
            }],
            ..AudienceEvidence::default()
        };
        assert!(matches!(
            AudienceRegistry::build(&custom).expansions(&reserved),
            Err(EvidenceRefusal::ReservedPrincipal { .. })
        ));
    }

    #[test]
    fn lookups_canonicalize_and_not_found_is_definitive() {
        let registry = registry(corp_config());
        let evidence = AudienceEvidence {
            lookups: vec![
                MemberLookup {
                    provider: "slack".into(),
                    member: "slack:U012345".into(),
                    claims: Some(member("slack:U012345", Some("alice@corp.com"))),
                },
                MemberLookup {
                    provider: "slack".into(),
                    member: "slack:UGONE".into(),
                    claims: None,
                },
            ],
            ..AudienceEvidence::default()
        };
        let expansions = registry.expansions(&evidence).unwrap();
        assert_eq!(
            expansions.members(&SymbolicAtom::Reader(ReaderId::new("slack:U012345"))),
            Some(&BTreeSet::from([ReaderId::new("email:alice@corp.com")]))
        );
        assert_eq!(
            expansions.members(&SymbolicAtom::Reader(ReaderId::new("slack:UGONE"))),
            Some(&BTreeSet::from([ReaderId::new("slack:UGONE")])),
            "not found keeps the qualified identity"
        );
    }

    #[test]
    fn a_custom_identity_reads_pinned_mappings_only() {
        let mut config = corp_config();
        config.identity = Some(IdentityImplementation::Custom(IdentityImplementationName::new(
            "corp-identity",
        )));
        let registry = registry(config);
        let claims = SourceClaims {
            provider: "slack".into(),
            selector: "viewer".into(),
            members: vec![member("slack:U1", Some("who@corp.com"))],
        };
        let unmapped = AudienceEvidence {
            sources: vec![claims.clone()],
            ..AudienceEvidence::default()
        };
        assert!(matches!(
            registry.expansions(&unmapped),
            Err(EvidenceRefusal::UnmappedIdentity { .. })
        ));

        let mapped = AudienceEvidence {
            sources: vec![claims],
            identity: vec![IdentityMapping {
                id: "slack:U1".into(),
                principal: ReaderId::new("corp:alice"),
            }],
            ..AudienceEvidence::default()
        };
        let expansions = registry.expansions(&mapped).unwrap();
        assert_eq!(
            expansions.members(&SymbolicAtom::Group(GroupRef::Source {
                provider: "slack".into(),
                selector: "viewer".into()
            })),
            Some(&BTreeSet::from([ReaderId::new("corp:alice")]))
        );

        // A reserved principal never deserializes.
        for reserved in ["public", "self", "internal", "@finance"] {
            let wire = serde_json::json!({ "id": "slack:U1", "principal": reserved });
            assert!(serde_json::from_value::<IdentityMapping>(wire).is_err(), "{reserved}");
        }
    }
}
