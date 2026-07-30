//! G12 model-facing block feedback: the exact typed decision state, rendered once, shared by the
//! runtime turn-drive and the SDK so both surfaces are byte-identical.

use appa_engine::check::{Gap, Narrowing, RawBlock};
use appa_engine::plan::{ExecutableRemedyPlan, PlannedBlock, RedispatchEffect, RemedyPlan};
use serde::Serialize;

/// The trajectory a block's feedback addresses. It fixes two things: how far an acceptance reaches
/// (a `Root`'s over the session, a `Child`'s over its branch alone) and whether a branch
/// alternative may honestly be advised — only a `Root` that can fork.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedbackSurface {
    Root { can_fork: bool },
    Child,
}

#[derive(Serialize)]
struct WirePlan<'a> {
    plan_id: &'a str,
    rulings: Vec<WireRuling<'a>>,
    accepts_narrowing: bool,
}

#[derive(Serialize)]
struct WireRuling<'a> {
    authority: &'a str,
    covers: &'a [Gap],
}

#[derive(Serialize)]
struct WireRedispatch<'a> {
    tool: &'a str,
    clears: &'a [Gap],
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    enables_path: bool,
}

#[derive(Serialize)]
#[serde(untagged)]
enum WireRemedyPlan<'a> {
    Executable(WirePlan<'a>),
    Redispatch(WireRedispatch<'a>),
}

#[derive(Serialize)]
struct WireBlock<'a> {
    requirement_gaps: &'a [Gap],
    #[serde(skip_serializing_if = "Option::is_none")]
    narrowing: Option<&'a Narrowing>,
    remedy_plans: Vec<WireRemedyPlan<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fork: Option<&'a str>,
}

fn wire_plans(offers: &[(String, ExecutableRemedyPlan)]) -> Vec<WirePlan<'_>> {
    offers
        .iter()
        .map(|(handle, plan)| WirePlan {
            plan_id: handle,
            rulings: plan
                .required
                .iter()
                .map(|required| WireRuling {
                    authority: required.authority.as_str(),
                    covers: &required.covers,
                })
                .collect(),
            accepts_narrowing: plan
                .steps
                .iter()
                .any(|step| matches!(step, appa_engine::plan::RemedyStep::Accept(_))),
        })
        .collect()
}

fn fork_advice<'a>(
    raw: &RawBlock,
    planned: &'a PlannedBlock,
    offers: &[(String, ExecutableRemedyPlan)],
    surface: FeedbackSurface,
) -> Option<&'a str> {
    if !matches!(surface, FeedbackSurface::Root { can_fork: true }) || raw.narrowing.is_none() || offers.is_empty() {
        return None;
    }
    planned.fork_advice.as_deref()
}

fn payload(
    raw: &RawBlock,
    planned: &PlannedBlock,
    offers: &[(String, ExecutableRemedyPlan)],
    fork: Option<&str>,
) -> String {
    let mut remedy_plans: Vec<WireRemedyPlan> =
        wire_plans(offers).into_iter().map(WireRemedyPlan::Executable).collect();
    remedy_plans.extend(planned.plans.iter().filter_map(|plan| match plan {
        RemedyPlan::Redispatch { tool, effect } => Some(WireRemedyPlan::Redispatch(WireRedispatch {
            tool: tool.as_str(),
            clears: match effect {
                RedispatchEffect::Clears(gaps) => gaps.as_slice(),
                RedispatchEffect::EnablesPath => &[],
            },
            enables_path: matches!(effect, RedispatchEffect::EnablesPath),
        })),
        RemedyPlan::Executable(_) => None,
    }));
    let block = WireBlock {
        requirement_gaps: &raw.requirement_gaps,
        narrowing: raw.narrowing.as_ref(),
        remedy_plans,
        fork,
    };
    serde_json::to_string(&block).expect("the block payload serializes: engine types are Serialize")
}

/// Render a block's model-facing feedback: the fixed prose lead for its decision kind, then the
/// exact typed payload. A pure narrowing (no requirement gap) presents as an acceptance — the
/// agent's own step, no authority involved. Anything with gaps presents as a block to remedy.
pub fn block_feedback(
    raw: &RawBlock,
    planned: &PlannedBlock,
    offers: &[(String, ExecutableRemedyPlan)],
    surface: FeedbackSurface,
) -> String {
    let fork = fork_advice(raw, planned, offers, surface);
    let lead = if offers.is_empty() {
        if planned
            .plans
            .iter()
            .any(|plan| matches!(plan, RemedyPlan::Redispatch { .. }))
        {
            "blocked by policy; run a redispatch prerequisite first, then re-propose this call"
        } else {
            match surface {
                FeedbackSurface::Child => {
                    "blocked by policy; no remedy is available for this call in this branch — complete what this branch still can, then finish with submit_result: return the value the parent needs, or null after side-effect-only work"
                }
                FeedbackSurface::Root { .. } => "blocked by policy; no remedy is available for this call",
            }
        }
    } else if raw.requirement_gaps.is_empty() {
        match fork {
            Some(_) => {
                "narrowing: this call restricts the trajectory label, and acceptance is permanent for this session — no authority widens an audience, and trust never rises. Fork the restricting work into a child session to keep this session's label; or run every later step that needs the current label first, then accept with execute_remedy_plan in a later response"
            }
            None => match surface {
                FeedbackSurface::Root { .. } => {
                    "narrowing: this call restricts the trajectory label, and acceptance is permanent for this session — no authority widens an audience, and trust never rises. Run every later step that needs the current label first, then accept with execute_remedy_plan in a later response"
                }
                FeedbackSurface::Child => {
                    "narrowing: this call restricts this branch's label only — the parent session is unaffected — and acceptance is permanent for this branch. Run every later step of this branch that needs the current label first, then accept with execute_remedy_plan in a later response"
                }
            },
        }
    } else if raw.narrowing.is_some() {
        match fork {
            Some(_) => {
                "blocked by policy; every offered plan also accepts this call's narrowing, permanently for this session. Fork the restricting work into a child session to keep this session's label; or run every later step that needs the current label first, then execute a plan with execute_remedy_plan in a later response"
            }
            None => match surface {
                FeedbackSurface::Root { .. } => {
                    "blocked by policy; every offered plan also accepts this call's narrowing, permanently for this session. Run every later step that needs the current label first, then execute one with execute_remedy_plan in a later response"
                }
                FeedbackSurface::Child => {
                    "blocked by policy; every offered plan also accepts this call's narrowing, permanent for this branch — the parent session is unaffected. Run every later step of this branch that needs the current label first, then execute one with execute_remedy_plan in a later response"
                }
            },
        }
    } else {
        "blocked by policy; execute one offered plan with execute_remedy_plan"
    };
    format!("{lead}\n{}", payload(raw, planned, offers, fork))
}

fn acceptance_cost(surface: FeedbackSurface) -> &'static str {
    match surface {
        FeedbackSurface::Root { .. } => "permanent for this session",
        FeedbackSurface::Child => "permanent for this branch; the parent session is unaffected",
    }
}

/// Render the feedback after an authority declined one offer: the denial, then the remaining
/// sibling plans as the same typed payload shape (no gaps re-listed — the block is unchanged). A
/// sibling that carries an acceptance re-offers the narrowing, so its cost is named again here.
pub fn denial_feedback(remaining: &[(String, ExecutableRemedyPlan)], surface: FeedbackSurface) -> String {
    if remaining.is_empty() {
        return "the authority declined to authorize this call; no alternative plan remains".to_string();
    }
    #[derive(Serialize)]
    struct WireRemaining<'a> {
        remedy_plans: Vec<WirePlan<'a>>,
    }
    let plans = wire_plans(remaining);
    let accepts = plans.iter().any(|plan| plan.accepts_narrowing);
    let payload = serde_json::to_string(&WireRemaining { remedy_plans: plans })
        .expect("the plan payload serializes: engine types are Serialize");
    let cost = if accepts {
        format!(
            " — a plan marked accepts_narrowing restricts the label when executed, {}",
            acceptance_cost(surface)
        )
    } else {
        String::new()
    };
    format!("the authority declined to authorize this call; alternatives remain{cost}\n{payload}")
}

pub fn cast_offer_feedback(handle: &str, narrowing: &Narrowing, surface: FeedbackSurface) -> String {
    #[derive(Serialize)]
    struct WireOffer<'a> {
        plan_id: &'a str,
        narrowing: &'a Narrowing,
    }
    let payload = serde_json::to_string(&WireOffer {
        plan_id: handle,
        narrowing,
    })
    .expect("the narrowing payload serializes");
    format!(
        "result withheld: admitting it narrows the trajectory label, and acceptance is {}. Run every later step that needs the current label first, then accept with execute_remedy_plan in a later response\n{payload}",
        acceptance_cost(surface)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use appa_engine::fact::EffectKind;
    use appa_engine::label::{Audience, Dim, Label, ReaderId, Trust};
    use appa_engine::names::{AuthorityName, MarkName};
    use appa_engine::plan::{PlanId, RemedyStep, RequiredRuling};
    use appa_engine::value::ToolName;

    fn every_gap() -> Vec<Gap> {
        vec![
            Gap::TrustFloor {
                required: Trust::new(1),
                actual: Trust::new(0),
            },
            Gap::Includes {
                recipients: Audience::restricted([ReaderId::new("auditor")]),
            },
            Gap::Cap {
                cap: Audience::restricted([ReaderId::new("internal")]),
            },
            Gap::Prior(EffectKind::new("backup")),
            Gap::NoPrior(EffectKind::new("egress")),
            Gap::Attention(MarkName::new("signoff")),
        ]
    }

    fn narrowing() -> Narrowing {
        Narrowing {
            from: Label::new(Dim::Known(Trust::new(1)), Dim::Known(Audience::Public)),
            to: Label::new(
                Dim::Known(Trust::new(0)),
                Dim::Known(Audience::restricted([ReaderId::new("internal")])),
            ),
        }
    }

    fn plan_with(authority: &str, covers: Vec<Gap>) -> ExecutableRemedyPlan {
        ExecutableRemedyPlan {
            id: PlanId::new(0),
            steps: vec![RemedyStep::Authorize(AuthorityName::new(authority))],
            required: vec![RequiredRuling {
                authority: AuthorityName::new(authority),
                covers,
            }],
        }
    }

    fn parsed(feedback: &str) -> serde_json::Value {
        let (_, json) = feedback.split_once('\n').expect("a prose lead then the payload line");
        serde_json::from_str(json).expect("the payload line is JSON")
    }

    #[test]
    fn the_payload_carries_every_gap_variant_exactly() {
        let raw = RawBlock {
            requirement_gaps: every_gap(),
            narrowing: None,
        };
        let planned = PlannedBlock {
            raw: raw.clone(),
            plans: vec![RemedyPlan::Executable(plan_with("officer", every_gap()))],
            fork_advice: None,
        };
        let offers = vec![("remedy-7".to_string(), plan_with("officer", every_gap()))];
        let payload = parsed(&block_feedback(
            &raw,
            &planned,
            &offers,
            FeedbackSurface::Root { can_fork: true },
        ));

        let gaps = payload["requirement_gaps"].as_array().expect("gaps array");
        assert_eq!(gaps.len(), 6);
        assert!(
            gaps.iter()
                .any(|g| g["TrustFloor"]["required"] == 1 && g["TrustFloor"]["actual"] == 0)
        );
        assert!(
            gaps.iter()
                .any(|g| g["Includes"]["recipients"]["Restricted"][0] == "auditor")
        );
        assert!(gaps.iter().any(|g| g["Cap"]["cap"]["Restricted"][0] == "internal"));
        assert!(gaps.iter().any(|g| g["Prior"] == "backup"));
        assert!(gaps.iter().any(|g| g["NoPrior"] == "egress"));
        assert!(gaps.iter().any(|g| g["Attention"] == "signoff"));
        assert_eq!(payload["remedy_plans"][0]["plan_id"], "remedy-7");
        assert_eq!(payload["remedy_plans"][0]["rulings"][0]["authority"], "officer");
        assert_eq!(
            payload["remedy_plans"][0]["rulings"][0]["covers"]
                .as_array()
                .unwrap()
                .len(),
            6
        );
        assert_eq!(payload["remedy_plans"][0]["accepts_narrowing"], false);
    }

    #[test]
    fn a_pure_narrowing_presents_as_an_acceptance_with_exact_labels() {
        let raw = RawBlock {
            requirement_gaps: vec![],
            narrowing: Some(narrowing()),
        };
        let accept_plan = ExecutableRemedyPlan {
            id: PlanId::new(0),
            steps: vec![RemedyStep::Accept(narrowing())],
            required: vec![],
        };
        let planned = PlannedBlock {
            raw: raw.clone(),
            plans: vec![RemedyPlan::Executable(accept_plan.clone())],
            fork_advice: Some("confine the loss".to_string()),
        };
        let offers = vec![("remedy-0".to_string(), accept_plan)];
        let feedback = block_feedback(&raw, &planned, &offers, FeedbackSurface::Root { can_fork: true });
        let payload = parsed(&feedback);
        assert_eq!(payload["requirement_gaps"].as_array().unwrap().len(), 0);
        assert_eq!(payload["narrowing"]["from"]["trust"]["Known"], 1);
        assert_eq!(payload["narrowing"]["to"]["trust"]["Known"], 0);
        assert_eq!(
            payload["narrowing"]["to"]["audience"]["Known"]["Restricted"][0],
            "internal"
        );
        assert_eq!(payload["remedy_plans"][0]["accepts_narrowing"], true);
        assert_eq!(payload["remedy_plans"][0]["rulings"].as_array().unwrap().len(), 0);
        assert_eq!(payload["fork"], "confine the loss");
        let payload = parsed(&block_feedback(
            &raw,
            &planned,
            &offers,
            FeedbackSurface::Root { can_fork: false },
        ));
        assert!(payload.get("fork").is_none());
        let payload = parsed(&block_feedback(&raw, &planned, &offers, FeedbackSurface::Child));
        assert!(payload.get("fork").is_none());
    }

    #[test]
    fn a_mixed_block_carries_the_fork_alternative_only_for_a_forking_root() {
        let floor = Gap::TrustFloor {
            required: Trust::new(1),
            actual: Trust::new(0),
        };
        let raw = RawBlock {
            requirement_gaps: vec![floor.clone()],
            narrowing: Some(narrowing()),
        };
        let mut plan = plan_with("officer", vec![floor]);
        plan.steps.push(RemedyStep::Accept(narrowing()));
        let planned = PlannedBlock {
            raw: raw.clone(),
            plans: vec![RemedyPlan::Executable(plan.clone())],
            fork_advice: Some("confine the loss".to_string()),
        };
        let offers = vec![("remedy-0".to_string(), plan)];
        let payload = parsed(&block_feedback(
            &raw,
            &planned,
            &offers,
            FeedbackSurface::Root { can_fork: true },
        ));
        assert_eq!(payload["fork"], "confine the loss");
        assert_eq!(payload["remedy_plans"][0]["accepts_narrowing"], true);
        let payload = parsed(&block_feedback(
            &raw,
            &planned,
            &offers,
            FeedbackSurface::Root { can_fork: false },
        ));
        assert!(payload.get("fork").is_none());
        let payload = parsed(&block_feedback(&raw, &planned, &offers, FeedbackSurface::Child));
        assert!(payload.get("fork").is_none());
        let payload = parsed(&block_feedback(
            &raw,
            &planned,
            &[],
            FeedbackSurface::Root { can_fork: true },
        ));
        assert!(payload.get("fork").is_none());
    }

    #[test]
    fn alternatives_and_typed_redispatch_render_completely() {
        let floor = Gap::TrustFloor {
            required: Trust::new(1),
            actual: Trust::new(0),
        };
        let raw = RawBlock {
            requirement_gaps: vec![floor.clone()],
            narrowing: None,
        };
        let planned = PlannedBlock {
            raw: raw.clone(),
            plans: vec![
                RemedyPlan::Executable(plan_with("officer-a", vec![floor.clone()])),
                RemedyPlan::Executable(plan_with("officer-b", vec![floor.clone()])),
                RemedyPlan::Redispatch {
                    tool: ToolName::new("backup"),
                    effect: RedispatchEffect::Clears(vec![floor.clone()]),
                },
                RemedyPlan::Redispatch {
                    tool: ToolName::new("snapshot"),
                    effect: RedispatchEffect::EnablesPath,
                },
            ],
            fork_advice: Some("advisory".to_string()),
        };
        let offers = vec![
            ("remedy-0".to_string(), plan_with("officer-a", vec![floor.clone()])),
            ("remedy-1".to_string(), plan_with("officer-b", vec![floor.clone()])),
        ];
        let payload = parsed(&block_feedback(
            &raw,
            &planned,
            &offers,
            FeedbackSurface::Root { can_fork: true },
        ));
        let plans = payload["remedy_plans"].as_array().unwrap();
        assert_eq!(plans.len(), 4);
        assert_eq!(plans[0]["plan_id"], "remedy-0");
        assert_eq!(plans[0]["rulings"][0]["authority"], "officer-a");
        assert_eq!(plans[1]["plan_id"], "remedy-1");
        assert_eq!(plans[1]["rulings"][0]["authority"], "officer-b");
        assert_eq!(plans[2]["tool"], "backup");
        assert_eq!(plans[2]["clears"].as_array().unwrap().len(), 1);
        assert!(plans[2].get("enables_path").is_none());
        assert_eq!(plans[3]["tool"], "snapshot");
        assert_eq!(plans[3]["enables_path"], true);
        assert!(payload.get("fork").is_none());

        let none_planned = PlannedBlock {
            raw: raw.clone(),
            plans: planned
                .plans
                .iter()
                .filter(|plan| plan.executable().is_none())
                .cloned()
                .collect(),
            fork_advice: planned.fork_advice.clone(),
        };
        let payload = parsed(&block_feedback(
            &raw,
            &none_planned,
            &[],
            FeedbackSurface::Root { can_fork: true },
        ));
        let plans = payload["remedy_plans"].as_array().unwrap();
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0]["tool"], "backup");
    }

    #[test]
    fn a_denial_relists_the_surviving_siblings() {
        let floor = Gap::TrustFloor {
            required: Trust::new(1),
            actual: Trust::new(0),
        };
        let remaining = vec![("remedy-1".to_string(), plan_with("officer-b", vec![floor]))];
        let payload = parsed(&denial_feedback(&remaining, FeedbackSurface::Root { can_fork: false }));
        assert_eq!(payload["remedy_plans"].as_array().unwrap().len(), 1);
        assert_eq!(payload["remedy_plans"][0]["plan_id"], "remedy-1");
        assert!(!denial_feedback(&[], FeedbackSurface::Root { can_fork: false }).contains('\n'));
    }

    #[test]
    fn a_cast_offer_carries_the_exact_narrowing_and_handle() {
        let payload = parsed(&cast_offer_feedback(
            "remedy-3",
            &narrowing(),
            FeedbackSurface::Root { can_fork: false },
        ));
        assert_eq!(payload["plan_id"], "remedy-3");
        assert_eq!(payload["narrowing"]["from"]["trust"]["Known"], 1);
        assert_eq!(
            payload["narrowing"]["to"]["audience"]["Known"]["Restricted"][0],
            "internal"
        );
    }
}
