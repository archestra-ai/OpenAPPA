//! Atomic plan execution: turning gathered rulings into the one indivisible batch that admits a
//! blocked dispatch.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::check::{Gap, UnestablishedFact};
use crate::label::PartialLabel;
use crate::names::AuthorityName;
use crate::plan::covers_gap;
use crate::registry::Registry;
use crate::value::DispatchId;
#[cfg(test)]
use crate::{
    candidate::CallStage,
    check::{self, CheckOutcome},
    engine::opened_dispatch,
    fact::Fact,
    plan,
    projection::Views,
    value::ResolvedCall,
};

/// A ruling the runtime gathered from an authority for one **specific pending dispatch**: the exact
/// [`DispatchId`] (trajectory + canonical digest + occurrence) it was approved for, the mandate it
/// acts under, who exercised it, the gaps it claims to cover, and the review it was issued over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ruling {
    pub dispatch: DispatchId,
    pub authority: AuthorityName,
    pub covers: Vec<Gap>,
    pub reviewed: AuthorityReview,
}

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
    #[error("no contract registered for tool {0}")]
    UnknownTool(String),
    #[error("tool {0} is provider-run: no executor of this deployment can run a proposed call naming it")]
    ProviderRunTool(String),
    #[error("invalid call: {0}")]
    InvalidCall(crate::params::ArgumentError),
    #[error("the call is not blocked — dispatch it directly")]
    NotBlocked,
    #[error("the branch already ended its errand")]
    BranchEnded,
    #[error("the call has an unestablished dimension — a fact clears it, never a plan")]
    Unestablished(Vec<UnestablishedFact>),
    #[error("no plan {0} is offered for this block")]
    UnknownPlan(u32),
    #[error("a ruling was approved for a different dispatch (call or occurrence)")]
    RulingCallMismatch,
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
) -> Result<(), PlanError> {
    for (authority, covers) in rulings.clone() {
        let registered = registry
            .authority(authority)
            .ok_or_else(|| PlanError::UnknownAuthority(authority.as_str().to_string()))?;
        for gap in covers {
            if !block.requirement_gaps.contains(gap) {
                return Err(PlanError::RulingClaimsAbsentGap(gap.clone()));
            }
            if !covers_gap(registered, gap, &contract.tags) {
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

/// Execute a remedy plan: verify coverage, then emit the atomic
/// rulings + acceptance + dispatch batch. See the module docs.
#[cfg(test)]
pub(crate) fn execute_remedy_plan(
    registry: &Registry,
    views: &Views,
    chosen: &plan::ExecutableRemedyPlan,
    call: &ResolvedCall,
    rulings: &[Ruling],
) -> Result<Vec<Fact>, PlanError> {
    let contract = registry
        .tool(call.tool())
        .ok_or_else(|| PlanError::UnknownTool(call.tool().as_str().to_string()))?;
    contract
        .parameters
        .validate(call.arguments())
        .map_err(PlanError::InvalidCall)?;

    let block = match check::evaluate(contract, views, call, &CallStage::default()) {
        CheckOutcome::Block(block) => block,
        CheckOutcome::Allow => return Err(PlanError::NotBlocked),
    };
    if !block.unestablished.is_empty() {
        return Err(PlanError::Unestablished(block.unestablished));
    }

    let planned = plan::plan(
        registry,
        views,
        call,
        &block,
        &CallStage::default(),
        plan::CallRole::Ordinary,
    );
    if !planned
        .plans
        .iter()
        .filter_map(plan::RemedyPlan::executable)
        .any(|offered| offered == chosen)
    {
        return Err(PlanError::UnknownPlan(chosen.id.value()));
    }
    let plan = chosen.id;

    if rulings.len() != chosen.required.len() {
        return Err(PlanError::RulingAssignmentMismatch);
    }
    for required in &chosen.required {
        let matched = rulings
            .iter()
            .filter(|ruling| ruling.authority == required.authority && ruling.covers == required.covers)
            .count();
        if matched != 1 {
            return Err(PlanError::RulingAssignmentMismatch);
        }
    }

    let live_label = views.current_label();
    for ruling in rulings {
        if ruling.reviewed.tool != contract.name || ruling.reviewed.trajectory_label != live_label {
            return Err(PlanError::ReviewMismatch);
        }
    }

    let (dispatch, dispatch_opened) = opened_dispatch(contract, views, call, None);

    // Each ruling must be scoped to this exact dispatch.
    for ruling in rulings {
        if ruling.dispatch != dispatch {
            return Err(PlanError::RulingCallMismatch);
        }
    }
    rulings_cover(
        registry,
        contract,
        &block,
        rulings
            .iter()
            .map(|ruling| (&ruling.authority, ruling.covers.as_slice())),
    )?;

    let trajectory = views.trajectory().clone();

    let mut facts = Vec::new();
    let legacy_residual = chosen
        .sanitizer()
        .and_then(|name| crate::plan::predicted_residual(registry, contract, call, name, &views.current_label()));
    if let Some(narrowing) = chosen.narrowing().cloned().or(legacy_residual) {
        facts.push(Fact::Acceptance {
            trajectory: trajectory.clone(),
            dispatch: dispatch.clone(),
            plan,
            narrowing,
        });
    }
    for required in &chosen.required {
        let ruling = rulings
            .iter()
            .find(|ruling| ruling.authority == required.authority && ruling.covers == required.covers)
            .expect("the assignment check matched each required entry to exactly one ruling");
        facts.push(Fact::Ruling {
            trajectory: trajectory.clone(),
            dispatch: dispatch.clone(),
            plan,
            authority: ruling.authority.clone(),
            covers: ruling.covers.clone(),
            reviewed: ruling.reviewed.clone(),
        });
    }
    if let Some(sanitizer) = chosen.sanitizer() {
        facts.push(Fact::OutputSanitizerBound {
            trajectory: trajectory.clone(),
            dispatch: dispatch.clone(),
            plan,
            sanitizer: sanitizer.clone(),
            contribution: crate::plan::bound_contribution(registry, contract, call, sanitizer)
                .expect("the matched plan binds an output sanitizer enumeration found applicable"),
        });
    }
    facts.push(dispatch_opened);

    Ok(facts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::{Authority, Mandate, Scope};
    use crate::contract::{Delta, LabelRequirements, Requires, ToolContract};
    use crate::fact::{EffectKind, EffectSet, Fact};
    use crate::label::{Audience, Dim, EstablishedLabel, Label, ReaderId, Trust};
    use crate::names::{MarkName, SanitizerName};
    use crate::projection::Projection;
    use crate::value::{LabeledValue, Provenance, ToolName, TrajectoryId, ValueBody};
    use serde_json::json;

    const SUSPICIOUS: Trust = Trust::new(0);
    const TRUSTED: Trust = Trust::new(1);

    fn traj() -> TrajectoryId {
        TrajectoryId::new("t")
    }

    fn chain() -> crate::registry::TrustChain {
        crate::registry::TrustChain::new(vec!["suspicious".into(), "trusted".into()])
    }

    fn user_value(label: Label) -> Fact {
        Fact::ValueAdmitted {
            trajectory: traj(),
            value: LabeledValue::new(ValueBody::new("body"), label),
            provenance: Provenance::UserInput,
        }
    }

    fn known(trust: Trust, audience: Audience) -> Label {
        Label::new(Dim::Known(trust), Dim::Known(audience))
    }

    fn established(trust: Trust, audience: Audience) -> EstablishedLabel {
        EstablishedLabel::new(trust, audience)
    }

    fn partial(trust: Trust, audience: Audience) -> PartialLabel {
        PartialLabel::established(EstablishedLabel::new(trust, audience))
    }

    fn registry() -> Registry {
        let wire = ToolContract {
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
        };
        let officer = Authority {
            name: AuthorityName::new("officer"),
            mandate: Mandate {
                trust_ceiling: Some(TRUSTED),
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        Registry::build_covered(crate::registry::RegistryConfig {
            trust_chain: chain(),
            tools: vec![wire],
            authorities: vec![officer],
            sanitizers: vec![],
            casts: vec![],
            membership: None,
        })
        .unwrap()
    }

    fn call(tool: &str, args: serde_json::Value) -> ResolvedCall {
        ResolvedCall::new(ToolName::new(tool), crate::params::test_arguments(&args))
    }

    fn floor_gap() -> Gap {
        Gap::TrustFloor {
            required: TRUSTED,
            actual: SUSPICIOUS,
        }
    }

    fn top_review() -> AuthorityReview {
        AuthorityReview {
            tool: ToolName::new("wire"),
            trajectory_label: partial(SUSPICIOUS, Audience::Public),
        }
    }

    fn wire_dispatch() -> DispatchId {
        DispatchId::new(traj(), call("wire", json!({})).digest(), 0)
    }

    fn run(registry: &Registry, log: &[Fact], call: &ResolvedCall, rulings: &[Ruling]) -> Result<Vec<Fact>, PlanError> {
        let projection = Projection::build(log, log.len() as u64);
        let trajectory = traj();
        let views = projection.view(&trajectory);
        let chosen = offered_plan(registry, &views, call);
        execute_remedy_plan(registry, &views, &chosen, call, rulings)
    }

    fn offered_plan(registry: &Registry, views: &Views, call: &ResolvedCall) -> plan::ExecutableRemedyPlan {
        let planned = match check::evaluate(registry.tool(call.tool()).unwrap(), views, call, &CallStage::default()) {
            CheckOutcome::Block(block) => plan::plan(
                registry,
                views,
                call,
                &block,
                &CallStage::default(),
                plan::CallRole::Ordinary,
            ),
            _ => {
                return plan::ExecutableRemedyPlan {
                    id: plan::PlanId::new(0),
                    steps: vec![],
                    required: vec![],
                };
            }
        };
        planned
            .plans
            .iter()
            .filter_map(plan::RemedyPlan::executable)
            .next()
            .cloned()
            .unwrap_or(plan::ExecutableRemedyPlan {
                id: plan::PlanId::new(0),
                steps: vec![],
                required: vec![],
            })
    }

    #[test]
    fn ruling_admits_the_blocked_dispatch_atomically() {
        let registry = registry();
        let log = vec![user_value(known(SUSPICIOUS, Audience::Public))];
        let ruling = Ruling {
            dispatch: wire_dispatch(),
            authority: AuthorityName::new("officer"),
            reviewed: top_review(),
            covers: vec![floor_gap()],
        };
        let batch = run(&registry, &log, &call("wire", json!({})), &[ruling]).unwrap();
        assert_eq!(batch.len(), 2);
        assert!(matches!(batch[0], Fact::Ruling { .. }));
        assert!(matches!(batch[1], Fact::DispatchOpened { .. }));
    }

    #[test]
    fn a_waiver_executes_over_a_reservation_failed_no_prior() {
        let guard = ToolContract {
            name: ToolName::new("guard"),
            tags: vec![],
            delta: Some(Delta::NONE),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                history: vec![crate::contract::HistoryRequirement::NoPrior(EffectKind::new(
                    "email.sent",
                ))],
                ..Requires::default()
            },
        };
        let keeper = Authority {
            name: AuthorityName::new("keeper"),
            mandate: Mandate {
                waivers: vec![EffectKind::new("email.sent")],
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        let registry = Registry::build_covered(crate::registry::RegistryConfig {
            trust_chain: chain(),
            tools: vec![guard],
            authorities: vec![keeper],
            sanitizers: vec![],
            casts: vec![],
            membership: None,
        })
        .unwrap();
        let seed = call("send", json!({}));
        let log = vec![
            user_value(known(TRUSTED, Audience::Public)),
            Fact::DispatchOpened {
                trajectory: traj(),
                dispatch: DispatchId::new(traj(), seed.digest(), 0),
                tool: seed.tool().clone(),
                arguments: seed.canonical_arguments().clone(),
                proposed_label: established(TRUSTED, Audience::Public),
                receiving: established(TRUSTED, Audience::Public),
                proposed_effects: EffectSet::new([EffectKind::new("email.sent")]).unwrap(),
                dynamic_resolutions: vec![],
                memberships: Vec::new(),
                subject: None,
            },
        ];
        let guard_call = call("guard", json!({}));
        let ruling = Ruling {
            dispatch: DispatchId::new(traj(), guard_call.digest(), 0),
            authority: AuthorityName::new("keeper"),
            reviewed: AuthorityReview {
                tool: ToolName::new("guard"),
                trajectory_label: partial(TRUSTED, Audience::Public),
            },
            covers: vec![Gap::NoPrior(EffectKind::new("email.sent"))],
        };
        let batch = run(&registry, &log, &guard_call, &[ruling]).unwrap();
        assert_eq!(batch.len(), 2);
        assert!(matches!(batch[0], Fact::Ruling { .. }));
        assert!(matches!(batch[1], Fact::DispatchOpened { .. }));
    }

    #[test]
    fn a_valid_ruling_cannot_clear_an_unestablished_fact() {
        let vault = ToolContract {
            name: ToolName::new("vault"),
            tags: vec![],
            delta: Some(Delta::NONE),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(TRUSTED),
                    audience: vec![],
                },
                attention: vec![MarkName::new("signoff")],
                ..Requires::default()
            },
        };
        let steward = Authority {
            name: AuthorityName::new("steward"),
            mandate: Mandate {
                attends: vec![MarkName::new("signoff")],
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        let registry = Registry::build_covered(crate::registry::RegistryConfig {
            trust_chain: chain(),
            tools: vec![vault],
            authorities: vec![steward],
            sanitizers: vec![],
            casts: vec![],
            membership: None,
        })
        .unwrap();
        let log = vec![
            user_value(known(SUSPICIOUS, Audience::Public)),
            user_value(Label::new(Dim::Unknown, Dim::Known(Audience::Public))),
        ];
        let vault_call = call("vault", json!({}));
        let mut reviewed_label = partial(SUSPICIOUS, Audience::Public);
        reviewed_label.fold_value(
            crate::value::ValueId::new(1),
            &Label::new(Dim::Unknown, Dim::Known(Audience::Public)),
        );
        let ruling = Ruling {
            dispatch: DispatchId::new(traj(), vault_call.digest(), 0),
            authority: AuthorityName::new("steward"),
            reviewed: AuthorityReview {
                tool: ToolName::new("vault"),
                trajectory_label: reviewed_label,
            },
            covers: vec![Gap::Attention(MarkName::new("signoff"))],
        };
        match run(&registry, &log, &vault_call, &[ruling]) {
            Err(PlanError::Unestablished(facts)) => {
                assert_eq!(
                    facts,
                    vec![UnestablishedFact {
                        value: crate::value::ValueId::new(1),
                        dimensions: std::collections::BTreeSet::from([crate::label::Dimension::Trust]),
                    }]
                );
            }
            other => panic!("expected the unestablished refusal, got {other:?}"),
        }
    }

    #[test]
    fn ruling_approved_for_another_call_is_rejected() {
        let registry = registry();
        let log = vec![user_value(known(SUSPICIOUS, Audience::Public))];
        let ruling = Ruling {
            dispatch: DispatchId::new(traj(), call("wire", json!({ "to": "elsewhere" })).digest(), 0),
            authority: AuthorityName::new("officer"),
            reviewed: top_review(),
            covers: vec![floor_gap()],
        };
        assert_eq!(
            run(&registry, &log, &call("wire", json!({})), std::slice::from_ref(&ruling)),
            Err(PlanError::RulingCallMismatch)
        );
    }

    #[test]
    fn ruling_cannot_replay_across_occurrences() {
        let registry = registry();
        let wire = call("wire", json!({}));
        let prior = DispatchId::new(traj(), wire.digest(), 0);
        let log = vec![
            user_value(known(SUSPICIOUS, Audience::Public)),
            Fact::DispatchOpened {
                trajectory: traj(),
                dispatch: prior,
                tool: wire.tool().clone(),
                arguments: wire.canonical_arguments().clone(),
                proposed_label: EstablishedLabel::top(),
                receiving: EstablishedLabel::top(),
                proposed_effects: EffectSet::default(),
                dynamic_resolutions: Vec::new(),
                memberships: Vec::new(),
                subject: None,
            },
        ];
        let stale = Ruling {
            dispatch: wire_dispatch(),
            authority: AuthorityName::new("officer"),
            reviewed: top_review(),
            covers: vec![floor_gap()],
        };
        assert_eq!(
            run(&registry, &log, &wire, std::slice::from_ref(&stale)),
            Err(PlanError::RulingCallMismatch)
        );
    }

    #[test]
    fn plan_id_not_offered_is_rejected() {
        let registry = registry();
        let log = vec![user_value(known(SUSPICIOUS, Audience::Public))];
        let ruling = Ruling {
            dispatch: wire_dispatch(),
            authority: AuthorityName::new("officer"),
            reviewed: top_review(),
            covers: vec![floor_gap()],
        };
        let projection = Projection::build(&log, log.len() as u64);
        let trajectory = traj();
        let fabricated = plan::ExecutableRemedyPlan {
            id: plan::PlanId::new(999),
            steps: vec![plan::RemedyStep::Authorize(AuthorityName::new("officer"))],
            required: vec![plan::RequiredRuling {
                authority: AuthorityName::new("officer"),
                covers: vec![],
            }],
        };
        assert_eq!(
            execute_remedy_plan(
                &registry,
                &projection.view(&trajectory),
                &fabricated,
                &call("wire", json!({})),
                std::slice::from_ref(&ruling)
            ),
            Err(PlanError::UnknownPlan(999))
        );
    }

    #[test]
    fn uncovered_gap_is_rejected() {
        let registry = registry();
        let log = vec![user_value(known(SUSPICIOUS, Audience::Public))];
        assert!(matches!(
            run(&registry, &log, &call("wire", json!({})), &[]),
            Err(PlanError::RulingAssignmentMismatch)
        ));
    }

    #[test]
    fn ruling_gathered_for_a_different_call_does_not_transfer() {
        let registry = registry();
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let ruling = Ruling {
            dispatch: wire_dispatch(),
            authority: AuthorityName::new("officer"),
            reviewed: top_review(),
            covers: vec![floor_gap()],
        };
        assert_eq!(
            run(&registry, &log, &call("wire", json!({})), &[ruling]),
            Err(PlanError::NotBlocked)
        );
    }

    #[test]
    fn ruling_exceeding_its_mandate_is_rejected() {
        let attends_only = Authority {
            name: AuthorityName::new("attester"),
            mandate: Mandate {
                attends: vec![MarkName::new("signoff")],
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        let officer = Authority {
            name: AuthorityName::new("officer"),
            mandate: Mandate {
                trust_ceiling: Some(TRUSTED),
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        let wire = ToolContract {
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
        };
        let registry = Registry::build_covered(crate::registry::RegistryConfig {
            trust_chain: chain(),
            tools: vec![wire],
            authorities: vec![officer, attends_only],
            sanitizers: vec![],
            casts: vec![],
            membership: None,
        })
        .unwrap();
        let log = vec![user_value(known(SUSPICIOUS, Audience::Public))];
        let ruling = Ruling {
            dispatch: wire_dispatch(),
            authority: AuthorityName::new("attester"),
            reviewed: top_review(),
            covers: vec![floor_gap()],
        };
        assert!(matches!(
            run(&registry, &log, &call("wire", json!({})), &[ruling]),
            Err(PlanError::RulingAssignmentMismatch)
        ));
    }

    #[test]
    fn two_eyes_collects_several_rulings() {
        let wire = ToolContract {
            name: ToolName::new("wire"),
            tags: vec![],
            delta: Some(Delta::NONE),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                attention: vec![MarkName::new("m1"), MarkName::new("m2")],
                ..Requires::default()
            },
        };
        let a1 = Authority {
            name: AuthorityName::new("a1"),
            mandate: Mandate {
                attends: vec![MarkName::new("m1")],
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        let a2 = Authority {
            name: AuthorityName::new("a2"),
            mandate: Mandate {
                attends: vec![MarkName::new("m2")],
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        let registry = Registry::build_covered(crate::registry::RegistryConfig {
            trust_chain: chain(),
            tools: vec![wire],
            authorities: vec![a1, a2],
            sanitizers: vec![],
            casts: vec![],
            membership: None,
        })
        .unwrap();
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let review = AuthorityReview {
            tool: ToolName::new("wire"),
            trajectory_label: partial(TRUSTED, Audience::Public),
        };
        let rulings = vec![
            Ruling {
                dispatch: wire_dispatch(),
                authority: AuthorityName::new("a2"),
                reviewed: review.clone(),
                covers: vec![Gap::Attention(MarkName::new("m2"))],
            },
            Ruling {
                dispatch: wire_dispatch(),
                authority: AuthorityName::new("a1"),
                reviewed: review,
                covers: vec![Gap::Attention(MarkName::new("m1"))],
            },
        ];
        let batch = run(&registry, &log, &call("wire", json!({})), &rulings).unwrap();
        let ruled: Vec<&str> = batch
            .iter()
            .filter_map(|f| match f {
                Fact::Ruling { authority, .. } => Some(authority.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(ruled, ["a1", "a2"]);
        assert!(matches!(batch.last().unwrap(), Fact::DispatchOpened { .. }));
    }

    #[test]
    fn a_false_review_is_refused() {
        let registry = registry();
        let log = vec![user_value(known(SUSPICIOUS, Audience::Public))];
        let with_review = |reviewed: AuthorityReview| Ruling {
            dispatch: wire_dispatch(),
            authority: AuthorityName::new("officer"),
            reviewed,
            covers: vec![floor_gap()],
        };
        let false_label = AuthorityReview {
            trajectory_label: PartialLabel::established(EstablishedLabel::top()),
            ..top_review()
        };
        assert_eq!(
            run(&registry, &log, &call("wire", json!({})), &[with_review(false_label)]),
            Err(PlanError::ReviewMismatch)
        );
        let wrong_tool = AuthorityReview {
            tool: ToolName::new("other"),
            ..top_review()
        };
        assert_eq!(
            run(&registry, &log, &call("wire", json!({})), &[with_review(wrong_tool)]),
            Err(PlanError::ReviewMismatch)
        );
    }

    #[test]
    fn a_mixed_plan_lands_acceptance_before_its_rulings() {
        let post = ToolContract {
            name: ToolName::new("post"),
            tags: vec![],
            delta: Some(Delta {
                trust: None,
                audience: Some(Dim::Known(Audience::restricted([ReaderId::new("internal")])).into()),
            }),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                attention: vec![MarkName::new("signoff")],
                ..Requires::default()
            },
        };
        let steward = Authority {
            name: AuthorityName::new("steward"),
            mandate: Mandate {
                attends: vec![MarkName::new("signoff")],
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        let registry = Registry::build_covered(crate::registry::RegistryConfig {
            trust_chain: chain(),
            tools: vec![post],
            authorities: vec![steward],
            sanitizers: vec![],
            casts: vec![],
            membership: None,
        })
        .unwrap();
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let post_call = call("post", json!({}));
        let ruling = Ruling {
            dispatch: DispatchId::new(traj(), post_call.digest(), 0),
            authority: AuthorityName::new("steward"),
            reviewed: AuthorityReview {
                tool: ToolName::new("post"),
                trajectory_label: partial(TRUSTED, Audience::Public),
            },
            covers: vec![Gap::Attention(MarkName::new("signoff"))],
        };
        let batch = run(&registry, &log, &post_call, &[ruling]).unwrap();
        let offered = crate::check::Narrowing {
            from: established(TRUSTED, Audience::Public),
            to: established(TRUSTED, Audience::restricted([ReaderId::new("internal")])),
        };
        assert_eq!(batch.len(), 3);
        assert!(matches!(&batch[0], Fact::Acceptance { narrowing, .. } if *narrowing == offered));
        assert!(matches!(batch[1], Fact::Ruling { .. }));
        assert!(matches!(batch[2], Fact::DispatchOpened { .. }));
    }

    fn substituting_registry() -> Registry {
        let post = ToolContract {
            name: ToolName::new("post"),
            tags: vec![],
            delta: Some(Delta::NONE),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![crate::contract::AudienceRequirement::Includes(
                        crate::contract::RecipientSpec::Static(Audience::restricted([ReaderId::new("partner")])),
                    )],
                },
                ..Requires::default()
            },
        };
        Registry::build_covered(crate::registry::RegistryConfig {
            trust_chain: chain(),
            tools: vec![post],
            authorities: vec![],
            sanitizers: vec![crate::authority::Sanitizer {
                name: SanitizerName::new("redact"),
                on: crate::authority::SanitizerPoints {
                    input: true,
                    output: false,
                },
                transition: crate::authority::Transition::Audience {
                    from_includes: Audience::restricted([ReaderId::new("internal")]),
                    to: Audience::restricted([ReaderId::new("internal"), ReaderId::new("partner")]),
                },
                scope: Scope::default(),
                hint: None,
            }],
            casts: vec![],
            membership: None,
        })
        .unwrap()
    }

    #[test]
    fn the_composed_operation_refuses_an_input_hop_instead_of_releasing_it() {
        let registry = substituting_registry();
        let log = vec![user_value(known(
            TRUSTED,
            Audience::restricted([ReaderId::new("internal")]),
        ))];
        let call = call("post", json!({}));
        let projection = Projection::build(&log, 1);
        let trajectory = traj();
        let views = projection.view(&trajectory);
        let chosen = offered_plan(&registry, &views, &call);
        assert_eq!(chosen.hop(), Some(&SanitizerName::new("redact")));
        assert_eq!(
            execute_remedy_plan(&registry, &views, &chosen, &call, &[]),
            Err(PlanError::GapUncovered(Gap::Includes {
                recipients: Audience::restricted([ReaderId::new("partner")])
            }))
        );
    }

    fn narrowing_registry() -> Registry {
        let get = ToolContract {
            name: ToolName::new("get"),
            tags: vec![],
            delta: Some(Delta {
                trust: None,
                audience: Some(Dim::Known(Audience::restricted([ReaderId::new("internal")])).into()),
            }),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires::default(),
        };
        Registry::build_covered(crate::registry::RegistryConfig {
            trust_chain: chain(),
            tools: vec![get],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![],
            membership: None,
        })
        .unwrap()
    }

    #[test]
    fn narrowing_records_an_acceptance() {
        let registry = narrowing_registry();
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let batch = run(&registry, &log, &call("get", json!({})), &[]).unwrap();
        let offered = crate::check::Narrowing {
            from: established(TRUSTED, Audience::Public),
            to: established(TRUSTED, Audience::restricted([ReaderId::new("internal")])),
        };
        assert!(
            batch
                .iter()
                .any(|f| matches!(f, Fact::Acceptance { narrowing, .. } if *narrowing == offered))
        );
    }

    #[test]
    fn a_stale_acceptance_for_a_moved_narrowing_is_refused() {
        let registry = narrowing_registry();
        let trajectory = traj();
        let offered_log = vec![user_value(known(TRUSTED, Audience::Public))];
        let projection = Projection::build(&offered_log, 1);
        let stale = offered_plan(&registry, &projection.view(&trajectory), &call("get", json!({})));
        assert!(
            stale
                .steps
                .iter()
                .any(|step| matches!(step, plan::RemedyStep::Accept(_)))
        );

        let moved_log = vec![
            user_value(known(TRUSTED, Audience::Public)),
            user_value(known(
                TRUSTED,
                Audience::restricted([ReaderId::new("internal"), ReaderId::new("extra")]),
            )),
        ];
        let projection = Projection::build(&moved_log, 2);
        let views = projection.view(&trajectory);
        assert_eq!(
            execute_remedy_plan(&registry, &views, &stale, &call("get", json!({})), &[]),
            Err(PlanError::UnknownPlan(0))
        );

        let live = offered_plan(&registry, &views, &call("get", json!({})));
        let batch = execute_remedy_plan(&registry, &views, &live, &call("get", json!({})), &[]).unwrap();
        let live_narrowing = crate::check::Narrowing {
            from: established(
                TRUSTED,
                Audience::restricted([ReaderId::new("internal"), ReaderId::new("extra")]),
            ),
            to: established(TRUSTED, Audience::restricted([ReaderId::new("internal")])),
        };
        assert!(
            batch
                .iter()
                .any(|f| matches!(f, Fact::Acceptance { narrowing, .. } if *narrowing == live_narrowing))
        );
    }
}
