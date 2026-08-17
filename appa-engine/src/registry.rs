//! The immutable registry: the engine's static capability, built once and validated at load.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::authority::{Authority, Cast, CastResolution, DeclaredTransition, Hint, Sanitizer};
use crate::contract::{AudienceDelta, AudienceRequirement, Delta, RecipientSpec, ToolContract};
use crate::groups::{DeclaredAudience, ExpansionRefusal, Expansions, GroupExpansion, GroupResolution};
use crate::label::{Audience, Dim, Dimension, ReaderId, Trust};
use crate::names::{AuthorityName, CastName, GroupName, MembershipResolverName, SanitizerName, TagName};
use crate::value::ToolName;

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

    pub fn len(&self) -> usize {
        self.ranks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ranks.is_empty()
    }

    fn contains_rank(&self, trust: Trust) -> bool {
        (trust.rank() as usize) < self.ranks.len()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryConfig {
    pub trust_chain: TrustChain,
    pub tools: Vec<ToolContract>,
    pub authorities: Vec<Authority>,
    pub sanitizers: Vec<Sanitizer>,
    pub casts: Vec<Cast>,
    /// The deployment's one membership resolver, registered by name. Every
    /// `@group` a placeholder argument names resolves through it; without one, such an argument
    /// names a reader set nothing can expand.
    #[serde(default)]
    pub membership: Option<MembershipResolverName>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LoadError {
    #[error("empty trust chain: at least one rank required")]
    EmptyTrustChain,
    #[error("trust chain too long: {len} ranks (a rank is a u8, so at most {max})")]
    TrustChainTooLong { len: usize, max: usize },
    #[error("duplicate trust rank {0:?} in the chain")]
    DuplicateRank(String),
    #[error("duplicate tool contract: {0}")]
    DuplicateTool(String),
    #[error("duplicate authority: {0}")]
    DuplicateAuthority(String),
    #[error("duplicate sanitizer: {0}")]
    DuplicateSanitizer(String),
    #[error("duplicate cast: {0}")]
    DuplicateCast(String),
    #[error("authority {0} has an empty mandate (covers nothing)")]
    EmptyMandate(String),
    #[error("trust rank {rank} out of the chain (length {len}) in {context}")]
    RankOutOfChain { rank: u8, len: usize, context: String },
    #[error("cast {0} is unreachable: no registered origin in its scope can use it")]
    UnreachableCast(String),
    #[error("cast {cast} is shadowed by earlier constant {by} at every origin it can receive")]
    ShadowedCast { cast: String, by: String },
    #[error("tool {0} declares both output dimensions pending-cast (a cast resolves exactly one)")]
    DualPendingCast(String),
    #[error("tool {tool} declares a pending-cast {dimension:?} output and a `requires` on that dimension")]
    PendingCastWithRequirement { tool: String, dimension: Dimension },
    #[error(
        "tool {0} is unannotated (no delta) but declares label requirements: declare its delta (`delta = {{}}` for a deliberately neutral output) so the committed label the requirements check is established"
    )]
    UnannotatedWithLabelRequirement(String),
    #[error(
        "tool {tool}: {count} worst-case alternative remedy plans exceed the planner cap of {max} — reduce the requirement entries, the competent authorities, or the clearing tools, or raise `[limits] planner_cap`"
    )]
    TooManyPlanAlternatives { tool: String, count: u128, max: u128 },
    #[error(
        "confined-return stage: {count} worst-case sanitizer alternatives exceed the planner cap of {max} — reduce the registered output sanitizers or raise `[limits] planner_cap`"
    )]
    TooManyReturnPlanAlternatives { count: u128, max: u128 },
    #[error("{context}: hint is {len} characters, over the {max} a plan offer carries")]
    HintTooLong { context: String, len: usize, max: usize },
    #[error(
        "{context}: {reader:?} is not a literal reader ID — `public` names the whole audience, and the `@` mark is reserved for groups a membership resolver expands"
    )]
    NonLiteralReader { context: String, reader: String },
    #[error("group {group} is written in a configuration that registers no membership resolver")]
    GroupWithoutResolver { group: String },
    #[error(
        "deployment starting label: the {dimension:?} dimension is unestablished — an unknown starting dimension has no source value a cast could resolve"
    )]
    UnresolvedStartingDimension { dimension: Dimension },
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
        "tool {tool} declares a pending-cast delta but the deployment does not confine its result point — the offer needs a raw result the model has not seen"
    )]
    PendingCastUnconfined { tool: String },
    #[error(
        "sanitizer {sanitizer} registers on tool_output but the deployment confines no application point — neither a result point nor the child-return crossing"
    )]
    OutputSanitizerUncovered { sanitizer: String },
    #[error("[child] declares a return binding but the deployment does not control child context")]
    ChildWithoutContextControl,
    #[error("[child] return_sanitizer names unregistered sanitizer {0}")]
    ChildReturnSanitizerUnknown(String),
    #[error("[child] return_sanitizer {0} is not registered for tool output")]
    ChildReturnSanitizerNotOutput(String),
    #[error(
        "[child] return_sanitizer {0} declares a scope: a child return originates from no tool, so only an unscoped sanitizer can be bound to it"
    )]
    ChildReturnSanitizerScoped(String),
    #[error(
        "sanitizer {0} registers on tool_input with a trust transition: only the `includes` check reads an input substitution, so a trust `to` can never help a call and the sanitizer would sit inert"
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
    #[error(
        "provider-run tool {tool} declares {construct}: a provider-run contract may declare only a static delta"
    )]
    ProviderRunConstruct {
        tool: String,
        construct: crate::profile::ProviderRunConstruct,
    },
    #[error(
        "{context} binds audience argument {argument:?}, which {fault}: a placeholder or dynamic binding names a required top-level string property of the tool's `parameters`"
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
/// sets it via `[limits] planner_cap`; omitted, the cap is 64. Zero is unrepresentable: every
/// stage's worst case is at least one, so a zero cap would refuse every registry.
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
        PlannerCap(64)
    }
}

/// The longest hint a registration may carry. Every offer of every block repeats the
/// hints of the entities it names, so an unbounded one is a way to flood the agent's context from
/// configuration. A sentence or two is the intended shape.
pub const MAX_HINT_CHARS: usize = 512;

fn worst_case_plan_alternatives(
    tool: &ToolContract,
    confined: bool,
    tools: &BTreeMap<ToolName, ToolContract>,
    authorities: &[Authority],
    sanitizers: &[Sanitizer],
    literal: &Expansions,
) -> u128 {
    use crate::check::Gap;
    use crate::contract::{AudienceRequirement, HistoryRequirement};
    use crate::fact::EffectKind;
    use crate::plan::covers_gap;

    let mut count: u128 = 1;
    let mut multiply = |competent: usize| count = count.saturating_mul(competent.max(1) as u128);

    if let Some(floor) = tool.requires.label.trust_floor {
        let gap = Gap::TrustFloor {
            required: floor,
            actual: floor,
        };
        multiply(
            authorities
                .iter()
                .filter(|authority| covers_gap(authority, &gap, &tool.tags, literal))
                .count(),
        );
    }
    let mut seen_includes: Vec<&AudienceRequirement> = Vec::new();
    for requirement in &tool.requires.label.audience {
        match requirement {
            AudienceRequirement::Includes(_) if !seen_includes.contains(&requirement) => {
                seen_includes.push(requirement);
                multiply(
                    authorities
                        .iter()
                        .filter(|authority| {
                            authority.scope.covers(&tool.tags) && authority.mandate.reader_ceiling.is_some()
                        })
                        .count(),
                );
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
                        .filter(|authority| covers_gap(authority, &gap, &tool.tags, literal))
                        .count(),
                );
            }
            HistoryRequirement::NoPrior(_) | HistoryRequirement::Prior(_) => {}
        }
    }
    let mut seen_marks: Vec<&crate::names::MarkName> = Vec::new();
    for mark in &tool.requires.attention {
        if seen_marks.contains(&mark) {
            continue;
        }
        seen_marks.push(mark);
        let gap = Gap::Attention(mark.clone());
        multiply(
            authorities
                .iter()
                .filter(|authority| covers_gap(authority, &gap, &tool.tags, literal))
                .count(),
        );
    }
    let output = tool.output_label(literal);
    let applicable = match tool.pending_cast_dim() {
        _ if !confined => 0,
        Some(_) => 0,
        None if matches!(
            tool.delta.as_ref().and_then(|delta| delta.audience.as_ref()),
            Some(AudienceDelta::Dynamic(_))
        ) || tool.delta.iter().flat_map(Delta::groups).next().is_some() =>
        {
            sanitizers
                .iter()
                .filter(|sanitizer| !sanitizer.name.is_attest_schema())
                .filter(|sanitizer| sanitizer.on.output && sanitizer.applies_to(&tool.tags))
                .count()
        }
        None => sanitizers
            .iter()
            .filter(|sanitizer| !sanitizer.name.is_attest_schema())
            .filter(|sanitizer| {
                sanitizer.on.output && sanitizer.applies_to(&tool.tags) && sanitizer.transition.may_admit(&output)
            })
            .count(),
    };
    multiply(applicable + 1);

    let priors: Vec<&EffectKind> = tool
        .requires
        .history
        .iter()
        .filter_map(|requirement| match requirement {
            HistoryRequirement::Prior(kind) => Some(kind),
            HistoryRequirement::NoPrior(_) => None,
        })
        .collect();
    let has_cap = tool.requires.label.audience.iter().any(|requirement| {
        matches!(
            requirement,
            AudienceRequirement::Cap(DeclaredAudience::Restricted { .. })
        )
    });
    let redispatches = tools
        .values()
        .filter(|candidate| {
            candidate.emits.iter().any(|kind| priors.contains(&kind))
                || (has_cap
                    && matches!(
                        candidate.delta.as_ref().and_then(|delta| delta.audience.as_ref()),
                        Some(AudienceDelta::Static(DeclaredAudience::Restricted { .. }))
                    ))
        })
        .count() as u128;
    let input_hops = sanitizers
        .iter()
        .filter(|sanitizer| sanitizer.on.input && sanitizer.applies_to(&tool.tags))
        .count() as u128;
    count
        .saturating_add(redispatches)
        .saturating_add(input_hops)
        .max(worst_case_confined_stage(
            sanitizers,
            confined,
            tool.pending_cast_dim().is_some(),
            &tool.tags,
        ))
}

fn worst_case_confined_stage(sanitizers: &[Sanitizer], confined: bool, pending_cast: bool, tags: &[TagName]) -> u128 {
    if !confined || pending_cast {
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

fn worst_case_return_stage(sanitizers: &[Sanitizer], confined: bool) -> u128 {
    if !confined {
        return 1;
    }
    1u128.saturating_add(
        sanitizers
            .iter()
            .filter(|sanitizer| sanitizer.on.output && sanitizer.applies_to(&[]))
            .count() as u128,
    )
}

/// The validated, indexed, immutable registry: the engine's whole static capability, contracts
/// and coverage together. The deployment profile splits the catalogue at build: provider-run
/// tools live apart from the checkable contracts, so the check, plan enumeration,
/// redispatch offers, and the planner-cap bound exclude them by construction — no call site
/// filters. The profile itself rides along, so plan and branch enumeration read confinement and
/// context control from the one capability object they already hold.
#[derive(Clone, Debug)]
pub struct Registry {
    trust_chain: TrustChain,
    tools: BTreeMap<ToolName, ToolContract>,
    provider_run: BTreeMap<ToolName, ToolContract>,
    authorities: Vec<Authority>,
    sanitizers: BTreeMap<SanitizerName, Sanitizer>,
    casts: Vec<Cast>,
    membership: Option<MembershipResolverName>,
    groups: Vec<GroupName>,
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
                    check_declared_readers(from_includes, || format!("{} from", context()))?;
                    check_declared_readers(to, || format!("{} to", context()))?;
                }
            }
            check_hint(sanitizer.hint.as_ref(), context)?;
            if sanitizers.insert(sanitizer.name.clone(), sanitizer.clone()).is_some() {
                return Err(LoadError::DuplicateSanitizer(sanitizer.name.as_str().to_string()));
            }
        }

        let mut tools = BTreeMap::new();
        let mut provider_run = BTreeMap::new();
        for tool in config.tools {
            let declared_trust = match tool.delta.as_ref().and_then(|d| d.trust.as_ref()) {
                Some(Dim::Known(t)) => Some(*t),
                Some(Dim::Unknown) | None => None,
            };
            check_rank(&config.trust_chain, declared_trust, || {
                format!("tool {} delta", tool.name.as_str())
            })?;
            check_rank(&config.trust_chain, tool.requires.label.trust_floor, || {
                format!("tool {} trust floor", tool.name.as_str())
            })?;
            if let Some(AudienceDelta::Static(audience)) = tool.delta.as_ref().and_then(|d| d.audience.as_ref()) {
                check_declared_readers(audience, || format!("tool {} delta", tool.name.as_str()))?;
            }
            for requirement in &tool.requires.label.audience {
                match requirement {
                    AudienceRequirement::Includes(RecipientSpec::Static(recipients)) => {
                        check_declared_readers(recipients, || format!("tool {} includes", tool.name.as_str()))?;
                    }
                    AudienceRequirement::Cap(cap) => {
                        check_declared_readers(cap, || format!("tool {} cap", tool.name.as_str()))?;
                    }
                    AudienceRequirement::Includes(RecipientSpec::Placeholder(_) | RecipientSpec::Dynamic(_)) => {}
                }
            }
            validate_pending_cast(&tool)?;
            let split = if profile.is_provider_run(&tool.name) {
                &mut provider_run
            } else {
                &mut tools
            };
            if split.insert(tool.name.clone(), tool.clone()).is_some() {
                return Err(LoadError::DuplicateTool(tool.name.as_str().to_string()));
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
                check_declared_readers(ceiling, || {
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

        for tool in tools.values() {
            check_audience_bindings(tool)?;
        }

        let mut groups: Vec<GroupName> = tools
            .values()
            .chain(provider_run.values())
            .flat_map(ToolContract::groups)
            .chain(
                config
                    .authorities
                    .iter()
                    .flat_map(|authority| authority.mandate.groups()),
            )
            .chain(sanitizers.values().flat_map(Sanitizer::groups))
            .chain(config.casts.iter().flat_map(|cast| cast.resolution.groups()))
            .cloned()
            .collect();
        groups.sort();
        groups.dedup();
        if config.membership.is_none()
            && let Some(group) = groups.first()
        {
            return Err(LoadError::GroupWithoutResolver {
                group: group.to_string(),
            });
        }
        let literal = Expansions::empty_members(&groups);

        let sanitizer_list: Vec<Sanitizer> = sanitizers.values().cloned().collect();
        for tool in tools.values() {
            let count = worst_case_plan_alternatives(
                tool,
                profile.confines_result(&tool.name),
                &tools,
                &config.authorities,
                &sanitizer_list,
                &literal,
            );
            if count > planner_cap.0 {
                return Err(LoadError::TooManyPlanAlternatives {
                    tool: tool.name.as_str().to_string(),
                    count,
                    max: planner_cap.0,
                });
            }
        }
        let confined = worst_case_return_stage(&sanitizer_list, profile.confines_child_return());
        if confined > planner_cap.0 {
            return Err(LoadError::TooManyReturnPlanAlternatives {
                count: confined,
                max: planner_cap.0,
            });
        }

        let mut casts: Vec<Cast> = Vec::new();
        for cast in config.casts {
            match &cast.resolution {
                CastResolution::Resolver { may_cast } => {
                    for rank in &may_cast.trust {
                        check_rank(&config.trust_chain, Some(*rank), || {
                            format!("cast {} may_cast", cast.name.as_str())
                        })?;
                    }
                    check_declared_readers(&may_cast.audience, || format!("cast {} may_cast", cast.name.as_str()))?;
                }
                CastResolution::Constant(constant) => {
                    check_rank(&config.trust_chain, Some(constant.trust), || {
                        format!("cast {} constant", cast.name.as_str())
                    })?;
                    check_declared_readers(&constant.audience, || format!("cast {} constant", cast.name.as_str()))?;
                }
            }
            if casts.iter().any(|earlier| earlier.name == cast.name) {
                return Err(LoadError::DuplicateCast(cast.name.as_str().to_string()));
            }
            casts.push(cast);
        }

        let writes_group = |tool: &ToolContract, cast: &Cast| {
            tool.delta.iter().flat_map(Delta::groups).next().is_some() || cast.resolution.groups().next().is_some()
        };
        for (i, cast) in casts.iter().enumerate() {
            let castable: Vec<&ToolContract> = tools
                .values()
                .filter(|tool| cast.scope.covers(&tool.tags))
                .filter(|tool| crate::label::EstablishedLabel::from_label(&tool.output_label(&literal)).is_none())
                .collect();
            if !cast.scope.is_unscoped() {
                let usable = castable.iter().any(|tool| match &cast.resolution {
                    CastResolution::Constant(constant) => {
                        writes_group(tool, cast)
                            || cast
                                .resolution
                                .validate(&tool.output_label(&literal), &constant.resolve(&literal), &literal)
                                .is_ok()
                    }
                    CastResolution::Resolver { may_cast } => {
                        matches!(tool.output_label(&literal).trust, Dim::Known(_)) || !may_cast.trust.is_empty()
                    }
                });
                if !usable {
                    return Err(LoadError::UnreachableCast(cast.name.as_str().to_string()));
                }
            }
            if castable.is_empty() {
                continue;
            }
            let shadowing = casts[..i].iter().find(|earlier| {
                let CastResolution::Constant(constant) = &earlier.resolution else {
                    return false;
                };
                earlier.scope.covers_scope(&cast.scope)
                    && castable.iter().all(|tool| {
                        !pins_audience_beside_pending_trust(tool)
                            && !writes_group(tool, earlier)
                            && earlier
                                .resolution
                                .validate(&tool.output_label(&literal), &constant.resolve(&literal), &literal)
                                .is_ok()
                    })
            });
            if let Some(earlier) = shadowing {
                return Err(LoadError::ShadowedCast {
                    cast: cast.name.as_str().to_string(),
                    by: earlier.name.as_str().to_string(),
                });
            }
        }

        Ok(Registry {
            trust_chain: config.trust_chain,
            tools,
            provider_run,
            authorities: config.authorities,
            sanitizers,
            casts,
            membership: config.membership,
            groups,
            profile,
        })
    }

    pub fn membership(&self) -> Option<&MembershipResolverName> {
        self.membership.as_ref()
    }

    /// Every group this policy's declarations write, in name order: the table records
    /// index resolutions into ([`crate::groups::GroupIndex`]) and the set an event's expansions may name.
    pub fn groups(&self) -> &[GroupName] {
        &self.groups
    }

    /// An event's answers as one operation's expansions: each names a group this
    /// policy writes, once.
    pub(crate) fn expansions_from_event(&self, answers: &[GroupExpansion]) -> Result<Expansions, ExpansionRefusal> {
        Expansions::from_event(&self.groups, answers)
    }

    pub(crate) fn expansions_from_resolutions(
        &self,
        resolutions: &[GroupResolution],
    ) -> Result<Expansions, ExpansionRefusal> {
        Expansions::from_resolutions(&self.groups, resolutions)
    }

    pub(crate) fn resolutions(&self, expansions: &Expansions) -> Vec<GroupResolution> {
        expansions.resolutions(&self.groups)
    }

    pub fn profile(&self) -> &crate::profile::DeploymentProfile {
        &self.profile
    }

    pub fn trust_chain(&self) -> &TrustChain {
        &self.trust_chain
    }

    pub fn tool(&self, name: &ToolName) -> Option<&ToolContract> {
        self.tools.get(name)
    }

    /// The declared contract of a provider-run tool: never checked or planned; its
    /// static `delta` is what an exposed result is admitted under.
    pub fn provider_run_contract(&self, name: &ToolName) -> Option<&ToolContract> {
        self.provider_run.get(name)
    }

    pub fn provider_run_contracts(&self) -> impl Iterator<Item = &ToolContract> {
        self.provider_run.values()
    }

    pub fn tools(&self) -> impl Iterator<Item = &ToolContract> {
        self.tools.values()
    }

    pub fn authorities(&self) -> &[Authority] {
        &self.authorities
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

    pub fn cast(&self, name: &CastName) -> Option<&Cast> {
        self.casts.iter().find(|cast| &cast.name == name)
    }

    pub fn casts(&self) -> &[Cast] {
        &self.casts
    }
}

fn pins_audience_beside_pending_trust(tool: &ToolContract) -> bool {
    tool.delta.as_ref().is_some_and(|delta| {
        matches!(delta.trust, Some(Dim::Unknown)) && matches!(delta.audience, Some(AudienceDelta::Dynamic(_)))
    })
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

fn validate_pending_cast(tool: &ToolContract) -> Result<(), LoadError> {
    let Some(delta) = &tool.delta else {
        let requires_label = tool.requires.label.trust_floor.is_some() || !tool.requires.label.audience.is_empty();
        return if requires_label {
            Err(LoadError::UnannotatedWithLabelRequirement(
                tool.name.as_str().to_string(),
            ))
        } else {
            Ok(())
        };
    };
    if matches!(delta.trust, Some(Dim::Unknown))
        && matches!(delta.audience, Some(crate::contract::AudienceDelta::PendingCast))
    {
        return Err(LoadError::DualPendingCast(tool.name.as_str().to_string()));
    }
    match delta.pending_cast_dim() {
        Some(Dimension::Trust) if tool.requires.label.trust_floor.is_some() => {
            Err(LoadError::PendingCastWithRequirement {
                tool: tool.name.as_str().to_string(),
                dimension: Dimension::Trust,
            })
        }
        Some(Dimension::Audience) if !tool.requires.label.audience.is_empty() => {
            Err(LoadError::PendingCastWithRequirement {
                tool: tool.name.as_str().to_string(),
                dimension: Dimension::Audience,
            })
        }
        _ => Ok(()),
    }
}

fn check_audience_bindings(tool: &ToolContract) -> Result<(), LoadError> {
    let check = |argument: &str, site: &str| {
        tool.parameters
            .required_string_property(argument)
            .map_err(|fault| LoadError::AudienceBindingSchema {
                context: format!("tool {} {site}", tool.name.as_str()),
                argument: argument.to_string(),
                fault,
            })
    };
    if let Some(AudienceDelta::Dynamic(binding)) = tool.delta.as_ref().and_then(|delta| delta.audience.as_ref()) {
        check(&binding.argument, "delta")?;
    }
    for requirement in &tool.requires.label.audience {
        match requirement {
            AudienceRequirement::Includes(RecipientSpec::Placeholder(argument)) => check(argument, "includes")?,
            AudienceRequirement::Includes(RecipientSpec::Dynamic(binding)) => check(&binding.argument, "includes")?,
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

/// Every reader ID a label the algebra holds directly names must be literal:
/// `public` is a reserved audience *state* — [`Audience::Public`] carries it, so it is never a
/// member of a restricted set — and the `@` mark names a group only a membership resolver may
/// expand. The deployment starting label is an [`Audience`] because no operation ever resolves it.
pub(crate) fn check_readers(audience: &Audience, context: impl Fn() -> String) -> Result<(), LoadError> {
    let Audience::Restricted(readers) = audience else {
        return Ok(());
    };
    check_literal(readers, context)
}

fn check_declared_readers(audience: &DeclaredAudience, context: impl Fn() -> String) -> Result<(), LoadError> {
    let DeclaredAudience::Restricted { readers, .. } = audience else {
        return Ok(());
    };
    check_literal(readers, context)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::SanitizerPoints;
    use crate::authority::{CastCeiling, DeclaredLabel, Mandate, Scope};
    use crate::contract::{
        AudienceRequirement, Delta, DynamicAudienceBinding, HistoryRequirement, LabelRequirements, Requires,
    };
    use crate::fact::{EffectKind, EffectSet};
    use crate::label::EstablishedLabel;
    use crate::label::{Audience, ReaderId, Trust};
    use crate::names::{AuthorityName, MarkName, TagName};

    fn chain() -> TrustChain {
        TrustChain::new(vec!["suspicious".into(), "trusted".into()])
    }

    fn base() -> RegistryConfig {
        RegistryConfig {
            trust_chain: chain(),
            tools: vec![],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![],
            membership: None,
        }
    }

    fn tool(name: &str) -> ToolContract {
        ToolContract {
            name: ToolName::new(name),
            tags: vec![],
            delta: Some(Delta::NONE),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires::default(),
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

    fn audience_sites(reader: &str) -> Vec<(&'static str, RegistryConfig)> {
        let named = Audience::restricted([ReaderId::new(reader)]);
        let literal = Audience::restricted([ReaderId::new("finance")]);

        let mut delta = base();
        let mut delta_tool = tool("emit");
        delta_tool.delta = Some(Delta {
            trust: None,
            audience: Some(AudienceDelta::Static(DeclaredAudience::literal(named.clone()))),
        });
        delta.tools = vec![delta_tool];

        let mut includes = base();
        let mut includes_tool = tool("emit");
        includes_tool.requires.label.audience = vec![AudienceRequirement::Includes(RecipientSpec::Static(
            DeclaredAudience::literal(named.clone()),
        ))];
        includes.tools = vec![includes_tool];

        let mut cap = base();
        let mut cap_tool = tool("emit");
        cap_tool.requires.label.audience = vec![AudienceRequirement::Cap(DeclaredAudience::literal(named.clone()))];
        cap.tools = vec![cap_tool];

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

        let cast = |name, resolution| Cast {
            name: CastName::new(name),
            resolution,
            scope: Scope::default(),
        };
        let mut may_cast = base();
        may_cast.casts = vec![cast(
            "classifier",
            CastResolution::Resolver {
                may_cast: CastCeiling {
                    trust: vec![Trust::new(0)],
                    audience: DeclaredAudience::literal(named.clone()),
                },
            },
        )];
        let mut constant = base();
        constant.casts = vec![cast(
            "paranoid",
            CastResolution::Constant(DeclaredLabel::literal(EstablishedLabel::new(Trust::new(0), named))),
        )];

        vec![
            ("tool emit delta", delta),
            ("tool emit includes", includes),
            ("tool emit cap", cap),
            ("authority officer reader ceiling", ceiling),
            ("sanitizer redactor from", transition_from),
            ("sanitizer redactor to", transition_to),
            ("cast classifier may_cast", may_cast),
            ("cast paranoid constant", constant),
        ]
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
        cfg.tools = vec![spoiled];
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
                reader_ceiling: Some(DeclaredAudience::literal(Audience::Public)),
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
        empty_cap.tools = vec![cap_tool];
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
        cfg.tools = vec![tool("get"), tool("send")];
        cfg.authorities = vec![attends_authority("officer")];
        let reg = Registry::build_covered(cfg).unwrap();
        assert!(reg.tool(&ToolName::new("get")).is_some());
        assert!(reg.authority(&AuthorityName::new("officer")).is_some());
    }

    #[test]
    fn refuses_duplicate_tool() {
        let mut cfg = base();
        cfg.tools = vec![tool("dup"), tool("dup")];
        assert!(matches!(
            Registry::build_covered(cfg),
            Err(LoadError::DuplicateTool(name)) if name == "dup"
        ));
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
        cfg.tools = vec![ToolContract {
            delta: Some(Delta {
                trust: Some(Dim::Known(Trust::new(9))),
                audience: None,
            }),
            ..tool("over")
        }];
        assert!(matches!(
            Registry::build_covered(cfg),
            Err(LoadError::RankOutOfChain { rank: 9, .. })
        ));
    }

    fn internal() -> Audience {
        Audience::restricted([ReaderId::new("internal")])
    }

    fn origin(name: &str, tags: &[&str], delta: Delta) -> ToolContract {
        ToolContract {
            name: ToolName::new(name),
            tags: tags.iter().copied().map(crate::names::TagName::new).collect(),
            delta: Some(delta),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires::default(),
        }
    }

    fn pending_trust(audience: Audience) -> Delta {
        Delta {
            trust: Some(Dim::Unknown),
            audience: Some(Dim::Known(audience).into()),
        }
    }

    fn pending_audience(trust: Trust) -> Delta {
        Delta {
            trust: Some(Dim::Known(trust)),
            audience: Some(Dim::Unknown.into()),
        }
    }

    fn scoped(tags: &[&str]) -> Scope {
        Scope {
            tags: tags.iter().copied().map(crate::names::TagName::new).collect(),
        }
    }

    fn constant_cast(name: &str, label: EstablishedLabel, scope: Scope) -> Cast {
        Cast {
            name: CastName::new(name),
            resolution: CastResolution::Constant(DeclaredLabel::literal(label)),
            scope,
        }
    }

    fn resolver_cast(name: &str, trust: Vec<Trust>, audience: Audience, scope: Scope) -> Cast {
        Cast {
            name: CastName::new(name),
            resolution: CastResolution::Resolver {
                may_cast: CastCeiling {
                    trust,
                    audience: DeclaredAudience::literal(audience),
                },
            },
            scope,
        }
    }

    #[test]
    fn casts_keep_registration_order() {
        let mut cfg = base();
        cfg.tools = vec![origin("inbox", &[], pending_trust(internal()))];
        cfg.casts = vec![
            resolver_cast("zeta", vec![Trust::new(0)], Audience::Public, Scope::default()),
            constant_cast(
                "alpha",
                EstablishedLabel::new(Trust::new(0), internal()),
                Scope::default(),
            ),
        ];
        let reg = Registry::build_covered(cfg).unwrap();
        let names: Vec<&str> = reg.casts().iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["zeta", "alpha"]);
    }

    #[test]
    fn an_earlier_resolver_never_shadows_a_later_cast() {
        let mut cfg = base();
        cfg.tools = vec![origin("inbox", &[], pending_trust(internal()))];
        cfg.casts = vec![
            resolver_cast("classifier", vec![Trust::new(0)], Audience::Public, Scope::default()),
            constant_cast(
                "fallback",
                EstablishedLabel::new(Trust::new(0), internal()),
                Scope::default(),
            ),
        ];
        assert!(Registry::build_covered(cfg).is_ok());
    }

    #[test]
    fn an_earlier_constant_valid_at_every_origin_shadows_a_later_cast() {
        let mut cfg = base();
        cfg.tools = vec![origin("inbox", &[], pending_trust(internal()))];
        cfg.casts = vec![
            constant_cast(
                "fallback",
                EstablishedLabel::new(Trust::new(0), internal()),
                Scope::default(),
            ),
            resolver_cast("classifier", vec![Trust::new(0)], Audience::Public, Scope::default()),
        ];
        assert!(matches!(
            Registry::build_covered(cfg),
            Err(LoadError::ShadowedCast { cast, by }) if cast == "classifier" && by == "fallback"
        ));
    }

    #[test]
    fn a_group_writing_constant_neither_shadows_nor_reads_as_unreachable() {
        let grouped = |trust: Trust| {
            CastResolution::Constant(crate::authority::DeclaredLabel {
                trust,
                audience: DeclaredAudience::declared([], [GroupName::new("team")]).unwrap(),
            })
        };
        let mut cfg = base();
        cfg.membership = Some(MembershipResolverName::new("directory"));
        cfg.tools = vec![origin("inbox", &[], pending_trust(internal()))];
        cfg.casts = vec![
            Cast {
                name: CastName::new("fallback"),
                resolution: grouped(Trust::new(0)),
                scope: Scope::default(),
            },
            resolver_cast("classifier", vec![Trust::new(0)], Audience::Public, Scope::default()),
        ];
        let registry = Registry::build_covered(cfg).expect("a group-writing constant shadows nothing");
        assert_eq!(registry.groups(), [GroupName::new("team")]);

        let mut cfg = base();
        cfg.membership = Some(MembershipResolverName::new("directory"));
        cfg.tools = vec![origin("inbox", &["mail"], pending_trust(internal()))];
        cfg.casts = vec![Cast {
            name: CastName::new("mailroom"),
            resolution: grouped(Trust::new(0)),
            scope: scoped(&["mail"]),
        }];
        assert!(Registry::build_covered(cfg).is_ok());

        let mut cfg = base();
        cfg.tools = vec![origin("inbox", &[], pending_trust(internal()))];
        cfg.casts = vec![Cast {
            name: CastName::new("fallback"),
            resolution: grouped(Trust::new(0)),
            scope: Scope::default(),
        }];
        assert!(matches!(
            Registry::build_covered(cfg),
            Err(LoadError::GroupWithoutResolver { group }) if group == "@team"
        ));
    }

    #[test]
    fn a_constant_failing_at_one_origin_shadows_nothing() {
        let mut cfg = base();
        cfg.tools = vec![
            origin("inbox", &[], pending_trust(internal())),
            origin("board", &[], pending_audience(Trust::new(1))),
        ];
        cfg.casts = vec![
            constant_cast(
                "fallback",
                EstablishedLabel::new(Trust::new(0), internal()),
                Scope::default(),
            ),
            resolver_cast("classifier", vec![Trust::new(0)], Audience::Public, Scope::default()),
        ];
        assert!(Registry::build_covered(cfg).is_ok());
    }

    #[test]
    fn a_dynamically_pinned_audience_defeats_constant_shadowing() {
        let mut cfg = base();
        let mut feed = origin(
            "feed",
            &[],
            Delta {
                trust: Some(Dim::Unknown),
                audience: Some(crate::contract::AudienceDelta::Dynamic(DynamicAudienceBinding {
                    resolver: crate::names::DynamicResolverName::new("directory"),
                    argument: "room".into(),
                })),
            },
        );
        feed.parameters = crate::params::test_string_argument_schema("room");
        cfg.tools = vec![feed];
        cfg.casts = vec![
            constant_cast(
                "fallback",
                EstablishedLabel::new(Trust::new(0), internal()),
                Scope::default(),
            ),
            resolver_cast("classifier", vec![Trust::new(0)], Audience::Public, Scope::default()),
        ];
        assert!(Registry::build_covered(cfg).is_ok());
    }

    #[test]
    fn a_tag_superset_scope_covers_and_shadows_the_subset() {
        let mut cfg = base();
        cfg.tools = vec![origin("inbox", &["mail"], pending_trust(internal()))];
        cfg.casts = vec![
            constant_cast(
                "fallback",
                EstablishedLabel::new(Trust::new(0), internal()),
                scoped(&["mail", "web"]),
            ),
            resolver_cast("classifier", vec![Trust::new(0)], Audience::Public, scoped(&["mail"])),
        ];
        assert!(matches!(
            Registry::build_covered(cfg),
            Err(LoadError::ShadowedCast { cast, by }) if cast == "classifier" && by == "fallback"
        ));
    }

    #[test]
    fn a_tag_subset_scope_does_not_cover_the_superset() {
        let mut cfg = base();
        cfg.tools = vec![origin("inbox", &["mail"], pending_trust(internal()))];
        cfg.casts = vec![
            constant_cast(
                "fallback",
                EstablishedLabel::new(Trust::new(0), internal()),
                scoped(&["mail"]),
            ),
            resolver_cast(
                "classifier",
                vec![Trust::new(0)],
                Audience::Public,
                scoped(&["mail", "web"]),
            ),
        ];
        assert!(Registry::build_covered(cfg).is_ok());
    }

    #[test]
    fn a_scoped_cast_covering_no_registered_origin_is_unreachable() {
        let mut cfg = base();
        cfg.tools = vec![origin("inbox", &["mail"], pending_trust(internal()))];
        cfg.casts = vec![resolver_cast(
            "classifier",
            vec![Trust::new(0)],
            Audience::Public,
            scoped(&["ghost"]),
        )];
        assert!(matches!(
            Registry::build_covered(cfg),
            Err(LoadError::UnreachableCast(name)) if name == "classifier"
        ));
    }

    #[test]
    fn a_scoped_resolver_no_covered_origin_can_use_is_unreachable() {
        let mut cfg = base();
        cfg.tools = vec![origin("inbox", &["mail"], pending_trust(internal()))];
        cfg.casts = vec![resolver_cast("classifier", vec![], Audience::Public, scoped(&["mail"]))];
        assert!(matches!(
            Registry::build_covered(cfg),
            Err(LoadError::UnreachableCast(name)) if name == "classifier"
        ));
    }

    #[test]
    fn an_audience_only_scoped_resolver_loads_for_an_established_trust_origin() {
        let mut cfg = base();
        cfg.tools = vec![
            origin("inbox", &["mail"], pending_trust(internal())),
            origin("board", &["mail"], pending_audience(Trust::new(1))),
        ];
        cfg.casts = vec![resolver_cast("classifier", vec![], Audience::Public, scoped(&["mail"]))];
        assert!(Registry::build_covered(cfg).is_ok());
    }

    #[test]
    fn an_audience_only_ceiling_loads() {
        let mut cfg = base();
        cfg.casts = vec![Cast {
            name: CastName::new("classifier"),
            resolution: CastResolution::Resolver {
                may_cast: CastCeiling {
                    trust: vec![],
                    audience: DeclaredAudience::literal(Audience::Public),
                },
            },
            scope: Scope::default(),
        }];
        assert!(Registry::build_covered(cfg).is_ok());
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

    #[test]
    fn refuses_duplicate_trust_rank() {
        let mut cfg = base();
        cfg.trust_chain = TrustChain::new(vec!["low".into(), "high".into(), "low".into()]);
        assert!(matches!(
            Registry::build_covered(cfg),
            Err(LoadError::DuplicateRank(name)) if name == "low"
        ));
    }

    #[test]
    fn refuses_dual_pending_cast_output() {
        let mut cfg = base();
        cfg.tools = vec![ToolContract {
            delta: Some(Delta {
                trust: Some(Dim::Unknown),
                audience: Some(Dim::Unknown.into()),
            }),
            ..tool("scan")
        }];
        assert!(matches!(
            Registry::build_covered(cfg),
            Err(LoadError::DualPendingCast(name)) if name == "scan"
        ));
    }

    #[test]
    fn refuses_a_requirement_on_a_pending_cast_dimension() {
        use crate::contract::{AudienceRequirement, LabelRequirements, Requires};
        use crate::label::Audience;

        let mut cfg = base();
        cfg.tools = vec![ToolContract {
            delta: Some(Delta {
                trust: Some(Dim::Unknown),
                audience: None,
            }),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(Trust::new(1)),
                    audience: vec![],
                },
                ..Requires::default()
            },
            ..tool("scan")
        }];
        assert!(matches!(
            Registry::build_covered(cfg),
            Err(LoadError::PendingCastWithRequirement {
                dimension: Dimension::Trust,
                ..
            })
        ));

        let mut cfg = base();
        cfg.tools = vec![ToolContract {
            delta: Some(Delta {
                trust: None,
                audience: Some(Dim::Unknown.into()),
            }),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Cap(DeclaredAudience::literal(Audience::Public))],
                },
                ..Requires::default()
            },
            ..tool("scan")
        }];
        assert!(matches!(
            Registry::build_covered(cfg),
            Err(LoadError::PendingCastWithRequirement {
                dimension: Dimension::Audience,
                ..
            })
        ));

        let mut cfg = base();
        cfg.tools = vec![ToolContract {
            delta: Some(Delta {
                trust: Some(Dim::Unknown),
                audience: None,
            }),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Cap(DeclaredAudience::literal(Audience::Public))],
                },
                ..Requires::default()
            },
            ..tool("scan")
        }];
        assert!(Registry::build_covered(cfg).is_ok());
    }

    fn binding_sites(parameters: &crate::params::ToolParameters) -> Vec<(&'static str, RegistryConfig)> {
        let binding = || DynamicAudienceBinding {
            resolver: crate::names::DynamicResolverName::new("directory"),
            argument: "to".into(),
        };
        let mut emitter = tool("emit");
        emitter.parameters = parameters.clone();

        let mut placeholder = emitter.clone();
        placeholder.requires.label.audience =
            vec![AudienceRequirement::Includes(RecipientSpec::Placeholder("to".into()))];
        let mut dynamic_includes = emitter.clone();
        dynamic_includes.requires.label.audience =
            vec![AudienceRequirement::Includes(RecipientSpec::Dynamic(binding()))];
        let mut dynamic_delta = emitter;
        dynamic_delta.delta = Some(Delta {
            trust: None,
            audience: Some(AudienceDelta::Dynamic(binding())),
        });

        [
            ("tool emit includes", placeholder),
            ("tool emit includes", dynamic_includes),
            ("tool emit delta", dynamic_delta),
        ]
        .into_iter()
        .map(|(context, tool)| {
            let mut cfg = base();
            cfg.tools = vec![tool];
            (context, cfg)
        })
        .collect()
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
            AudienceRequirement::Cap(DeclaredAudience::literal(Audience::Public)),
        ];
        cfg.tools = vec![emitter];
        assert!(Registry::build_covered(cfg).is_ok());
    }

    #[test]
    fn refuses_label_requirements_on_an_unannotated_tool() {
        use crate::contract::{LabelRequirements, Requires};

        let mut cfg = base();
        cfg.tools = vec![ToolContract {
            delta: None,
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(Trust::new(1)),
                    audience: vec![],
                },
                ..Requires::default()
            },
            ..tool("send")
        }];
        assert!(matches!(
            Registry::build_covered(cfg),
            Err(LoadError::UnannotatedWithLabelRequirement(name)) if name == "send"
        ));

        let mut cfg = base();
        cfg.tools = vec![ToolContract {
            delta: None,
            requires: Requires {
                history: vec![crate::contract::HistoryRequirement::Prior(
                    crate::fact::EffectKind::new("backup"),
                )],
                ..Requires::default()
            },
            ..tool("send")
        }];
        assert!(Registry::build_covered(cfg).is_ok());

        let mut cfg = base();
        cfg.tools = vec![ToolContract {
            delta: Some(Delta::NONE),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(Trust::new(1)),
                    audience: vec![],
                },
                ..Requires::default()
            },
            ..tool("send")
        }];
        assert!(Registry::build_covered(cfg).is_ok());
    }

    #[test]
    fn accepts_constant_cast() {
        let mut cfg = base();
        cfg.casts = vec![Cast {
            name: CastName::new("paranoid"),
            resolution: CastResolution::Constant(DeclaredLabel::literal(EstablishedLabel::new(
                Trust::new(0),
                Audience::Public,
            ))),
            scope: Scope::default(),
        }];
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
        cfg.tools = vec![two_marks];
        cfg.authorities = (0..n).map(|i| attester(format!("a{i}"))).collect();
        cfg
    }

    #[test]
    fn the_default_planner_cap_refuses_an_over_wide_registry_at_sixty_four() {
        assert!(Registry::build_covered(n_squared_config(8)).is_ok());
        assert!(matches!(
            Registry::build_covered(n_squared_config(9)),
            Err(LoadError::TooManyPlanAlternatives { count: 81, max: 64, .. })
        ));
    }

    #[test]
    fn the_alternative_bound_counts_every_sanitizer_for_a_dynamic_output() {
        let mut dynamic = tool("lookup");
        dynamic.parameters = crate::params::test_string_argument_schema("customer");
        dynamic.delta.as_mut().unwrap().audience = Some(AudienceDelta::Dynamic(DynamicAudienceBinding {
            resolver: crate::names::DynamicResolverName::new("directory"),
            argument: "customer".into(),
        }));
        let sanitizer = |index| Sanitizer {
            name: SanitizerName::new(format!("sanitizer-{index}")),
            on: SanitizerPoints {
                input: false,
                output: true,
            },
            transition: DeclaredTransition::Audience {
                from_includes: DeclaredAudience::literal(Audience::Public),
                to: DeclaredAudience::literal(Audience::Public),
            },
            scope: Scope::default(),
            hint: None,
        };
        let mut cfg = base();
        cfg.tools = vec![dynamic];
        cfg.sanitizers = (0..16).map(sanitizer).collect();
        let cap = PlannerCap::new(16).expect("nonzero");
        assert!(matches!(
            Registry::build_covered_with_cap(cfg, cap),
            Err(LoadError::TooManyPlanAlternatives { count: 17, max: 16, .. })
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

    #[test]
    fn the_cap_counts_the_reserved_attest_schema_only_for_the_return_stage() {
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
        narrowing.delta = Some(Delta {
            trust: Some(Dim::Known(Trust::new(0))),
            audience: None,
        });
        let mut cfg = base();
        cfg.tools = vec![narrowing];
        cfg.sanitizers = vec![lifting("attest-schema", Scope::default()), scoped("s1"), scoped("s2")];
        let cap = PlannerCap::new(3).expect("nonzero");
        assert!(Registry::build_covered_with_cap(cfg, cap).is_ok());

        let mut only_attest = base();
        only_attest.sanitizers = vec![lifting("attest-schema", Scope::default())];
        let tight = PlannerCap::new(1).expect("nonzero");
        assert!(matches!(
            Registry::build_covered_with_cap(only_attest, tight),
            Err(LoadError::TooManyReturnPlanAlternatives { count: 2, max: 1 })
        ));
    }

    fn output_sanitizer(index: usize) -> Sanitizer {
        Sanitizer {
            name: SanitizerName::new(format!("sanitizer-{index}")),
            on: SanitizerPoints {
                input: false,
                output: true,
            },
            transition: DeclaredTransition::Audience {
                from_includes: DeclaredAudience::literal(Audience::Public),
                to: DeclaredAudience::literal(Audience::Public),
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
        cfg.tools = tools;
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

    fn cap_target_config(narrowers: usize) -> RegistryConfig {
        let mut target = tool("send");
        target.requires = Requires {
            label: LabelRequirements {
                trust_floor: None,
                audience: vec![AudienceRequirement::Cap(DeclaredAudience::literal(
                    Audience::restricted([ReaderId::new("a")]),
                ))],
            },
            ..Requires::default()
        };
        let mut tools = vec![target];
        for i in 0..narrowers {
            let mut narrower = tool(&format!("narrow{i}"));
            narrower.delta.as_mut().unwrap().audience = Some(AudienceDelta::Static(DeclaredAudience::literal(
                Audience::restricted([ReaderId::new("a"), ReaderId::new("c")]),
            )));
            tools.push(narrower);
        }
        let mut public = tool("public-delta");
        public.delta.as_mut().unwrap().audience =
            Some(AudienceDelta::Static(DeclaredAudience::literal(Audience::Public)));
        let mut dynamic = tool("dynamic-delta");
        dynamic.parameters = crate::params::test_string_argument_schema("to");
        dynamic.delta.as_mut().unwrap().audience = Some(AudienceDelta::Dynamic(DynamicAudienceBinding {
            resolver: crate::names::DynamicResolverName::new("directory"),
            argument: "to".into(),
        }));
        let mut pending = tool("pending-delta");
        pending.delta.as_mut().unwrap().audience = Some(AudienceDelta::PendingCast);
        let neutral = tool("neutral");
        let mut unannotated = tool("unannotated");
        unannotated.delta = None;
        tools.extend([public, dynamic, pending, neutral, unannotated]);
        let mut cfg = base();
        cfg.tools = tools;
        cfg
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

    #[test]
    fn a_vacuous_public_cap_arms_no_redispatch_count() {
        let mut cfg = cap_target_config(4);
        let cap = PlannerCap::new(4).expect("nonzero");
        assert!(matches!(
            Registry::build_covered_with_cap(cfg.clone(), cap),
            Err(LoadError::TooManyPlanAlternatives { count: 5, max: 4, .. })
        ));
        cfg.tools[0].requires.label.audience =
            vec![AudienceRequirement::Cap(DeclaredAudience::literal(Audience::Public))];
        assert!(Registry::build_covered_with_cap(cfg, cap).is_ok());
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
        fixer.delta.as_mut().unwrap().audience = Some(AudienceDelta::Static(DeclaredAudience::literal(
            Audience::restricted([ReaderId::new("a")]),
        )));
        let mut cfg = base();
        cfg.tools = vec![target, fixer];
        assert!(Registry::build_covered_with_cap(cfg, PlannerCap::new(2).expect("nonzero")).is_ok());
    }

    #[test]
    fn families_that_fit_alone_still_refuse_when_their_sum_exceeds_the_cap() {
        let mut cfg = prior_target_config(3);
        cfg.tools[0].requires.label.trust_floor = Some(Trust::new(1));
        let officer = |name: String| Authority {
            name: AuthorityName::new(name),
            mandate: Mandate {
                trust_ceiling: Some(Trust::new(1)),
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        cfg.authorities = (0..3).map(|i| officer(format!("officer{i}"))).collect();
        let cap = PlannerCap::new(5).expect("nonzero");
        assert!(matches!(
            Registry::build_covered_with_cap(cfg, cap),
            Err(LoadError::TooManyPlanAlternatives { count: 6, max: 5, ref tool }) if tool == "wire"
        ));
    }

    #[test]
    fn the_confined_return_stage_is_bounded_even_in_a_zero_tool_catalogue() {
        let mut cfg = base();
        cfg.tools = vec![];
        cfg.sanitizers = (0..5).map(output_sanitizer).collect();
        let cap = PlannerCap::new(4).expect("nonzero");
        assert!(matches!(
            Registry::build_covered_with_cap(cfg.clone(), cap),
            Err(LoadError::TooManyReturnPlanAlternatives { count: 6, max: 4 })
        ));
        assert!(Registry::build_covered_with_cap(cfg, PlannerCap::new(6).expect("nonzero")).is_ok());
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
        untagged.delta = Some(crate::contract::Delta::NONE);
        let mut cfg = base();
        cfg.tools = vec![untagged];
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
            from_includes: DeclaredAudience::literal(Audience::restricted([ReaderId::new("internal")])),
            to: DeclaredAudience::literal(Audience::restricted([ReaderId::new("partner")])),
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
            from_includes: DeclaredAudience::literal(Audience::restricted([ReaderId::new("internal")])),
            to: DeclaredAudience::literal(Audience::restricted([ReaderId::new("partner")])),
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
                from_includes: DeclaredAudience::literal(Audience::restricted([ReaderId::new("internal")])),
                to: DeclaredAudience::literal(Audience::restricted([ReaderId::new("partner")])),
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
            DeclaredAudience::literal(Audience::restricted([ReaderId::new("partner")])),
        ))];
        let with = |sanitizers: Vec<Sanitizer>| {
            let mut cfg = base();
            cfg.tools = vec![target.clone()];
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
        narrowing.delta.as_mut().unwrap().audience = Some(AudienceDelta::Static(DeclaredAudience::literal(
            Audience::restricted([ReaderId::new("a")]),
        )));
        let mut cfg = base();
        cfg.tools = vec![narrowing];
        cfg.sanitizers = (0..5).map(output_sanitizer).collect();
        assert!(Registry::build_covered_with_cap(cfg, PlannerCap::new(8).expect("nonzero")).is_ok());
    }
}
