//! The immutable registry: the engine's static capability, built once and validated at load.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::authority::{Authority, Cast, CastResolution, CastTarget, Sanitizer};
use crate::contract::ToolContract;
use crate::label::{Adequacy, Dim, Dimension, Trust};
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
    #[error("tool {tool} binds output sanitizer {sanitizer}, which is not registered")]
    UnknownOutputSanitizer { tool: String, sanitizer: String },
    #[error("tool {tool} binds {sanitizer}, which is not registered for tool output")]
    OutputSanitizerNotOutput { tool: String, sanitizer: String },
    #[error(
        "tool {0} binds an output sanitizer and declares a pending-cast output (the two Phase-2 disciplines do not compose)"
    )]
    OutputSanitizerWithPendingCast(String),
    #[error("tool {tool}'s declared raw output does not satisfy sanitizer {sanitizer}'s `from` precondition")]
    OutputSanitizerSourceUnmet { tool: String, sanitizer: String },
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
        config.trust_chain.validate()?;

        // Sanitizers index first: tool output-sanitizer bindings validate against them.
        let mut sanitizers = BTreeMap::new();
        for sanitizer in config.sanitizers {
            if sanitizers.insert(sanitizer.name.clone(), sanitizer.clone()).is_some() {
                return Err(LoadError::DuplicateSanitizer(sanitizer.name.as_str().to_string()));
            }
        }

        let mut tools = BTreeMap::new();
        for tool in config.tools {
            let declared_trust = match &tool.delta.trust {
                Some(Dim::Known(t)) => Some(*t),
                Some(Dim::Unknown) | None => None,
            };
            check_rank(&config.trust_chain, declared_trust, || {
                format!("tool {} delta", tool.name.as_str())
            })?;
            check_rank(&config.trust_chain, tool.requires.label.trust_floor, || {
                format!("tool {} trust floor", tool.name.as_str())
            })?;
            validate_pending_cast(&tool)?;
            validate_output_binding(&tool, &sanitizers)?;
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
            if seen_authorities.insert(authority.name.clone(), ()).is_some() {
                return Err(LoadError::DuplicateAuthority(authority.name.as_str().to_string()));
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
                }
                CastResolution::Constant(CastTarget::Trust(rank)) => {
                    check_rank(&config.trust_chain, Some(*rank), || {
                        format!("cast {} constant", cast.name.as_str())
                    })?;
                }
                CastResolution::Constant(CastTarget::Audience(_)) => {}
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

    pub fn cast(&self, name: &CastName) -> Option<&Cast> {
        self.casts.get(name)
    }
}

fn validate_pending_cast(tool: &ToolContract) -> Result<(), LoadError> {
    if matches!(tool.delta.trust, Some(Dim::Unknown)) && matches!(tool.delta.audience, Some(Dim::Unknown)) {
        return Err(LoadError::DualPendingCast(tool.name.as_str().to_string()));
    }
    match tool.delta.pending_cast_dim() {
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

fn validate_output_binding(
    tool: &ToolContract,
    sanitizers: &BTreeMap<SanitizerName, Sanitizer>,
) -> Result<(), LoadError> {
    let Some(name) = &tool.output_sanitizer else {
        return Ok(());
    };
    let sanitizer = sanitizers.get(name).ok_or_else(|| LoadError::UnknownOutputSanitizer {
        tool: tool.name.as_str().to_string(),
        sanitizer: name.as_str().to_string(),
    })?;
    if !sanitizer.on.output {
        return Err(LoadError::OutputSanitizerNotOutput {
            tool: tool.name.as_str().to_string(),
            sanitizer: name.as_str().to_string(),
        });
    }
    if tool.delta.pending_cast_dim().is_some() {
        return Err(LoadError::OutputSanitizerWithPendingCast(
            tool.name.as_str().to_string(),
        ));
    }
    let raw = tool.delta.output_label();
    if raw.audience.covers(&sanitizer.can_reduce.from_includes) != Adequacy::Holds {
        return Err(LoadError::OutputSanitizerSourceUnmet {
            tool: tool.name.as_str().to_string(),
            sanitizer: name.as_str().to_string(),
        });
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::{CastCeiling, CastTarget, Mandate, Scope};
    use crate::contract::{Delta, Requires};
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
            delta: Delta::NONE,
            emits: vec![],
            requires: Requires::default(),
            output_sanitizer: None,
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
        }
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
            delta: Delta {
                trust: Some(Dim::Known(Trust::new(9))),
                audience: None,
            },
            output_sanitizer: None,
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
            delta: Delta {
                trust: Some(Dim::Unknown),
                audience: Some(Dim::Unknown),
            },
            output_sanitizer: None,
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
            delta: Delta {
                trust: Some(Dim::Unknown),
                audience: None,
            },
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(Trust::new(1)),
                    audience: vec![],
                },
                ..Requires::default()
            },
            output_sanitizer: None,
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
            delta: Delta {
                trust: None,
                audience: Some(Dim::Unknown),
            },
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Cap(Audience::Public)],
                },
                ..Requires::default()
            },
            output_sanitizer: None,
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
            delta: Delta {
                trust: Some(Dim::Unknown),
                audience: None,
            },
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Cap(Audience::Public)],
                },
                ..Requires::default()
            },
            output_sanitizer: None,
            ..tool("scan")
        }];
        assert!(Registry::build(cfg).is_ok());
    }

    #[test]
    fn validates_the_output_sanitizer_binding() {
        use crate::authority::{AudienceTransition, SanitizerPoints};
        use crate::label::{Audience, ReaderId};

        let sanitizer = |name: &str, output: bool, from: Audience| Sanitizer {
            name: SanitizerName::new(name),
            on: SanitizerPoints { input: !output, output },
            can_reduce: AudienceTransition {
                from_includes: from,
                to: Audience::Public,
            },
        };
        let internal = || Audience::restricted([ReaderId::new("internal")]);
        let bound_tool = |sanitizer: &str| ToolContract {
            delta: Delta {
                trust: None,
                audience: Some(Dim::Known(internal())),
            },
            output_sanitizer: Some(SanitizerName::new(sanitizer)),
            ..tool("export")
        };

        let mut cfg = base();
        cfg.tools = vec![bound_tool("ghost")];
        assert!(matches!(
            Registry::build(cfg),
            Err(LoadError::UnknownOutputSanitizer { .. })
        ));

        let mut cfg = base();
        cfg.sanitizers = vec![sanitizer("input-only", false, internal())];
        cfg.tools = vec![bound_tool("input-only")];
        assert!(matches!(
            Registry::build(cfg),
            Err(LoadError::OutputSanitizerNotOutput { .. })
        ));

        let mut cfg = base();
        cfg.sanitizers = vec![sanitizer(
            "finance-only",
            true,
            Audience::restricted([ReaderId::new("finance")]),
        )];
        cfg.tools = vec![bound_tool("finance-only")];
        assert!(matches!(
            Registry::build(cfg),
            Err(LoadError::OutputSanitizerSourceUnmet { .. })
        ));

        let mut cfg = base();
        cfg.sanitizers = vec![sanitizer("declassify", true, internal())];
        cfg.tools = vec![ToolContract {
            delta: Delta {
                trust: Some(Dim::Unknown),
                audience: Some(Dim::Known(internal())),
            },
            ..bound_tool("declassify")
        }];
        assert!(matches!(
            Registry::build(cfg),
            Err(LoadError::OutputSanitizerWithPendingCast(name)) if name == "export"
        ));

        let mut cfg = base();
        cfg.sanitizers = vec![sanitizer("declassify", true, internal())];
        cfg.tools = vec![bound_tool("declassify")];
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
}
