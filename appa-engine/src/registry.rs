//! The immutable registry: the engine's static capability, built once and validated at load.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::authority::{Authority, Cast, CastResolution, CastTarget, Hint, Sanitizer, Transition};
use crate::contract::{AudienceDelta, AudienceRequirement, RecipientSpec, ToolContract};
use crate::label::{Adequacy, Audience, Dim, Dimension, Trust};
use crate::names::{AuthorityName, CastName, SanitizerName};
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
    #[error("resolver cast {0} declares an empty may_cast ceiling")]
    EmptyCastCeiling(String),
    #[error("trust rank {rank} out of the chain (length {len}) in {context}")]
    RankOutOfChain { rank: u8, len: usize, context: String },
    #[error("tool {0} declares both output dimensions pending-cast (a cast resolves exactly one)")]
    DualPendingCast(String),
    #[error("tool {tool} declares a pending-cast {dimension:?} output and a `requires` on that dimension")]
    PendingCastWithRequirement { tool: String, dimension: Dimension },
    #[error(
        "tool {0} is unannotated (no delta) but declares label requirements: declare its delta (`delta = {{}}` for a deliberately neutral output) so the committed label the requirements check is established"
    )]
    UnannotatedWithLabelRequirement(String),
    #[error(
        "tool {tool}: {count} worst-case alternative remedy assignments exceed the planner cap of {max} — reduce the requirement entries or the competent authorities, or raise `[limits] planner_cap`"
    )]
    TooManyPlanAlternatives { tool: String, count: u128, max: u128 },
    #[error("{context}: hint is {len} characters, over the {max} a plan offer carries")]
    HintTooLong { context: String, len: usize, max: usize },
    #[error(
        "{context}: {reader:?} is not a literal reader ID — `public` names the whole audience, and the `@` mark is reserved for groups a membership resolver expands"
    )]
    NonLiteralReader { context: String, reader: String },
}

/// The planner cap: the most unique grouped authority assignments one block may
/// enumerate — the bound that keeps alternative-plan enumeration total (`RMD-5`: no runtime
/// truncation, "every sound alternative" literal). Deployment configuration sets it via
/// `[limits] planner_cap`; omitted, the cap is 64. Zero is unrepresentable: a tool's worst
/// case is at least one, so a zero cap would refuse every registry with a tool.
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

fn worst_case_plan_alternatives(tool: &ToolContract, authorities: &[Authority], sanitizers: &[Sanitizer]) -> u128 {
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
                .filter(|authority| covers_gap(authority, &gap, &tool.tags))
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
                        .filter(|authority| covers_gap(authority, &gap, &tool.tags))
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
                .filter(|authority| covers_gap(authority, &gap, &tool.tags))
                .count(),
        );
    }
    let output = tool.output_label();
    let applicable = match tool.pending_cast_dim() {
        Some(_) => 0,
        None if matches!(
            tool.delta.as_ref().and_then(|delta| delta.audience.as_ref()),
            Some(AudienceDelta::Dynamic(_))
        ) =>
        {
            sanitizers.iter().filter(|sanitizer| sanitizer.on.output).count()
        }
        None => sanitizers
            .iter()
            .filter(|sanitizer| sanitizer.on.output && sanitizer.transition.admits(&output) == Adequacy::Holds)
            .count(),
    };
    multiply(applicable + 1);
    count
}

#[derive(Clone, Debug)]
pub struct Registry {
    trust_chain: TrustChain,
    tools: BTreeMap<ToolName, ToolContract>,
    authorities: Vec<Authority>,
    sanitizers: BTreeMap<SanitizerName, Sanitizer>,
    casts: BTreeMap<CastName, Cast>,
}

impl Registry {
    pub fn build(config: RegistryConfig) -> Result<Registry, LoadError> {
        Registry::build_with_cap(config, PlannerCap::default())
    }

    pub fn build_with_cap(config: RegistryConfig, planner_cap: PlannerCap) -> Result<Registry, LoadError> {
        config.trust_chain.validate()?;

        // Sanitizers index first: the child return-sanitizer binding validates against them.
        let mut sanitizers = BTreeMap::new();
        for sanitizer in config.sanitizers {
            let context = || format!("sanitizer {}", sanitizer.name.as_str());
            match &sanitizer.transition {
                Transition::Trust { from_floor, to } => {
                    check_rank(&config.trust_chain, Some(*from_floor), || format!("{} from", context()))?;
                    check_rank(&config.trust_chain, Some(*to), || format!("{} to", context()))?;
                }
                Transition::Audience { from_includes, to } => {
                    check_readers(from_includes, || format!("{} from", context()))?;
                    check_readers(to, || format!("{} to", context()))?;
                }
            }
            check_hint(sanitizer.hint.as_ref(), context)?;
            if sanitizers.insert(sanitizer.name.clone(), sanitizer.clone()).is_some() {
                return Err(LoadError::DuplicateSanitizer(sanitizer.name.as_str().to_string()));
            }
        }

        let mut tools = BTreeMap::new();
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
                check_readers(audience, || format!("tool {} delta", tool.name.as_str()))?;
            }
            for requirement in &tool.requires.label.audience {
                match requirement {
                    AudienceRequirement::Includes(RecipientSpec::Static(recipients)) => {
                        check_readers(recipients, || format!("tool {} includes", tool.name.as_str()))?;
                    }
                    AudienceRequirement::Cap(cap) => {
                        check_readers(cap, || format!("tool {} cap", tool.name.as_str()))?;
                    }
                    AudienceRequirement::Includes(RecipientSpec::Placeholder(_) | RecipientSpec::Dynamic(_)) => {}
                }
            }
            validate_pending_cast(&tool)?;
            if tools.insert(tool.name.clone(), tool.clone()).is_some() {
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
                check_readers(ceiling, || {
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

        let sanitizer_list: Vec<Sanitizer> = sanitizers.values().cloned().collect();
        for tool in tools.values() {
            let count = worst_case_plan_alternatives(tool, &config.authorities, &sanitizer_list);
            if count > planner_cap.0 {
                return Err(LoadError::TooManyPlanAlternatives {
                    tool: tool.name.as_str().to_string(),
                    count,
                    max: planner_cap.0,
                });
            }
        }

        let mut casts = BTreeMap::new();
        for cast in config.casts {
            match &cast.resolution {
                CastResolution::Resolver { may_cast } => {
                    if may_cast.is_empty() {
                        return Err(LoadError::EmptyCastCeiling(cast.name.as_str().to_string()));
                    }
                    for rank in &may_cast.trust {
                        check_rank(&config.trust_chain, Some(*rank), || {
                            format!("cast {} may_cast", cast.name.as_str())
                        })?;
                    }
                    if let Some(cap) = &may_cast.audience {
                        check_readers(cap, || format!("cast {} may_cast", cast.name.as_str()))?;
                    }
                }
                CastResolution::Constant(CastTarget::Trust(rank)) => {
                    check_rank(&config.trust_chain, Some(*rank), || {
                        format!("cast {} constant", cast.name.as_str())
                    })?;
                }
                CastResolution::Constant(CastTarget::Audience(audience)) => {
                    check_readers(audience, || format!("cast {} constant", cast.name.as_str()))?;
                }
            }
            if casts.insert(cast.name.clone(), cast.clone()).is_some() {
                return Err(LoadError::DuplicateCast(cast.name.as_str().to_string()));
            }
        }

        Ok(Registry {
            trust_chain: config.trust_chain,
            tools,
            authorities: config.authorities,
            sanitizers,
            casts,
        })
    }

    pub fn trust_chain(&self) -> &TrustChain {
        &self.trust_chain
    }

    pub fn tool(&self, name: &ToolName) -> Option<&ToolContract> {
        self.tools.get(name)
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
        self.casts.get(name)
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

fn check_rank(chain: &TrustChain, rank: Option<Trust>, context: impl Fn() -> String) -> Result<(), LoadError> {
    match rank {
        Some(t) if !chain.contains_rank(t) => Err(LoadError::RankOutOfChain {
            rank: t.rank(),
            len: chain.len(),
            context: context(),
        }),
        _ => Ok(()),
    }
}

fn check_readers(audience: &Audience, context: impl Fn() -> String) -> Result<(), LoadError> {
    let Audience::Restricted(readers) = audience else {
        return Ok(());
    };
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
    use crate::authority::{CastCeiling, CastTarget, Mandate, Scope};
    use crate::contract::{Delta, DynamicAudienceBinding, Requires};
    use crate::fact::EffectSet;
    use crate::label::{Audience, ReaderId};
    use crate::names::{AuthorityName, MarkName};

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
        }
    }

    fn tool(name: &str) -> ToolContract {
        ToolContract {
            name: ToolName::new(name),
            tags: vec![],
            delta: Some(Delta::NONE),
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
            audience: Some(AudienceDelta::Static(named.clone())),
        });
        delta.tools = vec![delta_tool];

        let mut includes = base();
        let mut includes_tool = tool("emit");
        includes_tool.requires.label.audience =
            vec![AudienceRequirement::Includes(RecipientSpec::Static(named.clone()))];
        includes.tools = vec![includes_tool];

        let mut cap = base();
        let mut cap_tool = tool("emit");
        cap_tool.requires.label.audience = vec![AudienceRequirement::Cap(named.clone())];
        cap.tools = vec![cap_tool];

        let mut ceiling = base();
        ceiling.authorities = vec![Authority {
            name: AuthorityName::new("officer"),
            mandate: Mandate {
                reader_ceiling: Some(named.clone()),
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
            hint: None,
        };
        let mut transition_from = base();
        transition_from.sanitizers = vec![sanitizer(Transition::Audience {
            from_includes: named.clone(),
            to: literal.clone(),
        })];
        let mut transition_to = base();
        transition_to.sanitizers = vec![sanitizer(Transition::Audience {
            from_includes: literal,
            to: named.clone(),
        })];

        let cast = |name, resolution| Cast {
            name: CastName::new(name),
            resolution,
        };
        let mut may_cast = base();
        may_cast.casts = vec![cast(
            "classifier",
            CastResolution::Resolver {
                may_cast: CastCeiling {
                    trust: vec![],
                    audience: Some(named.clone()),
                },
            },
        )];
        let mut constant = base();
        constant.casts = vec![cast("paranoid", CastResolution::Constant(CastTarget::Audience(named)))];

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
                match Registry::build(cfg) {
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
        spoiled.requires.label.audience = vec![AudienceRequirement::Cap(Audience::restricted([
            ReaderId::new("ap@corp.example"),
            ReaderId::new("finance"),
            ReaderId::new("public"),
        ]))];
        cfg.tools = vec![spoiled];
        assert!(matches!(
            Registry::build(cfg),
            Err(LoadError::NonLiteralReader { reader, .. }) if reader == "public"
        ));
    }

    #[test]
    fn the_group_mark_is_a_prefix_and_never_a_substring() {
        for (context, cfg) in audience_sites("ap@corp.example") {
            assert!(Registry::build(cfg).is_ok(), "{context} refused an ordinary reader ID");
        }
    }

    #[test]
    fn public_and_the_empty_set_stay_loadable_audiences() {
        let mut public_ceiling = base();
        public_ceiling.authorities = vec![Authority {
            name: AuthorityName::new("officer"),
            mandate: Mandate {
                reader_ceiling: Some(Audience::Public),
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        }];
        assert!(Registry::build(public_ceiling).is_ok());

        let mut empty_cap = base();
        let mut cap_tool = tool("emit");
        cap_tool.requires.label.audience = vec![AudienceRequirement::Cap(Audience::restricted([]))];
        empty_cap.tools = vec![cap_tool];
        assert!(Registry::build(empty_cap).is_ok());
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
        let reg = Registry::build(cfg).unwrap();
        assert!(reg.tool(&ToolName::new("get")).is_some());
        assert!(reg.authority(&AuthorityName::new("officer")).is_some());
    }

    #[test]
    fn refuses_duplicate_tool() {
        let mut cfg = base();
        cfg.tools = vec![tool("dup"), tool("dup")];
        assert!(matches!(
            Registry::build(cfg),
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
            Registry::build(cfg),
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
            Registry::build(cfg),
            Err(LoadError::RankOutOfChain { rank: 9, .. })
        ));
    }

    #[test]
    fn refuses_empty_resolver_ceiling() {
        let mut cfg = base();
        cfg.casts = vec![Cast {
            name: CastName::new("classifier"),
            resolution: CastResolution::Resolver {
                may_cast: CastCeiling::default(),
            },
        }];
        assert!(matches!(
            Registry::build(cfg),
            Err(LoadError::EmptyCastCeiling(name)) if name == "classifier"
        ));
    }

    #[test]
    fn refuses_overlong_trust_chain() {
        let mut cfg = base();
        cfg.trust_chain = TrustChain::new((0..=MAX_RANKS).map(|i| i.to_string()).collect());
        assert!(matches!(
            Registry::build(cfg),
            Err(LoadError::TrustChainTooLong { len, max }) if len == MAX_RANKS + 1 && max == MAX_RANKS
        ));
    }

    #[test]
    fn refuses_duplicate_trust_rank() {
        let mut cfg = base();
        cfg.trust_chain = TrustChain::new(vec!["low".into(), "high".into(), "low".into()]);
        assert!(matches!(
            Registry::build(cfg),
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
            Registry::build(cfg),
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
            Registry::build(cfg),
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
                    audience: vec![AudienceRequirement::Cap(Audience::Public)],
                },
                ..Requires::default()
            },
            ..tool("scan")
        }];
        assert!(matches!(
            Registry::build(cfg),
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
                    audience: vec![AudienceRequirement::Cap(Audience::Public)],
                },
                ..Requires::default()
            },
            ..tool("scan")
        }];
        assert!(Registry::build(cfg).is_ok());
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
            Registry::build(cfg),
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
        assert!(Registry::build(cfg).is_ok());

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
        assert!(Registry::build(cfg).is_ok());
    }

    #[test]
    fn accepts_constant_cast() {
        let mut cfg = base();
        cfg.casts = vec![Cast {
            name: CastName::new("paranoid"),
            resolution: CastResolution::Constant(CastTarget::Trust(Trust::new(0))),
        }];
        assert!(Registry::build(cfg).is_ok());
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
        assert!(Registry::build(n_squared_config(8)).is_ok());
        assert!(matches!(
            Registry::build(n_squared_config(9)),
            Err(LoadError::TooManyPlanAlternatives { count: 81, max: 64, .. })
        ));
    }

    #[test]
    fn the_alternative_bound_counts_every_sanitizer_for_a_dynamic_output() {
        let mut dynamic = tool("lookup");
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
            transition: Transition::Audience {
                from_includes: Audience::Public,
                to: Audience::Public,
            },
            hint: None,
        };
        let mut cfg = base();
        cfg.tools = vec![dynamic];
        cfg.sanitizers = (0..16).map(sanitizer).collect();
        let cap = PlannerCap::new(16).expect("nonzero");
        assert!(matches!(
            Registry::build_with_cap(cfg, cap),
            Err(LoadError::TooManyPlanAlternatives { count: 17, max: 16, .. })
        ));
    }

    #[test]
    fn a_configured_planner_cap_replaces_the_default_bound() {
        let cap = PlannerCap::new(9).expect("nonzero");
        assert!(matches!(
            Registry::build_with_cap(n_squared_config(4), cap),
            Err(LoadError::TooManyPlanAlternatives { count: 16, max: 9, .. })
        ));
        assert!(Registry::build_with_cap(n_squared_config(3), cap).is_ok());

        let raised = PlannerCap::new(100).expect("nonzero");
        assert!(Registry::build_with_cap(n_squared_config(9), raised).is_ok());
    }

    #[test]
    fn a_zero_planner_cap_is_unrepresentable() {
        assert_eq!(PlannerCap::new(0), None);
    }
}
