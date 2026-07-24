//! G12 model-facing block feedback: the exact typed decision state, rendered once, shared by the
//! runtime turn-drive and the SDK so both surfaces are byte-identical.
//!
//! A block's feedback is one fixed prose lead (what kind of decision this is and which reserved
//! tool acts on it) followed on its own line by a JSON payload carrying the engine's exact typed
//! state: every requirement gap with its values, the narrowing's from/to labels, every offered
//! plan with its handle and grouped ruling requirements, and the typed curative redispatch
//! recommendations. The payload is **derived, never stored** — feedback is re-renderable from the
//! engine state, so no fact shape changes here — and it carries no argument or value bytes, only
//! labels, names, and gap values the check already surfaced.
//!
//! A block whose call **narrows** — purely or alongside requirement gaps — additionally carries
//! the branch fact the trajectory needs to decide consciously, per its [`FeedbackSurface`]: a
//! root that can fork is told the fork alternative (the branch confines the label loss; any
//! requirement gaps follow the child and are remedied there) and the payload carries the engine's
//! `Fork` recommendation; a child is told the restriction stays in its branch — the parent is
//! unaffected — so the delegated work is accepted where it was sent, not re-delegated; a surface
//! that cannot fork (exhausted fork budget, the SDK's `CallSession`) gets neither. On a gap-only
//! block fork advice always stays out — a child begins at the same label, so a fork cures no gap
//! — and so does an unliftable block with no executable plan: the child would face the same
//! gaps, and a narrowing that never lands needs no confining. A child's unliftable block instead
//! carries the branch's own terminal fact: the block is decided for this branch, whose remaining
//! moves are its still-legal work and a `submit_result` finish — the return crossing stays
//! checked, so the lead names mechanism, never permission.

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

/// The payload's plan entry: the executable handle, the grouped ruling requirements the plan will
/// realize (per authority, the exact gaps its one ruling covers), and whether executing it accepts
/// the block's narrowing.
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
            // Derived from the plan itself — a denial re-listing has no block at hand, and a plan
            // that carries an Accept step accepts the narrowing wherever it is rendered.
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
    // Fork advice is actionable exactly when the call narrows, an offered plan could run it, and
    // the surface is a root that can fork: the branch confines the label loss, while requirement
    // gaps follow the child and are remedied there. On a gap-only block it stays out (a fork
    // cures no gap), likewise on an unliftable block (nothing would run in the child either); a
    // child already is the confining branch, so it hears acceptance, not further delegation.
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
/// Whenever the call narrows, the surface's branch fact rides along: a forking root is told the
/// branching alternative that keeps its label, a child that the restriction stays in its branch.
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
                // A child's terminal block still leaves it its structural moves: keep doing
                // branch-legal work, and finish through submit_result — the return crossing is
                // itself checked, so this names mechanism, never permission.
                FeedbackSurface::Child => {
                    "blocked by policy; no remedy is available for this call in this branch — complete what this branch still can, then finish with submit_result: return the value the parent needs, or null after side-effect-only work"
                }
                FeedbackSurface::Root { .. } => "blocked by policy; no remedy is available for this call",
            }
        }
    } else if raw.requirement_gaps.is_empty() {
        // A pure narrowing. Acceptance is informed — it executes only in a round after this offer
        // ("in your next response") — and the surface's branch fact rides along.
        match surface {
            FeedbackSurface::Root { can_fork: true } => {
                "narrowing: this call restricts the trajectory label; accept it with execute_remedy_plan in your next response, or fork the restricting work into a child session to keep this session's label"
            }
            FeedbackSurface::Root { can_fork: false } => {
                "narrowing: this call restricts the trajectory label; accept it with execute_remedy_plan in your next response"
            }
            FeedbackSurface::Child => {
                "narrowing: this call restricts this branch's label only — the parent session is unaffected; accept it with execute_remedy_plan in your next response"
            }
        }
    } else if raw.narrowing.is_some() {
        // Mixed block: every offered plan both covers the gaps and accepts the narrowing, so it is
        // round-gated (next response) and the branch fact rides along as on a pure narrowing.
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

/// Render a pending-cast acceptance offer (D2): the withheld result's exact narrowing, typed.
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

    /// The JSON line of a rendered feedback, parsed — the payload is the behavioral surface.
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
        // Exact values, not counts: the floor's ranks, the recipients, the cap, the effect kinds,
        // the mark all round-trip.
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
        // The offered plan carries its handle and grouped assignment.
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
        // The decision kind is in the payload, not pinned prose: no requirement gaps, a present
        // narrowing, and an offer that accepts it — an acceptance, never an authorization ("0
        // requirement gaps … authorize" was the G12 defect).
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
        // A pure narrowing on a forking root carries the branch alternative...
        assert_eq!(payload["fork"], "confine the loss");
        // ...a root that cannot fork never has it, whatever the planner attached...
        let payload = parsed(&block_feedback(
            &raw,
            &planned,
            &offers,
            FeedbackSurface::Root { can_fork: false },
        ));
        assert!(payload.get("fork").is_none());
        // ...and a child hears confinement, never further delegation.
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
        // The narrowing makes the fork actionable despite the gaps: they follow the child, but
        // the label loss stays confined there.
        let payload = parsed(&block_feedback(
            &raw,
            &planned,
            &offers,
            FeedbackSurface::Root { can_fork: true },
        ));
        assert_eq!(payload["fork"], "confine the loss");
        assert_eq!(payload["plans"][0]["accepts_narrowing"], true);
        // A root that cannot fork and a child hear no delegation advice...
        let payload = parsed(&block_feedback(
            &raw,
            &planned,
            &offers,
            FeedbackSurface::Root { can_fork: false },
        ));
        assert!(payload.get("fork").is_none());
        let payload = parsed(&block_feedback(&raw, &planned, &offers, FeedbackSurface::Child));
        assert!(payload.get("fork").is_none());
        // ...and an unliftable mixed block (no executable plan) advises no fork anywhere: the
        // child would face the same gaps, so the narrowing never lands.
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
        // Every offer with its own handle and authority; the curative redispatch typed with its
        // tool; the advisory fork absent even on a forking root — this call narrows nothing, so
        // there is no label loss a branch could confine.
        assert_eq!(payload["plans"].as_array().unwrap().len(), 2);
        assert_eq!(payload["plans"][0]["plan_id"], "remedy-0");
        assert_eq!(payload["plans"][0]["rulings"][0]["authority"], "officer-a");
        assert_eq!(payload["plans"][1]["plan_id"], "remedy-1");
        assert_eq!(payload["plans"][1]["rulings"][0]["authority"], "officer-b");
        // Every curative redispatch renders, in the engine's order.
        assert_eq!(payload["redispatch"][0]["tool"], "backup");
        assert_eq!(payload["redispatch"][1]["tool"], "snapshot");
        assert!(payload.get("fork").is_none());

        // A redispatch-only block still renders the typed payload, with no plans.
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
        // An exhausted cohort renders no payload line at all.
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
