//! The immutable registry: the engine's static capability, built once and validated at load.
//!
//! [`Registry::build`] indexes the declarations, refuses duplicates, and enforces the load-time
//! invariants the model depends on — chiefly the **no-empty-mandate** rule (an authority covering
//! nothing is a loud error, not a no-op; the empty-remedy proof relies on it). After building, the
//! registry never changes: remedy planning is over exactly this static set.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::authority::{Authority, Cast, CastResolution, CastTarget, Sanitizer};
use crate::contract::ToolContract;
use crate::label::{Adequacy, Dim, Dimension, Trust};
use crate::names::{AuthorityName, CastName, SanitizerName};
use crate::value::ToolName;

/// The deployment's finite trust chain: rank names in ascending order (index = rank).
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

/// The parsed, pre-validation bundle the loader hands to [`Registry::build`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryConfig {
    pub trust_chain: TrustChain,
    pub tools: Vec<ToolContract>,
    pub authorities: Vec<Authority>,
    pub sanitizers: Vec<Sanitizer>,
    pub casts: Vec<Cast>,
}

/// Why a registry failed to load.
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
    #[error(
        "tool {tool}: {count} worst-case alternative remedy assignments exceed the {max} the planner enumerates — reduce the requirement entries or the competent authorities"
    )]
    TooManyPlanAlternatives { tool: String, count: u128, max: u128 },
}

/// The most unique grouped authority assignments one block may enumerate — the bound that keeps
/// alternative-plan enumeration total (no runtime truncation, "every sound alternative" literal).
/// A fixed engine constant, deliberately not a config knob: a policy that trips it has a shape
/// problem (multiplying interchangeable authorities per gap), not a tuning problem.
pub(crate) const MAX_PLAN_ALTERNATIVES: u128 = 16;

/// The worst-case alternative count for one tool: every requirement entry unmet, each choosing
/// independently among its competent authorities. An entry with no competence contributes `1`, not
/// `0` — a state where *its* gap is unmet has no plans at all, but a state missing only the other
/// gaps still multiplies theirs, so zeroing the product would under-count. Duplicate entries (the
/// same mark or effect listed twice) count once, matching the check's canonical deduped gap set.
/// `includes` competence is recipient-dependent (call arguments), upper-bounded by every
/// scope-covering authority holding a reader ceiling. `cap` and `prior` are redispatch species
/// with no covering mandate.
fn worst_case_plan_alternatives(tool: &ToolContract, authorities: &[Authority]) -> u128 {
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
    count
}

/// The validated, indexed, immutable registry.
#[derive(Clone, Debug)]
pub struct Registry {
    trust_chain: TrustChain,
    tools: BTreeMap<ToolName, ToolContract>,
    /// Ordered by registration — routing resolves competent authorities in this order.
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

        // Bound alternative-plan enumeration: per tool, the worst case (every requirement unmet)
        // multiplies each requirement entry's competent-authority count. Static except `includes`,
        // whose recipients are call arguments — upper-bounded by every scope-covering authority
        // with a reader ceiling. Refused at load, so enumeration never truncates at runtime.
        for tool in tools.values() {
            let count = worst_case_plan_alternatives(tool, &config.authorities);
            if count > MAX_PLAN_ALTERNATIVES {
                return Err(LoadError::TooManyPlanAlternatives {
                    tool: tool.name.as_str().to_string(),
                    count,
                    max: MAX_PLAN_ALTERNATIVES,
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

    /// Every registered contract, in name order — the remedy reachability search enumerates these.
    pub fn tools(&self) -> impl Iterator<Item = &ToolContract> {
        self.tools.values()
    }

    /// Authorities in registration order (routing walks them in this order).
    pub fn authorities(&self) -> &[Authority] {
        &self.authorities
    }

    pub fn authority(&self, name: &AuthorityName) -> Option<&Authority> {
        self.authorities.iter().find(|a| &a.name == name)
    }

    pub fn sanitizer(&self, name: &SanitizerName) -> Option<&Sanitizer> {
        self.sanitizers.get(name)
    }

    /// Every registered sanitizer, in name order (deterministic plan enumeration relies on this).
    pub fn sanitizers(&self) -> impl Iterator<Item = &Sanitizer> {
        self.sanitizers.values()
    }

    pub fn cast(&self, name: &CastName) -> Option<&Cast> {
        self.casts.get(name)
    }
}

/// The pending-cast load rules (RP5). One Unknown output dimension at most — a cast resolves
/// exactly one. And never a `requires` on a pending-cast dimension: the check evaluates that
/// dimension as identity (the contribution folds only at admission, resolved), so a requirement on
/// it could be outrun by the call's own unestablished consequences — refused at load instead.
/// An unannotated tool (no delta at all) declares nothing pending, but the same outrun concern
/// applies to **both** dimensions at once: the check evaluates its unestablished contribution as
/// identity while the admitted result folds Unknown, so a label requirement on an unannotated
/// tool could pass on a state its own consequence invalidates. Refused at load, like the
/// pending-cast case — the author declares the delta (`delta = {}` for neutral) first. History
/// and attention requirements are fine: no label dimension is consumed.
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
    if matches!(delta.trust, Some(Dim::Unknown)) && matches!(delta.audience, Some(Dim::Unknown)) {
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

/// The output-sanitizer binding load rules (RP4). The bound name must resolve to a registered
/// `tool_output` sanitizer; the tool's declared raw output must satisfy the transition's `from`
/// (both sides are static, so an inapplicable transition is refused here, never at admission); and
/// the binding cannot pair with a pending-cast output (the two Phase-2 disciplines don't compose).
/// An unannotated tool cannot bind a sanitizer either: its raw output label is Unknown, which
/// never satisfies the `from` — the source-unmet arm below refuses it.
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
    if tool.pending_cast_dim().is_some() {
        return Err(LoadError::OutputSanitizerWithPendingCast(
            tool.name.as_str().to_string(),
        ));
    }
    let raw = tool.output_label();
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
            delta: Some(Delta::NONE),
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
            delta: Some(Delta {
                trust: Some(Dim::Known(Trust::new(9))),
                audience: None,
            }),
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
            delta: Some(Delta {
                trust: Some(Dim::Unknown),
                audience: Some(Dim::Unknown),
            }),
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
            delta: Some(Delta {
                trust: None,
                audience: Some(Dim::Unknown),
            }),
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

        // The other dimension's requirement composes fine with a pending-cast one.
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
            output_sanitizer: None,
            ..tool("scan")
        }];
        assert!(Registry::build(cfg).is_ok());
    }

    #[test]
    fn refuses_label_requirements_on_an_unannotated_tool() {
        use crate::contract::{LabelRequirements, Requires};

        // An unannotated tool's own result folds Unknown after the check ran on identity — a
        // label requirement could be outrun by the call's own consequence, so it is refused at
        // load (the pending-cast rule, applied to the wholly-unestablished case).
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

        // History/attention requirements consume no label dimension: fine without a delta.
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

        // The explicit neutral delta composes with any requirement.
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
            delta: Some(Delta {
                trust: None,
                audience: Some(Dim::Known(internal())),
            }),
            output_sanitizer: Some(SanitizerName::new(sanitizer)),
            ..tool("export")
        };

        // An unregistered binding is refused.
        let mut cfg = base();
        cfg.tools = vec![bound_tool("ghost")];
        assert!(matches!(
            Registry::build(cfg),
            Err(LoadError::UnknownOutputSanitizer { .. })
        ));

        // A binding to an input-only sanitizer is refused.
        let mut cfg = base();
        cfg.sanitizers = vec![sanitizer("input-only", false, internal())];
        cfg.tools = vec![bound_tool("input-only")];
        assert!(matches!(
            Registry::build(cfg),
            Err(LoadError::OutputSanitizerNotOutput { .. })
        ));

        // A binding whose `from` the declared raw output cannot satisfy is refused at load — both
        // sides are static, so the inapplicable transition never reaches admission.
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

        // A binding on a pending-cast output is refused (the two disciplines don't compose).
        let mut cfg = base();
        cfg.sanitizers = vec![sanitizer("declassify", true, internal())];
        cfg.tools = vec![ToolContract {
            delta: Some(Delta {
                trust: Some(Dim::Unknown),
                audience: Some(Dim::Known(internal())),
            }),
            ..bound_tool("declassify")
        }];
        assert!(matches!(
            Registry::build(cfg),
            Err(LoadError::OutputSanitizerWithPendingCast(name)) if name == "export"
        ));

        // The applicable binding loads.
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

    #[test]
    fn the_alternative_plan_bound_refuses_an_over_wide_registry() {
        // Two attention marks, each attended by N interchangeable authorities: N² worst-case
        // assignments. 4 authorities (16) sits exactly on the bound; 5 (25) is refused at load —
        // so runtime enumeration is total, never truncated.
        let two_marks = |name: &str| {
            let mut t = tool(name);
            t.requires = Requires {
                attention: vec![MarkName::new("m1"), MarkName::new("m2")],
                ..Requires::default()
            };
            t
        };
        let attester = |name: String| Authority {
            name: AuthorityName::new(name),
            mandate: Mandate {
                attends: vec![MarkName::new("m1"), MarkName::new("m2")],
                ..Mandate::default()
            },
            scope: Scope::default(),
        };
        let mut cfg = base();
        cfg.tools = vec![two_marks("wire")];
        cfg.authorities = (0..4).map(|i| attester(format!("a{i}"))).collect();
        assert!(Registry::build(cfg).is_ok());

        let mut cfg = base();
        cfg.tools = vec![two_marks("wire")];
        cfg.authorities = (0..5).map(|i| attester(format!("a{i}"))).collect();
        assert!(matches!(
            Registry::build(cfg),
            Err(LoadError::TooManyPlanAlternatives { count: 25, max: 16, .. })
        ));
    }
}
