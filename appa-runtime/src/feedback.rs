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
//! A block whose call **narrows** — purely or alongside requirement gaps — carries two independent
//! facts, and they must not be bundled. The **acceptance cost** is a property of narrowing, so
//! every [`FeedbackSurface`] states it: a root's acceptance is permanent for the session, a
//! child's for its branch alone (the parent is unaffected, so delegated work is accepted where it
//! was sent, not re-delegated). Because acceptance is one-way, the cost is stated with the move
//! that avoids it, and that move leads the sentence — run the steps that still need the current
//! label *first*, then accept — because the imperative a reader meets first is the one acted on. The
//! **branch alternative** is a property of the surface, so it is conditional: a root that can fork
//! is told to confine the label loss in a child (requirement gaps follow the child and are remedied
//! there) and the payload carries the engine's `Fork` recommendation; a surface that cannot fork
//! (exhausted fork budget, the SDK's `CallSession`) hears the cost without the alternative it has
//! no way to take. Prose and payload are driven from one [`fork_advice`] call so they cannot
//! disagree. On a gap-only block fork advice always stays out — a child begins at the same label,
//! so a fork cures no gap — and so does an unliftable block with no executable plan: the child
//! would face the same gaps, and a narrowing that never lands needs no confining. A child's
//! unliftable block instead
//! carries the branch's own terminal fact: the block is decided for this branch, whose remaining
//! moves are its still-legal work and a `submit_result` finish — the return crossing stays
//! checked, so the lead names mechanism, never permission.

use appa_engine::check::{Gap, Narrowing, RawBlock};
use appa_engine::plan::{PlannedBlock, Recommendation, RemedyPlan};
use serde::Serialize;

/// The trajectory a block's feedback addresses. It fixes two things: how far an acceptance reaches
/// (a `Root`'s over the session, a `Child`'s over its branch alone) and whether a branch
/// alternative may honestly be advised — only a `Root` that can fork.
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

/// The branch alternative this block may honestly advise, computed **once** and used for both the
/// prose lead and the payload's `fork` field so the two cannot drift. It is actionable exactly when
/// the call narrows, an offered plan could run it, and the surface is a root that can fork: the
/// branch confines the label loss, while requirement gaps follow the child and are remedied there.
/// On a gap-only block it stays out (a fork cures no gap), likewise on an unliftable block (nothing
/// would run in the child either); a child already is the confining branch, so it hears acceptance,
/// not further delegation.
fn fork_advice<'a>(
    raw: &RawBlock,
    planned: &'a PlannedBlock,
    offers: &[(String, RemedyPlan)],
    surface: FeedbackSurface,
) -> Option<&'a str> {
    if !matches!(surface, FeedbackSurface::Root { can_fork: true }) || raw.narrowing.is_none() || offers.is_empty() {
        return None;
    }
    planned
        .recommendations
        .iter()
        .find_map(|recommendation| match recommendation {
            Recommendation::Fork { reason } => Some(reason.as_str()),
            Recommendation::Redispatch { .. } => None,
        })
}

fn payload(raw: &RawBlock, planned: &PlannedBlock, offers: &[(String, RemedyPlan)], fork: Option<&str>) -> String {
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
/// Whenever the call narrows, the lead states what the acceptance costs on this surface, and adds
/// the branch alternative only where [`fork_advice`] finds one to give.
pub fn block_feedback(
    raw: &RawBlock,
    planned: &PlannedBlock,
    offers: &[(String, RemedyPlan)],
    surface: FeedbackSurface,
) -> String {
    let fork = fork_advice(raw, planned, offers, surface);
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
        // ("in a later response"). Every surface hears what the acceptance costs and how far the
        // cost reaches; only the branch alternative is conditional on being actionable, because a
        // surface that cannot fork can still reorder its own work. The option-preserving move
        // leads and acceptance is the terminal clause: an imperative read first is acted on first,
        // and an acceptance taken before the label-requiring work strands that work for good.
        match fork {
            // Fork leads: it is the reversible choice. Accepting folds the restriction into this
            // trajectory for good — no authority widens an audience, and trust never rises — so
            // an acceptance taken for one step can strand every later step that needed the label.
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
        // Mixed block: every offered plan both covers the gaps and accepts the narrowing, so it is
        // round-gated (next response) and carries the same acceptance cost as a pure narrowing.
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

/// The cost an acceptance carries, per the surface it lands on: a root folds the restriction into
/// the session for good, a child only into its branch. Stated wherever an acceptance is offered —
/// it is a fact about narrowing, not about forkability, so no surface may be left to infer it.
fn acceptance_cost(surface: FeedbackSurface) -> &'static str {
    match surface {
        FeedbackSurface::Root { .. } => "permanent for this session",
        FeedbackSurface::Child => "permanent for this branch; the parent session is unaffected",
    }
}

/// Render the feedback after an authority declined one offer: the denial, then the remaining
/// sibling plans as the same typed payload shape (no gaps re-listed — the block is unchanged). A
/// sibling that carries an acceptance re-offers the narrowing, so its cost is named again here.
pub fn denial_feedback(remaining: &[(String, RemedyPlan)], surface: FeedbackSurface) -> String {
    if remaining.is_empty() {
        return "the authority declined to authorize this call; no alternative plan remains".to_string();
    }
    #[derive(Serialize)]
    struct WireRemaining<'a> {
        plans: Vec<WirePlan<'a>>,
    }
    let plans = wire_plans(remaining);
    let accepts = plans.iter().any(|plan| plan.accepts_narrowing);
    let payload = serde_json::to_string(&WireRemaining { plans })
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

/// Render a pending-cast acceptance offer (D2): the withheld result's exact narrowing, typed.
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
        let payload = parsed(&denial_feedback(&remaining, FeedbackSurface::Root { can_fork: false }));
        assert_eq!(payload["plans"].as_array().unwrap().len(), 1);
        assert_eq!(payload["plans"][0]["plan_id"], "remedy-1");
        // An exhausted cohort renders no payload line at all.
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
