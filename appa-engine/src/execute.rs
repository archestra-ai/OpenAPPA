//! Atomic plan execution: turning gathered rulings into the one indivisible batch that admits a
//! blocked dispatch.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::check::{self, CheckOutcome, Gap, UnestablishedFact};
use crate::engine::opened_dispatch;
use crate::fact::{Fact, FactBatch};
use crate::label::Label;
use crate::names::AuthorityName;
use crate::plan::{self, covers_gap};
use crate::projection::Views;
use crate::registry::Registry;
use crate::value::{DispatchId, Provenance, ResolvedCall, ValueId};

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityReview {
    pub tool: crate::value::ToolName,
    pub trajectory_label: Label,
    pub arg_refs: Vec<ReviewedRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewedRef {
    pub value: ValueId,
    pub label: Label,
    pub provenance: Provenance,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PlanError {
    #[error("no contract registered for tool {0}")]
    UnknownTool(String),
    #[error("the call is not blocked — dispatch it directly")]
    NotBlocked,
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
}

/// Execute a remedy plan: verify coverage, then emit the atomic
/// rulings + acceptance + dispatch batch. See the module docs.
pub(crate) fn execute_remedy_plan(
    registry: &Registry,
    views: &Views,
    chosen: &plan::ExecutableRemedyPlan,
    call: &ResolvedCall,
    rulings: &[Ruling],
) -> Result<FactBatch, PlanError> {
    let contract = registry
        .tool(call.tool())
        .ok_or_else(|| PlanError::UnknownTool(call.tool().as_str().to_string()))?;

    let block = match check::evaluate(contract, views, call) {
        CheckOutcome::Block(block) => block,
        CheckOutcome::Allow => return Err(PlanError::NotBlocked),
    };
    if !block.unestablished.is_empty() {
        return Err(PlanError::Unestablished(block.unestablished));
    }

    let planned = plan::plan(registry, views, call, &block);
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
        let reviewed_ids: Vec<ValueId> = ruling.reviewed.arg_refs.iter().map(|r| r.value).collect();
        if reviewed_ids != call.arg_refs() {
            return Err(PlanError::ReviewMismatch);
        }
        for reviewed in &ruling.reviewed.arg_refs {
            let resolves = views.owns_value(reviewed.value)
                && views.value_label(reviewed.value) == Some(&reviewed.label)
                && views.value_provenance(reviewed.value) == Some(&reviewed.provenance);
            if !resolves {
                return Err(PlanError::ReviewMismatch);
            }
        }
    }

    let (dispatch, dispatch_opened) = opened_dispatch(contract, views, call);

    for ruling in rulings {
        if ruling.dispatch != dispatch {
            return Err(PlanError::RulingCallMismatch);
        }
        let authority = registry
            .authority(&ruling.authority)
            .ok_or_else(|| PlanError::UnknownAuthority(ruling.authority.as_str().to_string()))?;
        for gap in &ruling.covers {
            if !block.requirement_gaps.contains(gap) {
                return Err(PlanError::RulingClaimsAbsentGap(gap.clone()));
            }
            if !covers_gap(authority, gap, &contract.tags) {
                return Err(PlanError::RulingExceedsMandate {
                    authority: ruling.authority.as_str().to_string(),
                });
            }
        }
    }

    // Every requirement gap must be covered by some in-mandate ruling that claims it.
    for gap in &block.requirement_gaps {
        let covered = rulings.iter().any(|ruling| {
            ruling.covers.contains(gap)
                && registry
                    .authority(&ruling.authority)
                    .is_some_and(|authority| covers_gap(authority, gap, &contract.tags))
        });
        if !covered {
            return Err(PlanError::GapUncovered(gap.clone()));
        }
    }

    let trajectory = views.trajectory().clone();

    let mut facts = Vec::new();
    for ruling in rulings {
        facts.push(Fact::Ruling {
            trajectory: trajectory.clone(),
            dispatch: dispatch.clone(),
            plan,
            authority: ruling.authority.clone(),
            covers: ruling.covers.clone(),
            reviewed: ruling.reviewed.clone(),
        });
    }
    if let Some(narrowing) = chosen.steps.iter().find_map(|step| match step {
        plan::RemedyStep::Accept(narrowing) => Some(narrowing.clone()),
        plan::RemedyStep::Authorize(_) => None,
    }) {
        facts.push(Fact::Acceptance {
            trajectory: trajectory.clone(),
            dispatch: dispatch.clone(),
            plan,
            narrowing,
        });
    }
    facts.push(dispatch_opened);

    Ok(FactBatch::new(views.revision(), facts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::{Authority, Mandate, Scope};
    use crate::contract::{Delta, LabelRequirements, Requires, ToolContract};
    use crate::fact::{Fact, Revision};
    use crate::label::{Audience, Dim, Label, ReaderId, Trust};
    use crate::names::MarkName;
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

    fn registry() -> Registry {
        let wire = ToolContract {
            name: ToolName::new("wire"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![],
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
        };
        Registry::build(crate::registry::RegistryConfig {
            trust_chain: chain(),
            tools: vec![wire],
            authorities: vec![officer],
            sanitizers: vec![],
            casts: vec![],
        })
        .unwrap()
    }

    fn call(tool: &str, args: serde_json::Value) -> ResolvedCall {
        ResolvedCall::new(ToolName::new(tool), args, vec![])
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
            trajectory_label: known(SUSPICIOUS, Audience::Public),
            arg_refs: vec![],
        }
    }

    fn wire_dispatch() -> DispatchId {
        DispatchId::new(traj(), call("wire", json!({})).digest(), 0)
    }

    fn run(registry: &Registry, log: &[Fact], call: &ResolvedCall, rulings: &[Ruling]) -> Result<FactBatch, PlanError> {
        let projection = Projection::build(log, Revision::new(log.len() as u64));
        let trajectory = traj();
        let views = projection.view(&trajectory);
        let chosen = offered_plan(registry, &views, call);
        execute_remedy_plan(registry, &views, &chosen, call, rulings)
    }

    fn offered_plan(registry: &Registry, views: &Views, call: &ResolvedCall) -> plan::ExecutableRemedyPlan {
        let planned = match check::evaluate(registry.tool(call.tool()).unwrap(), views, call) {
            CheckOutcome::Block(block) => plan::plan(registry, views, call, &block),
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
        assert!(matches!(batch.facts[0], Fact::Ruling { .. }));
        assert!(matches!(batch.facts.last().unwrap(), Fact::DispatchOpened { .. }));
    }

    #[test]
    fn a_valid_ruling_cannot_clear_an_unestablished_fact() {
        let vault = ToolContract {
            name: ToolName::new("vault"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![],
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
        };
        let registry = Registry::build(crate::registry::RegistryConfig {
            trust_chain: chain(),
            tools: vec![vault],
            authorities: vec![steward],
            sanitizers: vec![],
            casts: vec![],
        })
        .unwrap();
        let log = vec![
            user_value(known(SUSPICIOUS, Audience::Public)),
            user_value(Label::new(Dim::Unknown, Dim::Known(Audience::Public))),
        ];
        let vault_call = call("vault", json!({}));
        let ruling = Ruling {
            dispatch: DispatchId::new(traj(), vault_call.digest(), 0),
            authority: AuthorityName::new("steward"),
            reviewed: AuthorityReview {
                tool: ToolName::new("vault"),
                trajectory_label: Label::new(Dim::Unknown, Dim::Known(Audience::Public)),
                arg_refs: vec![],
            },
            covers: vec![Gap::Attention(MarkName::new("signoff"))],
        };
        match run(&registry, &log, &vault_call, &[ruling]) {
            Err(PlanError::Unestablished(facts)) => {
                assert_eq!(facts.len(), 1);
                assert_eq!(facts[0].dimension, crate::label::Dimension::Trust);
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
                proposed_label: Label::top(),
                proposed_effects: vec![],
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
        let projection = Projection::build(&log, Revision::new(log.len() as u64));
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
        };
        let officer = Authority {
            name: AuthorityName::new("officer"),
            mandate: Mandate {
                trust_ceiling: Some(TRUSTED),
                ..Mandate::default()
            },
            scope: Scope::default(),
        };
        let wire = ToolContract {
            name: ToolName::new("wire"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![],
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(TRUSTED),
                    audience: vec![],
                },
                ..Requires::default()
            },
        };
        let registry = Registry::build(crate::registry::RegistryConfig {
            trust_chain: chain(),
            tools: vec![wire],
            authorities: vec![officer, attends_only],
            sanitizers: vec![],
            casts: vec![],
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
            emits: vec![],
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
        };
        let a2 = Authority {
            name: AuthorityName::new("a2"),
            mandate: Mandate {
                attends: vec![MarkName::new("m2")],
                ..Mandate::default()
            },
            scope: Scope::default(),
        };
        let registry = Registry::build(crate::registry::RegistryConfig {
            trust_chain: chain(),
            tools: vec![wire],
            authorities: vec![a1, a2],
            sanitizers: vec![],
            casts: vec![],
        })
        .unwrap();
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let review = AuthorityReview {
            tool: ToolName::new("wire"),
            trajectory_label: known(TRUSTED, Audience::Public),
            arg_refs: vec![],
        };
        let rulings = vec![
            Ruling {
                dispatch: wire_dispatch(),
                authority: AuthorityName::new("a1"),
                reviewed: review.clone(),
                covers: vec![Gap::Attention(MarkName::new("m1"))],
            },
            Ruling {
                dispatch: wire_dispatch(),
                authority: AuthorityName::new("a2"),
                reviewed: review,
                covers: vec![Gap::Attention(MarkName::new("m2"))],
            },
        ];
        let batch = run(&registry, &log, &call("wire", json!({})), &rulings).unwrap();
        let ruling_count = batch.facts.iter().filter(|f| matches!(f, Fact::Ruling { .. })).count();
        assert_eq!(ruling_count, 2);
    }

    #[test]
    fn a_false_or_dangling_review_is_refused() {
        let registry = registry();
        let log = vec![user_value(known(SUSPICIOUS, Audience::Public))];
        let with_review = |reviewed: AuthorityReview| Ruling {
            dispatch: wire_dispatch(),
            authority: AuthorityName::new("officer"),
            reviewed,
            covers: vec![floor_gap()],
        };
        let false_label = AuthorityReview {
            trajectory_label: Label::top(),
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
        let dangling = AuthorityReview {
            arg_refs: vec![ReviewedRef {
                value: ValueId::new(7),
                label: known(SUSPICIOUS, Audience::Public),
                provenance: Provenance::UserInput,
            }],
            ..top_review()
        };
        assert_eq!(
            run(&registry, &log, &call("wire", json!({})), &[with_review(dangling)]),
            Err(PlanError::ReviewMismatch)
        );
        let ref_call = ResolvedCall::new(ToolName::new("wire"), json!({}), vec![ValueId::new(0)]);
        assert_eq!(
            run(&registry, &log, &ref_call, &[with_review(top_review())]),
            Err(PlanError::ReviewMismatch)
        );
    }

    fn narrowing_registry() -> Registry {
        let get = ToolContract {
            name: ToolName::new("get"),
            tags: vec![],
            delta: Some(Delta {
                trust: None,
                audience: Some(Dim::Known(Audience::restricted([ReaderId::new("internal")]))),
            }),
            emits: vec![],
            requires: Requires::default(),
        };
        Registry::build(crate::registry::RegistryConfig {
            trust_chain: chain(),
            tools: vec![get],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![],
        })
        .unwrap()
    }

    #[test]
    fn narrowing_records_an_acceptance() {
        let registry = narrowing_registry();
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let batch = run(&registry, &log, &call("get", json!({})), &[]).unwrap();
        let offered = crate::check::Narrowing {
            from: known(TRUSTED, Audience::Public),
            to: known(TRUSTED, Audience::restricted([ReaderId::new("internal")])),
        };
        assert!(
            batch
                .facts
                .iter()
                .any(|f| matches!(f, Fact::Acceptance { narrowing, .. } if *narrowing == offered))
        );
    }

    #[test]
    fn a_stale_acceptance_for_a_moved_narrowing_is_refused() {
        let registry = narrowing_registry();
        let trajectory = traj();
        let offered_log = vec![user_value(known(TRUSTED, Audience::Public))];
        let projection = Projection::build(&offered_log, Revision::new(1));
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
        let projection = Projection::build(&moved_log, Revision::new(2));
        let views = projection.view(&trajectory);
        assert_eq!(
            execute_remedy_plan(&registry, &views, &stale, &call("get", json!({})), &[]),
            Err(PlanError::UnknownPlan(0))
        );

        let live = offered_plan(&registry, &views, &call("get", json!({})));
        let batch = execute_remedy_plan(&registry, &views, &live, &call("get", json!({})), &[]).unwrap();
        let live_narrowing = crate::check::Narrowing {
            from: known(
                TRUSTED,
                Audience::restricted([ReaderId::new("internal"), ReaderId::new("extra")]),
            ),
            to: known(TRUSTED, Audience::restricted([ReaderId::new("internal")])),
        };
        assert!(
            batch
                .facts
                .iter()
                .any(|f| matches!(f, Fact::Acceptance { narrowing, .. } if *narrowing == live_narrowing))
        );
    }
}
