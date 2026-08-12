//! G12 model-facing block feedback: the exact typed decision state, rendered once, shared by the
//! runtime turn-drive and the SDK so both surfaces are byte-identical.

use appa_engine::authority::Hint;
use appa_engine::check::{Gap, Narrowing, RawBlock, UnestablishedFact};
use appa_engine::label::Dimension;
use appa_engine::plan::{ExecutableRemedyPlan, PlannedBlock, RemedyPlan};
use appa_engine::projection::Views;
use appa_engine::registry::Registry;
use appa_engine::value::Provenance;
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
    #[serde(skip_serializing_if = "Option::is_none")]
    sanitizes: Option<WireSanitize<'a>>,
    accepts_narrowing: bool,
}

#[derive(Serialize)]
struct WireSanitize<'a> {
    sanitizer: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<&'a str>,
}

#[derive(Serialize)]
struct WireRuling<'a> {
    authority: &'a str,
    covers: &'a [Gap],
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<&'a str>,
}

#[derive(Serialize)]
struct WireRedispatch<'a> {
    tool: &'a str,
    clears: &'a [Gap],
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
    /// The values whose consumed dimension no registered cast could establish, named
    /// by a **feedback-local ordinal** plus the coarse origin kind. Deliberately non-correlating:
    /// the transcript stores no dispatch↔tool-call map, so no internal id (`ValueId`,
    /// `DispatchId`) crosses to the model — the ordinal indexes this rendered list and nothing
    /// else.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unestablished: Vec<WireUnestablished>,
    remedy_plans: Vec<WireRemedyPlan<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fork: Option<&'a str>,
}

/// One unestablished entry on the wire: which rendered entry (ordinal), the source's unresolved
/// dimensions (`CHK-16` names the source once, with all of them), and what kind of value carries
/// it. The ordinal duplicates the array index deliberately — it is the stable key the model can
/// quote back, and the only identity the entry carries.
#[derive(Serialize)]
struct WireUnestablished {
    ordinal: usize,
    dimensions: Vec<Dimension>,
    source_kind: &'static str,
}

fn wire_unestablished(facts: &[UnestablishedFact], views: &Views) -> Vec<WireUnestablished> {
    facts
        .iter()
        .enumerate()
        .map(|(ordinal, fact)| WireUnestablished {
            ordinal,
            dimensions: fact.dimensions.iter().copied().collect(),
            source_kind: match views
                .value_provenance(fact.value)
                .expect("unestablished facts name admitted values of this append-only family")
            {
                Provenance::UserInput => "user_input",
                Provenance::ToolResult { .. } => "tool_result",
                Provenance::ChildReturn { .. } => "child_return",
            },
        })
        .collect()
}

/// Render the offered plans, resolving each named authority's and sanitizer's hint from the
/// registry. Hints are read here rather than carried on the plan because a plan is an assignment of
/// mandates, not a message: the registry stays the one place the operator's prose lives.
fn wire_plans<'a>(registry: &'a Registry, offers: &'a [(String, ExecutableRemedyPlan)]) -> Vec<WirePlan<'a>> {
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
                    hint: registry
                        .authority(&required.authority)
                        .and_then(|authority| authority.hint.as_ref())
                        .map(Hint::as_str),
                })
                .collect(),
            sanitizes: plan.steps.iter().find_map(|step| match step {
                appa_engine::plan::RemedyStep::Sanitize(name) => Some(WireSanitize {
                    sanitizer: name.as_str(),
                    hint: registry
                        .sanitizer(name)
                        .and_then(|sanitizer| sanitizer.hint.as_ref())
                        .map(Hint::as_str),
                }),
                _ => None,
            }),
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
    // No fork advice while a fact is missing: fork seeding refuses an Unknown parent, so the
    // advice would name a move the engine then refuses.
    if !raw.unestablished.is_empty() {
        return None;
    }
    if !matches!(surface, FeedbackSurface::Root { can_fork: true }) || raw.narrowing.is_none() || offers.is_empty() {
        return None;
    }
    planned.fork_advice.as_deref()
}

fn free_sanitize(offers: &[(String, ExecutableRemedyPlan)]) -> Option<(&str, &str)> {
    offers.iter().find_map(|(handle, plan)| {
        if plan
            .steps
            .iter()
            .any(|step| matches!(step, appa_engine::plan::RemedyStep::Accept(_)))
        {
            return None;
        }
        plan.steps.iter().find_map(|step| match step {
            appa_engine::plan::RemedyStep::Sanitize(name) => Some((handle.as_str(), name.as_str())),
            _ => None,
        })
    })
}

fn payload(
    registry: &Registry,
    raw: &RawBlock,
    planned: &PlannedBlock,
    offers: &[(String, ExecutableRemedyPlan)],
    fork: Option<&str>,
    views: &Views,
) -> String {
    let mut remedy_plans: Vec<WireRemedyPlan> = wire_plans(registry, offers)
        .into_iter()
        .map(WireRemedyPlan::Executable)
        .collect();
    remedy_plans.extend(planned.plans.iter().filter_map(|plan| match plan {
        RemedyPlan::Redispatch(redispatch) => Some(WireRemedyPlan::Redispatch(WireRedispatch {
            tool: redispatch.tool().as_str(),
            clears: redispatch.clears(),
        })),
        RemedyPlan::Executable(_) => None,
    }));
    let block = WireBlock {
        requirement_gaps: &raw.requirement_gaps,
        narrowing: raw.narrowing.as_ref(),
        unestablished: wire_unestablished(&raw.unestablished, views),
        remedy_plans,
        fork,
    };
    serde_json::to_string(&block).expect("the block payload serializes: engine types are Serialize")
}

/// Render a block's model-facing feedback: the fixed prose lead for its decision kind, then the
/// exact typed payload. A pure narrowing (no requirement gap) presents as an acceptance — the
/// agent's own step, no authority involved. Anything with gaps presents as a block to remedy.
pub fn block_feedback(
    registry: &Registry,
    raw: &RawBlock,
    planned: &PlannedBlock,
    offers: &[(String, ExecutableRemedyPlan)],
    surface: FeedbackSurface,
    views: &Views,
) -> String {
    let fork = fork_advice(raw, planned, offers, surface);
    let free = free_sanitize(offers);
    let lead = if !raw.unestablished.is_empty() {
        if offers.is_empty() {
            "blocked: a value this call depends on has a label dimension no registered cast could establish. No plan applies — a fact clears this, not a ruling. The unestablished values are named in the payload; work that does not consume them still flows"
        } else {
            "blocked: some values carry a label dimension no registered cast could establish, and the offered plans stay gated until those facts land. The unestablished values are named in the payload alongside the plans for the block's other gaps"
        }
    } else if offers.is_empty() {
        if planned
            .plans
            .iter()
            .any(|plan| matches!(plan, RemedyPlan::Redispatch(_)))
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
    } else if let Some((handle, sanitizer)) = free {
        return format!(
            "{}\n{}",
            match raw.requirement_gaps.is_empty() {
                true => format!(
                    "narrowing: this call restricts the trajectory label, but one plan avoids that. Execute plan {handle} with execute_remedy_plan: it withholds the raw result and admits the {sanitizer} derivation instead, which this label absorbs without moving. Read that plan's sanitizes.hint to see what the derivation drops; if you need what it drops, accept the narrowing instead — permanently — with one of the other plans"
                ),
                false => format!(
                    "blocked by policy; the requirement gaps still need a ruling, but the narrowing need not be accepted. Plan {handle} withholds the raw result and admits the {sanitizer} derivation, which this label absorbs without moving; read its sanitizes.hint to see what that drops. Every other offered plan accepts the narrowing as well as covering the gaps, permanently"
                ),
            },
            payload(registry, raw, planned, offers, fork, views)
        );
    } else if raw.requirement_gaps.is_empty() {
        match fork {
            Some(_) => {
                "narrowing: this call restricts the trajectory label, and acceptance is permanent for this session — no authority widens an audience, and trust never rises. Fork the restricting work into a child session to keep this session's label (the child must finish the work or return a sanitized derivation — pulling the restricted value back raw costs this session the same narrowing at the merge); or run every later step that needs the current label first, then accept with execute_remedy_plan in a later response"
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
                "blocked by policy; every offered plan also accepts this call's narrowing, permanently for this session. Fork the restricting work into a child session to keep this session's label (the child must finish the work or return a sanitized derivation — pulling the restricted value back raw costs this session the same narrowing at the merge); or run every later step that needs the current label first, then execute a plan with execute_remedy_plan in a later response"
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
    format!("{lead}\n{}", payload(registry, raw, planned, offers, fork, views))
}

/// Render the executor's preflight refusal: an offered plan was invoked while a consumed
/// dimension stays unestablished. Nothing is consumed and no authority is consulted — the offers
/// stand, gated until the named facts land (`CHK-16`: a fact clears them, never a plan).
pub fn unestablished_gate_feedback(facts: &[UnestablishedFact], views: &Views) -> String {
    format!(
        "this plan stays gated: values the call depends on have a label dimension no registered cast could establish, and no ruling or acceptance may land until those facts do. The offer remains available; the unestablished values are named in the payload\n{}",
        unestablished_payload(facts, views)
    )
}

fn unestablished_payload(facts: &[UnestablishedFact], views: &Views) -> String {
    #[derive(Serialize)]
    struct WireUnestablishedOnly {
        unestablished: Vec<WireUnestablished>,
    }
    serde_json::to_string(&WireUnestablishedOnly {
        unestablished: wire_unestablished(facts, views),
    })
    .expect("the unestablished payload serializes: engine types are Serialize")
}

/// Render the merge refusal for a child return whose fold has unestablished dimensions:
/// the values are named, and no plans are offered — a fact clears the entry, nothing
/// the child executes. The child keeps its structural moves, as on its terminal block.
pub fn unestablished_return_feedback(facts: &[UnestablishedFact], views: &Views) -> String {
    format!(
        "the return cannot merge: a label dimension of this branch's result is one no registered cast could establish — a fact clears this, nothing you execute, so no plans are offered. The unestablished values are named in the payload. Complete what this branch still can, then return null after side-effect-only work\n{}",
        unestablished_payload(facts, views)
    )
}

fn acceptance_cost(surface: FeedbackSurface) -> &'static str {
    match surface {
        FeedbackSurface::Root { .. } => "permanent for this session",
        FeedbackSurface::Child => "permanent for this branch; the parent session is unaffected",
    }
}

fn offers_tail(registry: &Registry, offers: &[(String, ExecutableRemedyPlan)], surface: FeedbackSurface) -> String {
    #[derive(Serialize)]
    struct WireRemaining<'a> {
        remedy_plans: Vec<WirePlan<'a>>,
    }
    let plans = wire_plans(registry, offers);
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
    format!("{cost}\n{payload}")
}

/// Render the feedback after an authority declined one offer: the denial, then the remaining
/// sibling plans as the same typed payload shape. A sibling that carries an acceptance re-offers
/// the narrowing, so its cost is named again here.
pub fn denial_feedback(
    registry: &Registry,
    remaining: &[(String, ExecutableRemedyPlan)],
    surface: FeedbackSurface,
) -> String {
    if remaining.is_empty() {
        return "the authority declined to authorize this call; no alternative plan remains".to_string();
    }
    format!(
        "the authority declined to authorize this call; alternatives remain{}",
        offers_tail(registry, remaining, surface)
    )
}

/// Render the feedback after a consult returned no answer: nothing was consumed, so
/// every offer stands — the consulted plan included — and the same `plan_id` may be executed
/// again. A consult with no answer is not a denial and does not stick.
pub fn no_answer_feedback(
    registry: &Registry,
    handle: &str,
    offers: &[(String, ExecutableRemedyPlan)],
    surface: FeedbackSurface,
) -> String {
    format!(
        "the authority returned no answer; nothing was consumed — plan_id \"{handle}\" stands and may be executed again{}",
        offers_tail(registry, offers, surface)
    )
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
    use appa_engine::authority::{Authority, Mandate, Sanitizer, SanitizerPoints, Scope, Transition};
    use appa_engine::fact::{EffectKind, Revision};
    use appa_engine::label::{Audience, Dim, EstablishedLabel, Label, ReaderId, Trust};
    use appa_engine::names::{AuthorityName, MarkName, SanitizerName};
    use appa_engine::plan::{PlanId, RedispatchPlan, RemedyStep, RequiredRuling};
    use appa_engine::projection::Projection;
    use appa_engine::registry::{RegistryConfig, TrustChain};
    use appa_engine::value::{ToolName, TrajectoryId};

    fn with_empty_views<R>(render: impl FnOnce(&Views) -> R) -> R {
        let projection = Projection::build(&[], Revision::ZERO);
        render(&projection.view(&TrajectoryId::new("session")))
    }

    fn registry() -> Registry {
        let authority = |name: &str| Authority {
            name: AuthorityName::new(name),
            mandate: Mandate {
                trust_ceiling: Some(Trust::new(1)),
                reader_ceiling: Some(Audience::Public),
                waivers: vec![EffectKind::new("egress")],
                attends: vec![MarkName::new("signoff")],
            },
            scope: Scope::default(),
            hint: Some(Hint::new(format!("what {name} is for"))),
        };
        let config = RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![],
            authorities: vec![authority("officer"), authority("officer-a"), authority("officer-b")],
            sanitizers: vec![Sanitizer {
                name: SanitizerName::new("pii"),
                on: SanitizerPoints {
                    input: false,
                    output: true,
                },
                transition: Transition::Audience {
                    from_includes: Audience::restricted([ReaderId::new("internal")]),
                    to: Audience::Public,
                },
                hint: Some(Hint::new("drops personal details")),
            }],
            casts: vec![],
        };
        let profile = appa_engine::profile::ProfileDeclaration {
            starting_label: appa_engine::profile::neutral_starting_label(&config.trust_chain),
            context_control: true,
            dispatch: appa_engine::profile::ExecutorClass::Enforced,
            executor_exceptions: Default::default(),
            confined_results: Default::default(),
            confined_child_return: true,
            provider_surfaces: Default::default(),
            binding: appa_engine::profile::BindingMode::Harness,
        };
        appa_engine::engine::Engine::open(appa_engine::profile::DeploymentPolicy {
            registry: config,
            planner_cap: appa_engine::registry::PlannerCap::default(),
            dialect: appa_engine::profile::PolicyDialectVersion::new(1),
            child_return: appa_engine::fact::ReturnPolicy::Raw,
            profile,
        })
        .expect("the fixture registry loads")
        .registry()
        .clone()
    }

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
            from: EstablishedLabel::new(Trust::new(1), Audience::Public),
            to: EstablishedLabel::new(Trust::new(0), Audience::restricted([ReaderId::new("internal")])),
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
            unestablished: Vec::new(),
        };
        let planned = PlannedBlock {
            raw: raw.clone(),
            plans: vec![RemedyPlan::Executable(plan_with("officer", every_gap()))],
            fork_advice: None,
        };
        let offers = vec![("remedy-7".to_string(), plan_with("officer", every_gap()))];
        let payload = with_empty_views(|views| {
            parsed(&block_feedback(
                &registry(),
                &raw,
                &planned,
                &offers,
                FeedbackSurface::Root { can_fork: true },
                views,
            ))
        });

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
        assert_eq!(payload["remedy_plans"][0]["rulings"][0]["hint"], "what officer is for");
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
            unestablished: Vec::new(),
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
        let feedback = with_empty_views(|views| {
            block_feedback(
                &registry(),
                &raw,
                &planned,
                &offers,
                FeedbackSurface::Root { can_fork: true },
                views,
            )
        });
        let payload = parsed(&feedback);
        assert_eq!(payload["requirement_gaps"].as_array().unwrap().len(), 0);
        assert_eq!(payload["narrowing"]["from"]["trust"], 1);
        assert_eq!(payload["narrowing"]["to"]["trust"], 0);
        assert_eq!(payload["narrowing"]["to"]["audience"]["Restricted"][0], "internal");
        assert_eq!(payload["remedy_plans"][0]["accepts_narrowing"], true);
        assert_eq!(payload["remedy_plans"][0]["rulings"].as_array().unwrap().len(), 0);
        assert_eq!(payload["fork"], "confine the loss");
        let payload = with_empty_views(|views| {
            parsed(&block_feedback(
                &registry(),
                &raw,
                &planned,
                &offers,
                FeedbackSurface::Root { can_fork: false },
                views,
            ))
        });
        assert!(payload.get("fork").is_none());
        let payload = with_empty_views(|views| {
            parsed(&block_feedback(
                &registry(),
                &raw,
                &planned,
                &offers,
                FeedbackSurface::Child,
                views,
            ))
        });
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
            unestablished: Vec::new(),
        };
        let mut plan = plan_with("officer", vec![floor]);
        plan.steps.push(RemedyStep::Accept(narrowing()));
        let planned = PlannedBlock {
            raw: raw.clone(),
            plans: vec![RemedyPlan::Executable(plan.clone())],
            fork_advice: Some("confine the loss".to_string()),
        };
        let offers = vec![("remedy-0".to_string(), plan)];
        let payload = with_empty_views(|views| {
            parsed(&block_feedback(
                &registry(),
                &raw,
                &planned,
                &offers,
                FeedbackSurface::Root { can_fork: true },
                views,
            ))
        });
        assert_eq!(payload["fork"], "confine the loss");
        assert_eq!(payload["remedy_plans"][0]["accepts_narrowing"], true);
        let payload = with_empty_views(|views| {
            parsed(&block_feedback(
                &registry(),
                &raw,
                &planned,
                &offers,
                FeedbackSurface::Root { can_fork: false },
                views,
            ))
        });
        assert!(payload.get("fork").is_none());
        let payload = with_empty_views(|views| {
            parsed(&block_feedback(
                &registry(),
                &raw,
                &planned,
                &offers,
                FeedbackSurface::Child,
                views,
            ))
        });
        assert!(payload.get("fork").is_none());
        let payload = with_empty_views(|views| {
            parsed(&block_feedback(
                &registry(),
                &raw,
                &planned,
                &[],
                FeedbackSurface::Root { can_fork: true },
                views,
            ))
        });
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
            unestablished: Vec::new(),
        };
        let planned = PlannedBlock {
            raw: raw.clone(),
            plans: vec![
                RemedyPlan::Executable(plan_with("officer-a", vec![floor.clone()])),
                RemedyPlan::Executable(plan_with("officer-b", vec![floor.clone()])),
                RemedyPlan::Redispatch(
                    RedispatchPlan::new(
                        ToolName::new("backup"),
                        vec![Gap::Prior(EffectKind::new("backup.done"))],
                    )
                    .expect("a prior claim is a valid redispatch"),
                ),
            ],
            fork_advice: Some("advisory".to_string()),
        };
        let offers = vec![
            ("remedy-0".to_string(), plan_with("officer-a", vec![floor.clone()])),
            ("remedy-1".to_string(), plan_with("officer-b", vec![floor.clone()])),
        ];
        let payload = with_empty_views(|views| {
            parsed(&block_feedback(
                &registry(),
                &raw,
                &planned,
                &offers,
                FeedbackSurface::Root { can_fork: true },
                views,
            ))
        });
        let plans = payload["remedy_plans"].as_array().unwrap();
        assert_eq!(plans.len(), 3);
        assert_eq!(plans[0]["plan_id"], "remedy-0");
        assert_eq!(plans[0]["rulings"][0]["authority"], "officer-a");
        assert_eq!(plans[1]["plan_id"], "remedy-1");
        assert_eq!(plans[1]["rulings"][0]["authority"], "officer-b");
        assert_eq!(plans[2]["tool"], "backup");
        assert_eq!(plans[2]["clears"].as_array().unwrap().len(), 1);
        assert!(plans[2].get("enables_path").is_none());
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
        let payload = with_empty_views(|views| {
            parsed(&block_feedback(
                &registry(),
                &raw,
                &none_planned,
                &[],
                FeedbackSurface::Root { can_fork: true },
                views,
            ))
        });
        let plans = payload["remedy_plans"].as_array().unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0]["tool"], "backup");
    }

    #[test]
    fn a_denial_relists_the_surviving_siblings() {
        let floor = Gap::TrustFloor {
            required: Trust::new(1),
            actual: Trust::new(0),
        };
        let remaining = vec![("remedy-1".to_string(), plan_with("officer-b", vec![floor]))];
        let payload = parsed(&denial_feedback(
            &registry(),
            &remaining,
            FeedbackSurface::Root { can_fork: false },
        ));
        assert_eq!(payload["remedy_plans"].as_array().unwrap().len(), 1);
        assert_eq!(payload["remedy_plans"][0]["plan_id"], "remedy-1");
        assert!(!denial_feedback(&registry(), &[], FeedbackSurface::Root { can_fork: false }).contains('\n'));
    }

    #[test]
    fn a_cast_offer_carries_the_exact_narrowing_and_handle() {
        let payload = parsed(&cast_offer_feedback(
            "remedy-3",
            &narrowing(),
            FeedbackSurface::Root { can_fork: false },
        ));
        assert_eq!(payload["plan_id"], "remedy-3");
        assert_eq!(payload["narrowing"]["from"]["trust"], 1);
        assert_eq!(payload["narrowing"]["to"]["audience"]["Restricted"][0], "internal");
    }

    #[test]
    fn unestablished_entries_serialize_ordinals_and_origins_and_nothing_internal() {
        use appa_engine::check::UnestablishedFact;
        use appa_engine::fact::Fact;
        use appa_engine::label::Dimension;
        use appa_engine::value::{ChildReturnId, DispatchId, LabeledValue, Provenance, ValueBody, ValueId};

        let session = TrajectoryId::new("session");
        let child = TrajectoryId::new("child");
        let unknown = Label::new(Dim::Unknown, Dim::Known(Audience::Public));
        let admit = |provenance: Provenance| Fact::ValueAdmitted {
            trajectory: session.clone(),
            value: LabeledValue::new(ValueBody::new("body"), unknown.clone()),
            provenance,
        };
        let dispatch = DispatchId::new(
            session.clone(),
            crate::common::test_call("scan", serde_json::json!({})).digest(),
            0,
        );
        let log = vec![
            admit(Provenance::UserInput),
            admit(Provenance::ToolResult { dispatch }),
            admit(Provenance::ChildReturn {
                child: child.clone(),
                id: ChildReturnId::new(child, 0),
            }),
        ];
        let projection = Projection::build(&log, Revision::new(log.len() as u64));
        let views = projection.view(&session);
        let facts: Vec<UnestablishedFact> = (0..3)
            .map(|id| UnestablishedFact {
                value: ValueId::new(id),
                dimensions: [Dimension::Trust].into(),
            })
            .collect();

        let raw = RawBlock {
            requirement_gaps: Vec::new(),
            narrowing: None,
            unestablished: facts,
        };
        let planned = PlannedBlock {
            raw: raw.clone(),
            plans: vec![],
            fork_advice: None,
        };
        let payload = parsed(&block_feedback(
            &registry(),
            &raw,
            &planned,
            &[],
            FeedbackSurface::Root { can_fork: false },
            &views,
        ));
        let entries = payload["unestablished"].as_array().expect("unestablished entries");
        assert_eq!(entries.len(), 3);
        for (ordinal, kind) in ["user_input", "tool_result", "child_return"].iter().enumerate() {
            assert_eq!(entries[ordinal]["ordinal"], ordinal);
            assert_eq!(entries[ordinal]["dimensions"][0], "Trust");
            assert_eq!(entries[ordinal]["source_kind"], *kind);
            assert!(entries[ordinal].get("value").is_none());
            assert!(entries[ordinal].get("dispatch").is_none());
        }

        let raw = RawBlock {
            requirement_gaps: Vec::new(),
            narrowing: Some(narrowing()),
            unestablished: Vec::new(),
        };
        let planned = PlannedBlock {
            raw: raw.clone(),
            plans: vec![],
            fork_advice: None,
        };
        let payload = parsed(&block_feedback(
            &registry(),
            &raw,
            &planned,
            &[],
            FeedbackSurface::Child,
            &views,
        ));
        assert!(payload.get("unestablished").is_none());
    }
}
