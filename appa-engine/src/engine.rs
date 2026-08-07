//! The engine: a pure function of the log's views and the immutable registry.

use thiserror::Error;

use crate::admit::{self, AdmitError, CastAnswer, CastError, ResultAdmission};
use crate::branch::{self, BranchError, ReturnSubmission};
use crate::check::{self, CheckOutcome, Narrowing, RawBlock, UnestablishedFact};
use crate::contract::ToolContract;
use crate::execute::{self, PlanError, Ruling};
use crate::fact::{Fact, FactBatch, ReturnPolicy};
use crate::label::DimValue;
use crate::plan::{self, PlannedBlock};
use crate::projection::Views;
use crate::registry::Registry;
use crate::value::{DispatchId, ResolvedCall, TrajectoryId};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EngineError {
    #[error("no contract registered for tool {0}")]
    UnknownTool(String),
    #[error("the call does not pass the check as-is — remedy or accept it first")]
    NotAllowed,
}

#[derive(Clone, Debug)]
pub struct Engine {
    registry: Registry,
}

impl Engine {
    pub fn new(registry: Registry) -> Self {
        Engine { registry }
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Evaluate a proposed call: allow, or block carrying everything that stopped it at once —
    /// the requirement gaps, the narrowing where one fired, and the values whose consumed
    /// dimension no cast has established. Resolution is the runtime's job;
    /// the runtime re-checks after each landed cast, so a surfaced block is the residual.
    pub fn check(&self, views: &Views, call: &ResolvedCall) -> Result<CheckOutcome, EngineError> {
        let contract = self.contract(call)?;
        Ok(check::evaluate(contract, views, call))
    }

    /// Open a dispatch for a call that **passes the check as-is**. Re-checks and refuses any
    /// block — unestablished values included (a narrowing is accepted through
    /// [`Engine::execute_remedy_plan`], not here), so
    /// the engine never emits an appendable dispatch for a call it would not allow. Folds nothing —
    /// the label folds only when the result value is admitted.
    pub fn open_dispatch(&self, views: &Views, call: &ResolvedCall) -> Result<FactBatch, EngineError> {
        let contract = self.contract(call)?;
        match check::evaluate(contract, views, call) {
            CheckOutcome::Allow => {
                let (_, fact) = opened_dispatch(contract, views, call);
                Ok(FactBatch::new(views.revision(), vec![fact]))
            }
            _ => Err(EngineError::NotAllowed),
        }
    }

    /// Execute a remedy plan: land the covering rulings, the narrowing acceptance, and the dispatch
    /// as one atomic batch, enforcing the plan's exact grouped assignment and mandate coverage. The
    /// chosen plan is matched by value against the live offers — the return-path staleness story.
    pub fn execute_remedy_plan(
        &self,
        views: &Views,
        chosen: &plan::ExecutableRemedyPlan,
        call: &ResolvedCall,
        rulings: &[Ruling],
    ) -> Result<FactBatch, PlanError> {
        execute::execute_remedy_plan(&self.registry, views, chosen, call, rulings)
    }

    /// Close a dispatch and admit its result — raw, cast-resolved, or withheld. The label folds only
    /// from an admitted value, never from the close.
    pub fn admit_result(
        &self,
        views: &Views,
        dispatch: &DispatchId,
        call: &ResolvedCall,
        admission: ResultAdmission,
    ) -> Result<FactBatch, AdmitError> {
        admit::admit_result(&self.registry, views, dispatch, call, admission)
    }

    /// Record observed success for a still-open dispatch whose value finalization is deferred (a
    /// pending-cast offer): its declared effects commit now, at the one append point the spec puts
    /// at success, while the raw result stays confined. See [`crate::admit::observe_success`].
    pub fn observe_success(
        &self,
        views: &Views,
        dispatch: &DispatchId,
        call: &ResolvedCall,
    ) -> Result<FactBatch, AdmitError> {
        admit::observe_success(&self.registry, views, dispatch, call)
    }

    /// The narrowing admitting a cast-resolved value of `call` would fold into the live trajectory
    /// label, or `None` when it does not move it — the whole filled label, established dimensions
    /// included (see `admit::pending_cast_narrowing`). The runtime derives the acceptance offer
    /// from this; admission re-derives it under the family lock, so a stale offer refuses by value
    /// (D2).
    pub fn cast_narrowing(
        &self,
        views: &Views,
        dispatch: &DispatchId,
        call: &ResolvedCall,
        resolved: &DimValue,
    ) -> Result<Option<Narrowing>, EngineError> {
        let contract = self.contract(call)?;
        Ok(admit::pending_cast_narrowing(
            views,
            &admit::cast_filled_dispatch_label(contract, views, dispatch, resolved),
        ))
    }

    /// Attach the sound remedies to a raw block: executable plans and prose recommendations. An empty
    /// result (no plans, no curative recommendation) is a proof the block is unliftable over the
    /// implemented remedy subset — see [`crate::plan`].
    pub fn plan(&self, views: &Views, call: &ResolvedCall, raw: &RawBlock) -> Result<PlannedBlock, EngineError> {
        self.contract(call)?;
        Ok(plan::plan(&self.registry, views, call, raw))
    }

    pub fn admit_cast(
        &self,
        views: &Views,
        target: &UnestablishedFact,
        answer: CastAnswer,
    ) -> Result<FactBatch, CastError> {
        admit::admit_cast(&self.registry, views, target, answer)
    }

    /// Seed a child branch at the parent's current label with an immutable fork binding carrying
    /// its return policy. See [`crate::branch`].
    pub fn seed_child(
        &self,
        parent: &Views,
        child: &TrajectoryId,
        return_policy: ReturnPolicy,
    ) -> Result<FactBatch, BranchError> {
        branch::seed_child(&self.registry, parent, child, return_policy)
    }

    /// Record a child's returned value at an engine-derived label AND merge it into the direct
    /// parent — one atomic batch, no orphanable intermediate state. A raw crossing that would
    /// narrow the parent is refused (`ReturnNarrowsParent`): it exists only through an executed
    /// return plan. See [`crate::branch`].
    pub fn submit_child_return(
        &self,
        parent: &Views,
        child: &TrajectoryId,
        ret: ReturnSubmission,
    ) -> Result<FactBatch, BranchError> {
        branch::submit_child_return(&self.registry, parent, child, ret)
    }

    /// Decide whether a raw return by `child` may merge silently, and if not, which return plans
    /// could cross it. Both folds and the linkage come from the parent's one projection snapshot.
    /// See [`crate::branch`].
    pub fn check_child_return(&self, parent: &Views, child: &TrajectoryId) -> Result<branch::ReturnCheck, BranchError> {
        branch::check_child_return(&self.registry, parent, child)
    }

    /// The child fold's unestablished facts — what a cast must establish before this child's
    /// return can merge. Policy-independent: the runtime drives resolution *before*
    /// the return-policy split, so raw and sanitizer-bound returns resolve alike.
    pub fn child_fold_unestablished(&self, parent: &Views, child: &TrajectoryId) -> Vec<check::UnestablishedFact> {
        branch::child_fold_unestablished(parent, child)
    }

    /// Execute one offered return plan as a single atomic batch: crossing, acceptance where the
    /// plan carries one, and merge. Re-derives the block from the live views and refuses a chosen
    /// plan the fresh offers no longer contain. See [`crate::branch`].
    pub fn execute_child_return_plan(
        &self,
        parent: &Views,
        child: &TrajectoryId,
        chosen: branch::ReturnPlan,
        submission: ReturnSubmission,
    ) -> Result<FactBatch, BranchError> {
        branch::execute_child_return_plan(&self.registry, parent, child, chosen, submission)
    }

    fn contract(&self, call: &ResolvedCall) -> Result<&ToolContract, EngineError> {
        self.registry
            .tool(call.tool())
            .ok_or_else(|| EngineError::UnknownTool(call.tool().as_str().to_string()))
    }
}

/// Build the `DispatchOpened` fact for a call: its proposed committed label, the effects it would
/// commit on success, and its occurrence (a repeat identical call is a new dispatch). Shared by the
/// clean-allow path ([`Engine::open_dispatch`]) and atomic plan execution ([`crate::execute`]).
pub(crate) fn opened_dispatch(contract: &ToolContract, views: &Views, call: &ResolvedCall) -> (DispatchId, Fact) {
    let dispatch = DispatchId::new(
        views.trajectory().clone(),
        call.digest(),
        views.dispatch_count(&call.digest()),
    );
    let fact = Fact::DispatchOpened {
        trajectory: views.trajectory().clone(),
        dispatch: dispatch.clone(),
        proposed_label: check::committed_label_for_call(contract, &views.current_label(), call),
        proposed_effects: contract.emits.clone(),
        dynamic_resolutions: call.dynamic_resolutions().to_vec(),
    };
    (dispatch, fact)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::Gap;
    use crate::contract::{
        AudienceRequirement, Delta, HistoryRequirement, LabelRequirements, RecipientSpec, Requires, ToolContract,
    };
    use crate::fact::{EffectKind, Fact, Revision};
    use crate::label::{Audience, Dim, Dimension, Label, ReaderId, Trust};
    use crate::names::MarkName;
    use crate::projection::Projection;
    use crate::registry::{RegistryConfig, TrustChain};
    use crate::value::{LabeledValue, Provenance, ToolName, TrajectoryId, ValueBody};
    use serde_json::json;

    const SUSPICIOUS: Trust = Trust::new(0);
    const TRUSTED: Trust = Trust::new(1);

    fn traj() -> TrajectoryId {
        TrajectoryId::new("t")
    }

    fn engine(tools: Vec<ToolContract>) -> Engine {
        let cfg = RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools,
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![],
        };
        Engine::new(Registry::build(cfg).unwrap())
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

    fn call(tool: &str, args: serde_json::Value) -> ResolvedCall {
        ResolvedCall::new(ToolName::new(tool), args, vec![])
    }

    fn check(engine: &Engine, log: &[Fact], call: &ResolvedCall) -> CheckOutcome {
        let p = Projection::build(log, Revision::new(log.len() as u64));
        let t = traj();
        engine.check(&p.view(&t), call).unwrap()
    }

    fn crm_tool() -> ToolContract {
        ToolContract {
            name: ToolName::new("get_ticket"),
            tags: vec![],
            delta: Some(Delta {
                trust: None,
                audience: Some(Dim::Known(Audience::restricted([ReaderId::new("internal")])).into()),
            }),
            emits: vec![],
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(TRUSTED),
                    audience: vec![],
                },
                ..Requires::default()
            },
        }
    }

    #[test]
    fn clean_call_allows() {
        let e = engine(vec![crm_tool()]);
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        match check(&e, &log, &call("get_ticket", json!({}))) {
            CheckOutcome::Block(b) => {
                assert!(b.narrowing.is_some());
                assert!(b.requirement_gaps.is_empty());
            }
            other => panic!("expected narrowing block, got {other:?}"),
        }
    }

    #[test]
    fn repeat_at_same_label_is_not_a_narrowing() {
        let e = engine(vec![crm_tool()]);
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let log = vec![user_value(known(TRUSTED, internal))];
        assert_eq!(check(&e, &log, &call("get_ticket", json!({}))), CheckOutcome::Allow);
    }

    #[test]
    fn pending_cast_output_dispatches_before_resolution() {
        let scan = ToolContract {
            name: ToolName::new("scan_inbox"),
            tags: vec![],
            delta: Some(Delta {
                trust: Some(Dim::Unknown),
                audience: None,
            }),
            emits: vec![],
            requires: Requires::default(),
        };
        let e = engine(vec![scan]);
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        assert_eq!(check(&e, &log, &call("scan_inbox", json!({}))), CheckOutcome::Allow);
    }

    #[test]
    fn trust_floor_gap_when_suspicious() {
        let e = engine(vec![crm_tool()]);
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let log = vec![user_value(known(SUSPICIOUS, internal))];
        match check(&e, &log, &call("get_ticket", json!({}))) {
            CheckOutcome::Block(b) => assert!(b.requirement_gaps.contains(&Gap::TrustFloor {
                required: TRUSTED,
                actual: SUSPICIOUS,
            })),
            other => panic!("expected trust gap, got {other:?}"),
        }
    }

    #[test]
    fn includes_placeholder_resolves_from_arguments() {
        let send = ToolContract {
            name: ToolName::new("send_email"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![EffectKind::new("egress")],
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Includes(RecipientSpec::Placeholder("to".into()))],
                },
                ..Requires::default()
            },
        };
        let e = engine(vec![send]);
        let internal = Audience::restricted([ReaderId::new("auditor")]);
        let log = vec![user_value(known(TRUSTED, internal))];
        assert_eq!(
            check(&e, &log, &call("send_email", json!({ "to": "auditor" }))),
            CheckOutcome::Allow
        );
        match check(&e, &log, &call("send_email", json!({ "to": "stranger" }))) {
            CheckOutcome::Block(b) => assert!(matches!(
                b.requirement_gaps.as_slice(),
                [crate::check::Gap::Includes { .. }]
            )),
            other => panic!("expected includes gap, got {other:?}"),
        }
    }

    #[test]
    fn history_prior_and_no_prior() {
        let del = ToolContract {
            name: ToolName::new("delete_db"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![],
            requires: Requires {
                history: vec![
                    HistoryRequirement::Prior(EffectKind::new("backup.done")),
                    HistoryRequirement::NoPrior(EffectKind::new("db.deleted")),
                ],
                ..Requires::default()
            },
        };
        let e = engine(vec![del]);
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        match check(&e, &log, &call("delete_db", json!({}))) {
            CheckOutcome::Block(b) => {
                assert!(b.requirement_gaps.contains(&Gap::Prior(EffectKind::new("backup.done"))))
            }
            other => panic!("expected prior gap, got {other:?}"),
        }
    }

    #[test]
    fn attention_is_always_a_gap() {
        let tool = ToolContract {
            name: ToolName::new("wire"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![],
            requires: Requires {
                attention: vec![MarkName::new("signoff")],
                ..Requires::default()
            },
        };
        let e = engine(vec![tool]);
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        match check(&e, &log, &call("wire", json!({}))) {
            CheckOutcome::Block(b) => {
                assert!(b.requirement_gaps.contains(&Gap::Attention(MarkName::new("signoff"))))
            }
            other => panic!("expected attention gap, got {other:?}"),
        }
    }

    #[test]
    fn unknown_label_is_unestablished_not_a_gap() {
        let e = engine(vec![crm_tool()]);
        let log = vec![user_value(Label::new(Dim::Unknown, Dim::Known(Audience::Public)))];
        match check(&e, &log, &call("get_ticket", json!({}))) {
            CheckOutcome::Block(b) => {
                assert!(b.requirement_gaps.is_empty());
                assert!(b.narrowing.is_some(), "the audience narrowing reports alongside");
                assert_eq!(b.unestablished.len(), 1);
                assert_eq!(b.unestablished[0].dimension, Dimension::Trust);
            }
            other => panic!("expected an unestablished block, got {other:?}"),
        }
    }

    #[test]
    fn all_three_block_slots_coexist() {
        let vault = ToolContract {
            name: ToolName::new("vault"),
            tags: vec![],
            delta: Some(Delta {
                trust: None,
                audience: Some(Dim::Known(Audience::restricted([ReaderId::new("internal")])).into()),
            }),
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
        let e = engine(vec![vault]);
        let log = vec![user_value(Label::new(Dim::Unknown, Dim::Known(Audience::Public)))];
        match check(&e, &log, &call("vault", json!({}))) {
            CheckOutcome::Block(b) => {
                assert_eq!(b.requirement_gaps, vec![Gap::Attention(MarkName::new("signoff"))]);
                assert!(b.narrowing.is_some());
                assert_eq!(b.unestablished.len(), 1);
                assert_eq!(b.unestablished[0].dimension, Dimension::Trust);
            }
            other => panic!("expected a three-slot block, got {other:?}"),
        }
    }

    fn unannotated_tool(name: &str) -> ToolContract {
        ToolContract {
            name: ToolName::new(name),
            tags: vec![],
            delta: None,
            emits: vec![],
            requires: Requires::default(),
        }
    }

    #[test]
    fn an_unannotated_tool_dispatches_and_its_result_admits_unknown() {
        let e = engine(vec![unannotated_tool("probe")]);
        let mut log = vec![user_value(known(TRUSTED, Audience::Public))];
        let proposed = call("probe", json!({}));
        assert_eq!(check(&e, &log, &proposed), CheckOutcome::Allow);

        let t = traj();
        let p = Projection::build(&log, Revision::new(log.len() as u64));
        let batch = e.open_dispatch(&p.view(&t), &proposed).unwrap();
        log.extend(batch.facts);
        let p = Projection::build(&log, Revision::new(log.len() as u64));
        let dispatch = DispatchId::new(t.clone(), proposed.digest(), 0);
        let batch = e
            .admit_result(
                &p.view(&t),
                &dispatch,
                &proposed,
                ResultAdmission::SuccessRaw {
                    body: ValueBody::new("raw"),
                },
            )
            .unwrap();
        log.extend(batch.facts);
        let p = Projection::build(&log, Revision::new(log.len() as u64));
        let current = p.view(&t).current_label();
        assert_eq!(current.trust, Dim::Unknown);
        assert_eq!(current.audience, Dim::Unknown);
    }

    #[test]
    fn an_unknown_trajectory_blocks_only_requirement_consuming_calls() {
        let e = engine(vec![unannotated_tool("noop"), crm_tool()]);
        let log = vec![user_value(Label::new(Dim::Unknown, Dim::Unknown))];
        assert_eq!(check(&e, &log, &call("noop", json!({}))), CheckOutcome::Allow);
        match check(&e, &log, &call("get_ticket", json!({}))) {
            CheckOutcome::Block(b) => {
                assert!(b.requirement_gaps.is_empty());
                assert_eq!(b.unestablished.len(), 1);
                assert_eq!(b.unestablished[0].dimension, Dimension::Trust);
            }
            other => panic!("expected an unestablished block, got {other:?}"),
        }
    }

    #[test]
    fn unknown_tool_errors() {
        let e = engine(vec![]);
        let p = Projection::build(&[], Revision::ZERO);
        let t = traj();
        assert!(matches!(
            e.check(&p.view(&t), &call("ghost", json!({}))),
            Err(EngineError::UnknownTool(name)) if name == "ghost"
        ));
    }

    #[test]
    fn open_dispatch_refuses_a_blocked_call() {
        let e = engine(vec![crm_tool()]);
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let log = vec![user_value(known(SUSPICIOUS, internal))];
        let p = Projection::build(&log, Revision::new(log.len() as u64));
        let t = traj();
        assert_eq!(
            e.open_dispatch(&p.view(&t), &call("get_ticket", json!({}))),
            Err(EngineError::NotAllowed)
        );
    }

    #[test]
    fn includes_missing_placeholder_fails_closed_on_public() {
        let send = ToolContract {
            name: ToolName::new("send_email"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![],
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Includes(RecipientSpec::Placeholder("to".into()))],
                },
                ..Requires::default()
            },
        };
        let e = engine(vec![send]);
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        match check(&e, &log, &call("send_email", json!({}))) {
            CheckOutcome::Block(b) => assert!(matches!(b.requirement_gaps.as_slice(), [Gap::Includes { .. }])),
            other => panic!("expected includes gap on a malformed call, got {other:?}"),
        }

        let log = vec![user_value(Label::new(Dim::Known(TRUSTED), Dim::Unknown))];
        match check(&e, &log, &call("send_email", json!({}))) {
            CheckOutcome::Block(b) => {
                assert!(b.requirement_gaps.is_empty(), "the sentinel gap must be masked");
                assert_eq!(b.unestablished.len(), 1);
                assert_eq!(b.unestablished[0].dimension, Dimension::Audience);
            }
            other => panic!("expected an unestablished block on an Unknown audience, got {other:?}"),
        }
    }

    #[test]
    fn required_rulings_route_each_gap_to_its_authority() {
        use crate::authority::{Authority, Mandate, Scope};
        use crate::names::AuthorityName;

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
            hint: None,
        };
        let cfg = RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![wire],
            authorities: vec![officer],
            sanitizers: vec![],
            casts: vec![],
        };
        let e = Engine::new(Registry::build(cfg).unwrap());
        let log = vec![user_value(known(SUSPICIOUS, Audience::Public))];
        let p = Projection::build(&log, Revision::new(log.len() as u64));
        let t = traj();
        let wire_call = call("wire", json!({}));
        let raw = match e.check(&p.view(&t), &wire_call).unwrap() {
            CheckOutcome::Block(raw) => raw,
            other => panic!("expected a block, got {other:?}"),
        };
        let planned = e.plan(&p.view(&t), &wire_call, &raw).unwrap();
        assert_eq!(planned.plans.len(), 1);
        let required = &planned.plans[0].executable().expect("an authority plan").required;
        assert_eq!(required.len(), 1);
        assert_eq!(required[0].authority, AuthorityName::new("officer"));
        assert_eq!(
            required[0].covers,
            vec![Gap::TrustFloor {
                required: TRUSTED,
                actual: SUSPICIOUS,
            }]
        );
    }

    #[test]
    fn open_dispatch_records_proposed_label_and_effects() {
        let e = engine(vec![crm_tool()]);
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let log = vec![user_value(known(TRUSTED, internal.clone()))];
        let p = Projection::build(&log, Revision::new(log.len() as u64));
        let t = traj();
        let batch = e.open_dispatch(&p.view(&t), &call("get_ticket", json!({}))).unwrap();
        match &batch.facts[0] {
            Fact::DispatchOpened { proposed_label, .. } => {
                assert_eq!(*proposed_label, known(TRUSTED, internal));
            }
            other => panic!("expected DispatchOpened, got {other:?}"),
        }
    }
}
