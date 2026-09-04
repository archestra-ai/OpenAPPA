//! The immutable registry: the engine's static capability, built once and validated at load.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::audience::{AudienceConfig, AudienceRegistry, SelectorSpec, Unroutable};
use crate::authority::{Authority, DeclaredTransition, Hint, Sanitizer};
use crate::contract::{AudienceRequirement, HistoryRequirement, RecipientSpec, ToolAnnotation, ToolDeclaration};
use crate::fact::EffectKind;
use crate::label::{
    Audience, DeclaredAudience, Evaluation, Expansions, GroupRef, MembershipContext, ReaderId, SymbolicAtom, Trust,
};
use crate::names::{AnnotatorName, AuthorityName, MarkName, SanitizerName, TagName};
use crate::value::{ToolDeclarationId, ToolName};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) enum ToolMatcher {
    Bare,
    Arguments(ArgumentPatterns),
}

/// A non-empty conjunction of argument patterns, keyed by argument name. The map is the
/// normal form: a conjunction is commutative and one argument carries one pattern, so
/// `tool(owner:x,repo:y)` and `tool(repo:y,owner:x)` are the same matcher and the same
/// policy identity. Non-emptiness is the constructor's invariant, so a matcher that would
/// match every call vacuously cannot be built — that is what [`ToolMatcher::Bare`] is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ArgumentPatterns(BTreeMap<String, Vec<PatternPart>>);

impl ArgumentPatterns {
    /// The clause that proves a conjunction holds at least one: a conjunction is built from
    /// it outwards, so the empty state has no constructor at all.
    fn first(argument: String, pattern: Vec<PatternPart>) -> ArgumentPatterns {
        ArgumentPatterns(BTreeMap::from([(argument, pattern)]))
    }

    /// Add a conjunct. `None` when `argument` is already declared: one argument carries one
    /// pattern, so a repeated name is an authoring mistake rather than a second conjunct.
    fn and(&mut self, argument: String, pattern: Vec<PatternPart>) -> Option<()> {
        self.0.insert(argument, pattern).is_none().then_some(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) enum PatternPart {
    Literal(String),
    Wildcard,
}

impl ToolMatcher {
    /// Every declared clause must match: the argument is present, it is a string, and its
    /// value matches that clause's pattern. A missing or non-string argument does not match.
    fn matches(&self, arguments: &serde_json::Value) -> bool {
        match self {
            ToolMatcher::Bare => true,
            ToolMatcher::Arguments(ArgumentPatterns(clauses)) => {
                let clause_matches = |(argument, pattern): (&String, &Vec<PatternPart>)| {
                    arguments
                        .get(argument)
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|value| wildcard_matches(pattern, value))
                };
                // Clauses that reject on a lookup run before clauses that scan the value. A
                // conjunction is commutative, so this moves cost and never the answer: an
                // absent argument or a literal mismatch rejects the selector without reading
                // a long value at all. Argument-name order is the normal form the identity
                // digest reads, never an evaluation order, and normalizing it is exactly what
                // takes this choice away from the policy author.
                let scans = |pattern: &[PatternPart]| pattern.contains(&PatternPart::Wildcard);
                clauses
                    .iter()
                    .filter(|(_, pattern)| !scans(pattern))
                    .all(clause_matches)
                    && clauses.iter().filter(|(_, pattern)| scans(pattern)).all(clause_matches)
            }
        }
    }
}

fn wildcard_matches(pattern: &[PatternPart], value: &str) -> bool {
    let mut part = 0;
    let mut offset = 0;
    let mut wildcard = None;
    loop {
        match pattern.get(part) {
            Some(PatternPart::Wildcard) => {
                part += 1;
                wildcard = Some((part, offset));
            }
            Some(PatternPart::Literal(literal)) if value[offset..].starts_with(literal) => {
                part += 1;
                offset += literal.len();
            }
            None if offset == value.len() => return true,
            Some(PatternPart::Literal(_)) | None => {
                let Some((after, consumed)) = wildcard else {
                    return false;
                };
                let Some(next) = value[consumed..].chars().next() else {
                    return false;
                };
                offset = consumed + next.len_utf8();
                part = after;
                wildcard = Some((after, offset));
            }
        }
    }
}

fn push_wildcard(parts: &mut Vec<PatternPart>, literal: &mut String) {
    if !literal.is_empty() {
        parts.push(PatternPart::Literal(std::mem::take(literal)));
    }
    if !matches!(parts.last(), Some(PatternPart::Wildcard)) {
        parts.push(PatternPart::Wildcard);
    }
}

/// One `argument:pattern` clause, read from `chars` up to an unescaped `,` or the selector's
/// closing `)`. Returns the clause and whether the selector closed here; `None` is a
/// malformed clause.
fn parse_clause(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<(String, Vec<PatternPart>, bool)> {
    let mut argument = String::new();
    loop {
        match chars.next()? {
            ':' => break,
            // An argument name carries no escapes, so a backslash is malformed rather than
            // an escape here, and the clause and selector delimiters cannot appear in one.
            c if matches!(c, '(' | ')' | ',' | '\\') || c.is_control() => return None,
            c => argument.push(c),
        }
    }
    if argument.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    let mut literal = String::new();
    let closed = loop {
        match chars.next()? {
            '*' => push_wildcard(&mut parts, &mut literal),
            '\\' => match chars.next()? {
                next @ ('*' | ')' | '\\' | ',') => literal.push(next),
                _ => return None,
            },
            ',' => break false,
            ')' if chars.peek().is_none() => break true,
            ')' => return None,
            other => literal.push(other),
        }
    };
    if !literal.is_empty() {
        parts.push(PatternPart::Literal(literal));
    }
    Some((argument, parts, closed))
}

/// What a `[[tool]]` declaration names once its argument selector is split off: one tool by
/// its exact name, or the wildcard — the contract covering every tool call the policy does not
/// name. The wildcard is not a tool name: it never keys the registry, never appears in a
/// listing, and carries no metadata or selector.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ContractName {
    Wildcard,
    Named(ToolName),
}

impl ContractName {
    fn parse(tool: &str) -> ContractName {
        match tool {
            WILDCARD_SPELLING => ContractName::Wildcard,
            named => ContractName::Named(ToolName::new(named)),
        }
    }
}

/// Split an authored declaration name into the contract it names and the matcher that selects
/// it: `Tool` alone, or `Tool(argument:pattern[,argument:pattern...])`.
fn parse_tool_selector(authored: &str) -> Result<(ContractName, ToolMatcher), LoadError> {
    let malformed = || LoadError::MalformedToolSelector(authored.to_string());
    if !authored.contains(['(', ')']) {
        return (!authored.is_empty())
            .then(|| (ContractName::parse(authored), ToolMatcher::Bare))
            .ok_or_else(malformed);
    }
    let open = authored.find('(').ok_or_else(malformed)?;
    let tool = &authored[..open];
    if tool.is_empty() || tool.contains(')') {
        return Err(malformed());
    }
    let mut chars = authored[open + 1..].chars().peekable();
    let (argument, pattern, mut closed) = parse_clause(&mut chars).ok_or_else(malformed)?;
    let mut clauses = ArgumentPatterns::first(argument, pattern);
    while !closed {
        let (argument, pattern, next) = parse_clause(&mut chars).ok_or_else(malformed)?;
        clauses.and(argument, pattern).ok_or_else(malformed)?;
        closed = next;
    }
    Ok((ContractName::parse(tool), ToolMatcher::Arguments(clauses)))
}

#[cfg(test)]
pub(crate) fn contract_name(authored: &ToolName) -> Result<ContractName, LoadError> {
    parse_tool_selector(authored.as_str()).map(|(name, _)| name)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustChain {
    ranks: Vec<String>,
}

/// The most ranks a chain may hold: a rank is a [`Trust`] index held in a `u8`, so the chain cannot
/// exceed 256 ranks without a higher index silently truncating to a lower one.
pub const MAX_RANKS: usize = 256;

impl TrustChain {
    pub fn new(ranks: Vec<String>) -> Self {
        TrustChain { ranks }
    }

    /// Reject a chain that cannot map to distinct `u8` ranks: empty, over [`MAX_RANKS`] (index
    /// truncation), or with a repeated name (`rank_of` would silently alias the second to the first).
    pub fn validate(&self) -> Result<(), LoadError> {
        if self.ranks.is_empty() {
            return Err(LoadError::EmptyTrustChain);
        }
        if self.ranks.len() > MAX_RANKS {
            return Err(LoadError::TrustChainTooLong {
                len: self.ranks.len(),
                max: MAX_RANKS,
            });
        }
        for (i, rank) in self.ranks.iter().enumerate() {
            if self.ranks[..i].contains(rank) {
                return Err(LoadError::DuplicateRank(rank.clone()));
            }
        }
        Ok(())
    }

    pub fn rank_of(&self, name: &str) -> Option<Trust> {
        self.ranks.iter().position(|r| r == name).map(|i| Trust::new(i as u8))
    }

    pub fn name_of(&self, trust: Trust) -> Option<&str> {
        self.ranks.get(trust.rank() as usize).map(String::as_str)
    }

    /// Every rank name, lowest first.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.ranks.iter().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.ranks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ranks.is_empty()
    }

    pub(crate) fn contains_rank(&self, trust: Trust) -> bool {
        (trust.rank() as usize) < self.ranks.len()
    }
}

/// A registered Annotator: the boundary the policy routes per-call annotation through, named by
/// declarations that carry `annotator = "..."` instead of static semantics. Each optional field
/// narrows the vocabulary its produced annotations may draw on; an omitted field is the whole
/// policy vocabulary — every chain rank, every literal reader the policy writes, every declared
/// attention mark, every declared effect kind.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnotatorDeclaration {
    pub name: AnnotatorName,
    #[serde(default)]
    pub trust: Option<BTreeSet<Trust>>,
    #[serde(default)]
    pub audiences: Option<BTreeSet<ReaderId>>,
    #[serde(default)]
    pub marks: Option<BTreeSet<MarkName>>,
    #[serde(default)]
    pub effects: Option<BTreeSet<EffectKind>>,
}

/// One Annotator's compiled mandate: the closed vocabulary a produced annotation may use, with
/// every omitted bound resolved to the whole policy vocabulary at load. The engine holds the
/// answer to it at the check and again on replay; the runtime restates it to the Annotator so an
/// implementation knows the vocabulary before it answers. `public` is always an admissible
/// audience state — the reader set names who a restricted answer may include.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnnotatorMandate {
    trust: BTreeSet<Trust>,
    audiences: BTreeSet<ReaderId>,
    marks: BTreeSet<MarkName>,
    effects: BTreeSet<EffectKind>,
}

impl AnnotatorMandate {
    pub fn trust_ranks(&self) -> impl Iterator<Item = Trust> + '_ {
        self.trust.iter().copied()
    }

    pub fn audiences(&self) -> impl Iterator<Item = &ReaderId> {
        self.audiences.iter()
    }

    pub fn marks(&self) -> impl Iterator<Item = &MarkName> {
        self.marks.iter()
    }

    pub fn effects(&self) -> impl Iterator<Item = &EffectKind> {
        self.effects.iter()
    }

    pub(crate) fn permits_trust(&self, trust: Trust) -> bool {
        self.trust.contains(&trust)
    }

    pub(crate) fn permits_reader(&self, reader: &ReaderId) -> bool {
        self.audiences.contains(reader)
    }

    pub(crate) fn permits_mark(&self, mark: &MarkName) -> bool {
        self.marks.contains(mark)
    }

    pub(crate) fn permits_effect(&self, effect: &EffectKind) -> bool {
        self.effects.contains(effect)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryConfig {
    pub trust_chain: TrustChain,
    pub tools: Vec<ToolDeclaration>,
    /// The registered Annotators, by name. A tool declaration routing through a name not
    /// registered here is a load error.
    #[serde(default)]
    pub annotators: Vec<AnnotatorDeclaration>,
    pub authorities: Vec<Authority>,
    pub sanitizers: Vec<Sanitizer>,
    /// The audience side of the policy: registered sources, the chain mappings, the configured
    /// named audiences, and the identity implementation. All of it is policy meaning; how a
    /// deployment reaches a source or the identity implementation never enters.
    #[serde(default)]
    pub audience: AudienceConfig,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LoadError {
    #[error("empty trust chain: at least one rank required")]
    EmptyTrustChain,
    #[error("trust chain too long: {len} ranks (a rank is a u8, so at most {max})")]
    TrustChainTooLong { len: usize, max: usize },
    #[error("duplicate trust rank {0:?} in the chain")]
    DuplicateRank(String),
    #[error(
        "the wildcard tool \"*\" declares static semantics — it covers calls the policy does not name, so it routes through an annotator: give it `annotator` and nothing else"
    )]
    WildcardStatic,
    #[error(
        "the wildcard tool \"*\" carries metadata or an argument selector — it covers calls the policy does not name, so it describes none of them"
    )]
    WildcardMetadata,
    #[error("the policy writes more than one wildcard tool \"*\"")]
    DuplicateWildcard,
    #[error("duplicate tool contract: {0}")]
    DuplicateTool(String),
    #[error("malformed tool selector {0:?}")]
    MalformedToolSelector(String),
    #[error("provider-run tool {0} cannot use an argument selector")]
    ProviderRunSelector(String),
    #[error("tool {0} has more than u32::MAX ordered contracts")]
    TooManyToolVariants(String),
    #[error("duplicate annotator: {0}")]
    DuplicateAnnotator(String),
    #[error("tool {tool} names annotator {annotator}, which the policy does not register")]
    UnknownAnnotator { tool: String, annotator: String },
    #[error(
        "provider-run tool {0} routes through an Annotator: its result reaches the model inside the inference call, so nothing would consume a per-call annotation"
    )]
    ProviderRunAnnotated(String),
    #[error("duplicate authority: {0}")]
    DuplicateAuthority(String),
    #[error("duplicate sanitizer: {0}")]
    DuplicateSanitizer(String),
    #[error("authority {0} has an empty mandate (covers nothing)")]
    EmptyMandate(String),
    #[error("trust rank {rank} out of the chain (length {len}) in {context}")]
    RankOutOfChain { rank: u8, len: usize, context: String },
    #[error(
        "tool {tool}: {count} worst-case alternative remedy plans exceed the planner cap of {max} — reduce the requirement entries, the competent authorities, or the clearing tools, or raise `[limits] planner_cap`"
    )]
    TooManyPlanAlternatives { tool: String, count: u128, max: u128 },
    #[error(
        "confined-return stage: {count} worst-case sanitizer alternatives exceed the planner cap of {max} — reduce the registered output sanitizers or raise `[limits] planner_cap`"
    )]
    TooManyReturnPlanAlternatives { count: u128, max: u128 },
    #[error("{context}: hint is {len} characters, over the maximum {max}")]
    HintTooLong { context: String, len: usize, max: usize },
    #[error(
        "{context}: {reader:?} is not a literal reader ID — `public`, `self`, and `internal` are audience states, and the `@` mark is reserved for group references"
    )]
    NonLiteralReader { context: String, reader: String },
    #[error("{context}: {fault} — a policy-written audience reference must resolve at load")]
    UnroutableAudience { context: String, fault: Unroutable },
    #[error("audience source provider {0:?} is registered more than once")]
    DuplicateAudienceProvider(String),
    #[error(
        "audience source provider name {0:?} is malformed: a provider is a non-empty name without `:` or a leading `@`"
    )]
    MalformedAudienceProvider(String),
    #[error("named audience name {0:?} is malformed: a name is non-empty, written bare, and holds no `:`")]
    MalformedNamedAudience(String),
    #[error("named audience @{0} is configured more than once")]
    DuplicateNamedAudience(String),
    #[error("named audience @{0} lists no sources")]
    EmptyAudienceFrom(String),
    #[error("the deployment declaration names unregistered tool {tool} in {slot}")]
    UnknownDeploymentTool {
        slot: crate::profile::CoverageSlot,
        tool: String,
    },
    #[error(
        "tool {tool} is provider-run and cannot be a confined result point: its result reaches the model inside the inference call, before any host could withhold it"
    )]
    ConfinedProviderRun { tool: String },
    #[error(
        "sanitizer {sanitizer} registers on tool_output but the deployment confines no application point — neither a result point nor, under context control, a child's return"
    )]
    OutputSanitizerUncovered { sanitizer: String },
    #[error(
        "sanitizer {0} registers on tool_input with a trust transition: only the `contains` check reads an input substitution, so a trust `to` can never help a call and the sanitizer would sit inert"
    )]
    InputSanitizerTrust(String),
    #[error(
        "sanitizer attest-schema declares an audience mandate: the reserved builtin vouches the channel shape, and structure claims only trust"
    )]
    AttestSchemaAudienceMandate,
    #[error(
        "sanitizer attest-schema lacks the tool_output point: the quarantine exit it is reserved for is a child-return crossing, a tool_output application"
    )]
    AttestSchemaNotOutput,
    #[error(
        "sanitizer attest-schema declares a scope: a child return originates from no tool, so the reserved builtin is unscoped"
    )]
    AttestSchemaScoped,
    #[error("provider-run tool {tool} declares {construct}: a provider-run contract may declare only a static delta")]
    ProviderRunConstruct {
        tool: String,
        construct: crate::profile::ProviderRunConstruct,
    },
    #[error(
        "{context} binds audience argument {argument:?}, which {fault}: a placeholder names a required top-level string property of the tool's `parameters`"
    )]
    AudienceBindingSchema {
        context: String,
        argument: String,
        fault: crate::params::PropertyFault,
    },
}

/// The planner cap: the most alternatives one current-stage plan menu may hold — per
/// tool, the grouped-assignment product times its release paths plus its direct-redispatch
/// candidates; per catalogue, the confined child-return menu. The bound keeps enumeration total
/// (no runtime truncation: "every sound alternative" is literal). Deployment configuration
/// sets it via `[limits] planner_cap`; omitted, the cap is 4096. The bound is a sum as much as
/// a product: for an annotated tool every audience-narrowing tool in the catalogue counts as a
/// redispatch candidate, so the default admits a catalogue of a few thousand tools before a
/// deployment has to raise it. Zero is unrepresentable: every stage's worst case is at least
/// one, so a zero cap would refuse every registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlannerCap(u128);

impl PlannerCap {
    /// `None` for zero — refuse it at the configuration boundary rather than carry a cap that
    /// cannot admit any tool.
    pub fn new(cap: u64) -> Option<PlannerCap> {
        (cap > 0).then_some(PlannerCap(cap as u128))
    }
}

impl Default for PlannerCap {
    fn default() -> Self {
        PlannerCap(4096)
    }
}

/// The longest hint a registration may carry. OpenAPPA includes Authority and Sanitizer hints in
/// remedy offers and embeds component hints in model consult system prompts. Bounding the hint
/// length prevents trusted configuration from flooding either context. A sentence or two is the
/// intended shape.
pub const MAX_HINT_CHARS: usize = 512;

fn worst_case_plan_alternatives(
    declaration: &ToolDeclaration,
    confined: bool,
    context_control: bool,
    tools: &[&ToolDeclaration],
    authorities: &[Authority],
    sanitizers: &[Sanitizer],
    context: &MembershipContext<'_>,
) -> u128 {
    use crate::check::Gap;
    use crate::plan::{covers_gap, gap_cover};

    let tags = declaration.tags();
    let mut count: u128 = 1;
    let mut multiply = |competent: usize| count = count.saturating_mul(competent.max(1) as u128);
    // An Annotator-produced floor or requirement is unknown at load, so its competent-authority
    // count is the mandate-dimension approximation — computed only where one may appear.
    let trust_cap_competent = || {
        authorities
            .iter()
            .filter(|authority| authority.scope.covers(tags) && authority.mandate.trust_ceiling.is_some())
            .count()
    };
    let reader_cap_competent = || {
        authorities
            .iter()
            .filter(|authority| authority.scope.covers(tags) && authority.mandate.reader_ceiling.is_some())
            .count()
    };
    // A static `includes` is decided at load wherever the derivability calculus settles it.
    // The lint reads no directory, so a comparison that would need membership answers stays
    // counted: ruling such an authority out would UNDERCOUNT, and the cap is the only bound
    // on how many plans one block surfaces. Only a definitive non-cover drops out.
    let includes_competent = |recipients: &DeclaredAudience| {
        authorities
            .iter()
            .filter(|authority| {
                if authority.mandate.reader_ceiling.is_none() || !authority.scope.covers(tags) {
                    return false;
                }
                let gap = Gap::Includes {
                    recipients: recipients.clone(),
                };
                !matches!(gap_cover(authority, &gap, tags, context), Evaluation::Fails)
            })
            .count()
    };

    match declaration {
        // An Annotated declaration's requirements exist only per call: the Annotator may answer
        // any slot its mandate allows, so the lint takes the worst case on every slot at once —
        // a produced trust floor, a produced `contains`, and any mark an authority can attend.
        ToolDeclaration::Annotated { .. } => {
            multiply(trust_cap_competent());
            multiply(reader_cap_competent());
            let dynamic_marks: BTreeSet<_> = authorities
                .iter()
                .flat_map(|authority| authority.mandate.attends.iter())
                .cloned()
                .collect();
            for mark in dynamic_marks {
                let gap = Gap::Attention(mark);
                multiply(
                    authorities
                        .iter()
                        .filter(|authority| covers_gap(authority, &gap, tags, context))
                        .count(),
                );
            }
        }
        ToolDeclaration::Declared(tool) => {
            if let Some(floor) = tool.requires.trust_floor() {
                let gap = Gap::TrustFloor {
                    required: floor,
                    actual: floor,
                };
                multiply(
                    authorities
                        .iter()
                        .filter(|authority| covers_gap(authority, &gap, tags, context))
                        .count(),
                );
            }
            let mut seen_includes: Vec<&AudienceRequirement> = Vec::new();
            for requirement in tool.requires.audience_requirements() {
                match requirement {
                    AudienceRequirement::Includes(spec) if !seen_includes.contains(&requirement) => {
                        seen_includes.push(requirement);
                        match spec {
                            RecipientSpec::Static(recipients) => multiply(includes_competent(recipients)),
                            // A placeholder's recipients come from the call, so nothing about
                            // the gap is known here.
                            RecipientSpec::Placeholder(_) => multiply(reader_cap_competent()),
                        }
                    }
                    AudienceRequirement::Includes(_) | AudienceRequirement::Cap(_) => {}
                }
            }
            let mut seen_no_prior: Vec<&EffectKind> = Vec::new();
            for requirement in &tool.requires.history {
                match requirement {
                    HistoryRequirement::NoPrior(kind) if !seen_no_prior.contains(&kind) => {
                        seen_no_prior.push(kind);
                        let gap = Gap::NoPrior(kind.clone());
                        multiply(
                            authorities
                                .iter()
                                .filter(|authority| covers_gap(authority, &gap, tags, context))
                                .count(),
                        );
                    }
                    HistoryRequirement::NoPrior(_) | HistoryRequirement::Prior(_) => {}
                }
            }
            let mut seen_marks: Vec<&crate::names::MarkName> = Vec::new();
            for mark in tool.requires.attention_marks() {
                if seen_marks.contains(&mark) {
                    continue;
                }
                seen_marks.push(mark);
                let gap = Gap::Attention(mark.clone());
                multiply(
                    authorities
                        .iter()
                        .filter(|authority| covers_gap(authority, &gap, tags, context))
                        .count(),
                );
            }
        }
    }

    let applicable = match declaration {
        _ if !confined => 0,
        // A produced output label exists only per call, so no load-time `may_admit` filter can
        // rule a sanitizer out.
        ToolDeclaration::Annotated { .. } => sanitizers
            .iter()
            .filter(|sanitizer| !sanitizer.name.is_attest_schema())
            .filter(|sanitizer| sanitizer.on.output && sanitizer.applies_to(tags))
            .count(),
        ToolDeclaration::Declared(tool) if tool.delta.symbolic_atoms().next().is_some() => sanitizers
            .iter()
            .filter(|sanitizer| !sanitizer.name.is_attest_schema())
            .filter(|sanitizer| sanitizer.on.output && sanitizer.applies_to(tags))
            .count(),
        ToolDeclaration::Declared(tool) => {
            let output = tool.output_label();
            sanitizers
                .iter()
                .filter(|sanitizer| !sanitizer.name.is_attest_schema())
                .filter(|sanitizer| {
                    sanitizer.on.output
                        && sanitizer.applies_to(tags)
                        && sanitizer.transition.may_admit(&output, context)
                })
                .count()
        }
    };
    multiply(applicable + 1);
    // Under context control any call may be a marked spawn, whose every plan ends in one of the
    // return declarations: the bare floor, or the floor behind an untagged output sanitizer.
    if context_control {
        multiply(worst_case_return_options(sanitizers));
    }

    // What the Annotator may require is unknown at load, so every candidate that emits at all,
    // and every audience-narrowing candidate, stays counted as a redispatch.
    let (priors, has_cap): (Vec<&EffectKind>, bool) = match declaration {
        ToolDeclaration::Annotated { .. } => (Vec::new(), true),
        ToolDeclaration::Declared(tool) => (
            tool.requires
                .history
                .iter()
                .filter_map(|requirement| match requirement {
                    HistoryRequirement::Prior(kind) => Some(kind),
                    HistoryRequirement::NoPrior(_) => None,
                })
                .collect(),
            tool.requires
                .audience_requirements()
                .iter()
                .any(|requirement| matches!(requirement, AudienceRequirement::Cap(DeclaredAudience::Union(_)))),
        ),
    };
    let any_prior = matches!(declaration, ToolDeclaration::Annotated { .. });
    let redispatches = tools
        .iter()
        .filter(|candidate| match candidate {
            ToolDeclaration::Declared(tool) => {
                tool.emits.iter().any(|kind| priors.contains(&kind))
                    || (any_prior && !tool.emits.is_empty())
                    || (has_cap && matches!(tool.delta.audience.as_ref(), Some(DeclaredAudience::Union(_))))
            }
            // An Annotated candidate's emits and delta are unknown at load; the cap is the only
            // bound, so it stays counted wherever a prior or a cap could match it.
            ToolDeclaration::Annotated { .. } => any_prior || !priors.is_empty() || has_cap,
        })
        .count() as u128;
    let input_hops = sanitizers
        .iter()
        .filter(|sanitizer| sanitizer.on.input && sanitizer.applies_to(tags))
        .count() as u128;
    count
        .saturating_add(redispatches)
        .saturating_add(input_hops)
        .max(worst_case_confined_stage(sanitizers, confined, tags))
}

fn worst_case_confined_stage(sanitizers: &[Sanitizer], confined: bool, tags: &[TagName]) -> u128 {
    if !confined {
        return 1;
    }
    1u128.saturating_add(
        sanitizers
            .iter()
            .filter(|sanitizer| !sanitizer.name.is_attest_schema())
            .filter(|sanitizer| sanitizer.on.output && sanitizer.applies_to(tags))
            .count() as u128,
    )
}

fn worst_case_return_options(sanitizers: &[Sanitizer]) -> usize {
    1 + sanitizers
        .iter()
        .filter(|sanitizer| sanitizer.on.output && sanitizer.applies_to(&[]))
        .count()
}

/// The wildcard's spelling in a policy: `[[tool]] name = "*"` covers every tool call the policy
/// does not name exactly, and routes each covered call through its annotator.
pub(crate) const WILDCARD_SPELLING: &str = "*";

/// How the registry classifies a proposed tool name: declared and checkable, declared as
/// provider-run (never checked), or covered by the wildcard — annotated per call. A name in
/// none of these classes has no contract at all, and a proposal naming it is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolKind {
    Declared,
    ProviderRun,
    Wildcard,
}

/// The validated, indexed, immutable registry: the engine's whole static capability, declarations
/// and coverage together. The deployment profile splits the catalogue at build: provider-run
/// tools live apart from the checkable declarations, so the check, plan enumeration,
/// redispatch offers, and the planner-cap bound exclude them by construction — no call site
/// filters. The profile itself rides along, so plan and branch enumeration read confinement and
/// context control from the one capability object they already hold.
#[derive(Clone, Debug)]
pub struct Registry {
    trust_chain: TrustChain,
    audience_readers: BTreeSet<ReaderId>,
    tools: BTreeMap<ToolName, Vec<(ToolMatcher, ToolDeclaration)>>,
    provider_run: BTreeMap<ToolName, ToolAnnotation>,
    /// The wildcard declaration, when the policy writes one: the Annotated declaration every
    /// tool call the policy does not name exactly resolves to. In no listing or vector; the
    /// policy identity carries it through the declared configuration.
    wildcard: Option<ToolDeclaration>,
    annotators: BTreeMap<AnnotatorName, AnnotatorMandate>,
    authorities: Vec<Authority>,
    attention_marks: BTreeSet<MarkName>,
    sanitizers: BTreeMap<SanitizerName, Sanitizer>,
    audience: AudienceRegistry,
    /// The declared audience configuration as loaded — what the policy identity carries;
    /// the validated registry beside it is derived and never serialized.
    audience_config: crate::audience::AudienceConfig,
    profile: crate::profile::DeploymentProfile,
}

impl Registry {
    /// Build and validate the catalogue under the deployment profile: structural lints, the
    /// provider-run split, and the profile-exact planner-cap bound. The profile-blind
    /// form does not exist — [`crate::engine::Engine::open`] is the one public path here.
    pub(crate) fn build(
        config: RegistryConfig,
        planner_cap: PlannerCap,
        profile: crate::profile::DeploymentProfile,
    ) -> Result<Registry, LoadError> {
        config.trust_chain.validate()?;
        let audience_readers = configured_audience_readers(&config, &profile);
        let audience = validated_audience_registry(&config.audience)?;
        for clause in profile.starting_label().audience.clauses() {
            check_literal(clause.readers(), || "starting label".to_string())?;
        }
        check_routable(&audience, profile.starting_label().audience.symbolic_atoms(), || {
            "starting label".to_string()
        })?;

        // Sanitizers index first: the child return-sanitizer binding validates against them.
        let mut sanitizers = BTreeMap::new();
        for sanitizer in config.sanitizers {
            let context = || format!("sanitizer {}", sanitizer.name.as_str());
            if sanitizer.name.is_attest_schema() {
                if matches!(sanitizer.transition, DeclaredTransition::Audience { .. }) {
                    return Err(LoadError::AttestSchemaAudienceMandate);
                }
                if !sanitizer.on.output {
                    return Err(LoadError::AttestSchemaNotOutput);
                }
                if !sanitizer.scope.is_unscoped() {
                    return Err(LoadError::AttestSchemaScoped);
                }
            }
            match &sanitizer.transition {
                DeclaredTransition::Trust { from_floor, to } => {
                    check_rank(&config.trust_chain, Some(*from_floor), || format!("{} from", context()))?;
                    check_rank(&config.trust_chain, Some(*to), || format!("{} to", context()))?;
                    if sanitizer.on.input {
                        return Err(LoadError::InputSanitizerTrust(sanitizer.name.as_str().to_string()));
                    }
                }
                DeclaredTransition::Audience { from_includes, to } => {
                    check_declared(&audience, from_includes, || format!("{} from", context()))?;
                    check_declared(&audience, to, || format!("{} to", context()))?;
                }
            }
            check_hint(sanitizer.hint.as_ref(), context)?;
            if sanitizers.insert(sanitizer.name.clone(), sanitizer.clone()).is_some() {
                return Err(LoadError::DuplicateSanitizer(sanitizer.name.as_str().to_string()));
            }
        }

        // Annotators index next: a tool declaration routing through one validates against them.
        let mut annotator_declarations: BTreeMap<AnnotatorName, AnnotatorDeclaration> = BTreeMap::new();
        for annotator in config.annotators {
            let context = || format!("annotator {}", annotator.name.as_str());
            for rank in annotator.trust.iter().flatten() {
                check_rank(&config.trust_chain, Some(*rank), context)?;
            }
            if let Some(audiences) = &annotator.audiences {
                check_literal(audiences, context)?;
            }
            let name = annotator.name.clone();
            if annotator_declarations.insert(name.clone(), annotator).is_some() {
                return Err(LoadError::DuplicateAnnotator(name.as_str().to_string()));
            }
        }

        let mut tools: BTreeMap<ToolName, Vec<(ToolMatcher, ToolDeclaration)>> = BTreeMap::new();
        let mut provider_run: BTreeMap<ToolName, ToolAnnotation> = BTreeMap::new();
        let mut wildcard: Option<ToolDeclaration> = None;
        for mut declaration in config.tools {
            let (contract, matcher) = parse_tool_selector(declaration.name().as_str())?;
            let base_name = match contract {
                ContractName::Named(name) => name,
                ContractName::Wildcard => {
                    Self::admit_wildcard(&declaration, &matcher, &annotator_declarations)?;
                    if wildcard.replace(declaration).is_some() {
                        return Err(LoadError::DuplicateWildcard);
                    }
                    continue;
                }
            };
            declaration.set_name(base_name);
            match &declaration {
                ToolDeclaration::Declared(tool) => {
                    check_rank(&config.trust_chain, tool.delta.trust, || {
                        format!("tool {} delta", tool.name.as_str())
                    })?;
                    check_rank(&config.trust_chain, tool.requires.trust_floor(), || {
                        format!("tool {} trust floor", tool.name.as_str())
                    })?;
                    if let Some(declared) = tool.delta.audience.as_ref() {
                        check_declared(&audience, declared, || format!("tool {} delta", tool.name.as_str()))?;
                    }
                    for requirement in tool.requires.audience_requirements() {
                        match requirement {
                            AudienceRequirement::Includes(RecipientSpec::Static(recipients)) => {
                                check_declared(&audience, recipients, || {
                                    format!("tool {} contains", tool.name.as_str())
                                })?;
                            }
                            AudienceRequirement::Cap(cap) => {
                                check_declared(&audience, cap, || format!("tool {} within", tool.name.as_str()))?;
                            }
                            AudienceRequirement::Includes(RecipientSpec::Placeholder(_)) => {}
                        }
                    }
                }
                ToolDeclaration::Annotated { name, annotator, .. } => {
                    if !annotator_declarations.contains_key(annotator) {
                        return Err(LoadError::UnknownAnnotator {
                            tool: name.as_str().to_string(),
                            annotator: annotator.as_str().to_string(),
                        });
                    }
                }
            }
            if profile.is_provider_run(declaration.name()) {
                if matcher != ToolMatcher::Bare {
                    return Err(LoadError::ProviderRunSelector(declaration.name().as_str().into()));
                }
                let ToolDeclaration::Declared(tool) = declaration else {
                    return Err(LoadError::ProviderRunAnnotated(declaration.name().as_str().to_string()));
                };
                let name = tool.name.clone();
                if provider_run.insert(name.clone(), tool).is_some() {
                    return Err(LoadError::DuplicateTool(name.as_str().to_string()));
                }
            } else {
                let variants = tools.entry(declaration.name().clone()).or_default();
                if ToolDeclarationId::new(variants.len()).is_none() {
                    return Err(LoadError::TooManyToolVariants(declaration.name().as_str().to_string()));
                }
                variants.push((matcher, declaration));
            }
        }

        let mut seen_authorities = BTreeMap::new();
        for authority in &config.authorities {
            if authority.mandate.is_empty() {
                return Err(LoadError::EmptyMandate(authority.name.as_str().to_string()));
            }
            check_rank(&config.trust_chain, authority.mandate.trust_ceiling, || {
                format!("authority {} trust ceiling", authority.name.as_str())
            })?;
            if let Some(ceiling) = &authority.mandate.reader_ceiling {
                check_declared(&audience, ceiling, || {
                    format!("authority {} reader ceiling", authority.name.as_str())
                })?;
            }
            check_hint(authority.hint.as_ref(), || {
                format!("authority {}", authority.name.as_str())
            })?;
            if seen_authorities.insert(authority.name.clone(), ()).is_some() {
                return Err(LoadError::DuplicateAuthority(authority.name.as_str().to_string()));
            }
        }

        for tool in tools.values().flatten().filter_map(|(_, d)| d.declared()) {
            check_audience_bindings(tool)?;
        }

        // The planner-cap lint runs the same cover evaluation planning runs, against the
        // policy facts alone: the declared `within` assertions and no directory answers.
        let no_expansions = Expansions::default();
        let membership_context =
            MembershipContext::new(audience.within_assertions(), audience.providers(), &no_expansions);

        let sanitizer_list: Vec<Sanitizer> = sanitizers.values().cloned().collect();
        let checkable_tools: Vec<&ToolDeclaration> = tools.values().flatten().map(|(_, d)| d).collect();
        for declaration in tools.values().flatten().map(|(_, d)| d).chain(wildcard.as_ref()) {
            let count = worst_case_plan_alternatives(
                declaration,
                profile.confines_result(declaration.name()),
                profile.context_control(),
                &checkable_tools,
                &config.authorities,
                &sanitizer_list,
                &membership_context,
            );
            if count > planner_cap.0 {
                return Err(LoadError::TooManyPlanAlternatives {
                    tool: declaration.name().as_str().to_string(),
                    count,
                    max: planner_cap.0,
                });
            }
        }
        // Attention names are a policy vocabulary, not an authority vocabulary. A policy may
        // deliberately use a mark as an unremediable denial (for example `blocked`) while
        // registering no authority at all. An Annotator must be able to produce those marks,
        // but still remains confined to names the policy declares somewhere — and an
        // annotator's own explicit mark bound is itself such a declaration.
        let attention_marks: BTreeSet<MarkName> = tools
            .values()
            .flatten()
            .filter_map(|(_, d)| d.declared())
            .chain(provider_run.values())
            .flat_map(|tool| tool.requires.attention_marks().iter().cloned())
            .chain(
                config
                    .authorities
                    .iter()
                    .flat_map(|authority| authority.mandate.attends.iter().cloned()),
            )
            .chain(
                annotator_declarations
                    .values()
                    .flat_map(|annotator| annotator.marks.iter().flatten().cloned()),
            )
            .collect();

        // The policy's whole effect vocabulary: every kind a declaration emits or requires, and
        // every kind an annotator's explicit bound names.
        let effect_kinds: BTreeSet<EffectKind> = tools
            .values()
            .flatten()
            .filter_map(|(_, d)| d.declared())
            .chain(provider_run.values())
            .flat_map(|tool| {
                tool.emits
                    .iter()
                    .cloned()
                    .chain(tool.requires.history.iter().map(|requirement| match requirement {
                        HistoryRequirement::Prior(kind) | HistoryRequirement::NoPrior(kind) => kind.clone(),
                    }))
                    .collect::<Vec<_>>()
            })
            .chain(
                annotator_declarations
                    .values()
                    .flat_map(|annotator| annotator.effects.iter().flatten().cloned()),
            )
            .collect();

        // Resolve each mandate's omitted bounds to the whole policy vocabulary, now that the
        // vocabulary is known.
        let every_rank: BTreeSet<Trust> = (0..config.trust_chain.len())
            .map(|rank| Trust::new(rank as u8))
            .collect();
        let annotators: BTreeMap<AnnotatorName, AnnotatorMandate> = annotator_declarations
            .into_iter()
            .map(|(name, declaration)| {
                let mandate = AnnotatorMandate {
                    trust: declaration.trust.unwrap_or_else(|| every_rank.clone()),
                    audiences: declaration.audiences.unwrap_or_else(|| audience_readers.clone()),
                    marks: declaration.marks.unwrap_or_else(|| attention_marks.clone()),
                    effects: declaration.effects.unwrap_or_else(|| effect_kinds.clone()),
                };
                (name, mandate)
            })
            .collect();

        Ok(Registry {
            trust_chain: config.trust_chain,
            audience_readers,
            tools,
            provider_run,
            wildcard,
            annotators,
            authorities: config.authorities,
            attention_marks,
            sanitizers,
            audience,
            audience_config: config.audience,
            profile,
        })
    }

    /// The wildcard covers calls this policy knows nothing about, so it is an Annotated
    /// declaration and nothing more: metadata and an argument selector describe a specific
    /// tool, so it carries none.
    fn admit_wildcard(
        declaration: &ToolDeclaration,
        matcher: &ToolMatcher,
        annotators: &BTreeMap<AnnotatorName, AnnotatorDeclaration>,
    ) -> Result<(), LoadError> {
        let ToolDeclaration::Annotated {
            tags,
            description,
            parameters,
            annotator,
            ..
        } = declaration
        else {
            return Err(LoadError::WildcardStatic);
        };
        if !annotators.contains_key(annotator) {
            return Err(LoadError::UnknownAnnotator {
                tool: WILDCARD_SPELLING.to_string(),
                annotator: annotator.as_str().to_string(),
            });
        }
        if *matcher != ToolMatcher::Bare
            || !tags.is_empty()
            || description.is_some()
            || *parameters != crate::params::ToolParameters::open()
        {
            return Err(LoadError::WildcardMetadata);
        }
        Ok(())
    }

    /// The validated audience registry: sources, chain mappings, named audiences, `within`
    /// assertions, and the identity implementation.
    pub fn audience(&self) -> &AudienceRegistry {
        &self.audience
    }

    /// The declared audience configuration, for the policy identity document.
    pub fn audience_config(&self) -> &crate::audience::AudienceConfig {
        &self.audience_config
    }

    pub fn profile(&self) -> &crate::profile::DeploymentProfile {
        &self.profile
    }

    pub fn trust_chain(&self) -> &TrustChain {
        &self.trust_chain
    }

    /// The closed audience vocabulary written by this policy. `public` is the reserved
    /// unrestricted state; the remaining entries are literal reader IDs, in stable order.
    pub fn audiences(&self) -> impl Iterator<Item = &str> {
        std::iter::once("public").chain(self.audience_readers.iter().map(ReaderId::as_str))
    }

    /// The one classification every name lookup derives from. An exact declaration always wins;
    /// the wildcard covers only a name the policy does not write. `None` is a name no contract
    /// covers: a proposal naming it is refused.
    ///
    /// [`WILDCARD_SPELLING`] is the wildcard contract's own spelling and never a tool a host
    /// dispatches, so a proposal naming it names no tool: it classifies as `None` even under a
    /// policy that writes the wildcard, rather than resolving to the contract that covers
    /// everything else.
    pub fn classify(&self, name: &ToolName) -> Option<ToolKind> {
        if name.as_str() == WILDCARD_SPELLING {
            None
        } else if self.tools.contains_key(name) {
            Some(ToolKind::Declared)
        } else if self.provider_run.contains_key(name) {
            Some(ToolKind::ProviderRun)
        } else if self.wildcard.is_some() {
            Some(ToolKind::Wildcard)
        } else {
            None
        }
    }

    /// Whether the policy declares this checkable tool exactly. Deployment coverage and
    /// provider-result admission read this: the wildcard covers a name at a proposal, but a
    /// deployment declaration naming an unwritten tool is a typo.
    pub(crate) fn declared(&self, name: &ToolName) -> bool {
        self.classify(name) == Some(ToolKind::Declared)
    }

    /// The first declaration of a name, for tests that register one declaration per tool.
    #[cfg(test)]
    pub(crate) fn tool(&self, name: &ToolName) -> Option<&ToolDeclaration> {
        match self.classify(name)? {
            ToolKind::Declared => self.tools.get(name)?.first().map(|(_, d)| d),
            ToolKind::ProviderRun => None,
            ToolKind::Wildcard => self.wildcard.as_ref(),
        }
    }

    /// The declaration a persisted call names. A wildcard-covered tool has exactly one, at
    /// ordinal zero; a record naming another ordinal for it is forged.
    pub(crate) fn keyed_tool(&self, name: &ToolName, id: ToolDeclarationId) -> Option<&ToolDeclaration> {
        match self.classify(name)? {
            ToolKind::Declared => self.tools.get(name)?.get(id.ordinal()).map(|(_, d)| d),
            ToolKind::ProviderRun => None,
            ToolKind::Wildcard => (id.ordinal() == 0).then_some(self.wildcard.as_ref()).flatten(),
        }
    }

    pub fn declaration(&self, call: &crate::value::ResolvedCall) -> Option<&ToolDeclaration> {
        self.keyed_tool(call.tool(), call.declaration_id())
    }

    /// The one annotation this call is judged under: its declaration's static annotation,
    /// borrowed, or the annotation its pin materializes under the declaration — the
    /// declaration's operational metadata around the pinned semantic fields. `None` when the
    /// call names no declaration, or names an Annotated declaration while carrying no pin —
    /// the caller decides whether that is a missing annotation to request or a record to
    /// refuse. Whether a carried pin is *admissible* is
    /// [`crate::check::validate_annotation`]'s question, not this lookup's.
    pub(crate) fn annotation_of<'a>(
        &'a self,
        call: &'a crate::value::ResolvedCall,
    ) -> Option<std::borrow::Cow<'a, ToolAnnotation>> {
        let declaration = self.declaration(call)?;
        match call.annotation() {
            Some(pinned) => Some(std::borrow::Cow::Owned(
                pinned.tool_annotation(declaration, call.tool()),
            )),
            None => declaration.declared().map(std::borrow::Cow::Borrowed),
        }
    }

    pub(crate) fn select_tool(
        &self,
        name: &ToolName,
        arguments: &serde_json::Value,
    ) -> Option<(ToolDeclarationId, &ToolDeclaration)> {
        match self.classify(name)? {
            ToolKind::Declared => {
                self.tools
                    .get(name)?
                    .iter()
                    .enumerate()
                    .find_map(|(ordinal, (matcher, declaration))| {
                        if matcher.matches(arguments) {
                            ToolDeclarationId::new(ordinal).map(|id| (id, declaration))
                        } else {
                            None
                        }
                    })
            }
            ToolKind::ProviderRun => None,
            ToolKind::Wildcard => self
                .wildcard
                .as_ref()
                .map(|declaration| (ToolDeclarationId::default(), declaration)),
        }
    }

    pub(crate) fn selection_matches(&self, call: &crate::value::ResolvedCall) -> bool {
        self.select_tool(call.tool(), call.arguments())
            .is_some_and(|(selected, _)| selected == call.declaration_id())
    }

    /// Whether a proposal naming this tool has a checkable declaration: written exactly, or
    /// covered by the wildcard. A provider-run tool or a name nothing covers has none.
    pub(crate) fn contains_tool(&self, name: &ToolName) -> bool {
        matches!(self.classify(name), Some(ToolKind::Declared | ToolKind::Wildcard))
    }

    pub fn variants(&self, name: &ToolName) -> impl Iterator<Item = &ToolDeclaration> {
        self.tools.get(name).into_iter().flatten().map(|(_, d)| d)
    }

    /// The declared annotation of a provider-run tool: never checked or planned; its
    /// static `delta` is what an exposed result is admitted under. Always static — a
    /// provider-run declaration routing through an Annotator is refused at load.
    pub fn provider_run_annotation(&self, name: &ToolName) -> Option<&ToolAnnotation> {
        self.provider_run.get(name)
    }

    pub fn provider_run_annotations(&self) -> impl Iterator<Item = &ToolAnnotation> {
        self.provider_run.values()
    }

    pub fn tools(&self) -> impl Iterator<Item = &ToolDeclaration> {
        self.tools.values().flatten().map(|(_, d)| d)
    }

    pub(crate) fn tool_names(&self) -> impl Iterator<Item = &ToolName> {
        self.tools.keys()
    }

    /// Every declaration the policy identity hashes over: the ordered contracts and, when
    /// the policy writes one, the wildcard — its presence and its annotator change what an
    /// unwritten tool call does, so two policies differing only there are different policies.
    pub(crate) fn semantic_tools(&self) -> impl Iterator<Item = (&ToolMatcher, &ToolDeclaration)> {
        self.tools
            .values()
            .flatten()
            .map(|(matcher, d)| (matcher, d))
            .chain(self.wildcard.iter().map(|d| (&ToolMatcher::Bare, d)))
    }

    /// One registered Annotator's compiled mandate, with every omitted bound already resolved
    /// to the policy vocabulary.
    pub fn annotator_mandate(&self, name: &AnnotatorName) -> Option<&AnnotatorMandate> {
        self.annotators.get(name)
    }

    pub fn annotators(&self) -> impl Iterator<Item = (&AnnotatorName, &AnnotatorMandate)> {
        self.annotators.iter()
    }

    pub fn authorities(&self) -> &[Authority] {
        &self.authorities
    }

    /// Every attention mark declared by a static tool requirement, an authority permit, or an
    /// annotator's explicit bound, in stable name order. This is the closed policy vocabulary an
    /// Annotator's answer may draw on; a mark need not have an authority remedy and may
    /// deliberately make a call unexecutable.
    pub fn attention_marks(&self) -> impl Iterator<Item = &MarkName> {
        self.attention_marks.iter()
    }

    pub fn authority(&self, name: &AuthorityName) -> Option<&Authority> {
        self.authorities.iter().find(|a| &a.name == name)
    }

    pub fn sanitizer(&self, name: &SanitizerName) -> Option<&Sanitizer> {
        self.sanitizers.get(name)
    }

    pub fn sanitizers(&self) -> impl Iterator<Item = &Sanitizer> {
        self.sanitizers.values()
    }
}

#[cfg(test)]
impl Registry {
    pub(crate) fn build_covered(config: RegistryConfig) -> Result<Registry, LoadError> {
        Registry::build_covered_with_cap(config, PlannerCap::default())
    }

    pub(crate) fn build_covered_with_cap(
        config: RegistryConfig,
        planner_cap: PlannerCap,
    ) -> Result<Registry, LoadError> {
        let profile = crate::profile::covering_profile(&config);
        Registry::build(config, planner_cap, profile)
    }
}

fn check_audience_bindings(tool: &ToolAnnotation) -> Result<(), LoadError> {
    let check = |argument: &str, site: &str| {
        tool.parameters
            .required_string_property(argument)
            .map_err(|fault| LoadError::AudienceBindingSchema {
                context: format!("tool {} {site}", tool.name.as_str()),
                argument: argument.to_string(),
                fault,
            })
    };
    for requirement in tool.requires.audience_requirements() {
        match requirement {
            AudienceRequirement::Includes(RecipientSpec::Placeholder(argument)) => check(argument, "contains")?,
            AudienceRequirement::Includes(RecipientSpec::Static(_)) | AudienceRequirement::Cap(_) => {}
        }
    }
    Ok(())
}

pub(crate) fn check_rank(
    chain: &TrustChain,
    rank: Option<Trust>,
    context: impl Fn() -> String,
) -> Result<(), LoadError> {
    match rank {
        Some(t) if !chain.contains_rank(t) => Err(LoadError::RankOutOfChain {
            rank: t.rank(),
            len: chain.len(),
            context: context(),
        }),
        _ => Ok(()),
    }
}

/// Validate the audience configuration and index it: providers registered once, named
/// audiences unique and sourced, and every configured
/// selector owned by a registered source. A static typo dies here, at load — only a
/// dynamically supplied reference can fail operationally.
fn validated_audience_registry(config: &AudienceConfig) -> Result<AudienceRegistry, LoadError> {
    let mut providers = BTreeSet::new();
    for source in &config.sources {
        // A provider name with a `:` makes one member id qualified under two providers, a
        // leading `@` makes its members non-literal readers, and an empty name owns no
        // namespace at all. The one qualification rule (`ReaderId::provider_prefix`) stays
        // unambiguous only over names this shape.
        if source.provider.is_empty() || source.provider.contains(':') || source.provider.starts_with('@') {
            return Err(LoadError::MalformedAudienceProvider(source.provider.clone()));
        }
        if !providers.insert(source.provider.as_str()) {
            return Err(LoadError::DuplicateAudienceProvider(source.provider.clone()));
        }
    }
    let mut named = BTreeSet::new();
    for group in &config.groups {
        // A name with a `:` round-trips through its `@` spelling as a source selector — a
        // durable label would decode to a different atom — and an empty name spells `@`.
        if group.name.as_str().is_empty() || group.name.as_str().contains(':') || group.name.as_str().starts_with('@') {
            return Err(LoadError::MalformedNamedAudience(group.name.as_str().to_string()));
        }
        if !named.insert(group.name.clone()) {
            return Err(LoadError::DuplicateNamedAudience(group.name.as_str().to_string()));
        }
        if group.from.is_empty() {
            return Err(LoadError::EmptyAudienceFrom(group.name.as_str().to_string()));
        }
    }
    let registry = AudienceRegistry::build(config);
    let sourced = |specs: &[SelectorSpec], context: &str| -> Result<(), LoadError> {
        for spec in specs {
            check_selector(&registry, spec, || context.to_string())?;
        }
        Ok(())
    };
    sourced(&config.self_from, "[audience.self]")?;
    sourced(&config.internal_from, "[audience.internal]")?;
    for group in &config.groups {
        sourced(&group.from, &format!("named audience @{}", group.name.as_str()))?;
    }
    Ok(registry)
}

fn check_selector(
    registry: &AudienceRegistry,
    spec: &SelectorSpec,
    context: impl Fn() -> String,
) -> Result<(), LoadError> {
    registry
        .route_selector(&spec.provider, &spec.selector)
        .map(|_| ())
        .map_err(|fault| LoadError::UnroutableAudience {
            context: context(),
            fault,
        })
}

/// Every group reference a policy declaration writes must resolve at load: a named audience
/// must be configured, and a source-qualified selector must match a template of its
/// registered provider. Chain words and readers pass — they are always meaningful.
pub(crate) fn check_routable(
    registry: &AudienceRegistry,
    atoms: impl IntoIterator<Item = SymbolicAtom>,
    context: impl Fn() -> String,
) -> Result<(), LoadError> {
    for atom in atoms {
        let SymbolicAtom::Group(group) = atom else {
            continue;
        };
        let fault = match &group {
            GroupRef::Named(name) => {
                if registry.group(name).is_some() {
                    continue;
                }
                Unroutable::UnknownGroup(name.clone())
            }
            GroupRef::Source { provider, selector } => {
                match check_selector(
                    registry,
                    &SelectorSpec {
                        provider: provider.clone(),
                        selector: selector.clone(),
                    },
                    &context,
                ) {
                    Ok(()) => continue,
                    Err(error) => return Err(error),
                }
            }
        };
        return Err(LoadError::UnroutableAudience {
            context: context(),
            fault,
        });
    }
    Ok(())
}

/// One test for every audience a policy declaration writes: its readers are literal — no
/// reserved spelling, no `@` mark smuggled into a reader set built in code — and its group
/// references route.
fn check_declared(
    registry: &AudienceRegistry,
    declared: &DeclaredAudience,
    context: impl Fn() -> String + Copy,
) -> Result<(), LoadError> {
    if let DeclaredAudience::Union(clause) = declared {
        check_literal(clause.readers(), context)?;
    }
    check_routable(registry, declared.symbolic_atoms(), context)
}

fn check_literal(readers: &BTreeSet<ReaderId>, context: impl Fn() -> String) -> Result<(), LoadError> {
    match readers.iter().find(|reader| !reader.is_literal()) {
        Some(reader) => Err(LoadError::NonLiteralReader {
            context: context(),
            reader: reader.as_str().to_string(),
        }),
        None => Ok(()),
    }
}

fn check_hint(hint: Option<&Hint>, context: impl Fn() -> String) -> Result<(), LoadError> {
    match hint {
        Some(hint) if hint.as_str().chars().count() > MAX_HINT_CHARS => Err(LoadError::HintTooLong {
            context: context(),
            len: hint.as_str().chars().count(),
            max: MAX_HINT_CHARS,
        }),
        _ => Ok(()),
    }
}

/// Every literal reader the loaded policy writes, across every audience-bearing declaration —
/// an annotator's explicit audience bound included. Annotators choose from this vocabulary;
/// call arguments are evidence, not a source of new policy labels. Groups and placeholders are
/// deliberately absent because their members are resolved per operation rather than declared by
/// the policy.
fn configured_audience_readers(
    config: &RegistryConfig,
    profile: &crate::profile::DeploymentProfile,
) -> BTreeSet<ReaderId> {
    fn add_audience(readers: &mut BTreeSet<ReaderId>, audience: &Audience) {
        for clause in audience.clauses() {
            readers.extend(clause.readers().iter().cloned());
        }
    }

    fn add_declared(readers: &mut BTreeSet<ReaderId>, audience: &DeclaredAudience) {
        if let DeclaredAudience::Union(clause) = audience {
            readers.extend(clause.readers().iter().cloned());
        }
    }

    let mut readers = BTreeSet::new();
    add_audience(&mut readers, &profile.starting_label().audience);
    for declaration in &config.tools {
        let Some(tool) = declaration.declared() else {
            continue;
        };
        if let Some(audience) = &tool.delta.audience {
            add_declared(&mut readers, audience);
        }
        {
            let requirements = &tool.requires.label.audience;
            for requirement in requirements {
                match requirement {
                    AudienceRequirement::Includes(RecipientSpec::Static(audience))
                    | AudienceRequirement::Cap(audience) => add_declared(&mut readers, audience),
                    AudienceRequirement::Includes(RecipientSpec::Placeholder(_)) => {}
                }
            }
        }
    }
    for annotator in &config.annotators {
        readers.extend(annotator.audiences.iter().flatten().cloned());
    }
    for authority in &config.authorities {
        if let Some(audience) = &authority.mandate.reader_ceiling {
            add_declared(&mut readers, audience);
        }
    }
    for sanitizer in &config.sanitizers {
        if let DeclaredTransition::Audience { from_includes, to } = &sanitizer.transition {
            add_declared(&mut readers, from_includes);
            add_declared(&mut readers, to);
        }
    }
    readers
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::authority::SanitizerPoints;
    use crate::authority::{Mandate, Scope};
    use crate::contract::{AudienceRequirement, Delta, HistoryRequirement, LabelRequirements, Requires};
    use crate::fact::{EffectKind, EffectSet};
    use crate::label::{Audience, ReaderId, Trust};
    use crate::names::{AuthorityName, MarkName, TagName};

    fn chain() -> TrustChain {
        TrustChain::new(vec!["suspicious".into(), "trusted".into()])
    }

    fn base() -> RegistryConfig {
        RegistryConfig {
            trust_chain: chain(),
            tools: vec![],
            annotators: vec![],
            authorities: vec![],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
        }
    }

    fn tool(name: &str) -> ToolAnnotation {
        ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new(name),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires::default(),
        }
    }

    fn declared(annotations: Vec<ToolAnnotation>) -> Vec<ToolDeclaration> {
        annotations.into_iter().map(ToolDeclaration::Declared).collect()
    }

    fn annotator(name: &str) -> AnnotatorDeclaration {
        AnnotatorDeclaration {
            name: AnnotatorName::new(name),
            trust: None,
            audiences: None,
            marks: None,
            effects: None,
        }
    }

    fn annotated(name: &str, by: &str) -> ToolDeclaration {
        ToolDeclaration::Annotated {
            name: ToolName::new(name),
            tags: vec![],
            description: None,
            parameters: crate::params::ToolParameters::open(),
            annotator: AnnotatorName::new(by),
        }
    }

    fn attends_authority(name: &str) -> Authority {
        Authority {
            name: AuthorityName::new(name),
            mandate: Mandate {
                attends: vec![MarkName::new("signoff")],
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        }
    }

    #[test]
    fn static_requirements_declare_attention_vocabulary_without_an_authority() {
        let mut blocked = tool("blocked");
        blocked.requires.attention = vec![MarkName::new("blocked")];
        let mut cfg = base();
        cfg.tools = declared(vec![blocked]);

        let registry = Registry::build_covered(cfg).expect("an unremediable attention mark is valid policy");

        assert_eq!(
            registry.attention_marks().map(MarkName::as_str).collect::<Vec<_>>(),
            ["blocked"]
        );
    }

    #[test]
    fn declared_readers_form_a_closed_audience_vocabulary_with_public() {
        let mut classified = tool("classified");
        classified.delta = Delta {
            trust: None,
            audience: Some(DeclaredAudience::restricted([ReaderId::new("private")])),
        };
        classified.requires.label.audience =
            vec![AudienceRequirement::Cap(DeclaredAudience::restricted([ReaderId::new(
                "partner",
            )]))];
        let mut cfg = base();
        cfg.tools = declared(vec![classified]);

        let registry = Registry::build_covered(cfg).expect("literal policy audiences are valid");

        assert_eq!(
            registry.audiences().collect::<Vec<_>>(),
            ["public", "partner", "private"]
        );
    }

    fn audience_sites(reader: &str) -> Vec<(&'static str, RegistryConfig)> {
        let named = Audience::restricted([ReaderId::new(reader)]);
        let literal = Audience::restricted([ReaderId::new("finance")]);

        let mut delta = base();
        let mut delta_tool = tool("emit");
        delta_tool.delta = Delta {
            trust: None,
            audience: Some(DeclaredAudience::literal(named.clone())),
        };
        delta.tools = declared(vec![delta_tool]);

        let mut includes = base();
        let mut includes_tool = tool("emit");
        includes_tool.requires.label.audience = vec![AudienceRequirement::Includes(RecipientSpec::Static(
            DeclaredAudience::literal(named.clone()),
        ))];
        includes.tools = declared(vec![includes_tool]);

        let mut cap = base();
        let mut cap_tool = tool("emit");
        cap_tool.requires.label.audience = vec![AudienceRequirement::Cap(DeclaredAudience::literal(named.clone()))];
        cap.tools = declared(vec![cap_tool]);

        let mut ceiling = base();
        ceiling.authorities = vec![Authority {
            name: AuthorityName::new("officer"),
            mandate: Mandate {
                reader_ceiling: Some(DeclaredAudience::literal(named.clone())),
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        }];

        let sanitizer = |transition| Sanitizer {
            name: SanitizerName::new("redactor"),
            on: SanitizerPoints {
                input: false,
                output: true,
            },
            transition,
            scope: Scope::default(),
            hint: None,
        };
        let mut transition_from = base();
        transition_from.sanitizers = vec![sanitizer(DeclaredTransition::Audience {
            from_includes: DeclaredAudience::literal(named.clone()),
            to: DeclaredAudience::literal(literal.clone()),
        })];
        let mut transition_to = base();
        transition_to.sanitizers = vec![sanitizer(DeclaredTransition::Audience {
            from_includes: DeclaredAudience::literal(literal),
            to: DeclaredAudience::literal(named.clone()),
        })];

        vec![
            ("tool emit delta", delta),
            ("tool emit contains", includes),
            ("tool emit within", cap),
            ("authority officer reader ceiling", ceiling),
            ("sanitizer redactor from", transition_from),
            ("sanitizer redactor to", transition_to),
        ]
    }

    #[test]
    fn audience_provider_and_group_names_are_shaped_at_load() {
        use crate::audience::{NamedAudience, SourceRegistration};
        let source = |provider: &str| SourceRegistration {
            provider: provider.to_string(),
            templates: vec![crate::audience::SelectorTemplate::new("viewer")],
        };
        // A `:` makes one member id qualified under two providers, `@` makes members
        // non-literal, and an empty name owns no namespace.
        for provider in ["", "a:b", "@evil"] {
            let mut cfg = base();
            cfg.audience.sources = vec![source(provider)];
            assert!(
                matches!(
                    Registry::build_covered(cfg),
                    Err(LoadError::MalformedAudienceProvider(_))
                ),
                "{provider:?}"
            );
        }
        // A group name with `:` round-trips through its `@` spelling as a source selector.
        for name in ["", "a:b", "@x"] {
            let mut cfg = base();
            cfg.audience.sources = vec![source("slack")];
            cfg.audience.groups = vec![NamedAudience {
                name: crate::names::GroupName::new(name),
                within: None,
                from: vec![SelectorSpec {
                    provider: "slack".into(),
                    selector: "viewer".into(),
                }],
            }];
            assert!(
                matches!(Registry::build_covered(cfg), Err(LoadError::MalformedNamedAudience(_))),
                "{name:?}"
            );
        }
    }

    #[test]
    fn every_declared_audience_refuses_a_reserved_or_group_reader() {
        for reserved in ["public", "@auditors"] {
            for (context, cfg) in audience_sites(reserved) {
                match Registry::build_covered(cfg) {
                    Err(LoadError::NonLiteralReader {
                        context: reported,
                        reader,
                    }) => {
                        assert_eq!(reader, reserved, "{context} reported the wrong reader");
                        assert_eq!(reported, context, "{context} reported the wrong site");
                    }
                    other => panic!("{context} admitted {reserved:?}: {other:?}"),
                }
            }
        }
    }

    #[test]
    fn one_reserved_member_spoils_an_otherwise_literal_set() {
        let mut cfg = base();
        let mut spoiled = tool("emit");
        spoiled.requires.label.audience = vec![AudienceRequirement::Cap(DeclaredAudience::literal(
            Audience::restricted([
                ReaderId::new("ap@corp.example"),
                ReaderId::new("finance"),
                ReaderId::new("public"),
            ]),
        ))];
        cfg.tools = declared(vec![spoiled]);
        assert!(matches!(
            Registry::build_covered(cfg),
            Err(LoadError::NonLiteralReader { reader, .. }) if reader == "public"
        ));
    }

    #[test]
    fn the_group_mark_is_a_prefix_and_never_a_substring() {
        for (context, cfg) in audience_sites("ap@corp.example") {
            assert!(
                Registry::build_covered(cfg).is_ok(),
                "{context} refused an ordinary reader ID"
            );
        }
    }

    #[test]
    fn public_and_the_empty_set_stay_loadable_audiences() {
        let mut public_ceiling = base();
        public_ceiling.authorities = vec![Authority {
            name: AuthorityName::new("officer"),
            mandate: Mandate {
                reader_ceiling: Some(DeclaredAudience::literal(Audience::public())),
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        }];
        assert!(Registry::build_covered(public_ceiling).is_ok());

        let mut empty_cap = base();
        let mut cap_tool = tool("emit");
        cap_tool.requires.label.audience = vec![AudienceRequirement::Cap(DeclaredAudience::literal(
            Audience::restricted([]),
        ))];
        empty_cap.tools = declared(vec![cap_tool]);
        assert!(Registry::build_covered(empty_cap).is_ok());
    }

    #[test]
    fn chain_maps_names_and_ranks() {
        let c = chain();
        assert_eq!(c.rank_of("suspicious"), Some(Trust::new(0)));
        assert_eq!(c.rank_of("trusted"), Some(Trust::new(1)));
        assert_eq!(c.rank_of("bogus"), None);
        assert_eq!(c.name_of(Trust::new(1)), Some("trusted"));
    }

    #[test]
    fn builds_and_indexes() {
        let mut cfg = base();
        cfg.tools = declared(vec![tool("get"), tool("send")]);
        cfg.authorities = vec![attends_authority("officer")];
        let reg = Registry::build_covered(cfg).unwrap();
        assert!(reg.tool(&ToolName::new("get")).is_some());
        assert!(reg.authority(&AuthorityName::new("officer")).is_some());
    }

    #[test]
    fn duplicate_checkable_tools_are_ordered_variants() {
        let mut cfg = base();
        cfg.tools = declared(vec![tool("dup"), tool("dup")]);
        let registry = Registry::build_covered(cfg).expect("duplicate checkable names are variants");
        assert_eq!(registry.variants(&ToolName::new("dup")).count(), 2);
    }

    #[test]
    fn a_duplicate_annotator_is_refused() {
        let mut cfg = base();
        cfg.annotators = vec![annotator("classifier"), annotator("classifier")];
        assert!(matches!(
            Registry::build_covered(cfg),
            Err(LoadError::DuplicateAnnotator(name)) if name == "classifier"
        ));
    }

    #[test]
    fn a_declaration_may_route_only_through_a_registered_annotator() {
        let mut cfg = base();
        cfg.tools = vec![annotated("shell", "ghost")];
        assert!(matches!(
            Registry::build_covered(cfg),
            Err(LoadError::UnknownAnnotator { tool, annotator }) if tool == "shell" && annotator == "ghost"
        ));

        let mut cfg = base();
        cfg.annotators = vec![annotator("classifier")];
        cfg.tools = vec![annotated("shell", "classifier")];
        let registry = Registry::build_covered(cfg).expect("a registered annotator routes");
        let declaration = registry.tool(&ToolName::new("shell")).expect("shell is registered");
        assert_eq!(declaration.annotator().map(AnnotatorName::as_str), Some("classifier"));
        assert!(declaration.declared().is_none());
    }

    #[test]
    fn annotator_bounds_are_validated_against_the_policy_vocabulary() {
        let mut cfg = base();
        cfg.annotators = vec![AnnotatorDeclaration {
            trust: Some(BTreeSet::from([Trust::new(9)])),
            ..annotator("classifier")
        }];
        assert!(matches!(
            Registry::build_covered(cfg),
            Err(LoadError::RankOutOfChain { rank: 9, .. })
        ));

        let mut cfg = base();
        cfg.annotators = vec![AnnotatorDeclaration {
            audiences: Some(BTreeSet::from([ReaderId::new("@team")])),
            ..annotator("classifier")
        }];
        assert!(matches!(
            Registry::build_covered(cfg),
            Err(LoadError::NonLiteralReader { reader, .. }) if reader == "@team"
        ));
    }

    #[test]
    fn an_omitted_mandate_bound_resolves_to_the_whole_policy_vocabulary() {
        let mut catalogued = tool("send");
        catalogued.delta.audience = Some(DeclaredAudience::restricted([ReaderId::new("insider")]));
        catalogued.emits = EffectSet::new([EffectKind::new("mail.sent")]).unwrap();
        catalogued.requires.attention = vec![MarkName::new("signoff")];
        let mut cfg = base();
        cfg.tools = vec![ToolDeclaration::Declared(catalogued), annotated("shell", "classifier")];
        cfg.annotators = vec![
            annotator("classifier"),
            AnnotatorDeclaration {
                trust: Some(BTreeSet::from([Trust::new(0)])),
                audiences: Some(BTreeSet::from([ReaderId::new("support")])),
                marks: Some(BTreeSet::from([MarkName::new("reviewed")])),
                effects: Some(BTreeSet::from([EffectKind::new("audit.log")])),
                ..annotator("narrow")
            },
        ];
        let registry = Registry::build_covered(cfg).unwrap();

        let open = registry
            .annotator_mandate(&AnnotatorName::new("classifier"))
            .expect("classifier is registered");
        assert_eq!(open.trust_ranks().collect::<Vec<_>>(), [Trust::new(0), Trust::new(1)]);
        // The whole vocabulary includes what another annotator's explicit bound declares.
        assert_eq!(
            open.audiences().map(ReaderId::as_str).collect::<Vec<_>>(),
            ["insider", "support"]
        );
        assert_eq!(
            open.marks().map(MarkName::as_str).collect::<Vec<_>>(),
            ["reviewed", "signoff"]
        );
        assert_eq!(
            open.effects().cloned().collect::<BTreeSet<_>>(),
            BTreeSet::from([EffectKind::new("audit.log"), EffectKind::new("mail.sent")])
        );

        let narrow = registry
            .annotator_mandate(&AnnotatorName::new("narrow"))
            .expect("narrow is registered");
        assert_eq!(narrow.trust_ranks().collect::<Vec<_>>(), [Trust::new(0)]);
        assert_eq!(
            narrow.audiences().map(ReaderId::as_str).collect::<Vec<_>>(),
            ["support"]
        );
        assert_eq!(narrow.marks().map(MarkName::as_str).collect::<Vec<_>>(), ["reviewed"]);
        assert_eq!(
            narrow.effects().cloned().collect::<BTreeSet<_>>(),
            BTreeSet::from([EffectKind::new("audit.log")])
        );
    }

    #[test]
    fn selector_grammar_and_unicode_wildcards_are_exact() {
        let parsed = |name| parse_tool_selector(name).map(|(_, matcher)| matcher);
        let arguments = |value| serde_json::json!({ "path": value });
        for (pattern, matching, foreign, foreign_matches) in [
            ("read(path:*)", "", "anything", true),
            ("read(path:**)", "東京", "anything", true),
            (r"read(path:\*)", "*", "x", false),
            (r"read(path:\))", ")", "x", false),
            (r"read(path:\\)", "\\", "x", false),
            ("read(path:a(b:c)", "a(b:c", "x", false),
        ] {
            let matcher = parsed(pattern).expect("the selector is valid");
            assert!(matcher.matches(&arguments(matching)), "{pattern}");
            assert_eq!(matcher.matches(&arguments(foreign)), foreign_matches, "{pattern}");
        }
        let empty = parsed("read(path:)").expect("an empty pattern is valid");
        assert!(empty.matches(&arguments("")));
        assert!(!empty.matches(&serde_json::json!({})));
        assert!(!empty.matches(&serde_json::json!({ "path": 1 })));

        let repeated = parsed("read(path:*secret)").expect("the selector is valid");
        assert!(repeated.matches(&arguments("secretsecret")));

        let unicode = parsed("read(path:*京)").expect("the selector is valid");
        assert!(unicode.matches(&arguments("東京京")));

        let multiple = parsed("read(path:*ab*bc)").expect("the selector is valid");
        assert!(multiple.matches(&arguments("xxabyyabzzbc")));

        let dotted = parsed("read(a.b:x)").expect("a dotted argument name is valid");
        assert!(dotted.matches(&serde_json::json!({ "a.b": "x" })));
    }

    /// The conjunction: a selector may name several arguments, and a call is selected only
    /// when every one of them is present, a string, and matches its own pattern. The repo
    /// case is `fork_repository(owner:archestra-ai,repo:website)`, which must not reach
    /// another account's repository that happens to carry the same name.
    #[test]
    fn every_argument_clause_must_match_for_the_contract_to_select() {
        let parsed = |name| parse_tool_selector(name).map(|(_, matcher)| matcher);
        let call = |owner, repo| serde_json::json!({ "owner": owner, "repo": repo });

        // One clause still ignores every argument it does not name.
        let repo_only = parsed("fork(repo:website)").expect("the selector is valid");
        assert!(repo_only.matches(&call("archestra-ai", "website")));
        assert!(repo_only.matches(&call("somebody-else", "website")));

        let both = parsed("fork(owner:archestra-ai,repo:website)").expect("the selector is valid");
        assert!(both.matches(&call("archestra-ai", "website")));
        assert!(
            !both.matches(&call("somebody-else", "website")),
            "a foreign owner is out"
        );
        assert!(
            !both.matches(&call("archestra-ai", "docs")),
            "another repository is out"
        );

        // A clause whose argument is missing or is not a string fails the conjunction, the
        // same as a single-argument selector does.
        assert!(!both.matches(&serde_json::json!({ "repo": "website" })));
        assert!(!both.matches(&serde_json::json!({ "owner": "archestra-ai" })));
        assert!(!both.matches(&serde_json::json!({ "owner": 1, "repo": "website" })));
        assert!(!both.matches(&serde_json::json!({ "owner": "archestra-ai", "repo": null })));

        // Wildcards run independently per clause.
        let wild = parsed("fork(owner:archestra-*,repo:web*)").expect("the selector is valid");
        assert!(wild.matches(&call("archestra-ai", "website")));
        assert!(wild.matches(&call("archestra-labs", "webhooks")));
        assert!(!wild.matches(&call("archestra-ai", "docs")));
        assert!(!wild.matches(&call("elsewhere", "website")));

        // A comma separates clauses, so a literal comma is escaped. The clause after it is
        // still read as a clause, not as pattern text.
        let comma = parsed(r"search(query:a\,b)").expect("an escaped comma is valid");
        assert!(comma.matches(&serde_json::json!({ "query": "a,b" })));
        assert!(!comma.matches(&serde_json::json!({ "query": "ab" })));
        let mixed = parsed(r"search(query:a\,b,scope:repo)").expect("the selector is valid");
        assert!(mixed.matches(&serde_json::json!({ "query": "a,b", "scope": "repo" })));
        assert!(!mixed.matches(&serde_json::json!({ "query": "a,b" })));

        // Clause order carries no meaning, so the two spellings are one matcher.
        assert_eq!(
            parsed("fork(owner:archestra-ai,repo:website)").expect("the selector is valid"),
            parsed("fork(repo:website,owner:archestra-ai)").expect("the selector is valid"),
        );
    }

    #[test]
    fn malformed_selector_forms_are_refused() {
        for malformed in [
            "",
            "read(",
            "read)",
            "(x:y)",
            "read(:x)",
            "read(x)",
            "read(x:y",
            "read(x:y))",
            "read(x:y)tail",
            r"read(x:\q)",
            "read(a(b:c)",
            "read(a\\:x)",
            "read(a\n:x)",
            // A conjunction holds at least one clause, and every clause holds a name, a
            // colon, and nothing empty between the commas.
            "read()",
            "read(a:x,)",
            "read(,a:x)",
            "read(a:x,,b:y)",
            "read(a:x,b)",
            "read(a:x,:y)",
            "read(a:x,b(c:y)",
            "read(a:x,b\\:y)",
            // One argument carries one pattern, so a repeated name is refused rather than
            // silently overwritten.
            "read(a:x,a:y)",
            "read(a:x,a:x)",
        ] {
            assert!(
                matches!(parse_tool_selector(malformed), Err(LoadError::MalformedToolSelector(_))),
                "{malformed:?}"
            );
        }
    }

    #[test]
    fn refuses_empty_mandate() {
        let mut cfg = base();
        cfg.authorities = vec![Authority {
            name: AuthorityName::new("noop"),
            mandate: Mandate::default(),
            scope: Scope::default(),
            hint: None,
        }];
        assert!(matches!(
            Registry::build_covered(cfg),
            Err(LoadError::EmptyMandate(name)) if name == "noop"
        ));
    }

    #[test]
    fn refuses_rank_out_of_chain() {
        let mut cfg = base();
        cfg.tools = declared(vec![ToolAnnotation {
            delta: Delta {
                trust: Some(Trust::new(9)),
                audience: None,
            },
            ..tool("over")
        }]);
        assert!(matches!(
            Registry::build_covered(cfg),
            Err(LoadError::RankOutOfChain { rank: 9, .. })
        ));
    }

    #[test]
    fn refuses_overlong_trust_chain() {
        let mut cfg = base();
        cfg.trust_chain = TrustChain::new((0..=MAX_RANKS).map(|i| i.to_string()).collect());
        assert!(matches!(
            Registry::build_covered(cfg),
            Err(LoadError::TrustChainTooLong { len, max }) if len == MAX_RANKS + 1 && max == MAX_RANKS
        ));
    }

    /// The wildcard is one Annotated declaration covering every name the policy does not
    /// write, at ordinal zero only, exact declarations first, and in no listing.
    #[test]
    fn the_wildcard_covers_every_name_the_policy_does_not_write() {
        let mut cfg = base();
        cfg.tools = declared(vec![tool("read")]);
        cfg.tools.push(annotated(WILDCARD_SPELLING, "any"));
        cfg.annotators = vec![annotator("any")];
        let registry = Registry::build_covered(cfg).unwrap();
        let read = ToolName::new("read");
        let ghost = ToolName::new("ghost");
        assert_eq!(registry.classify(&read), Some(ToolKind::Declared));
        assert_eq!(registry.classify(&ghost), Some(ToolKind::Wildcard));
        assert!(registry.declared(&read));
        assert!(!registry.declared(&ghost));
        assert!(registry.contains_tool(&ghost));

        let (id, covered) = registry.select_tool(&ghost, &serde_json::json!({})).unwrap();
        assert_eq!(id, ToolDeclarationId::default());
        assert_eq!(covered.annotator(), Some(&AnnotatorName::new("any")));
        assert!(covered.declared().is_none(), "the wildcard carries no static contract");

        let (_, exact) = registry.select_tool(&read, &serde_json::json!({})).unwrap();
        assert!(exact.declared().is_some(), "an exact declaration beats the wildcard");
        assert!(
            registry
                .keyed_tool(&ghost, ToolDeclarationId::new(1).unwrap())
                .is_none()
        );
        assert!(registry.tools().all(|tool| tool.name().as_str() != WILDCARD_SPELLING));
        assert!(registry.tool_names().all(|name| name.as_str() != WILDCARD_SPELLING));
        assert!(
            !registry.declared(&ToolName::new(WILDCARD_SPELLING)),
            "the wildcard's spelling names no tool"
        );
    }

    /// The wildcard's spelling is a contract, not a tool: a caller that proposes the literal
    /// `*` names a tool no host dispatches, so it resolves to nothing — the wildcard covers
    /// every *other* name — and the proposal is refused instead of annotated and checked.
    #[test]
    fn a_call_proposing_the_wildcards_own_spelling_resolves_to_no_declaration() {
        let mut cfg = base();
        cfg.tools = declared(vec![tool("read")]);
        cfg.tools.push(annotated(WILDCARD_SPELLING, "any"));
        cfg.annotators = vec![annotator("any")];
        let registry = Registry::build_covered(cfg).unwrap();
        let literal = ToolName::new(WILDCARD_SPELLING);

        assert_eq!(registry.classify(&literal), None);
        assert!(!registry.contains_tool(&literal));
        assert!(!registry.declared(&literal));
        assert!(registry.select_tool(&literal, &serde_json::json!({})).is_none());
        assert!(registry.keyed_tool(&literal, ToolDeclarationId::default()).is_none());
        assert_eq!(
            registry.classify(&ToolName::new("ghost")),
            Some(ToolKind::Wildcard),
            "every other unwritten name still resolves to the wildcard"
        );
    }

    #[test]
    fn the_wildcard_parses_apart_from_every_tool_name() {
        assert_eq!(
            parse_tool_selector(WILDCARD_SPELLING).map(|(name, _)| name),
            Ok(ContractName::Wildcard)
        );
        assert!(matches!(
            parse_tool_selector("*(path:x)"),
            Ok((ContractName::Wildcard, ToolMatcher::Arguments(_)))
        ));
        for named in ["read", "a*", "**", "read(path:*)"] {
            assert!(
                matches!(parse_tool_selector(named), Ok((ContractName::Named(_), _))),
                "{named} names a tool"
            );
        }
    }

    #[test]
    fn a_name_no_declaration_and_no_wildcard_covers_has_no_contract() {
        let mut cfg = base();
        cfg.tools = declared(vec![tool("read")]);
        let registry = Registry::build_covered(cfg).unwrap();
        let ghost = ToolName::new("ghost");
        assert_eq!(registry.classify(&ghost), None);
        assert!(!registry.contains_tool(&ghost));
        assert!(registry.select_tool(&ghost, &serde_json::json!({})).is_none());
        assert!(registry.keyed_tool(&ghost, ToolDeclarationId::default()).is_none());
    }

    #[test]
    fn a_wildcard_declares_no_statics_no_metadata_and_registers_once() {
        let statics = {
            let mut cfg = base();
            cfg.tools = declared(vec![tool(WILDCARD_SPELLING)]);
            Registry::build_covered(cfg)
        };
        assert!(matches!(statics, Err(LoadError::WildcardStatic)));

        let tagged = {
            let mut cfg = base();
            cfg.annotators = vec![annotator("any")];
            cfg.tools = vec![ToolDeclaration::Annotated {
                name: ToolName::new(WILDCARD_SPELLING),
                tags: vec![crate::names::TagName::new("web")],
                description: None,
                parameters: crate::params::ToolParameters::open(),
                annotator: AnnotatorName::new("any"),
            }];
            Registry::build_covered(cfg)
        };
        assert!(matches!(tagged, Err(LoadError::WildcardMetadata)));

        let selected = {
            let mut cfg = base();
            cfg.annotators = vec![annotator("any")];
            cfg.tools = vec![annotated("*(path:*)", "any")];
            Registry::build_covered(cfg)
        };
        assert!(matches!(selected, Err(LoadError::WildcardMetadata)));

        let doubled = {
            let mut cfg = base();
            cfg.annotators = vec![annotator("any")];
            cfg.tools = vec![annotated(WILDCARD_SPELLING, "any"), annotated(WILDCARD_SPELLING, "any")];
            Registry::build_covered(cfg)
        };
        assert!(matches!(doubled, Err(LoadError::DuplicateWildcard)));

        let unregistered = {
            let mut cfg = base();
            cfg.tools = vec![annotated(WILDCARD_SPELLING, "ghost")];
            Registry::build_covered(cfg)
        };
        assert!(matches!(unregistered, Err(LoadError::UnknownAnnotator { .. })));
    }

    #[test]
    fn an_unannotated_tool_composes_with_history_and_attention_requirements() {
        let mut cfg = base();
        let mut guard = tool("guard");
        guard.delta = Delta::NONE;
        guard.requires = Requires {
            label: LabelRequirements::default(),
            history: vec![HistoryRequirement::NoPrior(EffectKind::new("email.sent"))],
            attention: vec![MarkName::new("signoff")],
        };
        cfg.tools = declared(vec![guard]);
        cfg.authorities = vec![attends_authority("steward")];
        let registry = Registry::build_covered(cfg).expect("history and attention consume no label dimension");
        assert!(registry.tool(&ToolName::new("guard")).is_some());
    }

    #[test]
    fn refuses_duplicate_trust_rank() {
        let mut cfg = base();
        cfg.trust_chain = TrustChain::new(vec!["low".into(), "high".into(), "low".into()]);
        assert!(matches!(
            Registry::build_covered(cfg),
            Err(LoadError::DuplicateRank(name)) if name == "low"
        ));
    }

    fn binding_sites(parameters: &crate::params::ToolParameters) -> Vec<(&'static str, RegistryConfig)> {
        let mut placeholder = tool("emit");
        placeholder.parameters = parameters.clone();
        placeholder.requires.label.audience =
            vec![AudienceRequirement::Includes(RecipientSpec::Placeholder("to".into()))];
        let mut cfg = base();
        cfg.tools = declared(vec![placeholder]);
        vec![("tool emit contains", cfg)]
    }

    #[test]
    fn every_audience_argument_binding_names_a_required_top_level_string() {
        use crate::params::{PropertyFault, ToolParameters};
        let schema = |value: serde_json::Value| ToolParameters::compile(&value).unwrap();
        let refused = [
            (ToolParameters::open(), PropertyFault::Undeclared),
            (
                schema(serde_json::json!({
                    "type": "object",
                    "properties": { "cc": { "type": "string" } },
                    "required": ["cc"],
                })),
                PropertyFault::Undeclared,
            ),
            (
                schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "envelope": {
                            "type": "object",
                            "properties": { "to": { "type": "string" } },
                            "required": ["to"],
                        }
                    },
                    "required": ["envelope"],
                })),
                PropertyFault::Undeclared,
            ),
            (
                schema(serde_json::json!({
                    "type": "object",
                    "properties": { "to": { "type": "string" } },
                })),
                PropertyFault::Optional,
            ),
            (
                schema(serde_json::json!({
                    "type": "object",
                    "properties": { "to": { "type": "array", "items": { "type": "string" } } },
                    "required": ["to"],
                })),
                PropertyFault::NotString,
            ),
        ];
        for (parameters, expected) in refused {
            for (expected_context, cfg) in binding_sites(&parameters) {
                match Registry::build_covered(cfg) {
                    Err(LoadError::AudienceBindingSchema {
                        context,
                        argument,
                        fault,
                    }) => {
                        assert_eq!(context, expected_context);
                        assert_eq!(argument, "to");
                        assert_eq!(fault, expected, "at {expected_context}");
                    }
                    other => {
                        panic!("{expected_context} under {parameters:?} must refuse with {expected:?}, got {other:?}")
                    }
                }
            }
        }

        let accepted = [
            schema(serde_json::json!({
                "type": "object",
                "properties": { "to": { "type": "string" }, "body": { "type": "string" } },
                "required": ["to"],
                "additionalProperties": true,
            })),
            schema(serde_json::json!({
                "type": "object",
                "properties": { "to": { "type": "string", "enum": ["ops", "dev"] } },
                "required": ["to"],
            })),
        ];
        for parameters in accepted {
            for (context, cfg) in binding_sites(&parameters) {
                assert!(
                    Registry::build_covered(cfg).is_ok(),
                    "{context} under {parameters:?} must load"
                );
            }
        }

        let mut cfg = base();
        let mut emitter = tool("emit");
        emitter.requires.label.audience = vec![
            AudienceRequirement::Includes(RecipientSpec::Static(DeclaredAudience::literal(Audience::restricted(
                [ReaderId::new("finance")],
            )))),
            AudienceRequirement::Cap(DeclaredAudience::literal(Audience::public())),
        ];
        cfg.tools = declared(vec![emitter]);
        assert!(Registry::build_covered(cfg).is_ok());
    }

    fn n_squared_config(n: usize) -> RegistryConfig {
        let mut two_marks = tool("wire");
        two_marks.requires = Requires {
            attention: vec![MarkName::new("m1"), MarkName::new("m2")],
            ..Requires::default()
        };
        let attester = |name: String| Authority {
            name: AuthorityName::new(name),
            mandate: Mandate {
                attends: vec![MarkName::new("m1"), MarkName::new("m2")],
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        let mut cfg = base();
        cfg.tools = declared(vec![two_marks]);
        cfg.authorities = (0..n).map(|i| attester(format!("a{i}"))).collect();
        cfg
    }

    #[test]
    fn the_default_planner_cap_refuses_an_over_wide_registry_at_four_thousand_ninety_six() {
        assert!(Registry::build_covered(n_squared_config(64)).is_ok());
        assert!(matches!(
            Registry::build_covered(n_squared_config(65)),
            Err(LoadError::TooManyPlanAlternatives {
                count: 4225,
                max: 4096,
                ..
            })
        ));
    }

    /// A produced output label exists only per call, so no load-time `may_admit` filter can rule
    /// a sanitizer out of an Annotated declaration's bound, and the declaration itself stays
    /// counted as a worst-case redispatch candidate.
    #[test]
    fn the_alternative_bound_counts_every_sanitizer_for_an_annotated_declaration() {
        let mut cfg = base();
        cfg.annotators = vec![annotator("classifier")];
        cfg.tools = vec![annotated("lookup", "classifier")];
        cfg.sanitizers = (0..16).map(output_sanitizer).collect();
        // Without context control no return declaration multiplies the menu:
        // 1 × (16 sanitizers + 1 bare release) + the declaration itself as a redispatch = 18.
        let mut uncontrolled = crate::profile::covering_declaration(&cfg);
        uncontrolled.context_control = false;
        let uncontrolled =
            crate::profile::DeploymentProfile::declare(uncontrolled).expect("the declaration normalizes");
        assert!(matches!(
            Registry::build(cfg.clone(), PlannerCap::new(17).expect("nonzero"), uncontrolled.clone()),
            Err(LoadError::TooManyPlanAlternatives { count: 18, max: 17, ref tool }) if tool == "lookup"
        ));
        assert!(Registry::build(cfg, PlannerCap::new(18).expect("nonzero"), uncontrolled).is_ok());
    }

    /// An Annotated declaration's requirements exist only per call, so the lint takes the worst
    /// case on every mandate dimension at once: a produced floor, a produced `contains`, and any
    /// mark an authority can attend each multiply.
    #[test]
    fn an_annotated_declaration_takes_the_worst_case_on_every_mandate_dimension() {
        let wide = |n: usize| {
            let officer = |name: String| Authority {
                name: AuthorityName::new(name),
                mandate: Mandate {
                    trust_ceiling: Some(Trust::new(1)),
                    reader_ceiling: Some(DeclaredAudience::literal(Audience::public())),
                    attends: vec![MarkName::new("signoff")],
                    ..Mandate::default()
                },
                scope: Scope::default(),
                hint: None,
            };
            let mut cfg = base();
            cfg.annotators = vec![annotator("classifier")];
            cfg.tools = vec![annotated("wire", "classifier")];
            cfg.authorities = (0..n).map(|i| officer(format!("a{i}"))).collect();
            cfg
        };
        assert!(Registry::build_covered_with_cap(wide(3), PlannerCap::new(64).expect("nonzero")).is_ok());
        assert!(matches!(
            Registry::build_covered_with_cap(wide(4), PlannerCap::new(64).expect("nonzero")),
            Err(LoadError::TooManyPlanAlternatives { count: 65, max: 64, ref tool }) if tool == "wire"
        ));
    }

    /// The cap bounds the plans a block can surface, so it must count the authorities
    /// planning would admit. For a wholly literal `contains`, that is decidable here:
    /// an authority capped at readers that exclude the recipient covers nothing.
    #[test]
    fn the_cap_counts_only_the_authorities_a_literal_contains_can_reach() {
        let recipient = ReaderId::new("auditor");
        let mut cfg = base();
        let mut emit = tool("emit");
        emit.requires.label.audience = vec![AudienceRequirement::Includes(RecipientSpec::Static(
            DeclaredAudience::restricted([recipient.clone()]),
        ))];
        cfg.tools = declared(vec![emit]);
        let capped_at = |name: &str, ceiling: DeclaredAudience| Authority {
            name: AuthorityName::new(name),
            mandate: Mandate {
                reader_ceiling: Some(ceiling),
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        // None of these reaches `auditor`, so none of them can cover the gap.
        cfg.authorities = (0..80)
            .map(|index| {
                capped_at(
                    &format!("a{index}"),
                    DeclaredAudience::restricted([ReaderId::new(format!("other-{index}"))]),
                )
            })
            .collect();
        assert!(
            Registry::build_covered_with_cap(cfg.clone(), PlannerCap::new(64).expect("nonzero")).is_ok(),
            "authorities that cannot cover the recipient are not alternatives"
        );

        // Authorities that do reach it are still counted, beside the ones that cannot.
        let mut reaching = cfg.clone();
        reaching.authorities.extend(
            (0..65).map(|index| capped_at(&format!("r{index}"), DeclaredAudience::restricted([recipient.clone()]))),
        );
        assert!(matches!(
            Registry::build_covered_with_cap(reaching, PlannerCap::new(64).expect("nonzero")),
            Err(LoadError::TooManyPlanAlternatives { count: 65, max: 64, .. })
        ));

        // A symbolic ceiling is a membership answer the lint cannot have, so those
        // authorities stay counted rather than being ruled out.
        let mut grouped = cfg.clone();
        grouped.authorities = (0..80)
            .map(|index| {
                capped_at(
                    &format!("g{index}"),
                    DeclaredAudience::Union(
                        crate::label::Clause::new(
                            [],
                            [crate::label::GroupRef::Named(crate::names::GroupName::new("desk"))],
                            [],
                        )
                        .expect("a group clause"),
                    ),
                )
            })
            .collect();
        grouped.audience = crate::audience::AudienceConfig {
            sources: vec![crate::audience::SourceRegistration {
                provider: "slack".to_string(),
                templates: vec![crate::audience::SelectorTemplate::new("user-group/<handle>")],
            }],
            groups: vec![crate::audience::NamedAudience {
                name: crate::names::GroupName::new("desk"),
                within: None,
                from: vec![crate::audience::SelectorSpec {
                    provider: "slack".to_string(),
                    selector: "user-group/desk".to_string(),
                }],
            }],
            ..crate::audience::AudienceConfig::default()
        };
        assert!(matches!(
            Registry::build_covered_with_cap(grouped, PlannerCap::new(64).expect("nonzero")),
            Err(LoadError::TooManyPlanAlternatives { count: 80, max: 64, .. })
        ));
    }

    #[test]
    fn a_configured_planner_cap_replaces_the_default_bound() {
        let cap = PlannerCap::new(9).expect("nonzero");
        assert!(matches!(
            Registry::build_covered_with_cap(n_squared_config(4), cap),
            Err(LoadError::TooManyPlanAlternatives { count: 16, max: 9, .. })
        ));
        assert!(Registry::build_covered_with_cap(n_squared_config(3), cap).is_ok());

        let raised = PlannerCap::new(100).expect("nonzero");
        assert!(Registry::build_covered_with_cap(n_squared_config(9), raised).is_ok());
    }

    #[test]
    fn a_zero_planner_cap_is_unrepresentable() {
        assert_eq!(PlannerCap::new(0), None);
    }

    /// Under context control any call may be a marked spawn, so every tool's menu multiplies by
    /// the return declarations: the bare floor plus each untagged output sanitizer, the reserved
    /// attestation included. A confined result's own settlements never count the attestation.
    #[test]
    fn return_declarations_multiply_every_menu_only_under_context_control() {
        let lifting = |name: &str, scope: Scope| Sanitizer {
            name: SanitizerName::new(name),
            on: SanitizerPoints {
                input: false,
                output: true,
            },
            transition: DeclaredTransition::Trust {
                from_floor: Trust::new(0),
                to: Trust::new(1),
            },
            scope,
            hint: None,
        };
        let scoped = |name: &str| {
            lifting(
                name,
                Scope {
                    tags: vec![TagName::new("t")],
                },
            )
        };
        let mut narrowing = tool("wire");
        narrowing.tags = vec![TagName::new("t")];
        narrowing.delta = Delta {
            trust: Some(Trust::new(0)),
            audience: None,
        };
        let mut cfg = base();
        cfg.tools = declared(vec![narrowing]);
        cfg.sanitizers = vec![lifting("attest-schema", Scope::default()), scoped("s1"), scoped("s2")];
        // Three settlements (bare, s1, s2) times two return declarations (bare, attest-schema).
        assert!(matches!(
            Registry::build_covered_with_cap(cfg.clone(), PlannerCap::new(5).expect("nonzero")),
            Err(LoadError::TooManyPlanAlternatives { count: 6, max: 5, ref tool }) if tool == "wire"
        ));
        assert!(Registry::build_covered_with_cap(cfg.clone(), PlannerCap::new(6).expect("nonzero")).is_ok());

        let mut uncontrolled = crate::profile::covering_declaration(&cfg);
        uncontrolled.context_control = false;
        let uncontrolled =
            crate::profile::DeploymentProfile::declare(uncontrolled).expect("the declaration normalizes");
        assert!(matches!(
            Registry::build(cfg.clone(), PlannerCap::new(2).expect("nonzero"), uncontrolled.clone()),
            Err(LoadError::TooManyPlanAlternatives { count: 3, max: 2, ref tool }) if tool == "wire"
        ));
        assert!(Registry::build(cfg, PlannerCap::new(3).expect("nonzero"), uncontrolled).is_ok());
    }

    fn output_sanitizer(index: usize) -> Sanitizer {
        Sanitizer {
            name: SanitizerName::new(format!("sanitizer-{index}")),
            on: SanitizerPoints {
                input: false,
                output: true,
            },
            transition: DeclaredTransition::Audience {
                from_includes: DeclaredAudience::literal(Audience::public()),
                to: DeclaredAudience::literal(Audience::public()),
            },
            scope: Scope::default(),
            hint: None,
        }
    }

    fn prior_target_config(emitters: usize) -> RegistryConfig {
        let mut target = tool("wire");
        target.requires = Requires {
            history: vec![HistoryRequirement::Prior(EffectKind::new("k"))],
            ..Requires::default()
        };
        let mut tools = vec![target];
        for i in 0..emitters {
            let mut emitter = tool(&format!("emit{i}"));
            emitter.emits = EffectSet::new([EffectKind::new("k")]).unwrap();
            tools.push(emitter);
        }
        let mut bystander = tool("bystander");
        bystander.emits = EffectSet::new([EffectKind::new("other")]).unwrap();
        tools.push(bystander);
        let mut cfg = base();
        cfg.tools = declared(tools);
        cfg
    }

    #[test]
    fn the_bound_counts_every_direct_prior_emitter() {
        let cap = PlannerCap::new(4).expect("nonzero");
        assert!(Registry::build_covered_with_cap(prior_target_config(3), cap).is_ok());
        assert!(matches!(
            Registry::build_covered_with_cap(prior_target_config(4), cap),
            Err(LoadError::TooManyPlanAlternatives { count: 5, max: 4, ref tool }) if tool == "wire"
        ));
    }

    fn cap_target_config_with(narrowers: usize, cap: DeclaredAudience) -> RegistryConfig {
        let mut target = tool("send");
        target.requires = Requires {
            label: LabelRequirements {
                trust_floor: None,
                audience: vec![AudienceRequirement::Cap(cap)],
            },
            ..Requires::default()
        };
        let mut tools = vec![target];
        for i in 0..narrowers {
            let mut narrower = tool(&format!("narrow{i}"));
            narrower.delta.audience = Some(DeclaredAudience::restricted([ReaderId::new("a"), ReaderId::new("c")]));
            tools.push(narrower);
        }
        let mut public = tool("public-delta");
        public.delta.audience = Some(DeclaredAudience::literal(Audience::public()));
        let neutral = tool("neutral");
        let mut unannotated = tool("unannotated");
        unannotated.delta = Delta::NONE;
        tools.extend([public, neutral, unannotated]);
        let mut cfg = base();
        cfg.tools = declared(tools);
        cfg
    }

    fn cap_target_config(narrowers: usize) -> RegistryConfig {
        cap_target_config_with(narrowers, DeclaredAudience::restricted([ReaderId::new("a")]))
    }

    #[test]
    fn the_bound_counts_only_static_restricted_contributions_for_a_cap() {
        let cap = PlannerCap::new(4).expect("nonzero");
        assert!(Registry::build_covered_with_cap(cap_target_config(3), cap).is_ok());
        assert!(matches!(
            Registry::build_covered_with_cap(cap_target_config(4), cap),
            Err(LoadError::TooManyPlanAlternatives { count: 5, max: 4, ref tool }) if tool == "send"
        ));
    }

    /// An Annotated candidate's delta is unknown at load; the cap is the only bound, so it stays
    /// counted as a redispatch wherever a cap could match it.
    #[test]
    fn an_annotated_candidate_stays_counted_wherever_a_cap_could_match_it() {
        let mut cfg = cap_target_config(3);
        cfg.annotators = vec![annotator("classifier")];
        cfg.tools.push(annotated("shell", "classifier"));
        assert!(matches!(
            Registry::build_covered_with_cap(cfg, PlannerCap::new(4).expect("nonzero")),
            Err(LoadError::TooManyPlanAlternatives { count: 5, max: 4, ref tool }) if tool == "send"
        ));
    }

    #[test]
    fn a_vacuous_public_cap_arms_no_redispatch_count() {
        let cap = PlannerCap::new(4).expect("nonzero");
        assert!(matches!(
            Registry::build_covered_with_cap(cap_target_config(4), cap),
            Err(LoadError::TooManyPlanAlternatives { count: 5, max: 4, .. })
        ));
        assert!(
            Registry::build_covered_with_cap(
                cap_target_config_with(4, DeclaredAudience::literal(Audience::public())),
                cap
            )
            .is_ok()
        );
    }

    #[test]
    fn a_tool_clearing_both_gap_species_counts_once() {
        let mut target = tool("send");
        target.requires = Requires {
            label: LabelRequirements {
                trust_floor: None,
                audience: vec![AudienceRequirement::Cap(DeclaredAudience::literal(
                    Audience::restricted([ReaderId::new("a")]),
                ))],
            },
            history: vec![HistoryRequirement::Prior(EffectKind::new("k"))],
            ..Requires::default()
        };
        let mut fixer = tool("fixer");
        fixer.emits = EffectSet::new([EffectKind::new("k")]).unwrap();
        fixer.delta.audience = Some(DeclaredAudience::restricted([ReaderId::new("a")]));
        let mut cfg = base();
        cfg.tools = declared(vec![target, fixer]);
        assert!(Registry::build_covered_with_cap(cfg, PlannerCap::new(2).expect("nonzero")).is_ok());
    }

    #[test]
    fn families_that_fit_alone_still_refuse_when_their_sum_exceeds_the_cap() {
        let mut target = tool("wire");
        target.requires = Requires {
            label: LabelRequirements {
                trust_floor: Some(Trust::new(1)),
                audience: vec![],
            },
            history: vec![HistoryRequirement::Prior(EffectKind::new("k"))],
            ..Requires::default()
        };
        let mut tools = vec![target];
        for i in 0..3 {
            let mut emitter = tool(&format!("emit{i}"));
            emitter.emits = EffectSet::new([EffectKind::new("k")]).unwrap();
            tools.push(emitter);
        }
        let mut bystander = tool("bystander");
        bystander.emits = EffectSet::new([EffectKind::new("other")]).unwrap();
        tools.push(bystander);
        let officer = |name: String| Authority {
            name: AuthorityName::new(name),
            mandate: Mandate {
                trust_ceiling: Some(Trust::new(1)),
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        let mut cfg = base();
        cfg.tools = declared(tools);
        cfg.authorities = (0..3).map(|i| officer(format!("officer{i}"))).collect();
        let cap = PlannerCap::new(5).expect("nonzero");
        assert!(matches!(
            Registry::build_covered_with_cap(cfg, cap),
            Err(LoadError::TooManyPlanAlternatives { count: 6, max: 5, ref tool }) if tool == "wire"
        ));
    }

    #[test]
    fn a_confined_stage_bound_counts_only_the_sanitizers_whose_scope_reaches_it() {
        let mut cfg = base();
        cfg.tools = vec![];
        cfg.sanitizers = (0..9)
            .map(|index| Sanitizer {
                scope: Scope {
                    tags: vec![TagName::new("outbound")],
                },
                ..output_sanitizer(index)
            })
            .collect();
        assert!(Registry::build_covered_with_cap(cfg, PlannerCap::new(1).expect("nonzero")).is_ok());

        let mut untagged = tool("read");
        untagged.delta = crate::contract::Delta::NONE;
        let mut cfg = base();
        cfg.tools = declared(vec![untagged]);
        cfg.sanitizers = (0..9)
            .map(|index| Sanitizer {
                scope: Scope {
                    tags: vec![TagName::new("outbound")],
                },
                ..output_sanitizer(index)
            })
            .collect();
        assert!(Registry::build_covered_with_cap(cfg, PlannerCap::new(1).expect("nonzero")).is_ok());
    }

    #[test]
    fn a_trust_transition_is_refused_at_the_input_point() {
        let sanitizer = |on: SanitizerPoints, transition| Sanitizer {
            name: SanitizerName::new("vouch"),
            on,
            transition,
            scope: Scope::default(),
            hint: None,
        };
        let trust = DeclaredTransition::Trust {
            from_floor: Trust::new(0),
            to: Trust::new(1),
        };
        let audience = DeclaredTransition::Audience {
            from_includes: DeclaredAudience::restricted([ReaderId::new("insider")]),
            to: DeclaredAudience::restricted([ReaderId::new("partner")]),
        };
        let built = |sanitizer: Sanitizer| {
            let mut cfg = base();
            cfg.sanitizers = vec![sanitizer];
            Registry::build_covered(cfg).map(|_| ())
        };
        let input_only = SanitizerPoints {
            input: true,
            output: false,
        };
        let both = SanitizerPoints {
            input: true,
            output: true,
        };
        let output_only = SanitizerPoints {
            input: false,
            output: true,
        };
        assert!(matches!(
            built(sanitizer(input_only, trust.clone())),
            Err(LoadError::InputSanitizerTrust(ref name)) if name == "vouch"
        ));
        assert!(matches!(
            built(sanitizer(both, trust.clone())),
            Err(LoadError::InputSanitizerTrust(_))
        ));
        assert_eq!(built(sanitizer(output_only, trust)), Ok(()));
        assert_eq!(built(sanitizer(input_only, audience)), Ok(()));
    }

    #[test]
    fn the_reserved_attest_schema_declaration_is_validated_at_load() {
        let attest = |on: SanitizerPoints, transition, scope| Sanitizer {
            name: SanitizerName::new("attest-schema"),
            on,
            transition,
            scope,
            hint: None,
        };
        let built = |sanitizer: Sanitizer| {
            let mut cfg = base();
            cfg.sanitizers = vec![sanitizer];
            Registry::build_covered(cfg).map(|_| ())
        };
        let trust = DeclaredTransition::Trust {
            from_floor: Trust::new(0),
            to: Trust::new(1),
        };
        let output_only = SanitizerPoints {
            input: false,
            output: true,
        };
        let audience = DeclaredTransition::Audience {
            from_includes: DeclaredAudience::restricted([ReaderId::new("insider")]),
            to: DeclaredAudience::restricted([ReaderId::new("partner")]),
        };
        assert!(matches!(
            built(attest(output_only, audience, Scope::default())),
            Err(LoadError::AttestSchemaAudienceMandate)
        ));
        let input_only = SanitizerPoints {
            input: true,
            output: false,
        };
        assert!(matches!(
            built(attest(input_only, trust.clone(), Scope::default())),
            Err(LoadError::AttestSchemaNotOutput)
        ));
        let scoped = Scope {
            tags: vec![TagName::new("outbound")],
        };
        assert!(matches!(
            built(attest(output_only, trust.clone(), scoped)),
            Err(LoadError::AttestSchemaScoped)
        ));
        let both = SanitizerPoints {
            input: true,
            output: true,
        };
        assert!(matches!(
            built(attest(both, trust.clone(), Scope::default())),
            Err(LoadError::InputSanitizerTrust(ref name)) if name == "attest-schema"
        ));
        assert_eq!(built(attest(output_only, trust, Scope::default())), Ok(()));
    }

    #[test]
    fn input_hops_add_to_the_call_stage_bound_and_only_where_they_are_in_scope() {
        let input_sanitizer = |index: usize, scope: Scope| Sanitizer {
            name: SanitizerName::new(format!("redact-{index}")),
            on: SanitizerPoints {
                input: true,
                output: false,
            },
            transition: DeclaredTransition::Audience {
                from_includes: DeclaredAudience::restricted([ReaderId::new("insider")]),
                to: DeclaredAudience::restricted([ReaderId::new("partner")]),
            },
            scope,
            hint: None,
        };
        let outbound = Scope {
            tags: vec![TagName::new("outbound")],
        };
        let mut target = tool("post");
        target.tags = vec![TagName::new("outbound")];
        target.requires.label.audience = vec![AudienceRequirement::Includes(RecipientSpec::Static(
            DeclaredAudience::restricted([ReaderId::new("partner")]),
        ))];
        let with = |sanitizers: Vec<Sanitizer>| {
            let mut cfg = base();
            cfg.tools = declared(vec![target.clone()]);
            cfg.sanitizers = sanitizers;
            cfg
        };
        let cap = PlannerCap::new(4).expect("nonzero");
        let in_scope: Vec<Sanitizer> = (0..3).map(|i| input_sanitizer(i, outbound.clone())).collect();
        assert!(Registry::build_covered_with_cap(with(in_scope.clone()), cap).is_ok());
        assert!(matches!(
            Registry::build_covered_with_cap(
                with([in_scope, vec![input_sanitizer(3, outbound)]].concat()),
                cap
            ),
            Err(LoadError::TooManyPlanAlternatives { count: 5, max: 4, ref tool }) if tool == "post"
        ));
        let elsewhere = Scope {
            tags: vec![TagName::new("inbound")],
        };
        let scoped_away: Vec<Sanitizer> = (0..9).map(|i| input_sanitizer(i, elsewhere.clone())).collect();
        assert!(Registry::build_covered_with_cap(with(scoped_away), PlannerCap::new(1).expect("nonzero")).is_ok());
    }

    #[test]
    fn sanitizer_chains_do_not_multiply_either_stage_bound() {
        let mut narrowing = tool("fetch");
        narrowing.delta.audience = Some(DeclaredAudience::restricted([ReaderId::new("a")]));
        let mut cfg = base();
        cfg.tools = declared(vec![narrowing]);
        cfg.sanitizers = (0..5).map(output_sanitizer).collect();
        assert!(Registry::build_covered_with_cap(cfg, PlannerCap::new(8).expect("nonzero")).is_ok());
    }
}
