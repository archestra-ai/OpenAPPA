//! Atomic plan execution: turning gathered rulings into the one indivisible batch that admits a
//! blocked dispatch.
//!
//! Executing a remedy plan lands, in a single [`FactBatch`] on the current [`Revision`], every
//! ruling that covers a requirement gap, the agent's acceptance of any narrowing, and the
//! `DispatchOpened` — nothing can intervene between approval and dispatch. The engine re-derives the
//! block from the current views (never trusting a stale one), verifies **each ruling stays within
//! its authority's mandate** and **every gap is covered**, and enforces the one structural
//! response-sink bar: **no end-user issuer covers a response-sink gap** (an in-band self-confirmation
//! on the channel being released is not a check). Rulings are bound to the exact `DispatchId` —
//! call-scoped and single-use (a repeat call is a new occurrence and takes a fresh ruling).
//!
//! **Trust boundary (important).** A [`Ruling`] *represents* an authority's decision that the
//! runtime — the trusted mediator of authorities — relays; the engine does not witness the external
//! approval act and cannot cryptographically verify it (an authority is a human, model, or regex the
//! runtime talks to). What the engine *does* guarantee bounds even a misbehaving relayer: a ruling
//! can never exceed the named authority's **static mandate** (from the immutable registry), so no
//! caller can conjure a power no registered authority holds — the declared mandates are the whole
//! policy envelope. The `DispatchId` binding additionally stops an *honest* runtime from
//! accidentally reusing a ruling object across calls or occurrences. It is **not** a defense against
//! a compromised runtime reconstructing the binding from public state — no check in a pure engine
//! called by that runtime could be, since the runtime supplies the inputs. Authenticity of the
//! approval is the runtime's responsibility; the engine's job is the mandate envelope and the audit
//! trail (every ruling is a logged fact).

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::check::{self, CheckOutcome, Gap};
use crate::engine::opened_dispatch;
use crate::fact::{Fact, FactBatch};
use crate::label::Label;
use crate::names::AuthorityName;
use crate::plan::{self, covers_gap};
use crate::projection::Views;
use crate::registry::Registry;
use crate::value::{DispatchId, Provenance, ResolvedCall, ValueId};

/// Who exercised a ruling. The mandate is the named authority's; the issuer records who pressed the
/// button, because one release — the response sink — bars the end user structurally.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Issuer {
    /// An authority ruling in the ordinary way.
    Authority,
    /// The end user acting under an authority's mandate. Barred from response-sink gaps.
    EndUser,
}

/// The sink a dispatch releases to. Only [`Sink::Response`] — the assistant's own reply to the user —
/// carries the end-user bar; every tool sink is [`Sink::Tool`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sink {
    Tool,
    Response,
}

/// A ruling the runtime gathered from an authority for one **specific pending dispatch**: the exact
/// [`DispatchId`] (trajectory + canonical digest + occurrence) it was approved for, the mandate it
/// acts under, who exercised it, the gaps it claims to cover, and the review it was issued over.
/// Binding the whole dispatch — not just the digest — makes a ruling both call-scoped
/// (`transfer(A,$1)` cannot admit `transfer(B,$100)`) and single-use (a repeat identical call is a
/// new occurrence and takes a fresh ruling).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ruling {
    pub dispatch: DispatchId,
    pub authority: AuthorityName,
    pub issuer: Issuer,
    pub covers: Vec<Gap>,
    pub reviewed: AuthorityReview,
}

/// The context an Authority reviewed when it ruled — persisted with the Ruling so the log carries
/// exactly what was put to the reviewer, not merely a digest of hidden state: the reviewed tool,
/// the trajectory label fold at review time, and, per referenced argument Value, its label and
/// provenance. Argument and Value bytes never appear here (they never cross to an Authority);
/// recipients live in the ruling's `covers` (`Gap::Includes`), never duplicated. Plan execution
/// re-validates this context against the live views before persisting it — a relayer cannot land a
/// review naming a different tool, a false fold, or a dangling reference.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityReview {
    pub tool: crate::value::ToolName,
    pub trajectory_label: Label,
    pub arg_refs: Vec<ReviewedRef>,
}

/// One referenced argument Value as the Authority reviewed it: its id, label, and provenance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewedRef {
    pub value: ValueId,
    pub label: Label,
    pub provenance: Provenance,
}

/// Why a plan could not execute.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PlanError {
    #[error("no contract registered for tool {0}")]
    UnknownTool(String),
    #[error("the call is not blocked — dispatch it directly")]
    NotBlocked,
    #[error("the call has an unresolved dimension — cast it first")]
    Unresolved,
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
    #[error("an end-user ruling cannot cover a response-sink gap")]
    EndUserResponseSink,
    #[error("the supplied rulings do not realize the chosen plan's grouped assignment exactly")]
    RulingAssignmentMismatch,
    #[error("a ruling's recorded review does not match the live state it would admit")]
    ReviewMismatch,
}

/// Execute a remedy plan: verify coverage and the issuer bar, then emit the atomic
/// rulings + acceptance + dispatch batch. See the module docs.
pub(crate) fn execute_plan(
    registry: &Registry,
    views: &Views,
    chosen: &plan::RemedyPlan,
    call: &ResolvedCall,
    rulings: &[Ruling],
    sink: Sink,
) -> Result<FactBatch, PlanError> {
    let contract = registry
        .tool(call.tool())
        .ok_or_else(|| PlanError::UnknownTool(call.tool().as_str().to_string()))?;

    let block = match check::evaluate(registry, contract, views, call) {
        CheckOutcome::Block(block) => block,
        CheckOutcome::Allow => return Err(PlanError::NotBlocked),
        CheckOutcome::Unresolved(_) => return Err(PlanError::Unresolved),
    };

    // The chosen plan must be one this block offers at the current revision, matched **by value**
    // — a stale ordinal cannot retarget a different assignment after state drift.
    let planned = plan::plan(registry, views, call, &block);
    if !planned.plans.iter().any(|offered| offered == chosen) {
        return Err(PlanError::UnknownPlan(chosen.id.value()));
    }
    let plan = chosen.id;

    // The supplied rulings must realize exactly the chosen plan's grouped assignment: one ruling
    // per required entry, with precisely its authority and covers, and nothing extra — overlapping
    // mandates cannot flatten or reroute the offered grouping.
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

    // The recorded review must match the live state this execution admits: the reviewed tool is
    // this call's, the fold is the current one, and every reviewed reference resolves to a live
    // value of this branch with the label and provenance the authority saw. A relayer cannot land
    // a review of some other state; a race that moved what was reviewed refuses here and the
    // authority is consulted afresh.
    let live_label = views.current_label();
    for ruling in rulings {
        if ruling.reviewed.tool != contract.name || ruling.reviewed.trajectory_label != live_label {
            return Err(PlanError::ReviewMismatch);
        }
        // Completeness both ways: the review names exactly the call's argument references — an
        // omitted reference is as false a review as a fabricated one.
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

    // The exact dispatch this execution will open — including its occurrence. Every ruling must be
    // bound to it, so a ruling gathered for a different call, or for a prior occurrence of this one,
    // cannot admit it.
    let (dispatch, dispatch_opened) = opened_dispatch(registry, contract, views, call);

    // Each ruling must be scoped to this exact dispatch, claim only gaps the block carries, stay
    // within its authority's mandate, and — the one response-sink bar — an end-user issuer may never
    // carry a covering ruling for a response-sink release.
    for ruling in rulings {
        if ruling.dispatch != dispatch {
            return Err(PlanError::RulingCallMismatch);
        }
        let authority = registry
            .authority(&ruling.authority)
            .ok_or_else(|| PlanError::UnknownAuthority(ruling.authority.as_str().to_string()))?;
        if sink == Sink::Response && ruling.issuer == Issuer::EndUser && !ruling.covers.is_empty() {
            return Err(PlanError::EndUserResponseSink);
        }
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
            issuer: ruling.issuer,
            covers: ruling.covers.clone(),
            reviewed: ruling.reviewed.clone(),
        });
    }
    // The acceptance records the narrowing the *chosen plan* carries — what the agent was shown
    // and matched by value above — never a re-derived one. Post-match the two provably coincide
    // (live plans embed the live narrowing), so this is the same value with the honest provenance.
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

    /// A tool requiring `trusted`; an officer authority that can endorse up to trusted.
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
            output_sanitizer: None,
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

    /// The review these tests' honest relayer records: `wire` over the suspicious/public fold every
    /// test log folds to, no argument references. Execution validates this against the live views,
    /// so the fixture must state the real state, not a placeholder.
    fn top_review() -> AuthorityReview {
        AuthorityReview {
            tool: ToolName::new("wire"),
            trajectory_label: known(SUSPICIOUS, Audience::Public),
            arg_refs: vec![],
        }
    }

    /// The dispatch every ruling in these tests is scoped to — `wire({})`, first occurrence in `t`.
    fn wire_dispatch() -> DispatchId {
        DispatchId::new(traj(), call("wire", json!({})).digest(), 0)
    }

    /// Execute against the block's first offered plan (the all-first-choices assignment).
    fn run(
        registry: &Registry,
        log: &[Fact],
        call: &ResolvedCall,
        rulings: &[Ruling],
        sink: Sink,
    ) -> Result<FactBatch, PlanError> {
        let projection = Projection::build(log, Revision::new(log.len() as u64));
        let trajectory = traj();
        let views = projection.view(&trajectory);
        let chosen = offered_plan(registry, &views, call);
        execute_plan(registry, &views, &chosen, call, rulings, sink)
    }

    /// The first plan the live state offers for `call`, or a fabricated never-offered one when the
    /// state offers none (so refusal paths still exercise the value match).
    fn offered_plan(registry: &Registry, views: &Views, call: &ResolvedCall) -> plan::RemedyPlan {
        let planned = match check::evaluate(registry, registry.tool(call.tool()).unwrap(), views, call) {
            CheckOutcome::Block(block) => plan::plan(registry, views, call, &block),
            _ => {
                return plan::RemedyPlan {
                    id: plan::PlanId::new(0),
                    steps: vec![],
                    required: vec![],
                };
            }
        };
        planned.plans.first().cloned().unwrap_or(plan::RemedyPlan {
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
            issuer: Issuer::Authority,
            reviewed: top_review(),
            covers: vec![floor_gap()],
        };
        let batch = run(&registry, &log, &call("wire", json!({})), &[ruling], Sink::Tool).unwrap();
        // One ruling then the dispatch, in one batch on the current revision.
        assert!(matches!(batch.facts[0], Fact::Ruling { .. }));
        assert!(matches!(batch.facts.last().unwrap(), Fact::DispatchOpened { .. }));
    }

    #[test]
    fn ruling_approved_for_another_call_is_rejected() {
        // A ruling whose digest was approved against a different rendered call cannot admit this one.
        let registry = registry();
        let log = vec![user_value(known(SUSPICIOUS, Audience::Public))];
        let ruling = Ruling {
            dispatch: DispatchId::new(traj(), call("wire", json!({ "to": "elsewhere" })).digest(), 0),
            authority: AuthorityName::new("officer"),
            issuer: Issuer::Authority,
            reviewed: top_review(),
            covers: vec![floor_gap()],
        };
        assert_eq!(
            run(
                &registry,
                &log,
                &call("wire", json!({})),
                std::slice::from_ref(&ruling),
                Sink::Tool
            ),
            Err(PlanError::RulingCallMismatch)
        );
    }

    #[test]
    fn ruling_cannot_replay_across_occurrences() {
        // wire was already dispatched once (occurrence 0); the next dispatch is occurrence 1, so a
        // ruling still bound to occurrence 0 cannot admit it — one review is one review.
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
            issuer: Issuer::Authority,
            reviewed: top_review(),
            covers: vec![floor_gap()],
        };
        assert_eq!(
            run(&registry, &log, &wire, std::slice::from_ref(&stale), Sink::Tool),
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
            issuer: Issuer::Authority,
            reviewed: top_review(),
            covers: vec![floor_gap()],
        };
        let projection = Projection::build(&log, Revision::new(log.len() as u64));
        let trajectory = traj();
        // A plan value the live block does not offer — same steps shape, wrong assignment — is
        // refused by the value match, whatever its ordinal claims.
        let fabricated = plan::RemedyPlan {
            id: plan::PlanId::new(999),
            steps: vec![plan::RemedyStep::Authorize(AuthorityName::new("officer"))],
            required: vec![plan::RequiredRuling {
                authority: AuthorityName::new("officer"),
                covers: vec![],
            }],
        };
        assert_eq!(
            execute_plan(
                &registry,
                &projection.view(&trajectory),
                &fabricated,
                &call("wire", json!({})),
                std::slice::from_ref(&ruling),
                Sink::Tool,
            ),
            Err(PlanError::UnknownPlan(999))
        );
    }

    #[test]
    fn uncovered_gap_is_rejected() {
        let registry = registry();
        let log = vec![user_value(known(SUSPICIOUS, Audience::Public))];
        // No rulings supplied for a one-ruling plan → the assignment is not realized.
        assert!(matches!(
            run(&registry, &log, &call("wire", json!({})), &[], Sink::Tool),
            Err(PlanError::RulingAssignmentMismatch)
        ));
    }

    #[test]
    fn ruling_gathered_for_a_different_call_does_not_transfer() {
        // The ruling claims a floor gap, but the call as it stands passes trusted → NotBlocked; a
        // ruling cannot manufacture a dispatch for a call that is not blocked.
        let registry = registry();
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let ruling = Ruling {
            dispatch: wire_dispatch(),
            authority: AuthorityName::new("officer"),
            issuer: Issuer::Authority,
            reviewed: top_review(),
            covers: vec![floor_gap()],
        };
        assert_eq!(
            run(&registry, &log, &call("wire", json!({})), &[ruling], Sink::Tool),
            Err(PlanError::NotBlocked)
        );
    }

    #[test]
    fn ruling_exceeding_its_mandate_is_rejected() {
        // A plan is offered (officer can cover the trust floor), but the supplied ruling is from an
        // attester whose mandate only attends a mark — it cannot cover a trust floor gap.
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
            output_sanitizer: None,
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
            issuer: Issuer::Authority,
            reviewed: top_review(),
            covers: vec![floor_gap()],
        };
        // The offered plan's assignment names the officer; a ruling from the attester does not
        // realize it — refused at the assignment check, before the mandate re-verification even
        // runs (which stays beneath it as defense in depth).
        assert!(matches!(
            run(&registry, &log, &call("wire", json!({})), &[ruling], Sink::Tool),
            Err(PlanError::RulingAssignmentMismatch)
        ));
    }

    #[test]
    fn end_user_cannot_self_approve_a_response_sink() {
        let registry = registry();
        let log = vec![user_value(known(SUSPICIOUS, Audience::Public))];
        let ruling = Ruling {
            dispatch: wire_dispatch(),
            authority: AuthorityName::new("officer"),
            issuer: Issuer::EndUser,
            reviewed: top_review(),
            covers: vec![floor_gap()],
        };
        // The identical ruling is fine for a tool sink but barred for the response sink.
        assert!(
            run(
                &registry,
                &log,
                &call("wire", json!({})),
                std::slice::from_ref(&ruling),
                Sink::Tool
            )
            .is_ok()
        );
        assert_eq!(
            run(&registry, &log, &call("wire", json!({})), &[ruling], Sink::Response),
            Err(PlanError::EndUserResponseSink)
        );
    }

    #[test]
    fn two_eyes_collects_several_rulings() {
        // Two marks attended by two authorities; both rulings land in one atomic batch.
        let wire = ToolContract {
            name: ToolName::new("wire"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![],
            requires: Requires {
                attention: vec![MarkName::new("m1"), MarkName::new("m2")],
                ..Requires::default()
            },
            output_sanitizer: None,
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
        // This log folds to trusted/public; the recorded reviews must state that state exactly.
        let review = AuthorityReview {
            tool: ToolName::new("wire"),
            trajectory_label: known(TRUSTED, Audience::Public),
            arg_refs: vec![],
        };
        let rulings = vec![
            Ruling {
                dispatch: wire_dispatch(),
                authority: AuthorityName::new("a1"),
                issuer: Issuer::Authority,
                reviewed: review.clone(),
                covers: vec![Gap::Attention(MarkName::new("m1"))],
            },
            Ruling {
                dispatch: wire_dispatch(),
                authority: AuthorityName::new("a2"),
                issuer: Issuer::Authority,
                reviewed: review,
                covers: vec![Gap::Attention(MarkName::new("m2"))],
            },
        ];
        let batch = run(&registry, &log, &call("wire", json!({})), &rulings, Sink::Tool).unwrap();
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
            issuer: Issuer::Authority,
            reviewed,
            covers: vec![floor_gap()],
        };
        // A review claiming a fold the live state does not hold cannot land.
        let false_label = AuthorityReview {
            trajectory_label: Label::top(),
            ..top_review()
        };
        assert_eq!(
            run(
                &registry,
                &log,
                &call("wire", json!({})),
                &[with_review(false_label)],
                Sink::Tool
            ),
            Err(PlanError::ReviewMismatch)
        );
        // A review naming a different tool than the dispatch cannot land.
        let wrong_tool = AuthorityReview {
            tool: ToolName::new("other"),
            ..top_review()
        };
        assert_eq!(
            run(
                &registry,
                &log,
                &call("wire", json!({})),
                &[with_review(wrong_tool)],
                Sink::Tool
            ),
            Err(PlanError::ReviewMismatch)
        );
        // A review referencing a value the branch does not hold cannot land.
        let dangling = AuthorityReview {
            arg_refs: vec![ReviewedRef {
                value: ValueId::new(7),
                label: known(SUSPICIOUS, Audience::Public),
                provenance: Provenance::UserInput,
            }],
            ..top_review()
        };
        assert_eq!(
            run(
                &registry,
                &log,
                &call("wire", json!({})),
                &[with_review(dangling)],
                Sink::Tool
            ),
            Err(PlanError::ReviewMismatch)
        );
        // Completeness is two-way: a review that OMITS a reference the call carries is as false as
        // a fabricated one — the digest ignores refs, so only this check catches the omission.
        let ref_call = ResolvedCall::new(ToolName::new("wire"), json!({}), vec![ValueId::new(0)]);
        assert_eq!(
            run(&registry, &log, &ref_call, &[with_review(top_review())], Sink::Tool),
            Err(PlanError::ReviewMismatch)
        );
    }

    /// A tool whose delta narrows the audience to `internal` — the pure-narrowing fixture.
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
            output_sanitizer: None,
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
        // A pure narrowing (delta narrows audience, no requirement gap): the acceptance is recorded,
        // and it carries exactly the narrowing the offered plan embedded — never a re-derived one.
        let registry = narrowing_registry();
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let batch = run(&registry, &log, &call("get", json!({})), &[], Sink::Tool).unwrap();
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
        // The offer embeds its narrowing, so after the fold moves (a later admitted value shrank
        // the audience) the stale plan mismatches the re-derived live plans by value and is
        // refused — it cannot silently accept the newly live narrowing nobody was shown.
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
            execute_plan(&registry, &views, &stale, &call("get", json!({})), &[], Sink::Tool),
            Err(PlanError::UnknownPlan(0))
        );

        // The same acceptance re-derived at the live state executes and records the live narrowing.
        let live = offered_plan(&registry, &views, &call("get", json!({})));
        let batch = execute_plan(&registry, &views, &live, &call("get", json!({})), &[], Sink::Tool).unwrap();
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
