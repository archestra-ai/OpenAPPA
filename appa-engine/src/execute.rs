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
use crate::names::AuthorityName;
use crate::plan::{self, PlanId, covers_gap};
use crate::projection::Views;
use crate::registry::Registry;
use crate::value::{DispatchId, ResolvedCall};

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
/// acts under, who exercised it, and the gaps it claims to cover. Binding the whole dispatch — not
/// just the digest — makes a ruling both call-scoped (`transfer(A,$1)` cannot admit `transfer(B,$100)`)
/// and single-use (a repeat identical call is a new occurrence and takes a fresh ruling).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ruling {
    pub dispatch: DispatchId,
    pub authority: AuthorityName,
    pub issuer: Issuer,
    pub covers: Vec<Gap>,
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
}

/// Execute a remedy plan: verify coverage and the issuer bar, then emit the atomic
/// rulings + acceptance + dispatch batch. See the module docs.
pub(crate) fn execute_plan(
    registry: &Registry,
    views: &Views,
    plan: PlanId,
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

    // The plan id must name a plan this block actually offers at the current revision — not an
    // arbitrary or stale id.
    let planned = plan::plan(registry, views, call, &block);
    if !planned.plans.iter().any(|offered| offered.id == plan) {
        return Err(PlanError::UnknownPlan(plan.value()));
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
        });
    }
    if let Some(narrowing) = block.narrowing {
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

    /// The dispatch every ruling in these tests is scoped to — `wire({})`, first occurrence in `t`.
    fn wire_dispatch() -> DispatchId {
        DispatchId::new(traj(), call("wire", json!({})).digest(), 0)
    }

    fn run(
        registry: &Registry,
        log: &[Fact],
        call: &ResolvedCall,
        rulings: &[Ruling],
        sink: Sink,
    ) -> Result<FactBatch, PlanError> {
        let projection = Projection::build(log, Revision::new(log.len() as u64));
        let trajectory = traj();
        execute_plan(
            registry,
            &projection.view(&trajectory),
            PlanId::new(0),
            call,
            rulings,
            sink,
        )
    }

    #[test]
    fn ruling_admits_the_blocked_dispatch_atomically() {
        let registry = registry();
        let log = vec![user_value(known(SUSPICIOUS, Audience::Public))];
        let ruling = Ruling {
            dispatch: wire_dispatch(),
            authority: AuthorityName::new("officer"),
            issuer: Issuer::Authority,
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
            covers: vec![floor_gap()],
        };
        let projection = Projection::build(&log, Revision::new(log.len() as u64));
        let trajectory = traj();
        assert_eq!(
            execute_plan(
                &registry,
                &projection.view(&trajectory),
                PlanId::new(999),
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
        // No rulings supplied → the trust floor gap is uncovered.
        assert!(matches!(
            run(&registry, &log, &call("wire", json!({})), &[], Sink::Tool),
            Err(PlanError::GapUncovered(_))
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
            covers: vec![floor_gap()],
        };
        assert!(matches!(
            run(&registry, &log, &call("wire", json!({})), &[ruling], Sink::Tool),
            Err(PlanError::RulingExceedsMandate { .. })
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
        let rulings = vec![
            Ruling {
                dispatch: wire_dispatch(),
                authority: AuthorityName::new("a1"),
                issuer: Issuer::Authority,
                covers: vec![Gap::Attention(MarkName::new("m1"))],
            },
            Ruling {
                dispatch: wire_dispatch(),
                authority: AuthorityName::new("a2"),
                issuer: Issuer::Authority,
                covers: vec![Gap::Attention(MarkName::new("m2"))],
            },
        ];
        let batch = run(&registry, &log, &call("wire", json!({})), &rulings, Sink::Tool).unwrap();
        let ruling_count = batch.facts.iter().filter(|f| matches!(f, Fact::Ruling { .. })).count();
        assert_eq!(ruling_count, 2);
    }

    #[test]
    fn narrowing_records_an_acceptance() {
        // A pure narrowing (delta narrows audience, no requirement gap): the acceptance is recorded.
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
        let registry = Registry::build(crate::registry::RegistryConfig {
            trust_chain: chain(),
            tools: vec![get],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![],
        })
        .unwrap();
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let batch = run(&registry, &log, &call("get", json!({})), &[], Sink::Tool).unwrap();
        assert!(batch.facts.iter().any(|f| matches!(f, Fact::Acceptance { .. })));
    }
}
