//! Labels: the two-dimensional restrictive lattice APPA folds and checks.
//!
//! The audience dimension is symbolic: a canonical intersection of union clauses, where a
//! clause names a built-in chain audience (`self` ⊆ `internal` ⊆ `public`), configured or
//! source-qualified groups, and literal readers. Symbols survive in labels and durable
//! events; a check answers from the derivability calculus where policy-declared facts
//! suffice, and otherwise evaluates the exact denotation from the operation's pinned
//! membership answers. Permanent canonicalization uses only policy-independent facts —
//! clause dedup, the built-in chain, and exact reader-set operations — so label equality
//! and serialization never depend on a deployment's `within` assertions or on any mutable
//! directory answer.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::names::GroupName;

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

/// A literal reader identity — an opaque atom to the pure algebra. Restricted audiences
/// intersect and compare readers exactly, so equality is exact string equality; a
/// provider-qualified reader (`slack:U012345`) is canonicalized to its principal by the
/// deployment's identity implementation *before* it reaches a comparison, and the pinned
/// principal is what an audience holds. An email address is that principal already: a
/// reader written as one denotes the same person a verified-email claim resolves to, so
/// `alice@corp.com` in a policy, in a tool argument, and behind a directory's verified
/// claim are one reader. Four spellings are reserved and never readers: `public` (the
/// universal audience state), `self` and `internal` (the built-in chain), and any leading
/// `@` (a group reference). The constructor cannot enforce that, so the rule is
/// [`is_literal`](ReaderId::is_literal), applied on every ingress that builds a reader set:
/// registry declarations at load, annotation answers, and membership answers.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ReaderId(String);

/// Every reader that arrives as data — a persisted event at replay, an external's answer,
/// an API payload — carries the same one spelling per identity as a constructed one.
/// Deserializing straight into the field would let `alice@CORP.com` off the wire compare
/// as a second reader.
impl<'de> Deserialize<'de> for ReaderId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<ReaderId, D::Error> {
        Ok(ReaderId::new(String::deserialize(deserializer)?))
    }
}

impl ReaderId {
    /// Build a reader, normalized so that one identity has one spelling: an address keeps
    /// its local part exactly and lowercases its domain, because a domain is
    /// case-insensitive and a local part is not. Every other spelling is untouched — no
    /// dot folding, no `+suffix` stripping, no alias folding.
    pub fn new(id: impl Into<String>) -> Self {
        let id: String = id.into();
        match address_parts(&id) {
            Some((local, domain)) => ReaderId(format!("{local}@{}", domain.to_lowercase())),
            None => ReaderId(id),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// A literal reader ID: `public`, `self`, and `internal` are reserved audience states —
    /// never readers — and the `@` mark is reserved for group references. Every ingress that
    /// builds a reader set applies this rule.
    pub fn is_literal(&self) -> bool {
        !matches!(self.0.as_str(), "public" | "self" | "internal") && !self.0.starts_with('@')
    }

    /// The provider prefix of a qualified reader (`slack:U012345` → `slack`), when one exists.
    /// Only a prefix naming a *registered* audience source routes the reader through
    /// canonicalization; every other spelling compares exactly as written. A bare prefix with
    /// nothing after the `:` names no member, so it is not qualified: it denotes itself. This
    /// is the one qualification rule; every namespace check derives from it.
    pub fn provider_prefix(&self) -> Option<&str> {
        self.0
            .split_once(':')
            .filter(|(provider, rest)| !provider.is_empty() && !rest.is_empty())
            .map(|(provider, _)| provider)
    }

    /// A reader that denotes itself under *every* deployment: it carries no provider prefix
    /// a source could own. Email principals qualify — an address holds no `:` — which is
    /// why a directory's verified claim and a policy-written address compare directly. Only
    /// stable readers may participate in permanent canonicalization's exact intersection: a
    /// qualified reader's meaning is an operation-pinned principal, and canonical label
    /// equality must never depend on it.
    fn is_stable(&self) -> bool {
        self.provider_prefix().is_none()
    }
}

/// The local part and domain of a reader written as one address: exactly one `@`, neither
/// side empty, no whitespace or control character. Deliberately shape-only — it decides how
/// a reader is *spelled*, never whether an address exists or who owns it.
pub(crate) fn address_parts(id: &str) -> Option<(&str, &str)> {
    let (local, domain) = id.split_once('@')?;
    let malformed = local.is_empty()
        || domain.is_empty()
        || domain.contains('@')
        || id.chars().any(|c| c.is_whitespace() || c.is_control());
    (!malformed).then_some((local, domain))
}

/// A built-in chain audience below `public`: `self` ⊆ `internal` ⊆ `public`. The chain is
/// shipped and fixed — deployments map sources into its levels but never add levels. `public`
/// is not a member: it is the universal audience state, represented by the absence of any
/// constraint, exactly as it is for reader sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ChainAudience {
    /// The innermost audience: the deployment's configured operating principal — whoever its
    /// credentials represent, which need not be a person — extensionally the union of the
    /// configured `viewer` sources.
    Self_,
    /// The organization: extensionally the union of the configured `internal` sources, the
    /// members of `self`, and every group declared `within` either.
    Internal,
}

impl ChainAudience {
    pub fn as_str(self) -> &'static str {
        match self {
            ChainAudience::Self_ => "self",
            ChainAudience::Internal => "internal",
        }
    }

    pub fn parse(token: &str) -> Option<ChainAudience> {
        match token {
            "self" => Some(ChainAudience::Self_),
            "internal" => Some(ChainAudience::Internal),
            _ => None,
        }
    }
}

impl Serialize for ChainAudience {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ChainAudience {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let token = String::deserialize(deserializer)?;
        ChainAudience::parse(&token)
            .ok_or_else(|| serde::de::Error::custom(format!("{token:?} is not a chain audience")))
    }
}

impl std::fmt::Display for ChainAudience {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A group reference: a configured named audience (`@finance`) or a source-qualified selector
/// (`@google-workspace:group/finance@corp.com`) that a registered audience source serves
/// without an individual declaration. The spelling after `@` is the one grammar — the same
/// selector vocabulary the `from` mappings use. A named group's spelling never contains `:`;
/// the first `:` splits provider from selector.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GroupRef {
    Named(GroupName),
    Source { provider: String, selector: String },
}

impl GroupRef {
    /// Parse the text after the `@` mark. Empty, a bare provider (`slack:`), or a bare
    /// selector (`:x`) are malformed and read as nothing.
    pub fn parse(after_at: &str) -> Option<GroupRef> {
        if after_at.is_empty() {
            return None;
        }
        match after_at.split_once(':') {
            Some((provider, selector)) => {
                if provider.is_empty() || selector.is_empty() {
                    None
                } else {
                    Some(GroupRef::Source {
                        provider: provider.to_string(),
                        selector: selector.to_string(),
                    })
                }
            }
            None => Some(GroupRef::Named(GroupName::new(after_at))),
        }
    }
}

impl std::fmt::Display for GroupRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GroupRef::Named(name) => write!(f, "{name}"),
            GroupRef::Source { provider, selector } => write!(f, "@{provider}:{selector}"),
        }
    }
}

impl Serialize for GroupRef {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for GroupRef {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let spelled = String::deserialize(deserializer)?;
        let after_at = spelled
            .strip_prefix('@')
            .ok_or_else(|| serde::de::Error::custom(format!("group reference {spelled:?} lacks the @ mark")))?;
        match GroupRef::parse(after_at) {
            Some(GroupRef::Named(name)) if name.as_str().is_empty() => {
                Err(serde::de::Error::custom("empty group name"))
            }
            Some(group) => Ok(group),
            None => Err(serde::de::Error::custom(format!(
                "malformed group reference {spelled:?}"
            ))),
        }
    }
}

/// One symbolic atom a decision may need an extensional answer for: a chain audience, a group
/// reference, or a provider-qualified reader awaiting canonicalization to its principal (its
/// answer is a singleton). This is the identity of a membership question, never of what a
/// record stores — records pin the primitive source and identity answers the question was
/// computed from.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SymbolicAtom {
    Chain(ChainAudience),
    Group(GroupRef),
    Reader(ReaderId),
}

impl std::fmt::Display for SymbolicAtom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SymbolicAtom::Chain(chain) => write!(f, "{chain}"),
            SymbolicAtom::Group(group) => write!(f, "{group}"),
            SymbolicAtom::Reader(reader) => f.write_str(reader.as_str()),
        }
    }
}

/// One union term: the set of readers named by the chain audience, the groups, and the
/// literal readers together. A clause with no atoms is the empty set — nobody — and is a
/// legal, load-bearing state: intersecting disjoint reader lists produces it and
/// canonicalization preserves it. At most one chain atom is representable because a union of
/// chain audiences is their maximum, a built-in fact the constructor applies.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Clause {
    #[serde(skip_serializing_if = "Option::is_none")]
    chain: Option<ChainAudience>,
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    groups: BTreeSet<GroupRef>,
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    readers: BTreeSet<ReaderId>,
}

/// A clause reader must be literal; the reserved spellings are audience states and group
/// marks, never readers.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "{reader:?} is not a literal reader ID — `public`, `self`, and `internal` are audience states, and the `@` mark is reserved for group references"
)]
pub struct NonLiteralReader {
    pub reader: String,
}

impl Clause {
    /// The union of the given atoms. Multiple chain atoms collapse to their maximum. Refuses a
    /// reader with a reserved spelling.
    pub fn new(
        chain: impl IntoIterator<Item = ChainAudience>,
        groups: impl IntoIterator<Item = GroupRef>,
        readers: impl IntoIterator<Item = ReaderId>,
    ) -> Result<Clause, NonLiteralReader> {
        let readers: BTreeSet<ReaderId> = readers.into_iter().collect();
        if let Some(reader) = readers.iter().find(|reader| !reader.is_literal()) {
            return Err(NonLiteralReader {
                reader: reader.as_str().to_string(),
            });
        }
        Ok(Clause {
            chain: chain.into_iter().max(),
            groups: groups.into_iter().collect(),
            readers,
        })
    }

    pub(crate) fn of_readers(readers: BTreeSet<ReaderId>) -> Clause {
        Clause {
            chain: None,
            groups: BTreeSet::new(),
            readers,
        }
    }

    fn empty() -> Clause {
        Clause {
            chain: None,
            groups: BTreeSet::new(),
            readers: BTreeSet::new(),
        }
    }

    /// No atoms: the empty union, denoting nobody.
    pub fn is_empty(&self) -> bool {
        self.chain.is_none() && self.groups.is_empty() && self.readers.is_empty()
    }

    /// Purely literal AND stable: no symbolic atom, and no reader whose meaning any
    /// deployment could canonicalize away. Only these merge by exact intersection.
    fn is_stable_readers(&self) -> bool {
        self.chain.is_none() && self.groups.is_empty() && self.readers.iter().all(ReaderId::is_stable)
    }

    pub fn chain(&self) -> Option<ChainAudience> {
        self.chain
    }

    pub fn groups(&self) -> impl Iterator<Item = &GroupRef> {
        self.groups.iter()
    }

    pub fn readers(&self) -> &BTreeSet<ReaderId> {
        &self.readers
    }

    pub(crate) fn symbolic_atoms(&self) -> impl Iterator<Item = SymbolicAtom> + '_ {
        self.chain
            .into_iter()
            .map(SymbolicAtom::Chain)
            .chain(self.groups.iter().cloned().map(SymbolicAtom::Group))
    }

    /// Every atom an exact evaluation of this clause may ask for: the symbolic sets, plus
    /// each reader whose provider prefix names a registered source — its canonicalization to
    /// a principal is an operation answer too.
    pub(crate) fn needed_atoms<'a>(
        &'a self,
        providers: &'a BTreeSet<String>,
    ) -> impl Iterator<Item = SymbolicAtom> + 'a {
        self.symbolic_atoms().chain(
            self.readers
                .iter()
                .filter(|reader| {
                    reader
                        .provider_prefix()
                        .is_some_and(|provider| providers.contains(provider))
                })
                .cloned()
                .map(SymbolicAtom::Reader),
        )
    }

    /// Structural containment ⟦self⟧ ⊆ ⟦other⟧, from policy-independent facts only:
    /// atom equality, the built-in chain order, and exact reader subset. `within`
    /// assertions never enter — they belong to derivation, not to canonical equality.
    fn structurally_within(&self, other: &Clause) -> bool {
        let chain_ok = match (self.chain, other.chain) {
            (None, _) => true,
            (Some(mine), Some(theirs)) => mine <= theirs,
            (Some(_), None) => false,
        };
        chain_ok && self.groups.is_subset(&other.groups) && self.readers.is_subset(&other.readers)
    }

    /// Derivability ⊑⊢ against the declared `within` assertions: every atom of `self` derives
    /// into some atom of `other`. Sound and incomplete; a failed derivation says nothing.
    fn derives_within(&self, other: &Clause, within: &WithinAssertions) -> bool {
        let chain_ok = match self.chain {
            None => true,
            Some(mine) => other.chain.is_some_and(|theirs| mine <= theirs),
        };
        chain_ok
            && self.groups.iter().all(|group| {
                other.groups.contains(group)
                    || other
                        .chain
                        .is_some_and(|theirs| within.target(group).is_some_and(|target| target <= theirs))
            })
            && self.readers.is_subset(&other.readers)
    }

    /// The exact reader set this union denotes under the operation's answers, in principal
    /// space: a provider-qualified reader under a registered source reads as its canonicalized
    /// principal; every other reader reads as written.
    fn members(&self, context: &MembershipContext<'_>) -> Result<BTreeSet<ReaderId>, MembershipNeeded> {
        let mut members = BTreeSet::new();
        let mut needed = Vec::new();
        for reader in &self.readers {
            match context.principal(reader) {
                Ok(principal) => {
                    members.insert(principal);
                }
                Err(atom) => needed.push(atom),
            }
        }
        for atom in self.symbolic_atoms() {
            match context.expansions.members(&atom) {
                Some(answered) => members.extend(answered.iter().cloned()),
                None => needed.push(atom),
            }
        }
        if needed.is_empty() {
            Ok(members)
        } else {
            Err(MembershipNeeded { needed })
        }
    }
}

impl<'de> Deserialize<'de> for Clause {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            #[serde(default)]
            chain: Option<ChainAudience>,
            #[serde(default)]
            groups: BTreeSet<GroupRef>,
            #[serde(default)]
            readers: BTreeSet<ReaderId>,
        }

        let wire = Wire::deserialize(deserializer)?;
        Clause::new(wire.chain, wire.groups, wire.readers).map_err(serde::de::Error::custom)
    }
}

/// A reader set as configuration and checks name one: the universal audience or one union
/// clause. A written audience list means the union of its entries; `public` is legal only
/// alone, because a union with the universe is the universe.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeclaredAudience {
    Public,
    Union(Clause),
}

impl DeclaredAudience {
    pub fn restricted(readers: impl IntoIterator<Item = ReaderId>) -> DeclaredAudience {
        DeclaredAudience::Union(Clause::of_readers(readers.into_iter().collect()))
    }

    /// Test fixture: the declared spelling of a public or single-clause audience.
    #[cfg(test)]
    pub(crate) fn literal(audience: Audience) -> DeclaredAudience {
        if audience.is_public() {
            return DeclaredAudience::Public;
        }
        let mut clauses = audience.clauses();
        let clause = clauses.next().expect("a non-public audience holds a clause").clone();
        assert!(clauses.next().is_none(), "a declared literal is one clause");
        DeclaredAudience::Union(clause)
    }

    pub(crate) fn symbolic_atoms(&self) -> Box<dyn Iterator<Item = SymbolicAtom> + '_> {
        match self {
            DeclaredAudience::Public => Box::new(std::iter::empty()),
            DeclaredAudience::Union(clause) => Box::new(clause.symbolic_atoms()),
        }
    }

    /// See [`Clause::needed_atoms`].
    pub(crate) fn needed_atoms<'a>(
        &'a self,
        providers: &'a BTreeSet<String>,
    ) -> Box<dyn Iterator<Item = SymbolicAtom> + 'a> {
        match self {
            DeclaredAudience::Public => Box::new(std::iter::empty()),
            DeclaredAudience::Union(clause) => Box::new(clause.needed_atoms(providers)),
        }
    }
}

/// The declared `within` assertions: each configured named audience's chain target. A trusted
/// policy assertion — `@finance within internal` makes every finance member internal, whoever
/// the source reports — consulted by derivation and by extensional closure, never by
/// canonicalization.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WithinAssertions {
    targets: BTreeMap<GroupName, ChainAudience>,
}

impl WithinAssertions {
    pub fn new(targets: impl IntoIterator<Item = (GroupName, ChainAudience)>) -> WithinAssertions {
        WithinAssertions {
            targets: targets.into_iter().collect(),
        }
    }

    fn target(&self, group: &GroupRef) -> Option<ChainAudience> {
        match group {
            GroupRef::Named(name) => self.targets.get(name).copied(),
            GroupRef::Source { .. } => None,
        }
    }
}

/// The symbolic atoms an evaluation needs extensional membership for, in first-need order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MembershipNeeded {
    pub needed: Vec<SymbolicAtom>,
}

/// The membership answers one operation reads: one exact reader set per group or chain
/// atom, post-identity and post-closure, and one principal per canonicalized reader. Built
/// by the operation's driver from pinned primitive evidence — source answers, member
/// lookups, identity mappings — and rebuilt identically on replay, so a live decision and
/// its replay read the same directory answers. An empty set is a valid answer.
///
/// Every ask — answered or not — lands in the reads log, so after a decision runs the log
/// names exactly the atoms the operation deterministically requested. The log is
/// bookkeeping, never an answer: it takes no part in equality.
#[derive(Clone, Debug, Default)]
pub struct Expansions {
    members: BTreeMap<SymbolicAtom, BTreeSet<ReaderId>>,
    principals: BTreeMap<ReaderId, ReaderId>,
    reads: std::cell::RefCell<BTreeSet<SymbolicAtom>>,
}

impl PartialEq for Expansions {
    fn eq(&self, other: &Expansions) -> bool {
        self.members == other.members && self.principals == other.principals
    }
}

impl Eq for Expansions {}

impl Expansions {
    pub(crate) fn new(
        members: impl IntoIterator<Item = (SymbolicAtom, BTreeSet<ReaderId>)>,
        principals: impl IntoIterator<Item = (ReaderId, ReaderId)>,
    ) -> Expansions {
        Expansions {
            members: members.into_iter().collect(),
            principals: principals.into_iter().collect(),
            reads: std::cell::RefCell::default(),
        }
    }

    /// The reader set a group or chain atom denotes, where the operation answered it.
    pub(crate) fn members(&self, atom: &SymbolicAtom) -> Option<&BTreeSet<ReaderId>> {
        self.reads.borrow_mut().insert(atom.clone());
        self.members.get(atom)
    }

    /// The principal a qualified reader canonicalizes to, where the operation answered it.
    pub(crate) fn principal(&self, reader: &ReaderId) -> Option<&ReaderId> {
        self.reads.borrow_mut().insert(SymbolicAtom::Reader(reader.clone()));
        self.principals.get(reader)
    }

    /// Is the atom answered, whichever kind it is?
    pub(crate) fn answered(&self, atom: &SymbolicAtom) -> bool {
        match atom {
            SymbolicAtom::Reader(reader) => self.principal(reader).is_some(),
            SymbolicAtom::Chain(_) | SymbolicAtom::Group(_) => self.members(atom).is_some(),
        }
    }

    /// The atoms asked so far, sorted. A snapshot: later asks keep logging.
    pub(crate) fn reads(&self) -> Vec<SymbolicAtom> {
        self.reads.borrow().iter().cloned().collect()
    }

    /// Fold another context's asks into this log — an overlay context reads on behalf of the
    /// same act, and the act's justification must count those asks.
    pub(crate) fn absorb_reads(&self, other: &Expansions) {
        let absorbed: Vec<SymbolicAtom> = other.reads.borrow().iter().cloned().collect();
        self.reads.borrow_mut().extend(absorbed);
    }
}

/// Everything an audience evaluation reads beside the audiences themselves: the policy's
/// `within` assertions, the registered source providers (which decide *which* qualified
/// readers canonicalize), and the operation's answers. The first two are policy, fixed for
/// the trajectory; the answers are the operation's pinned evidence.
#[derive(Clone, Copy, Debug)]
pub struct MembershipContext<'a> {
    pub within: &'a WithinAssertions,
    pub providers: &'a BTreeSet<String>,
    pub expansions: &'a Expansions,
}

/// Test fixture: owned context parts, so a unit test borrows one binding instead of three.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct TestContext {
    pub(crate) within: WithinAssertions,
    pub(crate) providers: BTreeSet<String>,
    pub(crate) expansions: Expansions,
}

#[cfg(test)]
impl TestContext {
    pub(crate) fn context(&self) -> MembershipContext<'_> {
        MembershipContext::new(&self.within, &self.providers, &self.expansions)
    }
}

impl<'a> MembershipContext<'a> {
    pub fn new(
        within: &'a WithinAssertions,
        providers: &'a BTreeSet<String>,
        expansions: &'a Expansions,
    ) -> MembershipContext<'a> {
        MembershipContext {
            within,
            providers,
            expansions,
        }
    }

    /// The principal a reader compares as: itself, unless its provider prefix names a
    /// registered source — then the operation's pinned canonicalization, or the atom to ask.
    fn principal(&self, reader: &ReaderId) -> Result<ReaderId, SymbolicAtom> {
        match reader.provider_prefix() {
            Some(provider) if self.providers.contains(provider) => self
                .expansions
                .principal(reader)
                .cloned()
                .ok_or_else(|| SymbolicAtom::Reader(reader.clone())),
            _ => Ok(reader.clone()),
        }
    }
}

/// A three-valued evaluation: established, refuted, or awaiting the named memberships.
/// "Needs" is not a label state — the label stays concrete and symbolic; only the check
/// stops until the operation pins the answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Evaluation {
    Holds,
    Fails,
    Needs(MembershipNeeded),
}

impl Evaluation {
    pub(crate) fn of_exact(holds: bool) -> Evaluation {
        if holds { Evaluation::Holds } else { Evaluation::Fails }
    }
}

/// The audience dimension: a canonical intersection of union clauses. The empty intersection
/// is `public` (the universe); an empty clause anywhere collapses the meaning to nobody.
/// Canonical form is policy-independent — see [`Audience::canonicalize`] — so equal meanings
/// under the structural facts compare equal, and equality never shifts when a deployment's
/// `within` assertions or directory answers change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Audience {
    clauses: BTreeSet<Clause>,
}

impl Audience {
    /// The universal audience: the empty intersection.
    pub fn public() -> Audience {
        Audience {
            clauses: BTreeSet::new(),
        }
    }

    /// The empty audience: nobody. The fail-closed floor.
    pub fn nobody() -> Audience {
        Audience {
            clauses: BTreeSet::from([Clause::empty()]),
        }
    }

    pub fn restricted(readers: impl IntoIterator<Item = ReaderId>) -> Audience {
        Audience::of_clauses([Clause::of_readers(readers.into_iter().collect())])
    }

    /// One declared union as a whole audience.
    pub fn of_declared(declared: &DeclaredAudience) -> Audience {
        match declared {
            DeclaredAudience::Public => Audience::public(),
            DeclaredAudience::Union(clause) => Audience::of_clauses([clause.clone()]),
        }
    }

    pub fn of_clauses(clauses: impl IntoIterator<Item = Clause>) -> Audience {
        Audience {
            clauses: Self::canonicalize(clauses.into_iter().collect()),
        }
    }

    pub fn is_public(&self) -> bool {
        self.clauses.is_empty()
    }

    pub fn clauses(&self) -> impl Iterator<Item = &Clause> {
        self.clauses.iter()
    }

    /// Every symbolic atom this audience names, in canonical order.
    pub fn symbolic_atoms(&self) -> BTreeSet<SymbolicAtom> {
        self.clauses.iter().flat_map(Clause::symbolic_atoms).collect()
    }

    /// Sound structural inclusion ⟦self⟧ ⊆ ⟦other⟧ from the declared facts alone — the chain,
    /// `within` assertions, exact reader subset. Incomplete: `false` means "not derived",
    /// never "refuted". Plan ranking reads this; no admission decision does.
    pub(crate) fn derives_within_audience(&self, other: &Audience, within: &WithinAssertions) -> bool {
        if self.is_public() {
            return other.is_public();
        }
        other
            .clauses
            .iter()
            .all(|target| self.clauses.iter().any(|clause| clause.derives_within(target, within)))
    }

    /// The policy-independent canonical form. Exactly three rules, in order: an empty clause
    /// collapses everything to nobody (X ∩ ∅ = ∅); clauses of stable readers merge into one
    /// by exact intersection (which may itself produce the empty clause — disjoint plain
    /// reader lists mean nobody); a clause structurally contained in another survives and the
    /// broader one drops (self ∩ internal = self by the built-in chain). Dedup is the set
    /// itself. Two things never apply here: declared `within` relations — `@finance ∩
    /// internal` keeps both clauses even when finance is within internal, because harmless
    /// redundancy beats policy-dependent equality — and any reader a deployment could
    /// canonicalize (`slack:U012345`): its meaning is an operation-pinned principal, so its
    /// clause survives unmerged and the evaluation intersects in principal space.
    fn canonicalize(clauses: BTreeSet<Clause>) -> BTreeSet<Clause> {
        if clauses.iter().any(Clause::is_empty) {
            return BTreeSet::from([Clause::empty()]);
        }
        let (stable, mixed): (Vec<Clause>, Vec<Clause>) = clauses.into_iter().partition(Clause::is_stable_readers);
        let mut mixed: BTreeSet<Clause> = mixed.into_iter().collect();
        if let Some((first, rest)) = stable.split_first() {
            let merged = rest.iter().fold(first.readers.clone(), |merged, clause| {
                merged.intersection(&clause.readers).cloned().collect()
            });
            if merged.is_empty() {
                return BTreeSet::from([Clause::empty()]);
            }
            mixed.insert(Clause::of_readers(merged));
        }
        mixed
            .iter()
            .filter(|clause| {
                !mixed
                    .iter()
                    .any(|narrower| narrower != *clause && narrower.structurally_within(clause))
            })
            .cloned()
            .collect()
    }

    fn combine(&self, other: &Self) -> Self {
        Audience::of_clauses(self.clauses.iter().chain(other.clauses.iter()).cloned())
    }

    /// `⟦self⟧ ⊇ ⟦recipients⟧` — the audience includes every named recipient. Decides the
    /// universal cases outright, then derives from the declared facts, then evaluates the
    /// exact denotation; a failed derivation never refutes by itself.
    pub(crate) fn includes(&self, recipients: &DeclaredAudience, context: &MembershipContext<'_>) -> Evaluation {
        if self.is_public() {
            return Evaluation::Holds;
        }
        let recipients = match recipients {
            // A restricted audience never covers the universe: definitive, no fallback.
            DeclaredAudience::Public => return Evaluation::Fails,
            DeclaredAudience::Union(clause) => clause,
        };
        if self
            .clauses
            .iter()
            .all(|clause| recipients.derives_within(clause, context.within))
        {
            return Evaluation::Holds;
        }
        let wanted = match recipients.members(context) {
            Ok(wanted) => wanted,
            Err(needed) => return Evaluation::Needs(needed),
        };
        match self.exact_members(context) {
            Ok(held) => Evaluation::of_exact(wanted.is_subset(&held)),
            Err(needed) => Evaluation::Needs(needed),
        }
    }

    /// `⟦self⟧ ⊆ ⟦cap⟧` — the audience stays within the declared cap. Same three steps as
    /// [`includes`](Self::includes), from the other side.
    pub(crate) fn within(&self, cap: &DeclaredAudience, context: &MembershipContext<'_>) -> Evaluation {
        let cap = match cap {
            DeclaredAudience::Public => return Evaluation::Holds,
            DeclaredAudience::Union(clause) => clause,
        };
        if self.is_public() {
            // The universe never fits a restricted cap: every denotation here is finite.
            return Evaluation::Fails;
        }
        if self
            .clauses
            .iter()
            .any(|clause| clause.derives_within(cap, context.within))
        {
            return Evaluation::Holds;
        }
        let allowed = match cap.members(context) {
            Ok(allowed) => allowed,
            Err(needed) => return Evaluation::Needs(needed),
        };
        match self.exact_members(context) {
            Ok(held) => Evaluation::of_exact(held.is_subset(&allowed)),
            Err(needed) => Evaluation::Needs(needed),
        }
    }

    /// The exact denotation: the intersection of every clause's member union, in principal
    /// space. Total once the context answers every symbolic atom; every denotation is finite.
    pub(crate) fn exact_members(
        &self,
        context: &MembershipContext<'_>,
    ) -> Result<BTreeSet<ReaderId>, MembershipNeeded> {
        debug_assert!(!self.is_public(), "the universal audience has no member list");
        let mut members: Option<BTreeSet<ReaderId>> = None;
        let mut needed = Vec::new();
        for clause in &self.clauses {
            match clause.members(context) {
                Ok(clause_members) => {
                    members = Some(match members {
                        None => clause_members,
                        Some(held) => held.intersection(&clause_members).cloned().collect(),
                    });
                }
                Err(mut missing) => needed.append(&mut missing.needed),
            }
        }
        if needed.is_empty() {
            Ok(members.unwrap_or_default())
        } else {
            needed.sort();
            needed.dedup();
            Err(MembershipNeeded { needed })
        }
    }
}

impl<'de> Deserialize<'de> for Audience {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let clauses = BTreeSet::<Clause>::deserialize(deserializer)?;
        let canonical = Audience::of_clauses(clauses.clone());
        if canonical.clauses == clauses {
            Ok(canonical)
        } else {
            Err(serde::de::Error::custom(
                "audience is not in canonical form — records hold canonical audiences only",
            ))
        }
    }
}

/// The one label: both dimensions concrete, always. Every admitted value carries exactly one of
/// these, every trajectory fold is one, and every check reads one — no partial or pending
/// state is representable. Symbolic audience atoms are concrete label content, not pending
/// state: what may still be outstanding is a check's membership question, never the label.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Label {
    pub trust: Trust,
    pub audience: Audience,
}

impl Label {
    pub fn new(trust: Trust, audience: Audience) -> Self {
        Label { trust, audience }
    }

    /// The fold identity: maximally permissive (top trust, public audience). Top trust is
    /// `u8::MAX`, an upper bound on any configured chain rank, so it clears every floor;
    /// audience `public` includes every recipient.
    pub fn top() -> Self {
        Label::new(Trust::new(u8::MAX), Audience::public())
    }

    /// The maximally restrictive label: the lowest rank and no readers at all. The fail-closed
    /// reading of a basis source whose record the log does not hold — folding it narrows the
    /// trajectory to the floor, and can never widen anything.
    pub(crate) fn bottom() -> Self {
        Label {
            trust: Trust::new(0),
            audience: Audience::nobody(),
        }
    }

    /// The restrictive meet: minimum trust, intersect audience (union the clause sets).
    /// Commutative, associative, idempotent, and it never widens either dimension.
    pub fn combine(&self, other: &Label) -> Label {
        Label {
            trust: self.trust.combine(other.trust),
            audience: self.audience.combine(&other.audience),
        }
    }

    /// Fold one contribution in place — the meet, assigned.
    pub(crate) fn fold(&mut self, other: &Label) {
        *self = self.combine(other);
    }

    /// Does this label's trust meet `floor`? Two-valued: a label is always concrete, so the
    /// check holds or fails and nothing else.
    pub fn meets_floor(&self, floor: Trust) -> bool {
        self.trust >= floor
    }

    /// Do this label's readers include every named recipient?
    pub(crate) fn covers(&self, recipients: &DeclaredAudience, context: &MembershipContext<'_>) -> Evaluation {
        self.audience.includes(recipients, context)
    }

    /// Do this label's readers stay within `cap`?
    pub(crate) fn within_cap(&self, cap: &DeclaredAudience, context: &MembershipContext<'_>) -> Evaluation {
        self.audience.within(cap, context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn reader(id: &str) -> ReaderId {
        ReaderId::new(id)
    }

    fn named(name: &str) -> GroupRef {
        GroupRef::Named(GroupName::new(name))
    }

    fn clause(
        chain: impl IntoIterator<Item = ChainAudience>,
        groups: impl IntoIterator<Item = &'static str>,
        readers: impl IntoIterator<Item = &'static str>,
    ) -> Clause {
        Clause::new(chain, groups.into_iter().map(named), readers.into_iter().map(reader)).unwrap()
    }

    fn trust_strategy() -> impl Strategy<Value = Trust> {
        (0u8..4).prop_map(Trust::new)
    }

    fn reader_set_strategy() -> impl Strategy<Value = BTreeSet<ReaderId>> {
        prop::collection::btree_set((b'a'..=b'e').prop_map(|c| ReaderId::new((c as char).to_string())), 0..5)
    }

    /// Clause readers may also be provider-qualified: `slack:u1` canonicalizes through the
    /// assignment's pinned principal before any comparison.
    fn clause_reader_set_strategy() -> impl Strategy<Value = BTreeSet<ReaderId>> {
        prop::collection::btree_set(
            prop_oneof![
                (b'a'..=b'e').prop_map(|c| ReaderId::new((c as char).to_string())),
                Just(ReaderId::new("slack:u1")),
            ],
            0..5,
        )
    }

    fn providers() -> BTreeSet<String> {
        BTreeSet::from(["slack".to_string()])
    }

    fn clause_strategy() -> impl Strategy<Value = Clause> {
        (
            prop::option::of(prop_oneof![Just(ChainAudience::Self_), Just(ChainAudience::Internal)]),
            prop::collection::btree_set(
                prop_oneof![
                    Just(named("finance")),
                    Just(named("legal")),
                    Just(GroupRef::Source {
                        provider: "slack".into(),
                        selector: "user-group/eng".into()
                    }),
                ],
                0..3,
            ),
            clause_reader_set_strategy(),
        )
            .prop_map(|(chain, groups, readers)| Clause::new(chain, groups, readers).unwrap())
    }

    fn audience_strategy() -> impl Strategy<Value = Audience> {
        prop::collection::btree_set(clause_strategy(), 0..4).prop_map(Audience::of_clauses)
    }

    fn label_strategy() -> impl Strategy<Value = Label> {
        (trust_strategy(), audience_strategy()).prop_map(|(t, a)| Label::new(t, a))
    }

    /// A membership assignment every deployment could really produce: `self ⊆ internal`
    /// always, each group asserted `within` a chain level lands inside that level's set (the
    /// runtime's symmetric closure guarantees exactly that), and the one qualified test
    /// reader has one pinned principal — possibly colliding with a plain reader, which is
    /// exactly the cross-provider matching the canonicalization exists for.
    fn assignment_strategy() -> impl Strategy<Value = (WithinAssertions, Expansions)> {
        (
            reader_set_strategy(),
            reader_set_strategy(),
            reader_set_strategy(),
            reader_set_strategy(),
            reader_set_strategy(),
            prop::option::of(prop_oneof![Just(ChainAudience::Self_), Just(ChainAudience::Internal)]),
            prop_oneof![Just(ReaderId::new("a")), Just(ReaderId::new("u1@corp.example")),],
        )
            .prop_map(
                |(self_members, extra_internal, finance, legal, eng, finance_within, principal)| {
                    let within =
                        WithinAssertions::new(finance_within.map(|target| (GroupName::new("finance"), target)));
                    let mut self_closed = self_members;
                    let mut internal: BTreeSet<ReaderId> = self_closed.union(&extra_internal).cloned().collect();
                    if let Some(target) = finance_within {
                        internal.extend(finance.iter().cloned());
                        if target == ChainAudience::Self_ {
                            // A within="self" assertion folds the group into self's closure too.
                            self_closed.extend(finance.iter().cloned());
                            internal.extend(self_closed.iter().cloned());
                        }
                    }
                    let expansions = Expansions::new(
                        [
                            (SymbolicAtom::Chain(ChainAudience::Self_), self_closed),
                            (SymbolicAtom::Chain(ChainAudience::Internal), internal),
                            (SymbolicAtom::Group(named("finance")), finance),
                            (SymbolicAtom::Group(named("legal")), legal),
                            (
                                SymbolicAtom::Group(GroupRef::Source {
                                    provider: "slack".into(),
                                    selector: "user-group/eng".into(),
                                }),
                                eng,
                            ),
                        ],
                        [(ReaderId::new("slack:u1"), principal)],
                    );
                    (within, expansions)
                },
            )
    }

    fn eval(audience: &Audience, context: &MembershipContext<'_>) -> Option<BTreeSet<ReaderId>> {
        if audience.is_public() {
            None
        } else {
            Some(audience.exact_members(context).expect("assignment answers every atom"))
        }
    }

    /// Denotational containment under one full assignment: `None` is the universe.
    fn contained(a: &Option<BTreeSet<ReaderId>>, b: &Option<BTreeSet<ReaderId>>) -> bool {
        match (a, b) {
            (_, None) => true,
            (None, Some(_)) => false,
            (Some(a), Some(b)) => a.is_subset(b),
        }
    }

    proptest! {
        #[test]
        fn combine_is_commutative(a in label_strategy(), b in label_strategy()) {
            prop_assert_eq!(a.combine(&b), b.combine(&a));
        }

        #[test]
        fn combine_is_associative(
            a in label_strategy(),
            b in label_strategy(),
            c in label_strategy(),
        ) {
            prop_assert_eq!(a.combine(&b).combine(&c), a.combine(&b.combine(&c)));
        }

        #[test]
        fn combine_is_idempotent(a in label_strategy()) {
            prop_assert_eq!(a.combine(&a), a.clone());
        }

        #[test]
        fn top_is_identity(a in label_strategy()) {
            let identity = Label::top();
            prop_assert_eq!(identity.combine(&a), a.clone());
            prop_assert_eq!(a.combine(&identity), a.clone());
        }

        /// An empty clause anywhere is nobody: the fail-closed floor absorbs every fold, and
        /// denotes the empty set under every assignment.
        #[test]
        fn nobody_absorbs(a in label_strategy(), (within, expansions) in assignment_strategy()) {
            let nobody = Label::new(a.trust, Audience::nobody());
            prop_assert_eq!(a.combine(&nobody).audience, Audience::nobody());
            let providers = providers();
            let context = MembershipContext::new(&within, &providers, &expansions);
            prop_assert_eq!(eval(&Audience::nobody(), &context), Some(BTreeSet::new()));
        }

        /// The four universal edges decide without a single membership answer: public covers
        /// everything and fits only a public cap; a restricted audience never covers the
        /// universe and always fits it.
        #[test]
        fn universal_edges_decide_without_membership(
            label in label_strategy(),
            clause in clause_strategy(),
        ) {
            let nothing = Expansions::new([], []);
            let within = WithinAssertions::default();
            let providers = providers();
            let context = MembershipContext::new(&within, &providers, &nothing);
            let restricted = DeclaredAudience::Union(clause);
            prop_assert_eq!(label.within_cap(&DeclaredAudience::Public, &context), Evaluation::Holds);
            if label.audience.is_public() {
                prop_assert_eq!(label.covers(&DeclaredAudience::Public, &context), Evaluation::Holds);
                prop_assert_eq!(label.covers(&restricted, &context), Evaluation::Holds);
                prop_assert_eq!(label.within_cap(&restricted, &context), Evaluation::Fails);
            } else {
                prop_assert_eq!(label.covers(&DeclaredAudience::Public, &context), Evaluation::Fails);
            }
        }

        #[test]
        fn canonicalization_is_idempotent(a in audience_strategy()) {
            let again = Audience::of_clauses(a.clauses.iter().cloned());
            prop_assert_eq!(again, a);
        }

        /// The semantic never-widens law: under EVERY membership assignment a deployment could
        /// produce, folding shrinks the denotation of both operands. Strictly stronger than a
        /// structural check — it quantifies over the mutable directory state.
        #[test]
        fn combine_never_widens(
            a in label_strategy(),
            b in label_strategy(),
            (within, expansions) in assignment_strategy(),
        ) {
            let providers = providers();
            let context = MembershipContext::new(&within, &providers, &expansions);
            let folded = a.combine(&b);
            prop_assert!(folded.trust <= a.trust);
            prop_assert!(folded.trust <= b.trust);
            let folded_members = eval(&folded.audience, &context);
            prop_assert!(contained(&folded_members, &eval(&a.audience, &context)));
            prop_assert!(contained(&folded_members, &eval(&b.audience, &context)));
        }

        /// Canonicalization preserves meaning under every assignment: the canonical form of a
        /// clause set denotes exactly what the raw intersection denotes.
        #[test]
        fn canonicalization_preserves_denotation(
            clauses in prop::collection::vec(clause_strategy(), 0..4),
            (within, expansions) in assignment_strategy(),
        ) {
            let providers = providers();
            let context = MembershipContext::new(&within, &providers, &expansions);
            let canonical = Audience::of_clauses(clauses.clone());
            let raw: Option<BTreeSet<ReaderId>> = clauses.iter()
                .map(|clause| clause.members(&context).expect("assignment answers every atom"))
                .reduce(|held, next| held.intersection(&next).cloned().collect());
            match (eval(&canonical, &context), raw) {
                (None, None) => {}
                (Some(canonical), Some(raw)) => prop_assert_eq!(canonical, raw),
                (canonical, raw) => prop_assert!(false, "canonical {:?} vs raw {:?}", canonical, raw),
            }
        }

        /// Soundness of the derivability calculus: a symbolically established or refuted check
        /// agrees with the exact denotation under every valid assignment; `Needs` never appears
        /// when the assignment is total.
        #[test]
        fn symbolic_answers_are_sound(
            label in label_strategy(),
            recipients in prop_oneof![
                Just(DeclaredAudience::Public),
                clause_strategy().prop_map(DeclaredAudience::Union),
            ],
            (within, expansions) in assignment_strategy(),
        ) {
            let providers = providers();
            let context = MembershipContext::new(&within, &providers, &expansions);
            let declared_members = |declared: &DeclaredAudience| match declared {
                DeclaredAudience::Public => None,
                DeclaredAudience::Union(clause) =>
                    Some(clause.members(&context).expect("assignment answers every atom")),
            };
            let exact_covers = contained(&declared_members(&recipients), &eval(&label.audience, &context));
            match label.covers(&recipients, &context) {
                Evaluation::Holds => prop_assert!(exact_covers, "derived covers must hold exactly"),
                Evaluation::Fails => prop_assert!(!exact_covers, "refuted covers must fail exactly"),
                Evaluation::Needs(_) => prop_assert!(false, "total assignment cannot leave needs"),
            }
            let exact_within = contained(&eval(&label.audience, &context), &declared_members(&recipients));
            match label.within_cap(&recipients, &context) {
                Evaluation::Holds => prop_assert!(exact_within, "derived within must hold exactly"),
                Evaluation::Fails => prop_assert!(!exact_within, "refuted within must fail exactly"),
                Evaluation::Needs(_) => prop_assert!(false, "total assignment cannot leave needs"),
            }
        }

        /// Every admitted value contributes one concrete label, so the trajectory fold is the
        /// plain combine reduction — order-independent, one label, nothing pending alongside it.
        #[test]
        fn every_admitted_value_has_one_concrete_label(
            start in label_strategy(),
            values in prop::collection::vec(label_strategy(), 0..6),
        ) {
            let mut forward = start.clone();
            for value in &values {
                forward.fold(value);
            }
            let reduced = values.iter().fold(start.clone(), |fold, value| fold.combine(value));
            prop_assert_eq!(&forward, &reduced);
            let mut reversed = start;
            for value in values.iter().rev() {
                reversed.fold(value);
            }
            prop_assert_eq!(forward, reversed);
        }
    }

    #[test]
    fn floor_holds_at_or_above() {
        let floor = Trust::new(2);
        let at = Label::new(Trust::new(2), Audience::public());
        let above = Label::new(Trust::new(3), Audience::public());
        let below = Label::new(Trust::new(1), Audience::public());
        assert!(at.meets_floor(floor));
        assert!(above.meets_floor(floor));
        assert!(!below.meets_floor(floor));
    }

    #[test]
    fn reserved_spellings_are_never_readers() {
        for reserved in ["public", "self", "internal", "@finance"] {
            assert!(!ReaderId::new(reserved).is_literal());
            assert!(Clause::new([], [], [reader(reserved)]).is_err());
        }
        assert!(ReaderId::new("alice@corp.com").is_literal());
        assert!(ReaderId::new("slack:U012345").is_literal());
        assert!(ReaderId::new("Self").is_literal(), "reserved spellings are exact");
    }

    #[test]
    fn a_reader_off_the_wire_normalizes_like_a_constructed_one() {
        let wire: ReaderId = serde_json::from_str("\"Alice@CORP.com\"").expect("a reader deserializes from a string");
        assert_eq!(wire, ReaderId::new("Alice@corp.com"));
        assert_eq!(
            wire.as_str(),
            "Alice@corp.com",
            "the local part survives the domain fold"
        );
        assert_eq!(
            serde_json::to_string(&wire).expect("a reader serializes"),
            "\"Alice@corp.com\"",
            "a replayed record round-trips to the spelling every comparison uses"
        );

        // A reader that is no address keeps every byte, provider prefixes included.
        let qualified: ReaderId = serde_json::from_str("\"slack:U012345\"").expect("a reader deserializes");
        assert_eq!(qualified.as_str(), "slack:U012345");
    }

    fn context_free() -> (WithinAssertions, BTreeSet<String>, Expansions) {
        (WithinAssertions::default(), providers(), Expansions::default())
    }

    #[test]
    fn the_chain_derives_without_answers() {
        let (nothing, providers, unanswered) = context_free();
        let context = MembershipContext::new(&nothing, &providers, &unanswered);
        let self_label = Label::new(
            Trust::new(1),
            Audience::of_clauses([clause([ChainAudience::Self_], [], [])]),
        );
        let internal_cap = DeclaredAudience::Union(clause([ChainAudience::Internal], [], []));
        assert_eq!(
            self_label.within_cap(&internal_cap, &context),
            Evaluation::Holds,
            "self ⊆ internal is built in — zero consults"
        );
    }

    #[test]
    fn within_assertions_derive_but_never_canonicalize() {
        let within = WithinAssertions::new([(GroupName::new("finance"), ChainAudience::Internal)]);
        let providers = providers();
        let unanswered = Expansions::default();
        let context = MembershipContext::new(&within, &providers, &unanswered);
        let finance_label = Label::new(Trust::new(1), Audience::of_clauses([clause([], ["finance"], [])]));
        let internal_cap = DeclaredAudience::Union(clause([ChainAudience::Internal], [], []));
        assert_eq!(
            finance_label.within_cap(&internal_cap, &context),
            Evaluation::Holds,
            "@finance within internal derives symbolically — zero consults"
        );
        // But canonical form keeps both clauses: equality is policy-independent.
        let both = Audience::of_clauses([clause([], ["finance"], []), clause([ChainAudience::Internal], [], [])]);
        assert_eq!(both.clauses().count(), 2);
    }

    #[test]
    fn an_unproved_check_asks_instead_of_denying() {
        let (nothing, providers, unanswered) = context_free();
        let internal_label = Label::new(
            Trust::new(1),
            Audience::of_clauses([clause([ChainAudience::Internal], [], [])]),
        );
        let alice = DeclaredAudience::restricted([reader("alice@corp.com")]);
        match internal_label.covers(&alice, &MembershipContext::new(&nothing, &providers, &unanswered)) {
            Evaluation::Needs(MembershipNeeded { needed }) => {
                assert_eq!(needed, vec![SymbolicAtom::Chain(ChainAudience::Internal)]);
            }
            other => panic!("expected a membership ask, got {other:?}"),
        }
        let answered = Expansions::new(
            [(
                SymbolicAtom::Chain(ChainAudience::Internal),
                BTreeSet::from([reader("alice@corp.com")]),
            )],
            [],
        );
        assert_eq!(
            internal_label.covers(&alice, &MembershipContext::new(&nothing, &providers, &answered)),
            Evaluation::Holds
        );
        let empty = Expansions::new([(SymbolicAtom::Chain(ChainAudience::Internal), BTreeSet::new())], []);
        assert_eq!(
            internal_label.covers(&alice, &MembershipContext::new(&nothing, &providers, &empty)),
            Evaluation::Fails,
            "an empty member set is a valid answer"
        );
    }

    #[test]
    fn a_qualified_recipient_canonicalizes_before_comparison() {
        let (nothing, providers, _) = context_free();
        let internal_label = Label::new(
            Trust::new(1),
            Audience::of_clauses([clause([ChainAudience::Internal], [], [])]),
        );
        // $recipient = slack:U012345, where Slack reports Alice's verified corporate email
        // and the internal closure holds her principal: the cross-provider case end-to-end.
        let recipient = DeclaredAudience::restricted([reader("slack:U012345")]);
        let unanswered = Expansions::new(
            [(
                SymbolicAtom::Chain(ChainAudience::Internal),
                BTreeSet::from([reader("alice@corp.com")]),
            )],
            [],
        );
        match internal_label.covers(&recipient, &MembershipContext::new(&nothing, &providers, &unanswered)) {
            Evaluation::Needs(MembershipNeeded { needed }) => {
                assert_eq!(needed, vec![SymbolicAtom::Reader(reader("slack:U012345"))]);
            }
            other => panic!("expected a canonicalization ask, got {other:?}"),
        }
        let answered = Expansions::new(
            [(
                SymbolicAtom::Chain(ChainAudience::Internal),
                BTreeSet::from([reader("alice@corp.com")]),
            )],
            [(reader("slack:U012345"), reader("alice@corp.com"))],
        );
        assert_eq!(
            internal_label.covers(&recipient, &MembershipContext::new(&nothing, &providers, &answered)),
            Evaluation::Holds
        );
        // An unregistered prefix never canonicalizes: exact string, distinct namespace.
        let foreign = DeclaredAudience::restricted([reader("github:alice")]);
        assert_eq!(
            internal_label.covers(&foreign, &MembershipContext::new(&nothing, &providers, &answered)),
            Evaluation::Fails
        );
    }

    #[test]
    fn canonicalization_rules_are_exactly_three() {
        // Empty clause collapses to nobody and is never dropped.
        let disjoint = Audience::restricted([reader("alice")]).combine(&Audience::restricted([reader("bob")]));
        assert_eq!(disjoint, Audience::nobody());
        assert!(!disjoint.is_public(), "nobody is not public");
        let with_symbols = Audience::of_clauses([clause([ChainAudience::Internal], [], [])]).combine(&disjoint);
        assert_eq!(
            with_symbols,
            Audience::nobody(),
            "an empty clause absorbs the intersection"
        );

        // Stable-reader clauses merge by exact intersection.
        let ab = Audience::restricted([reader("a"), reader("b")]);
        let bc = Audience::restricted([reader("b"), reader("c")]);
        assert_eq!(ab.combine(&bc), Audience::restricted([reader("b")]));
        let email = Audience::restricted([reader("a@x.example"), reader("b")]);
        assert_eq!(
            email.combine(&Audience::restricted([reader("a@x.example")])),
            Audience::restricted([reader("a@x.example")]),
            "the email principal namespace is reserved, hence stable and mergeable"
        );

        // A reader a deployment could canonicalize keeps its clause: raw-disjoint is not
        // principal-disjoint, so no exact intersection may erase it.
        let qualified = Audience::restricted([reader("slack:u1")]);
        let plain = Audience::restricted([reader("a")]);
        assert_eq!(qualified.combine(&plain).clauses().count(), 2);

        // Structural subsumption retains the narrower clause and drops the broader one.
        let self_only = Audience::of_clauses([clause([ChainAudience::Self_], [], [])]);
        let internal = Audience::of_clauses([clause([ChainAudience::Internal], [], [])]);
        assert_eq!(self_only.combine(&internal), self_only);
        let narrow = Audience::of_clauses([clause([], ["finance"], ["a"])]);
        let broad = Audience::of_clauses([clause([], ["finance"], ["a", "b"])]);
        assert_eq!(narrow.combine(&broad), narrow);
    }

    #[test]
    fn a_clause_unions_and_a_union_of_chains_is_their_maximum() {
        let both = Clause::new([ChainAudience::Self_, ChainAudience::Internal], [], []).unwrap();
        assert_eq!(both.chain(), Some(ChainAudience::Internal));
    }

    #[test]
    fn group_references_use_one_grammar() {
        assert_eq!(GroupRef::parse("finance"), Some(named("finance")));
        assert_eq!(
            GroupRef::parse("google-workspace:group/finance@corp.com"),
            Some(GroupRef::Source {
                provider: "google-workspace".into(),
                selector: "group/finance@corp.com".into()
            })
        );
        assert_eq!(GroupRef::parse(""), None);
        assert_eq!(GroupRef::parse("slack:"), None);
        assert_eq!(GroupRef::parse(":x"), None);
        assert_eq!(named("finance").to_string(), "@finance");
        assert_eq!(
            GroupRef::Source {
                provider: "slack".into(),
                selector: "user-group/eng".into()
            }
            .to_string(),
            "@slack:user-group/eng"
        );
    }

    #[test]
    fn a_label_round_trips_through_serde_verbatim() {
        let label = Label::new(
            Trust::new(1),
            Audience::of_clauses([
                clause([ChainAudience::Internal], [], []),
                clause([], ["finance"], ["audit@corp.example"]),
            ]),
        );
        let bytes = serde_json::to_string(&label).expect("a label serializes");
        let back: Label = serde_json::from_str(&bytes).expect("and deserializes");
        assert_eq!(back, label);
        assert_eq!(serde_json::to_string(&back).unwrap(), bytes);
    }

    #[test]
    fn a_record_holds_canonical_audiences_only() {
        // {self} ∩ {internal} in one stored value is non-canonical: the broader clause must
        // already be gone. A forged record fails to decode.
        let forged = serde_json::json!([{ "chain": "self" }, { "chain": "internal" }]);
        assert!(serde_json::from_value::<Audience>(forged).is_err());
        let canonical = serde_json::json!([{ "chain": "self" }]);
        assert!(serde_json::from_value::<Audience>(canonical).is_ok());
        let reserved_reader = serde_json::json!([{ "readers": ["internal"] }]);
        assert!(serde_json::from_value::<Audience>(reserved_reader).is_err());
    }

    #[test]
    fn bottom_folds_fail_closed() {
        let held = Label::new(Trust::new(3), Audience::public());
        let folded = held.combine(&Label::bottom());
        assert_eq!(folded.trust, Trust::new(0));
        assert_eq!(folded.audience, Audience::nobody());
        assert!(!folded.meets_floor(Trust::new(1)));
        let (nothing, providers, unanswered) = context_free();
        assert_eq!(
            folded.covers(
                &DeclaredAudience::restricted([reader("a")]),
                &MembershipContext::new(&nothing, &providers, &unanswered),
            ),
            Evaluation::Fails
        );
    }
}
