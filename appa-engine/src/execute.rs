//! The mandate envelope: what an authority's approval may admit.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::check::{Gap, UnestablishedFact};
use crate::groups::Expansions;
use crate::label::PartialLabel;
use crate::names::AuthorityName;
use crate::plan::covers_gap;
use crate::registry::Registry;

/// One authority's approval of the exact canonical call an offer names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityEvidence {
    pub offer: crate::value::OfferId,
    pub authority: AuthorityName,
    pub covers: Vec<Gap>,
    pub reviewed: AuthorityReview,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityReview {
    pub tool: crate::value::ToolName,
    pub trajectory_label: PartialLabel,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PlanError {
    #[error("the call has an unestablished dimension — a fact clears it, never a plan")]
    Unestablished(Vec<UnestablishedFact>),
    #[error("no authority registered as {0}")]
    UnknownAuthority(String),
    #[error("a ruling claims a gap the current block does not carry")]
    RulingClaimsAbsentGap(Gap),
    #[error("requirement gap not covered by any supplied ruling")]
    GapUncovered(Gap),
    #[error("a ruling by {authority} claims a gap its mandate does not cover")]
    RulingExceedsMandate { authority: String },
    #[error("the supplied rulings do not realize the chosen plan's grouped assignment exactly")]
    RulingAssignmentMismatch,
    #[error("a ruling's recorded review does not match the live state it would admit")]
    ReviewMismatch,
    #[error("this authority response was approved for a different offer")]
    EvidenceOfferMismatch,
}

/// The mandate envelope of a released block: no ruling claims a gap the block
/// does not carry or its authority's mandate does not reach, and every requirement gap is claimed
/// by one that does. Shared by live execution and by the transition validator, so the envelope a
/// persisted release is held to is the one the live path enforced.
pub(crate) fn rulings_cover<'a>(
    registry: &Registry,
    contract: &crate::contract::ToolContract,
    block: &crate::check::RawBlock,
    rulings: impl Iterator<Item = (&'a AuthorityName, &'a [Gap])> + Clone,
    expansions: &Expansions,
) -> Result<(), PlanError> {
    for (authority, covers) in rulings.clone() {
        let registered = registry
            .authority(authority)
            .ok_or_else(|| PlanError::UnknownAuthority(authority.as_str().to_string()))?;
        for gap in covers {
            if !block.requirement_gaps.contains(gap) {
                return Err(PlanError::RulingClaimsAbsentGap(gap.clone()));
            }
            if !covers_gap(registered, gap, &contract.tags, expansions) {
                return Err(PlanError::RulingExceedsMandate {
                    authority: authority.as_str().to_string(),
                });
            }
        }
    }
    for gap in &block.requirement_gaps {
        if !rulings.clone().any(|(_, covers)| covers.contains(gap)) {
            return Err(PlanError::GapUncovered(gap.clone()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::{Authority, Mandate, Scope};
    use crate::contract::{Delta, LabelRequirements, Requires, ToolContract};
    use crate::fact::EffectSet;
    use crate::label::Trust;
    use crate::names::MarkName;
    use crate::value::ToolName;

    const SUSPICIOUS: Trust = Trust::new(0);
    const TRUSTED: Trust = Trust::new(1);

    fn chain() -> crate::registry::TrustChain {
        crate::registry::TrustChain::new(vec!["suspicious".into(), "trusted".into()])
    }

    fn floor_gap() -> Gap {
        Gap::TrustFloor {
            required: TRUSTED,
            actual: SUSPICIOUS,
        }
    }

    fn wire() -> ToolContract {
        ToolContract {
            description: Some("A test tool.".to_string()),
            uses: vec![],
            name: ToolName::new("wire"),
            tags: vec![],
            delta: Some(Delta::NONE),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(TRUSTED),
                    audience: vec![],
                },
                ..Requires::default()
            },
        }
    }

    fn registry() -> Registry {
        let officer = Authority {
            name: AuthorityName::new("officer"),
            mandate: Mandate {
                trust_ceiling: Some(TRUSTED),
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        let attester = Authority {
            name: AuthorityName::new("attester"),
            mandate: Mandate {
                attends: vec![MarkName::new("signoff")],
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        Registry::build_covered(crate::registry::RegistryConfig {
            trust_chain: chain(),
            tools: vec![wire()],
            authorities: vec![officer, attester],
            sanitizers: vec![],
            casts: vec![],
            membership: None,
        })
        .unwrap()
    }

    fn block(gaps: Vec<Gap>) -> crate::check::RawBlock {
        crate::check::RawBlock {
            requirement_gaps: gaps,
            narrowing: None,
            unestablished: vec![],
        }
    }

    fn envelope(authority: &str, covers: &[Gap], block: &crate::check::RawBlock) -> Result<(), PlanError> {
        let registry = registry();
        let name = AuthorityName::new(authority);
        rulings_cover(
            &registry,
            registry.tool(&ToolName::new("wire")).unwrap(),
            block,
            [(&name, covers)].into_iter(),
            &Expansions::default(),
        )
    }

    #[test]
    fn a_mandate_that_reaches_the_gap_admits_it() {
        assert_eq!(envelope("officer", &[floor_gap()], &block(vec![floor_gap()])), Ok(()));
    }

    #[test]
    fn a_gap_no_ruling_claims_is_refused() {
        let registry = registry();
        let refused = rulings_cover(
            &registry,
            registry.tool(&ToolName::new("wire")).unwrap(),
            &block(vec![floor_gap()]),
            std::iter::empty(),
            &Expansions::default(),
        );
        assert_eq!(refused, Err(PlanError::GapUncovered(floor_gap())));
    }

    #[test]
    fn a_ruling_claiming_a_gap_the_block_does_not_carry_is_refused() {
        assert_eq!(
            envelope("officer", &[floor_gap()], &block(vec![])),
            Err(PlanError::RulingClaimsAbsentGap(floor_gap()))
        );
    }

    #[test]
    fn a_ruling_outside_its_authoritys_mandate_is_refused() {
        assert_eq!(
            envelope("attester", &[floor_gap()], &block(vec![floor_gap()])),
            Err(PlanError::RulingExceedsMandate {
                authority: "attester".to_string()
            })
        );
    }

    #[test]
    fn a_ruling_by_an_unregistered_authority_is_refused() {
        assert_eq!(
            envelope("ghost", &[floor_gap()], &block(vec![floor_gap()])),
            Err(PlanError::UnknownAuthority("ghost".to_string()))
        );
    }
}
