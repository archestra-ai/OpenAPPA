//! G12 model-facing block feedback: the exact typed decision state, rendered once, shared by the
//! runtime turn-drive and the SDK so both surfaces are byte-identical.

use appa_engine::check::{Gap, Narrowing, RawBlock};
use appa_engine::plan::{PlannedBlock, Recommendation, RemedyPlan};
use serde::Serialize;

/// The trajectory a block's feedback addresses — what branching fact its narrowing lead may
/// honestly carry. A `Root` that can fork hears the fork alternative; a `Child` hears that its
/// restriction is branch-confined; anything else hears the acceptance alone.
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
    reason: &'a str,
}

#[derive(Serialize)]
struct WireBlock<'a> {
    requirement_gaps: &'a [Gap],
    #[serde(skip_serializing_if = "Option::is_none")]
    narrowing: Option<&'a Narrowing>,
    plans: Vec<WirePlan<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    redispatch: Vec<WireRedispatch<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fork: Option<&'a str>,
}

fn wire_plans(offers: &[(String, RemedyPlan)]) -> Vec<WirePlan<'_>> {
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

fn payload(
    raw: &RawBlock,
    planned: &PlannedBlock,
    offers: &[(String, RemedyPlan)],
    surface: FeedbackSurface,
) -> String {
    let advises_fork =
        matches!(surface, FeedbackSurface::Root { can_fork: true }) && raw.narrowing.is_some() && !offers.is_empty();
    let fork = if advises_fork {
        planned
            .recommendations
            .iter()
            .find_map(|recommendation| match recommendation {
                Recommendation::Fork { reason } => Some(reason.as_str()),
                Recommendation::Redispatch { .. } => None,
            })
    } else {
        None
    };
    let block = WireBlock {
        requirement_gaps: &raw.requirement_gaps,
        narrowing: raw.narrowing.as_ref(),
        plans: wire_plans(offers),
        redispatch: planned
            .recommendations
            .iter()
            .filter_map(|recommendation| match recommendation {
                Recommendation::Redispatch { tool, reason } => Some(WireRedispatch {
                    tool: tool.as_str(),
                    reason,
                }),
                Recommendation::Fork { .. } => None,
            })
            .collect(),
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
    offers: &[(String, RemedyPlan)],
    surface: FeedbackSurface,
) -> String {
    let lead = if offers.is_empty() {
        if planned.recommendations.iter().any(Recommendation::is_curative) {
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
        match surface {
            FeedbackSurface::Root { can_fork: true } => {
                "narrowing: this call restricts the trajectory label. Fork the restricting work into a child session to keep this session's label, or, if every later step can live with the restriction, accept it with execute_remedy_plan in your next response — acceptance is permanent for this session"
            }
            FeedbackSurface::Root { can_fork: false } => {
                "narrowing: this call restricts the trajectory label; accept it with execute_remedy_plan in your next response"
            }
            FeedbackSurface::Child => {
                "narrowing: this call restricts this branch's label only — the parent session is unaffected; accept it with execute_remedy_plan in your next response"
            }
        }
    } else if raw.narrowing.is_some() {
        match surface {
            FeedbackSurface::Root { can_fork: true } => {
                "blocked by policy; execute one offered plan with execute_remedy_plan in your next response — it also accepts this call's narrowing — or fork the restricting work into a child session to keep this session's label"
            }
            FeedbackSurface::Root { can_fork: false } => {
                "blocked by policy; execute one offered plan with execute_remedy_plan in your next response"
            }
            FeedbackSurface::Child => {
                "blocked by policy; execute one offered plan with execute_remedy_plan in your next response; its narrowing restricts this branch's label only — the parent session is unaffected"
            }
        }
    } else {
        "blocked by policy; execute one offered plan with execute_remedy_plan"
    };
    format!("{lead}\n{}", payload(raw, planned, offers, surface))
}

/// Render the feedback after an authority declined one offer: the denial, then the remaining
/// sibling plans as the same typed payload shape (no gaps re-listed — the block is unchanged).
pub fn denial_feedback(remaining: &[(String, RemedyPlan)]) -> String {
    if remaining.is_empty() {
        return "the authority declined to authorize this call; no alternative plan remains".to_string();
    }
    #[derive(Serialize)]
    struct WireRemaining<'a> {
        plans: Vec<WirePlan<'a>>,
    }
    let payload = serde_json::to_string(&WireRemaining {
        plans: wire_plans(remaining),
    })
    .expect("the plan payload serializes: engine types are Serialize");
    format!("the authority declined to authorize this call; alternatives remain\n{payload}")
}

pub fn cast_offer_feedback(handle: &str, narrowing: &Narrowing) -> String {
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
        "result withheld: admitting it narrows the trajectory label; accept with execute_remedy_plan in your next response\n{payload}"
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

    fn plan_with(authority: &str, covers: Vec<Gap>) -> RemedyPlan {
        RemedyPlan {
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
            plans: vec![plan_with("officer", every_gap())],
            recommendations: vec![],
        };
        let offers = vec![("remedy-7".to_string(), planned.plans[0].clone())];
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
        assert_eq!(payload["plans"][0]["plan_id"], "remedy-7");
        assert_eq!(payload["plans"][0]["rulings"][0]["authority"], "officer");
        assert_eq!(payload["plans"][0]["rulings"][0]["covers"].as_array().unwrap().len(), 6);
        assert_eq!(payload["plans"][0]["accepts_narrowing"], false);
    }

    #[test]
    fn a_pure_narrowing_presents_as_an_acceptance_with_exact_labels() {
        let raw = RawBlock {
            requirement_gaps: vec![],
            narrowing: Some(narrowing()),
        };
        let accept_plan = RemedyPlan {
            id: PlanId::new(0),
            steps: vec![RemedyStep::Accept(narrowing())],
            required: vec![],
        };
        let planned = PlannedBlock {
            raw: raw.clone(),
            plans: vec![accept_plan.clone()],
            recommendations: vec![Recommendation::Fork {
                reason: "confine the loss".to_string(),
            }],
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
        assert_eq!(payload["plans"][0]["accepts_narrowing"], true);
        assert_eq!(payload["plans"][0]["rulings"].as_array().unwrap().len(), 0);
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
            plans: vec![plan.clone()],
            recommendations: vec![Recommendation::Fork {
                reason: "confine the loss".to_string(),
            }],
        };
        let offers = vec![("remedy-0".to_string(), plan)];
        let payload = parsed(&block_feedback(
            &raw,
            &planned,
            &offers,
            FeedbackSurface::Root { can_fork: true },
        ));
        assert_eq!(payload["fork"], "confine the loss");
        assert_eq!(payload["plans"][0]["accepts_narrowing"], true);
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
                plan_with("officer-a", vec![floor.clone()]),
                plan_with("officer-b", vec![floor.clone()]),
            ],
            recommendations: vec![
                Recommendation::Redispatch {
                    tool: ToolName::new("backup"),
                    reason: "emit the prior".to_string(),
                },
                Recommendation::Redispatch {
                    tool: ToolName::new("snapshot"),
                    reason: "emit the prior".to_string(),
                },
                Recommendation::Fork {
                    reason: "advisory".to_string(),
                },
            ],
        };
        let offers = vec![
            ("remedy-0".to_string(), planned.plans[0].clone()),
            ("remedy-1".to_string(), planned.plans[1].clone()),
        ];
        let payload = parsed(&block_feedback(
            &raw,
            &planned,
            &offers,
            FeedbackSurface::Root { can_fork: true },
        ));
        assert_eq!(payload["plans"].as_array().unwrap().len(), 2);
        assert_eq!(payload["plans"][0]["plan_id"], "remedy-0");
        assert_eq!(payload["plans"][0]["rulings"][0]["authority"], "officer-a");
        assert_eq!(payload["plans"][1]["plan_id"], "remedy-1");
        assert_eq!(payload["plans"][1]["rulings"][0]["authority"], "officer-b");
        assert_eq!(payload["redispatch"][0]["tool"], "backup");
        assert_eq!(payload["redispatch"][1]["tool"], "snapshot");
        assert!(payload.get("fork").is_none());

        let none_planned = PlannedBlock {
            raw: raw.clone(),
            plans: vec![],
            recommendations: planned.recommendations.clone(),
        };
        let payload = parsed(&block_feedback(
            &raw,
            &none_planned,
            &[],
            FeedbackSurface::Root { can_fork: true },
        ));
        assert_eq!(payload["plans"].as_array().unwrap().len(), 0);
        assert_eq!(payload["redispatch"][0]["tool"], "backup");
    }

    #[test]
    fn a_denial_relists_the_surviving_siblings() {
        let floor = Gap::TrustFloor {
            required: Trust::new(1),
            actual: Trust::new(0),
        };
        let remaining = vec![("remedy-1".to_string(), plan_with("officer-b", vec![floor]))];
        let payload = parsed(&denial_feedback(&remaining));
        assert_eq!(payload["plans"].as_array().unwrap().len(), 1);
        assert_eq!(payload["plans"][0]["plan_id"], "remedy-1");
        assert!(!denial_feedback(&[]).contains('\n'));
    }

    #[test]
    fn a_cast_offer_carries_the_exact_narrowing_and_handle() {
        let payload = parsed(&cast_offer_feedback("remedy-3", &narrowing()));
        assert_eq!(payload["plan_id"], "remedy-3");
        assert_eq!(payload["narrowing"]["from"]["trust"]["Known"], 1);
        assert_eq!(
            payload["narrowing"]["to"]["audience"]["Known"]["Restricted"][0],
            "internal"
        );
    }
}
