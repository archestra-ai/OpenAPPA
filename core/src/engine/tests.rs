use std::collections::BTreeSet;

use super::application::RoutedRuling;
use super::capability::RESPONSE_SINK;
use super::planning::{AskVector, ask_cmp};
use super::*;
use crate::approval::{AuthorityMode, Ruling, TrajectoryView};
use crate::audit::{AuditEvent, AuthorityName};
use crate::contract::{Requirements, Unprovable, Violation};
use crate::dimension::{Audience, Effect, Effects, KnownTrust, Trust, UserId};
use crate::plan::{NonEmptyVec, RemedyPlan};
use crate::remedy::{Authorization, AuthorizationScope, DeltaCoordinate, LabelRaise, PlannedRemedy, ReductionTarget};
use crate::request::{ArgumentName, ArgumentSchema, ArgumentTree, EmissionRequest, ToolRequest};
use crate::revision::{PlanId, ValueId};
use crate::transition::AuthorityMandate;
use crate::turn::{Speaker, Trajectory};
use crate::value::{OpaqueValue, ValueLabel};

fn user(id: &str) -> UserId {
    UserId::new(id)
}

struct TerminalBlock {
    violations: Vec<Violation>,
    reason: BlockReason,
}

fn terminal_block<P: std::fmt::Debug>(outcome: FlowOutcome<P>) -> Option<TerminalBlock> {
    match outcome {
        FlowOutcome::Blocked {
            violations,
            terminal: Some(reason),
            ..
        } => Some(TerminalBlock { violations, reason }),
        _ => None,
    }
}

fn terminal_block_of<P: std::fmt::Debug>(outcome: Result<FlowOutcome<P>, FlowRefusal>) -> Option<TerminalBlock> {
    terminal_block(outcome.expect("expected a policy outcome, got a refusal"))
}

fn advanced_terminal(outcome: StepOutcome) -> Option<TerminalBlock> {
    match outcome {
        StepOutcome::Advanced(advanced) => terminal_block(advanced),
        _ => None,
    }
}

fn advanced_execution(outcome: StepOutcome) -> Option<ExecutionToken> {
    match outcome {
        StepOutcome::Advanced(advanced) => execution(advanced),
        _ => None,
    }
}

fn execution(outcome: FlowOutcome<FlowPermit>) -> Option<ExecutionToken> {
    match outcome {
        FlowOutcome::AllowedNow(FlowPermit::Execute(token)) => Some(token),
        _ => None,
    }
}

fn email_contract() -> ToolContract {
    ToolContract {
        name: ToolName::new("email.send"),
        requires: Some(Requirements {
            trust: Some(KnownTrust::Trusted),
            audience: crate::contract::AudienceRule::FromRecipients,
            ..Requirements::default()
        }),
        output_label: ValueLabel::identity(),
        effects: Effects::declared([Effect::Egress]),
        arguments: ArgumentSchema::with_recipients(ArgumentName::new("to")),
    }
}

fn engine_with(contracts: impl IntoIterator<Item = ToolContract>) -> PolicyEngine {
    let mut engine = PolicyEngine::new();
    for contract in contracts {
        engine.register(contract).unwrap();
    }
    engine
}

fn ingress(trajectory: &mut Trajectory, readers: &[&str], trust: Trust, body: &str) -> ValueId {
    trajectory.ingress(
        Speaker::user(user("alice")),
        ValueLabel {
            audience: Audience::readers(readers.iter().map(|r| user(r))),
            trust,
        },
        OpaqueValue::new(body),
    )
}

fn dispatch(trajectory: &mut Trajectory, token: ExecutionToken, body: &str) -> Result<ValueId, RejectedToken> {
    let (_, receipt) = trajectory.release(token)?;
    trajectory.record_output(receipt, OpaqueValue::new(body))
}

fn walk_to_permit(engine: &PolicyEngine, trajectory: &mut Trajectory, request: ToolRequest) -> ExecutionToken {
    match engine.pursue(trajectory, request, 16) {
        Pursuit::Permitted(token) => token,
        other => panic!("expected to reach a permit, got {other:?}"),
    }
}

fn identity_ingress(trajectory: &mut Trajectory, body: &str) -> ValueId {
    trajectory.ingress(
        Speaker::user(user("alice")),
        ValueLabel::identity(),
        OpaqueValue::new(body),
    )
}

fn email_request(trajectory: &mut Trajectory, body: ValueId, recipient: &str) -> ToolRequest {
    let to = identity_ingress(trajectory, recipient);
    ToolRequest::new(
        ToolName::new("email.send"),
        ArgumentTree::Object(std::collections::BTreeMap::from([
            (ArgumentName::new("to"), ArgumentTree::Value(to)),
            (ArgumentName::new("body"), ArgumentTree::Value(body)),
        ])),
        BTreeSet::new(),
    )
}

fn remediable(engine: &PolicyEngine, trajectory: &mut Trajectory, request: ToolRequest) -> NonEmptyVec<RemedyPlan> {
    match engine.evaluate(trajectory, request) {
        Ok(FlowOutcome::Blocked {
            plans, terminal: None, ..
        }) => NonEmptyVec::from_vec(plans).expect("a remediable block carries at least one plan"),
        other => panic!("expected a remediable block, got {other:?}"),
    }
}

fn apply_first_step(engine: &PolicyEngine, trajectory: &mut Trajectory, plan: PlanId) -> StepOutcome {
    let capability = engine.mint_step(trajectory, plan, 0).unwrap();
    engine.apply_step(trajectory, capability).unwrap()
}

fn raise_step(step: &PlannedRemedy) -> Option<(ValueId, LabelRaise)> {
    let PlannedRemedy::Authorize { authorization, .. } = step else {
        return None;
    };
    let AuthorizationScope::DerivedValue { source } = authorization.scope() else {
        return None;
    };
    let coordinates: Vec<_> = authorization.delta().coordinates().collect();
    match coordinates.as_slice() {
        [DeltaCoordinate::RaiseLabel(raise)] => Some((*source, raise.clone())),
        other => panic!("a durable-raise step must carry exactly one raise coordinate, got {other:?}"),
    }
}

fn step_targets(step: &PlannedRemedy) -> Option<&[Violation]> {
    match step {
        PlannedRemedy::Authorize { targets, .. } => Some(targets),
        PlannedRemedy::Reduce(_) => None,
    }
}

fn step_routes(step: &PlannedRemedy) -> Option<Vec<&str>> {
    match step {
        PlannedRemedy::Authorize { routes, .. } => Some(routes.iter().map(|name| name.as_str()).collect()),
        PlannedRemedy::Reduce(_) => None,
    }
}

fn release_step(step: &PlannedRemedy) -> Option<BTreeSet<ValueId>> {
    let PlannedRemedy::Authorize { authorization, .. } = step else {
        return None;
    };
    let AuthorizationScope::PolicyCheck { .. } = &authorization.scope() else {
        return None;
    };
    let release = authorization.delta().coordinates().find_map(|c| match c {
        DeltaCoordinate::ReleaseControl(deps) => Some(deps.clone()),
        _ => None,
    });
    Some(release.unwrap_or_default())
}

fn derive_step(step: &PlannedRemedy) -> Option<ValueId> {
    match step {
        PlannedRemedy::Reduce(ReductionTarget::DeriveValue { source, .. }) => Some(*source),
        _ => None,
    }
}

fn applied_raise(event: &AuditEvent) -> Option<(ValueId, &AuthorityName)> {
    match event {
        AuditEvent::AuthorizationApplied {
            authorization,
            authority,
            derived: Some(derived),
            ..
        } if matches!(authorization.scope(), AuthorizationScope::DerivedValue { .. }) => Some((*derived, authority)),
        _ => None,
    }
}

fn applied_lift(event: &AuditEvent) -> Option<&crate::remedy::AuthorizationDelta> {
    match event {
        AuditEvent::AuthorizationApplied { authorization, .. }
            if matches!(authorization.scope(), AuthorizationScope::PolicyCheck { .. }) =>
        {
            Some(authorization.delta())
        }
        _ => None,
    }
}

fn denied_delta(event: &AuditEvent) -> Option<&crate::remedy::AuthorizationDelta> {
    match event {
        AuditEvent::AuthorizationDenied { authorization, .. } => Some(authorization.delta()),
        _ => None,
    }
}

fn delta_raises(delta: &crate::remedy::AuthorizationDelta) -> bool {
    delta.coordinates().any(|c| matches!(c, DeltaCoordinate::RaiseLabel(_)))
}

fn delta_acknowledges(delta: &crate::remedy::AuthorizationDelta) -> bool {
    delta
        .coordinates()
        .any(|c| matches!(c, DeltaCoordinate::AcknowledgeUnknown(_)))
}

fn delta_releases_control(delta: &crate::remedy::AuthorizationDelta) -> bool {
    delta
        .coordinates()
        .any(|c| matches!(c, DeltaCoordinate::ReleaseControl(_)))
}

fn approve_all(
    _: &crate::remedy::Authorization,
    _: &[Violation],
    _: &crate::approval::TrajectoryView,
) -> Option<crate::approval::Ruling> {
    Some(Ruling::Approve {
        reason: "approved".to_owned(),
    })
}

fn abstain_all(
    _: &crate::remedy::Authorization,
    _: &[Violation],
    _: &crate::approval::TrajectoryView,
) -> Option<crate::approval::Ruling> {
    None
}

fn inline_authority(
    name: &str,
    mandate: crate::transition::AuthorityMandate,
    decide: crate::approval::AuthorityFn,
) -> Authority {
    Authority {
        name: AuthorityName::new(name),
        mandate,
        mode: AuthorityMode::Inline(decide),
    }
}

fn external_authority(name: &str, mandate: crate::transition::AuthorityMandate) -> Authority {
    Authority {
        name: AuthorityName::new(name),
        mandate,
        mode: AuthorityMode::External,
    }
}

#[test]
fn clean_flow_is_permitted_and_result_admitted_with_folded_label() {
    let engine = engine_with([email_contract()]);
    let mut trajectory = Trajectory::new();
    let body = ingress(&mut trajectory, &["alice", "bob"], Trust::TRUSTED, "the doc");
    let request = email_request(&mut trajectory, body, "bob");

    let token = walk_to_permit(&engine, &mut trajectory, request);
    assert!(trajectory.pending_action().is_some());
    assert_eq!(trajectory.past_effects(), &Effects::none());

    let result = dispatch(&mut trajectory, token, "sent").unwrap();
    assert!(trajectory.pending_action().is_none());
    // Output label folds intrinsic (identity) with the argument labels.
    assert_eq!(
        trajectory.value(result).unwrap().label().audience,
        Audience::readers([user("alice"), user("bob")])
    );
    // Effects were committed at dispatch, not before.
    assert_eq!(trajectory.past_effects(), &Effects::declared([Effect::Egress]));
}

#[test]
fn explicit_flow_taint_blocks_the_sink() {
    let engine = engine_with([email_contract()]);
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let body = ingress(&mut trajectory, &["alice", "bob"], Trust::SUSPICIOUS, "raw page");
    let request = email_request(&mut trajectory, body, "bob");

    let Some(block) = terminal_block_of(engine.evaluate(&mut trajectory, request)) else {
        panic!("expected terminal block");
    };
    assert_eq!(block.reason, BlockReason::NoRemedy);
    assert!(matches!(
        block.violations.as_slice(),
        [Violation::Breach(crate::contract::Breach::TrustBelow { .. })]
    ));
    assert!(trajectory.pending_action().is_none());
}

#[test]
fn control_dependence_taints_a_clean_payload() {
    let engine = engine_with([email_contract()]);
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let secret = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "secret");
    let clean_body = ingress(&mut trajectory, &["alice", "bob"], Trust::TRUSTED, "harmless");
    let to = identity_ingress(&mut trajectory, "bob");
    let request = ToolRequest::new(
        ToolName::new("email.send"),
        ArgumentTree::Object(std::collections::BTreeMap::from([
            (ArgumentName::new("to"), ArgumentTree::Value(to)),
            (ArgumentName::new("body"), ArgumentTree::Value(clean_body)),
        ])),
        BTreeSet::from([secret]),
    );

    let Some(block) = terminal_block_of(engine.evaluate(&mut trajectory, request)) else {
        panic!("expected terminal block");
    };
    assert!(matches!(
        block.violations.as_slice(),
        [Violation::Breach(crate::contract::Breach::AudienceExceeds { outside })]
            if *outside == BTreeSet::from([user("bob")])
    ));
}

#[test]
fn unregistered_tool_blocks_without_an_acknowledge_authority() {
    let engine = engine_with([]);
    let mut trajectory = Trajectory::new();
    let body = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "x");
    let request = ToolRequest::new(
        ToolName::new("mystery.tool"),
        ArgumentTree::Value(body),
        BTreeSet::new(),
    );

    let Some(block) = terminal_block_of(engine.evaluate(&mut trajectory, request)) else {
        panic!("expected terminal block");
    };
    assert_eq!(block.reason, BlockReason::NoRemedy);
}

#[test]
fn unregistered_tool_acknowledged_dispatches_with_unknown_output() {
    let mut engine = engine_with([]);
    engine
        .register_authority(inline_authority(
            "accept-unknowns",
            crate::transition::AuthorityMandate {
                acknowledge_unknown: true,
                ..crate::transition::AuthorityMandate::none()
            },
            approve_all,
        ))
        .unwrap();
    let mut trajectory = Trajectory::new();
    let body = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "x");
    let request = ToolRequest::new(
        ToolName::new("mystery.tool"),
        ArgumentTree::Value(body),
        BTreeSet::new(),
    );

    let token = walk_to_permit(&engine, &mut trajectory, request);
    assert!(
        trajectory
            .audit()
            .iter()
            .any(|e| applied_lift(e).is_some_and(delta_acknowledges))
    );

    let result = dispatch(&mut trajectory, token, "???").unwrap();
    // Intrinsic unknown poisons the output despite trusted inputs...
    assert_eq!(trajectory.value(result).unwrap().label(), &ValueLabel::unknown());
    // ...and the unknown effect commits at dispatch, absorbing the past.
    assert_eq!(trajectory.past_effects(), &Effects::UNKNOWN);
}

#[test]
fn unknown_trust_routes_as_an_endorse() {
    let mut engine = engine_with([email_contract()]);
    engine.register_authority(human()).unwrap();
    let mut trajectory = Trajectory::new();
    // Unknown trust cannot prove the sink's `Trusted` requirement.
    let doc = ingress(&mut trajectory, &["alice", "bob"], Trust::UNKNOWN, "doc");
    let request = email_request(&mut trajectory, doc, "bob");

    let Ok(FlowOutcome::Blocked {
        violations,
        plans,
        terminal: None,
    }) = engine.evaluate(&mut trajectory, request)
    else {
        panic!("expected a remediable block");
    };
    let plans = NonEmptyVec::from_vec(plans).expect("a remediable block carries at least one plan");
    assert!(
        violations
            .iter()
            .any(|v| matches!(v, Violation::Unprovable(crate::contract::Unprovable::TrustUnknown)))
    );
    assert!(
        raise_step(plans.first().steps.first())
            .is_some_and(|(source, raise)| source == doc && raise.trust == Some(KnownTrust::Trusted))
    );
    // ...routed to the trust-competent external human.
    let StepOutcome::NeedsApproval(pending) = apply_first_step(&engine, &mut trajectory, plans.first().id) else {
        panic!("expected the external human to be consulted");
    };
    assert_eq!(pending.authority().as_str(), "human");
}

#[test]
fn guarded_sink_without_recipients_is_structural() {
    let engine = engine_with([email_contract()]);
    let mut trajectory = Trajectory::new();
    let body = ingress(&mut trajectory, &["alice", "bob"], Trust::TRUSTED, "doc");
    let request = ToolRequest::new(
        ToolName::new("email.send"),
        ArgumentTree::Object(std::collections::BTreeMap::from([(
            ArgumentName::new("body"),
            ArgumentTree::Value(body),
        )])),
        BTreeSet::new(),
    );

    let Some(block) = terminal_block_of(engine.evaluate(&mut trajectory, request)) else {
        panic!("expected terminal block");
    };
    assert_eq!(block.reason, BlockReason::RequiresStructuralFix);
}

#[test]
fn stale_token_is_rejected_after_any_mutation() {
    let engine = engine_with([email_contract()]);
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let body = ingress(&mut trajectory, &["alice", "bob"], Trust::TRUSTED, "doc");
    let request = email_request(&mut trajectory, body, "bob");

    let Ok(FlowOutcome::AllowedNow(token)) = engine.evaluate(&mut trajectory, request) else {
        panic!("expected permit");
    };
    trajectory
        .admit_model_output(OpaqueValue::new("thinking"), BTreeSet::from([body]), BTreeSet::new())
        .unwrap();

    let err = dispatch(&mut trajectory, token, "sent").unwrap_err();
    assert!(matches!(err, RejectedToken::Stale { .. }));
}

#[test]
fn foreign_trajectory_token_is_rejected() {
    let engine = engine_with([email_contract()]);
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let body = ingress(&mut trajectory, &["alice", "bob"], Trust::TRUSTED, "doc");
    let request = email_request(&mut trajectory, body, "bob");
    let Ok(FlowOutcome::AllowedNow(token)) = engine.evaluate(&mut trajectory, request) else {
        panic!("expected permit");
    };

    let mut other = Trajectory::new();
    let err = dispatch(&mut other, token, "sent").unwrap_err();
    assert!(matches!(err, RejectedToken::ForeignTrajectory { .. }));
}

#[test]
fn second_distinct_proposal_is_refused_until_abandoned() {
    let engine = engine_with([email_contract()]);
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let body = ingress(&mut trajectory, &["alice", "bob"], Trust::TRUSTED, "doc");
    let first = email_request(&mut trajectory, body, "bob");
    let second = ToolRequest::new(ToolName::new("email.send"), ArgumentTree::Value(body), BTreeSet::new());

    let Ok(FlowOutcome::AllowedNow(_token)) = engine.evaluate(&mut trajectory, first.clone()) else {
        panic!("expected permit");
    };
    let pending = trajectory.pending_action().unwrap().id();

    let revision_before = trajectory.revision();
    let Err(refusal) = engine.evaluate(&mut trajectory, second.clone()) else {
        panic!("expected refusal");
    };
    assert_eq!(refusal, FlowRefusal::ActionAlreadyPending { pending });
    // The in-flight action is untouched by the refused proposal.
    assert_eq!(trajectory.pending_action().unwrap().id(), pending);
    assert_eq!(trajectory.revision(), revision_before);

    trajectory.abandon_pending().unwrap();
    assert!(trajectory.pending_action().is_none());
}

#[test]
fn re_entry_reuses_the_pending_action() {
    let engine = engine_with([email_contract()]);
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let body = ingress(&mut trajectory, &["alice", "bob"], Trust::TRUSTED, "doc");
    let request = email_request(&mut trajectory, body, "bob");

    let Ok(FlowOutcome::AllowedNow(first)) = engine.evaluate(&mut trajectory, request.clone()) else {
        panic!("expected permit");
    };
    let Ok(FlowOutcome::AllowedNow(second)) = engine.evaluate(&mut trajectory, request) else {
        panic!("expected permit on re-entry");
    };
    assert_eq!(first.action(), second.action());

    dispatch(&mut trajectory, second, "sent").unwrap();
    let err = dispatch(&mut trajectory, first, "again").unwrap_err();
    assert!(matches!(err, RejectedToken::Stale { .. }));
}

#[test]
fn committed_effects_feed_later_checks() {
    let mut report = email_contract();
    report.name = ToolName::new("report.generate");
    report.requires = Some(Requirements {
        forbid_prior_effects: BTreeSet::from([Effect::Egress]),
        ..Requirements::default()
    });
    report.effects = Effects::none();
    report.arguments = ArgumentSchema::opaque();

    let engine = engine_with([email_contract(), report]);
    let mut trajectory = Trajectory::new();
    let body = ingress(&mut trajectory, &["alice", "bob"], Trust::TRUSTED, "doc");
    let request = email_request(&mut trajectory, body, "bob");

    let token = walk_to_permit(&engine, &mut trajectory, request);
    dispatch(&mut trajectory, token, "sent").unwrap();

    let report_request = ToolRequest::new(
        ToolName::new("report.generate"),
        ArgumentTree::Value(body),
        BTreeSet::new(),
    );
    let Some(block) = terminal_block_of(engine.evaluate(&mut trajectory, report_request)) else {
        panic!("expected terminal block");
    };
    assert!(matches!(
        block.violations.as_slice(),
        [Violation::Breach(crate::contract::Breach::ForbiddenPriorEffects { .. })]
    ));
}

#[test]
fn duplicate_contract_is_refused() {
    let mut engine = PolicyEngine::new();
    engine.register(email_contract()).unwrap();
    assert!(matches!(
        engine.register(email_contract()),
        Err(crate::engine::ContractRefused::Duplicate(DuplicateContract { tool })) if tool == ToolName::new("email.send")
    ));
}

#[test]
fn unknown_value_reference_blocks_loudly() {
    let engine = engine_with([email_contract()]);
    let mut trajectory = Trajectory::new();
    let ghost = ValueId::new(1000);
    let request = ToolRequest::new(ToolName::new("email.send"), ArgumentTree::Value(ghost), BTreeSet::new());

    let revision_before = trajectory.revision();
    let Err(refusal) = engine.evaluate(&mut trajectory, request) else {
        panic!("expected refusal");
    };
    assert_eq!(refusal, FlowRefusal::UnknownValueReferenced { value: ghost });
    // The refusal touched nothing.
    assert_eq!(trajectory.revision(), revision_before);
}

#[test]
fn effects_survive_a_declared_dispatch_failure() {
    let engine = engine_with([email_contract()]);
    let mut trajectory = Trajectory::new();
    let body = ingress(&mut trajectory, &["alice", "bob"], Trust::TRUSTED, "doc");
    let request = email_request(&mut trajectory, body, "bob");

    let token = walk_to_permit(&engine, &mut trajectory, request);
    let (canonical, receipt) = trajectory.release(token).unwrap();
    assert_eq!(canonical.tool, ToolName::new("email.send"));
    // Effects are committed at release, before any result exists.
    assert_eq!(trajectory.past_effects(), &Effects::declared([Effect::Egress]));

    trajectory.record_failure(receipt).unwrap();
    assert!(trajectory.pending_action().is_none());
    // Failure removes nothing.
    assert_eq!(trajectory.past_effects(), &Effects::declared([Effect::Egress]));
}

#[test]
fn canonical_request_renders_the_checked_tree() {
    let engine = engine_with([email_contract()]);
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let body = ingress(&mut trajectory, &["alice", "bob"], Trust::TRUSTED, "the doc");
    let request = email_request(&mut trajectory, body, "bob");

    let Ok(FlowOutcome::AllowedNow(token)) = engine.evaluate(&mut trajectory, request) else {
        panic!("expected permit");
    };
    let (canonical, receipt) = trajectory.release(token).unwrap();
    assert_eq!(canonical.rendered, r#"{"body":"the doc","to":"bob"}"#);
    trajectory.record_output(receipt, OpaqueValue::new("sent")).unwrap();
}

#[test]
fn pursue_returns_a_permit_produced_by_the_final_allowed_step() {
    let mut engine = engine_with([email_contract()]);
    engine.register_transformer(redact_transformer()).unwrap();
    let mut trajectory = Trajectory::new();
    let body = ingress(&mut trajectory, &["alice", "bob"], Trust::SUSPICIOUS, "raw");
    let request = email_request(&mut trajectory, body, "bob");
    let Pursuit::Permitted(token) = engine.pursue(&mut trajectory, request, 1) else {
        panic!("the final allowed step's permit must be returned");
    };
    dispatch(&mut trajectory, token, "sent").unwrap();
}

#[test]
fn pursue_permit_commits_nothing_before_release() {
    let engine = engine_with([egress_tool()]);
    let mut trajectory = Trajectory::new();
    let body = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "ping");
    let Pursuit::Permitted(token) = engine.pursue(&mut trajectory, ping_request(body), 8) else {
        panic!("expected a permit");
    };
    assert_eq!(trajectory.past_effects(), &Effects::none());
    let (_, receipt) = trajectory.release(token).unwrap();
    assert_eq!(trajectory.past_effects(), &Effects::declared([Effect::Egress]));
    trajectory.record_output(receipt, OpaqueValue::new("pong")).unwrap();
}

#[test]
fn pursue_with_zero_bound_still_reports_a_proven_terminal() {
    let engine = engine_with([email_contract()]);
    let mut trajectory = Trajectory::new();
    // Suspicious body, no transformer, no authority: nothing can remedy.
    let body = ingress(&mut trajectory, &["alice", "bob"], Trust::SUSPICIOUS, "raw");
    let request = email_request(&mut trajectory, body, "bob");
    let Pursuit::Terminal { violations, reason } = engine.pursue(&mut trajectory, request, 0) else {
        panic!("a proven terminal must not stall on a zero bound");
    };
    assert_eq!(reason, BlockReason::NoRemedy);
    assert!(!violations.is_empty());
}

#[test]
fn pursue_stall_abandons_the_pending_action() {
    let mut engine = engine_with([email_contract()]);
    engine.register_transformer(redact_transformer()).unwrap();
    let mut trajectory = Trajectory::new();
    let body = ingress(&mut trajectory, &["alice", "bob"], Trust::SUSPICIOUS, "raw");
    let request = email_request(&mut trajectory, body, "bob");
    let Pursuit::Stalled { violations, cause } = engine.pursue(&mut trajectory, request.clone(), 0) else {
        panic!("a zero bound must stall a remediable flow");
    };
    assert_eq!(cause, StallCause::BoundExhausted);
    assert!(!violations.is_empty());
    assert!(trajectory.pending_action().is_none());
    let Pursuit::Permitted(token) = engine.pursue(&mut trajectory, request, 8) else {
        panic!("the trajectory must be free after a stall");
    };
    dispatch(&mut trajectory, token, "sent").unwrap();
}

/// Pursuing a different proposal while an action is in flight is refused
/// terminally WITHOUT touching the in-flight action: its token still
/// releases — the one terminal where clearing the slot would be a bug.
#[test]
fn pursue_of_a_different_proposal_leaves_the_inflight_action_untouched() {
    let engine = engine_with([email_contract()]);
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let body = ingress(&mut trajectory, &["alice", "bob"], Trust::TRUSTED, "doc");
    let to_bob = identity_ingress(&mut trajectory, "bob");
    let to_alice = identity_ingress(&mut trajectory, "alice");
    let first = ToolRequest::new(
        ToolName::new("email.send"),
        ArgumentTree::object([("to", to_bob), ("body", body)]),
        [],
    );
    let second = ToolRequest::new(
        ToolName::new("email.send"),
        ArgumentTree::object([("to", to_alice), ("body", body)]),
        [],
    );

    let Ok(FlowOutcome::AllowedNow(token)) = engine.evaluate(&mut trajectory, first) else {
        panic!("expected a permit");
    };
    let revision_before = trajectory.revision();
    let Pursuit::Refused(refusal) = engine.pursue(&mut trajectory, second, 8) else {
        panic!("a different proposal while one is pending must be refused");
    };
    assert!(matches!(refusal, FlowRefusal::ActionAlreadyPending { .. }));
    // The refusal touched nothing: the in-flight action and its token survive.
    assert_eq!(trajectory.revision(), revision_before);
    assert!(trajectory.pending_action().is_some());
    dispatch(&mut trajectory, token, "sent").unwrap();
}

#[test]
fn pursue_keeps_the_slot_for_an_external_ruling() {
    let mut engine = engine_with([email_contract()]);
    engine.register_authority(human()).unwrap();
    let mut trajectory = Trajectory::new();
    let body = ingress(&mut trajectory, &["alice"], Trust::UNKNOWN, "doc");
    let request = email_request(&mut trajectory, body, "alice");
    let Pursuit::NeedsApproval(pending) = engine.pursue(&mut trajectory, request, 8) else {
        panic!("the external endorser should defer");
    };
    assert!(trajectory.pending_action().is_some());
    let Some(token) = execution(
        engine
            .apply_approval(
                &mut trajectory,
                pending,
                crate::approval::Ruling::Approve {
                    reason: "acquired".to_owned(),
                },
            )
            .unwrap(),
    ) else {
        panic!("the approval should permit");
    };
    dispatch(&mut trajectory, token, "pong").unwrap();
}

#[test]
fn combinator_built_inline_authority_endorses_to_a_permit() {
    let mut engine = engine_with([email_contract()]);
    engine
        .register_authority(Authority::inline(
            "approver",
            crate::transition::AuthorityMandate::none().vouch_audience([user("carol")]),
            approve_all,
        ))
        .unwrap();
    let mut trajectory = Trajectory::new();
    let body = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "doc");
    let request = email_request(&mut trajectory, body, "carol");
    let token = walk_to_permit(&engine, &mut trajectory, request);
    dispatch(&mut trajectory, token, "sent").unwrap();
}

#[test]
fn combinator_built_external_authority_defers_naming_it() {
    let mut engine = engine_with([email_contract()]);
    engine
        .register_authority(Authority::external(
            "trust-approver",
            crate::transition::AuthorityMandate::none().endorse_trust(KnownTrust::Trusted),
        ))
        .unwrap();
    let mut trajectory = Trajectory::new();
    let body = ingress(&mut trajectory, &["alice"], Trust::UNKNOWN, "doc");
    let request = email_request(&mut trajectory, body, "alice");
    let Pursuit::NeedsApproval(pending) = engine.pursue(&mut trajectory, request, 8) else {
        panic!("the external endorser should defer");
    };
    assert_eq!(pending.authority(), &AuthorityName::new("trust-approver"));
}

#[test]
fn source_contract_output_wears_the_declared_label() {
    let internal = || ValueLabel::trusted_readers([user("alice"), user("bob")]);
    let engine = engine_with([ToolContract::source("invoices.list", internal())]);
    let mut trajectory = Trajectory::new();
    let request = ToolRequest::new(ToolName::new("invoices.list"), ArgumentTree::empty(), []);
    let Ok(FlowOutcome::AllowedNow(token)) = engine.evaluate(&mut trajectory, request) else {
        panic!("a pure read must permit");
    };
    let id = dispatch(&mut trajectory, token, "47 invoices").unwrap();
    assert_eq!(trajectory.value(id).unwrap().label(), &internal());
}

#[test]
fn egress_sink_contract_resolves_recipients_and_blocks_undeclared() {
    let engine = engine_with([ToolContract::egress_sink("email.send", "to")]);
    let mut trajectory = Trajectory::new();

    let body = ingress(&mut trajectory, &["bob"], Trust::TRUSTED, "doc");
    let request = email_request(&mut trajectory, body, "bob");
    let token = walk_to_permit(&engine, &mut trajectory, request);
    dispatch(&mut trajectory, token, "sent").unwrap();
    assert_eq!(trajectory.past_effects(), &Effects::declared([Effect::Egress]));

    let body = ingress(&mut trajectory, &["bob"], Trust::TRUSTED, "doc two");
    let bare = ToolRequest::new(ToolName::new("email.send"), ArgumentTree::object([("body", body)]), []);
    let Some(block) = terminal_block_of(engine.evaluate(&mut trajectory, bare)) else {
        panic!("an egress sink with no recipients argument must block terminally");
    };
    assert!(
        block
            .violations
            .contains(&Violation::Breach(crate::contract::Breach::UndeclaredRecipients))
    );
}

#[test]
fn object_built_request_checks_and_renders_like_the_literal_tree() {
    let engine = engine_with([email_contract()]);
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let body = ingress(&mut trajectory, &["alice", "bob"], Trust::TRUSTED, "the doc");
    let to = identity_ingress(&mut trajectory, "bob");
    let request = ToolRequest::new(
        ToolName::new("email.send"),
        ArgumentTree::object([("to", to), ("body", body)]),
        [body, body],
    );
    assert_eq!(request.control, BTreeSet::from([body]));

    let Ok(FlowOutcome::AllowedNow(token)) = engine.evaluate(&mut trajectory, request) else {
        panic!("expected permit");
    };
    let (canonical, receipt) = trajectory.release(token).unwrap();
    assert_eq!(canonical.rendered, r#"{"body":"the doc","to":"bob"}"#);
    trajectory.record_output(receipt, OpaqueValue::new("sent")).unwrap();
}

#[test]
fn a_receipt_survives_unrelated_mutations_and_closes_the_action() {
    let engine = engine_with([email_contract()]);
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let body = ingress(&mut trajectory, &["alice", "bob"], Trust::TRUSTED, "doc");
    let request = email_request(&mut trajectory, body, "bob");

    let Ok(FlowOutcome::AllowedNow(token)) = engine.evaluate(&mut trajectory, request) else {
        panic!("expected permit");
    };
    let (_, receipt) = trajectory.release(token).unwrap();
    trajectory
        .admit_model_output(OpaqueValue::new("meanwhile"), BTreeSet::from([body]), BTreeSet::new())
        .unwrap();
    trajectory.record_output(receipt, OpaqueValue::new("sent")).unwrap();
    assert!(trajectory.pending_action().is_none());
}

#[test]
fn a_receipt_closes_a_failed_dispatch_after_an_interleaved_emission() {
    let mut engine = response_engine(&["alice", "bob"]);
    engine.register(email_contract()).unwrap();
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let body = ingress(&mut trajectory, &["alice", "bob"], Trust::TRUSTED, "doc");
    let request = email_request(&mut trajectory, body, "bob");

    let Ok(FlowOutcome::AllowedNow(token)) = engine.evaluate(&mut trajectory, request) else {
        panic!("expected permit");
    };
    let (_, receipt) = trajectory.release(token).unwrap();

    let note = ingress(&mut trajectory, &["alice", "bob"], Trust::TRUSTED, "sending now");
    let emission = EmissionRequest {
        body: ArgumentTree::Value(note),
        control: BTreeSet::new(),
        basis: trajectory.revision(),
    };
    assert!(matches!(
        engine.evaluate_emission(&mut trajectory, emission),
        Ok(FlowOutcome::AllowedNow(Emitted { .. }))
    ));

    trajectory.record_failure(receipt).unwrap();
    assert!(trajectory.pending_action().is_none());
    // The may-effects committed at release survive the failure.
    assert_eq!(trajectory.past_effects(), &Effects::declared([Effect::Egress]));
}

#[test]
fn foreign_receipt_is_rejected() {
    let engine = engine_with([email_contract()]);
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let body = ingress(&mut trajectory, &["alice", "bob"], Trust::TRUSTED, "doc");
    let request = email_request(&mut trajectory, body, "bob");
    let Ok(FlowOutcome::AllowedNow(token)) = engine.evaluate(&mut trajectory, request) else {
        panic!("expected permit");
    };
    let (_, receipt) = trajectory.release(token).unwrap();

    let mut other = Trajectory::new();
    let err = other.record_output(receipt, OpaqueValue::new("sent")).unwrap_err();
    assert!(matches!(err, RejectedToken::ForeignTrajectory { .. }));
}

#[test]
fn a_confirmation_ruling_admits_exactly_one_dispatch() {
    let drop_contract = || ToolContract {
        name: ToolName::new("db.drop"),
        requires: Some(Requirements {
            attention: crate::contract::AttentionRule::ExplicitConfirmation,
            ..Requirements::default()
        }),
        output_label: ValueLabel::identity(),
        effects: Effects::declared([Effect::Mutation]),
        arguments: ArgumentSchema::opaque(),
    };

    let engine = engine_with([drop_contract()]);
    let mut trajectory = Trajectory::new();
    let go = identity_ingress(&mut trajectory, "yes, drop it");
    let request = ToolRequest::new(ToolName::new("db.drop"), ArgumentTree::Value(go), BTreeSet::new());
    let Some(block) = terminal_block_of(engine.evaluate(&mut trajectory, request)) else {
        panic!("expected terminal block without a confirming authority");
    };
    assert!(matches!(
        block.violations.as_slice(),
        [Violation::Breach(crate::contract::Breach::ConfirmationMissing { .. })]
    ));

    // A competent authority confirms: one ruling, one dispatch.
    let mut engine = engine_with([drop_contract()]);
    engine
        .register_authority(inline_authority(
            "confirmer",
            crate::transition::AuthorityMandate::none().confirms(),
            approve_all,
        ))
        .unwrap();
    let mut trajectory = Trajectory::new();
    let go = identity_ingress(&mut trajectory, "yes, drop it");
    let request = ToolRequest::new(ToolName::new("db.drop"), ArgumentTree::Value(go), BTreeSet::new());
    let token = walk_to_permit(&engine, &mut trajectory, request.clone());
    let (_, receipt) = trajectory.release(token).unwrap();
    trajectory.record_failure(receipt).unwrap();

    let Ok(FlowOutcome::Blocked { violations, .. }) = engine.evaluate(&mut trajectory, request) else {
        panic!("expected the repeat to demand a fresh confirmation");
    };
    assert!(matches!(
        violations.as_slice(),
        [Violation::Breach(crate::contract::Breach::ConfirmationMissing { .. })]
    ));
}

fn response_engine(readers: &[&str]) -> PolicyEngine {
    PolicyEngine::new()
        .with_response_policy(ResponsePolicy {
            requires: Requirements {
                audience: crate::contract::AudienceRule::FromRecipients,
                ..Requirements::default()
            },
            readers: readers.iter().map(|r| user(r)).collect(),
        })
        .unwrap()
}

#[test]
fn clean_response_is_emitted_from_the_exact_checked_tree() {
    let engine = response_engine(&["alice"]);
    let mut trajectory = Trajectory::new();
    let note = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "all done");
    let request = EmissionRequest {
        body: ArgumentTree::Value(note),
        control: BTreeSet::new(),
        basis: trajectory.revision(),
    };

    let Ok(FlowOutcome::AllowedNow(Emitted { value, rendered })) = engine.evaluate_emission(&mut trajectory, request)
    else {
        panic!("expected emission");
    };
    assert_eq!(rendered, "\"all done\"");
    // The emitted value is the rendered bytes, derived from the tree.
    assert_eq!(trajectory.value(value).unwrap().body().as_str(), rendered);
    assert!(matches!(
        trajectory.turns().last(),
        Some(crate::turn::Turn {
            actor: crate::turn::Actor::Assistant,
            ..
        })
    ));
}

#[test]
fn response_leaking_outside_readers_is_blocked() {
    let engine = response_engine(&["charlie"]);
    let mut trajectory = Trajectory::new();
    let secret = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "secret");
    let summary = trajectory
        .admit_model_output(
            OpaqueValue::new("about the secret"),
            BTreeSet::from([secret]),
            BTreeSet::new(),
        )
        .unwrap();
    let request = EmissionRequest {
        body: ArgumentTree::Value(summary),
        control: BTreeSet::new(),
        basis: trajectory.revision(),
    };

    let Some(block) = terminal_block_of(engine.evaluate_emission(&mut trajectory, request)) else {
        panic!("expected block");
    };
    assert!(matches!(
        block.violations.as_slice(),
        [Violation::Breach(crate::contract::Breach::AudienceExceeds { .. })]
    ));
}

#[test]
fn response_control_dependence_is_checked() {
    let engine = response_engine(&["charlie"]);
    let mut trajectory = Trajectory::new();
    let secret = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "secret");
    let bland = ingress(&mut trajectory, &["alice", "charlie"], Trust::TRUSTED, "ok");
    let request = EmissionRequest {
        body: ArgumentTree::Value(bland),
        control: BTreeSet::from([secret]),
        basis: trajectory.revision(),
    };

    let Some(block) = terminal_block_of(engine.evaluate_emission(&mut trajectory, request)) else {
        panic!("expected block");
    };
    assert!(matches!(
        block.violations.as_slice(),
        [Violation::Breach(crate::contract::Breach::AudienceExceeds { .. })]
    ));
}

#[test]
fn stale_response_basis_is_blocked_and_touches_nothing() {
    let engine = response_engine(&["alice"]);
    let mut trajectory = Trajectory::new();
    let note = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "done");
    let stale_basis = trajectory.revision();
    // The trajectory moves on before emission.
    trajectory
        .admit_model_output(OpaqueValue::new("more"), BTreeSet::from([note]), BTreeSet::new())
        .unwrap();
    let turns_before = trajectory.turns().len();

    let request = EmissionRequest {
        body: ArgumentTree::Value(note),
        control: BTreeSet::new(),
        basis: stale_basis,
    };
    let revision_before = trajectory.revision();
    let Err(refusal) = engine.evaluate_emission(&mut trajectory, request) else {
        panic!("expected refusal");
    };
    assert!(matches!(refusal, FlowRefusal::StaleBasis { .. }));
    // The refusal touched nothing.
    assert_eq!(trajectory.turns().len(), turns_before);
    assert_eq!(trajectory.revision(), revision_before);
    assert!(trajectory.pending_emission().is_none());
}

#[test]
fn response_without_policy_is_unprovable() {
    let engine = engine_with([]);
    let mut trajectory = Trajectory::new();
    let note = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "hi");
    let request = EmissionRequest {
        body: ArgumentTree::Value(note),
        control: BTreeSet::new(),
        basis: trajectory.revision(),
    };

    let Some(block) = terminal_block_of(engine.evaluate_emission(&mut trajectory, request)) else {
        panic!("expected block");
    };
    assert_eq!(block.reason, BlockReason::NoRemedy);
    assert!(matches!(
        block.violations.as_slice(),
        [Violation::Unprovable(Unprovable::NoContract { tool })] if *tool == ToolName::new(RESPONSE_SINK)
    ));
}

#[test]
fn duplicate_reentry_token_cannot_release_twice() {
    let engine = engine_with([email_contract()]);
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let body = ingress(&mut trajectory, &["alice", "bob"], Trust::TRUSTED, "doc");
    let request = email_request(&mut trajectory, body, "bob");

    let Ok(FlowOutcome::AllowedNow(first)) = engine.evaluate(&mut trajectory, request.clone()) else {
        panic!("expected permit");
    };
    let Ok(FlowOutcome::AllowedNow(second)) = engine.evaluate(&mut trajectory, request) else {
        panic!("expected permit on re-entry");
    };

    let (_, receipt) = trajectory.release(first).unwrap();
    let err = trajectory.release(second).unwrap_err();
    assert!(matches!(err, RejectedToken::Stale { .. }));
    trajectory.record_output(receipt, OpaqueValue::new("sent")).unwrap();
}

#[test]
fn unknown_control_dependency_blocks_loudly() {
    let engine = engine_with([email_contract()]);
    let mut trajectory = Trajectory::new();
    let body = ingress(&mut trajectory, &["alice", "bob"], Trust::TRUSTED, "doc");
    let ghost = ValueId::new(1000);
    let request = ToolRequest::new(
        ToolName::new("email.send"),
        ArgumentTree::Value(body),
        BTreeSet::from([ghost]),
    );

    let revision_before = trajectory.revision();
    let Err(refusal) = engine.evaluate(&mut trajectory, request) else {
        panic!("expected refusal");
    };
    assert_eq!(refusal, FlowRefusal::UnknownValueReferenced { value: ghost });
    assert_eq!(trajectory.revision(), revision_before);
}

#[test]
fn duplicate_transformer_registration_refused() {
    fn passthrough(v: &OpaqueValue) -> Result<OpaqueValue, crate::transition::TransformerError> {
        Ok(v.clone())
    }
    let entry = || RegisteredTransformer {
        descriptor: crate::transition::TransformerDescriptor {
            transformer: crate::value::TransformerRef {
                id: "pii.redact".into(),
                version: 1,
            },
            precondition: crate::transition::LabelPredicate::any(),
            output: ValueLabel::identity(),
        },
        run: passthrough,
    };
    let mut engine = PolicyEngine::new();
    engine.register_transformer(entry()).unwrap();
    assert!(engine.register_transformer(entry()).is_err());
}

fn redact_transformer() -> RegisteredTransformer {
    fn redact(_: &OpaqueValue) -> Result<OpaqueValue, crate::transition::TransformerError> {
        Ok(OpaqueValue::new("[redacted]"))
    }
    RegisteredTransformer {
        descriptor: crate::transition::TransformerDescriptor {
            transformer: crate::value::TransformerRef {
                id: "pii.redact".into(),
                version: 1,
            },
            precondition: crate::transition::LabelPredicate {
                trust: Some(Trust::SUSPICIOUS),
                audience: None,
            },
            output: ValueLabel::identity(),
        },
        run: redact,
    }
}

fn human() -> crate::approval::Authority {
    crate::approval::Authority {
        name: crate::audit::AuthorityName::new("human"),
        mandate: crate::transition::AuthorityMandate {
            trust: Some(crate::dimension::KnownTrust::Trusted),
            audience: Some(BTreeSet::from([user("alice"), user("bob"), user("charlie")])),
            waive_prior_effects: true,
            confirms: true,
            acknowledge_unknown: true,
            may_release_control: true,
        },
        mode: crate::approval::AuthorityMode::External,
    }
}

#[test]
fn same_label_transformers_are_distinct_frontier_routes() {
    fn scrub(_: &OpaqueValue) -> Result<OpaqueValue, crate::transition::TransformerError> {
        Ok(OpaqueValue::new("[scrubbed]"))
    }
    let mut sibling = redact_transformer();
    sibling.descriptor.transformer = crate::value::TransformerRef {
        id: "pii.scrub".into(),
        version: 1,
    };
    sibling.run = scrub;

    let mut engine = engine_with([email_contract()]);
    engine.register_transformer(redact_transformer()).unwrap();
    engine.register_transformer(sibling).unwrap();
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let raw = ingress(&mut trajectory, &["alice", "bob"], Trust::SUSPICIOUS, "raw page");
    let request = email_request(&mut trajectory, raw, "bob");

    let Ok(FlowOutcome::Blocked {
        plans, terminal: None, ..
    }) = engine.evaluate(&mut trajectory, request)
    else {
        panic!("expected remediable block");
    };
    let plans = NonEmptyVec::from_vec(plans).expect("a remediable block carries at least one plan");
    let mut derive_transformers: Vec<String> = plans
        .iter()
        .filter(|p| p.steps.len() == 1)
        .filter_map(|p| match p.steps.first() {
            PlannedRemedy::Reduce(crate::remedy::ReductionTarget::DeriveValue { transformer, .. }) => {
                Some(transformer.id.clone())
            }
            _ => None,
        })
        .collect();
    derive_transformers.sort();
    assert_eq!(derive_transformers, ["pii.redact", "pii.scrub"]);
}

#[test]
fn tainted_payload_plans_a_transform() {
    let mut engine = engine_with([email_contract()]);
    engine.register_transformer(redact_transformer()).unwrap();
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let raw = ingress(&mut trajectory, &["alice", "bob"], Trust::SUSPICIOUS, "raw page");
    let request = email_request(&mut trajectory, raw, "bob");

    let Ok(FlowOutcome::Blocked {
        violations,
        plans,
        terminal: None,
    }) = engine.evaluate(&mut trajectory, request)
    else {
        panic!("expected remediable block");
    };
    let plans = NonEmptyVec::from_vec(plans).expect("a remediable block carries at least one plan");
    assert!(matches!(
        violations.as_slice(),
        [Violation::Breach(crate::contract::Breach::TrustBelow { .. })]
    ));
    let transform_plan = plans
        .iter()
        .find(|p| p.steps.len() == 1)
        .expect("single-step transform plan");
    assert_eq!(derive_step(transform_plan.steps.first()), Some(raw));
    assert_eq!(trajectory.plans().len(), plans.len());
    assert_eq!(trajectory.plans()[0].basis, trajectory.revision());
    assert!(trajectory.pending_action().is_some());

    // The plan predicts a clean flow: walking its single step permits.
    let StepOutcome::Advanced(advanced) = apply_first_step(&engine, &mut trajectory, transform_plan.id) else {
        panic!("the transform step must advance");
    };
    assert!(execution(advanced).is_some(), "the transform plan must unlock the flow");
}

#[test]
fn audience_breach_plans_an_endorse() {
    let mut engine = engine_with([email_contract()]);
    engine.register_authority(human()).unwrap();
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    // Only alice may read the doc; sending to charlie exceeds it.
    let doc = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "private doc");
    let request = email_request(&mut trajectory, doc, "charlie");

    let plans = remediable(&engine, &mut trajectory, request);
    let endorse = plans.first();
    assert_eq!(endorse.steps.len(), 1);
    assert!(raise_step(endorse.steps.first()).is_some_and(|(source, raise)| {
        source == doc && raise.audience.as_ref().is_some_and(|r| r.contains(&user("charlie")))
    }));
    let StepOutcome::NeedsApproval(pending) = apply_first_step(&engine, &mut trajectory, endorse.id) else {
        panic!("expected the external human to be consulted");
    };
    assert_eq!(pending.authority().as_str(), "human");
}

#[test]
fn a_multi_source_audience_breach_endorses_every_contributing_leaf() {
    let mut engine = engine_with([email_contract()]);
    engine
        .register_authority(inline_authority("auto", human().mandate, approve_all))
        .unwrap();
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let part1 = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "part one");
    let part2 = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "part two");
    let to = identity_ingress(&mut trajectory, "bob");
    let request = ToolRequest::new(
        ToolName::new("email.send"),
        ArgumentTree::Object(std::collections::BTreeMap::from([
            (ArgumentName::new("to"), ArgumentTree::Value(to)),
            (
                ArgumentName::new("body"),
                ArgumentTree::Object(std::collections::BTreeMap::from([
                    (ArgumentName::new("0"), ArgumentTree::Value(part1)),
                    (ArgumentName::new("1"), ArgumentTree::Value(part2)),
                ])),
            ),
        ])),
        BTreeSet::new(),
    );

    let plans = remediable(&engine, &mut trajectory, request);
    let plan_id = plans.first().id;
    let endorsed: BTreeSet<ValueId> = plans
        .first()
        .steps
        .iter()
        .filter_map(|s| raise_step(s).map(|(source, _)| source))
        .collect();
    assert_eq!(
        endorsed,
        BTreeSet::from([part1, part2]),
        "both contributing leaves are endorsed"
    );

    // Applying only the first endorse does not yet clear the breach.
    let StepOutcome::Advanced(mut decision) = apply_first_step(&engine, &mut trajectory, plan_id) else {
        panic!("expected the step to advance");
    };
    assert!(
        matches!(decision, FlowOutcome::Blocked { terminal: None, .. }),
        "a single endorse does not clear a two-leaf intersection breach"
    );
    // Continuing endorses the second leaf and reaches a permit.
    loop {
        match decision {
            FlowOutcome::AllowedNow(_) => break,
            FlowOutcome::Blocked {
                plans, terminal: None, ..
            } => {
                let plan = plans.first().expect("a remediable block carries plans").id;
                decision = match apply_first_step(&engine, &mut trajectory, plan) {
                    StepOutcome::Advanced(d) => d,
                    other => panic!("unexpected outcome: {other:?}"),
                };
            }
            other => panic!("expected to reach a permit, got {other:?}"),
        }
    }
}

#[test]
fn a_granted_endorse_durably_relabels_the_source_and_permits() {
    let mut engine = engine_with([email_contract()]);
    engine.register_authority(human()).unwrap();
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let doc = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "private doc");
    let doc_label = trajectory.value(doc).unwrap().label().clone();
    let request = email_request(&mut trajectory, doc, "charlie");

    let plans = remediable(&engine, &mut trajectory, request);
    let endorse_plan = plans
        .iter()
        .find(|p| raise_step(p.steps.first()).is_some_and(|(source, _)| source == doc))
        .expect("an endorse plan for the doc");
    let StepOutcome::NeedsApproval(pending) = apply_first_step(&engine, &mut trajectory, endorse_plan.id) else {
        panic!("expected the external human to be consulted");
    };
    assert_eq!(pending.authority().as_str(), "human");

    let decision = engine
        .apply_approval(
            &mut trajectory,
            pending,
            Ruling::Approve {
                reason: "vouched".into(),
            },
        )
        .unwrap();
    assert!(
        matches!(decision, FlowOutcome::AllowedNow(_)),
        "the raise clears the audience breach"
    );

    // Durability by construction: the source is untouched; a new value
    // carries the raised label with Endorsed provenance naming the authority.
    assert_eq!(trajectory.value(doc).unwrap().label(), &doc_label);
    let (derived, authority) = trajectory
        .audit()
        .iter()
        .find_map(|e| applied_raise(e).map(|(derived, authority)| (derived, authority.clone())))
        .expect("the endorse was audited");
    assert_eq!(authority.as_str(), "human");
    let labels = trajectory
        .audit()
        .iter()
        .find_map(|e| match e {
            AuditEvent::AuthorizationApplied {
                derived: Some(_),
                labels: Some(labels),
                ..
            } => Some(labels.clone()),
            _ => None,
        })
        .expect("a durable raise audits its labels");
    assert_eq!(labels.input, doc_label);
    let derived_stored = trajectory.value(derived).unwrap();
    assert_eq!(&labels.raised, derived_stored.label());
    assert_ne!(
        derived_stored.label(),
        &doc_label,
        "the derived value's label was raised"
    );
    assert!(matches!(
        derived_stored.provenance(),
        crate::value::Provenance::Endorsed { source, .. } if *source == doc
    ));
}

#[test]
fn an_endorse_routes_only_within_the_mandate_bounds() {
    let mut engine = engine_with([email_contract()]);
    engine.register_authority(human()).unwrap(); // may vouch alice/bob/charlie
    let mut trajectory = Trajectory::new();
    let doc = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "doc");
    let view = TrajectoryView::new(trajectory.view());

    let beyond = crate::remedy::Authorization::new(
        crate::remedy::AuthorizationDelta::single(crate::remedy::DeltaCoordinate::RaiseLabel(
            crate::remedy::LabelRaise {
                trust: None,
                audience: Some(BTreeSet::from([user("dave")])),
            },
        )),
        crate::remedy::AuthorizationScope::DerivedValue { source: doc },
    )
    .unwrap();
    assert!(matches!(
        engine.route_grant(&beyond, &[], &view),
        RoutedRuling::NoRuling
    ));

    let within = crate::remedy::Authorization::new(
        crate::remedy::AuthorizationDelta::single(crate::remedy::DeltaCoordinate::RaiseLabel(
            crate::remedy::LabelRaise {
                trust: None,
                audience: Some(BTreeSet::from([user("charlie")])),
            },
        )),
        crate::remedy::AuthorizationScope::DerivedValue { source: doc },
    )
    .unwrap();
    assert!(matches!(
        engine.route_grant(&within, &[], &view),
        RoutedRuling::External(_)
    ));
}

#[test]
fn a_denied_endorse_is_terminal_and_mints_no_value() {
    let mut engine = engine_with([email_contract()]);
    engine.register_authority(human()).unwrap();
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let doc = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "private doc");
    let request = email_request(&mut trajectory, doc, "charlie");

    let plans = remediable(&engine, &mut trajectory, request);
    let endorse_plan = plans
        .iter()
        .find(|p| raise_step(p.steps.first()).is_some_and(|(source, _)| source == doc))
        .expect("an endorse plan for the doc");
    let values_before = trajectory.store().len();
    let StepOutcome::NeedsApproval(pending) = apply_first_step(&engine, &mut trajectory, endorse_plan.id) else {
        panic!("expected the external human to be consulted");
    };

    let decision = engine
        .apply_approval(
            &mut trajectory,
            pending,
            Ruling::Deny {
                reason: "suspicious source".into(),
            },
        )
        .unwrap();
    assert!(matches!(decision, FlowOutcome::Blocked { terminal: Some(_), .. }));
    assert_eq!(
        trajectory.store().len(),
        values_before,
        "a denied endorse mints nothing"
    );
    assert!(
        trajectory
            .audit()
            .iter()
            .any(|e| denied_delta(e).is_some_and(delta_raises))
    );
}

#[test]
fn endorse_authority_refuses_a_suspicious_transitive_ancestry() {
    fn refuse_suspicious_ancestry(
        grant: &crate::remedy::Authorization,
        _: &[Violation],
        view: &crate::approval::TrajectoryView,
    ) -> Option<crate::approval::Ruling> {
        let crate::remedy::AuthorizationScope::DerivedValue { source } = &grant.scope() else {
            return None;
        };
        let tainted = view
            .ancestry(*source)
            .any(|(_, label, _)| label.trust == Trust::SUSPICIOUS);
        if tainted {
            None
        } else {
            Some(crate::approval::Ruling::Approve {
                reason: "clean ancestry".to_owned(),
            })
        }
    }
    let mut engine = engine_with([email_contract()]);
    engine
        .register_authority(inline_authority("vetter", human().mandate, refuse_suspicious_ancestry))
        .unwrap();

    let laundered_body = |trajectory: &mut Trajectory, root_trust: Trust| -> ValueId {
        let root = ingress(trajectory, &["alice"], root_trust, "raw");
        let trusted = ValueLabel {
            audience: Audience::readers([user("alice")]),
            trust: Trust::TRUSTED,
        };
        let mid = trajectory.seed_transformed(root, trusted.clone());
        trajectory.seed_transformed(mid, trusted)
    };

    // Suspicious root → the authority abstains → terminal.
    let mut tainted = Trajectory::new();
    tainted.seed_committed_effects(Effects::declared([Effect::Egress]));
    let body = laundered_body(&mut tainted, Trust::SUSPICIOUS);
    let request = email_request(&mut tainted, body, "charlie");
    let plans = remediable(&engine, &mut tainted, request);
    let Some(block) = advanced_terminal(apply_first_step(&engine, &mut tainted, plans.first().id)) else {
        panic!("a suspicious transitive ancestor should be refused");
    };
    assert_eq!(block.reason, BlockReason::NoAuthorityRuled);

    // Trusted root, same shape → endorsed and permitted.
    let mut clean = Trajectory::new();
    clean.seed_committed_effects(Effects::declared([Effect::Egress]));
    let body = laundered_body(&mut clean, Trust::TRUSTED);
    let request = email_request(&mut clean, body, "charlie");
    let _token = walk_to_permit(&engine, &mut clean, request);
}

#[test]
fn control_taint_plans_control_release_first() {
    let mut engine = engine_with([email_contract()]);
    engine.register_authority(human()).unwrap();
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let secret = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "secret");
    let clean = ingress(&mut trajectory, &["alice", "bob"], Trust::TRUSTED, "harmless");
    let to = identity_ingress(&mut trajectory, "bob");
    let request = ToolRequest::new(
        ToolName::new("email.send"),
        ArgumentTree::Object(std::collections::BTreeMap::from([
            (ArgumentName::new("to"), ArgumentTree::Value(to)),
            (ArgumentName::new("body"), ArgumentTree::Value(clean)),
        ])),
        BTreeSet::from([secret]),
    );

    let plans = remediable(&engine, &mut trajectory, request);
    assert_eq!(
        release_step(plans.first().steps.first()),
        Some(BTreeSet::from([secret]))
    );
}

#[test]
fn control_release_and_endorse_compose_for_a_mixed_audience_breach() {
    let mut engine = engine_with([email_contract()]);
    engine.register_authority(human()).unwrap();
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    // The body admits alice and bob; a control selector restricts to alice.
    let body = ingress(&mut trajectory, &["alice", "bob"], Trust::TRUSTED, "doc");
    let control = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "selector");
    let to_bob = identity_ingress(&mut trajectory, "bob");
    let to_charlie = identity_ingress(&mut trajectory, "charlie");
    let request = ToolRequest::new(
        ToolName::new("email.send"),
        ArgumentTree::Object(std::collections::BTreeMap::from([
            (
                ArgumentName::new("to"),
                ArgumentTree::Object(std::collections::BTreeMap::from([
                    (ArgumentName::new("0"), ArgumentTree::Value(to_bob)),
                    (ArgumentName::new("1"), ArgumentTree::Value(to_charlie)),
                ])),
            ),
            (ArgumentName::new("body"), ArgumentTree::Value(body)),
        ])),
        BTreeSet::from([control]),
    );
    let plans = remediable(&engine, &mut trajectory, request);
    let composes = plans.iter().any(|plan| {
        let endorses_charlie = plan.steps.iter().any(|step| {
            raise_step(step).is_some_and(|(source, raise)| {
                source == body && raise.audience.as_ref().is_some_and(|r| r.contains(&user("charlie")))
            })
        });
        let releases_control = plan
            .steps
            .iter()
            .any(|step| release_step(step) == Some(BTreeSet::from([control])));
        endorses_charlie && releases_control
    });
    assert!(
        composes,
        "the mixed breach should endorse the body for charlie and release control for bob"
    );
}

#[test]
fn control_release_is_least_privilege_over_joint_carriers() {
    let mut engine = engine_with([email_contract()]);
    engine.register_authority(human()).unwrap();
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    // Body admits alice and bob; the recipient is bob.
    let body = ingress(&mut trajectory, &["alice", "bob"], Trust::TRUSTED, "body");
    // Two controls each restrict the audience to alice (joint carriers).
    let secret_a = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "sel-a");
    let secret_b = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "sel-b");
    // An unrelated control at the identity label carries nothing.
    let noise = identity_ingress(&mut trajectory, "noise");
    let to_bob = identity_ingress(&mut trajectory, "bob");
    let request = ToolRequest::new(
        ToolName::new("email.send"),
        ArgumentTree::Object(std::collections::BTreeMap::from([
            (ArgumentName::new("to"), ArgumentTree::Value(to_bob)),
            (ArgumentName::new("body"), ArgumentTree::Value(body)),
        ])),
        BTreeSet::from([secret_a, secret_b, noise]),
    );
    let plans = remediable(&engine, &mut trajectory, request);
    let released = plans
        .iter()
        .any(|plan| release_step(plan.steps.first()) == Some(BTreeSet::from([secret_a, secret_b])));
    assert!(
        released,
        "release the two joint carriers only, never the unrelated control"
    );
    // And no enumerated plan may over-release the unrelated control.
    let over_releases = plans.iter().any(|plan| {
        plan.steps
            .iter()
            .any(|step| release_step(step).is_some_and(|release| release.contains(&noise)))
    });
    assert!(!over_releases, "the unrelated control must never be released");
}

#[test]
fn control_release_fixpoint_avoids_masked_over_release() {
    let sink = ToolContract {
        name: ToolName::new("email.send"),
        requires: Some(Requirements {
            trust: Some(KnownTrust::Suspicious),
            audience: crate::contract::AudienceRule::FromRecipients,
            ..Requirements::default()
        }),
        output_label: ValueLabel::identity(),
        effects: Effects::none(),
        arguments: ArgumentSchema::with_recipients(ArgumentName::new("to")),
    };
    let mut engine = engine_with([sink]);
    engine.register_authority(human()).unwrap();
    let mut trajectory = Trajectory::new();
    let body = ingress(&mut trajectory, &["alice", "bob"], Trust::TRUSTED, "body");
    let restrict = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "restrict");
    let unknown = trajectory.ingress(
        crate::turn::Speaker::user(user("alice")),
        ValueLabel {
            audience: Audience::PUBLIC,
            trust: Trust::UNKNOWN,
        },
        OpaqueValue::new("unk"),
    );
    let suspicious = trajectory.ingress(
        crate::turn::Speaker::user(user("alice")),
        ValueLabel {
            audience: Audience::PUBLIC,
            trust: Trust::SUSPICIOUS,
        },
        OpaqueValue::new("susp"),
    );
    let to_bob = identity_ingress(&mut trajectory, "bob");
    let request = ToolRequest::new(
        ToolName::new("email.send"),
        ArgumentTree::Object(std::collections::BTreeMap::from([
            (ArgumentName::new("to"), ArgumentTree::Value(to_bob)),
            (ArgumentName::new("body"), ArgumentTree::Value(body)),
        ])),
        BTreeSet::from([restrict, unknown, suspicious]),
    );
    let plans = remediable(&engine, &mut trajectory, request);
    let released_exactly_restrict = plans
        .iter()
        .any(|plan| release_step(plan.steps.first()) == Some(BTreeSet::from([restrict])));
    assert!(
        released_exactly_restrict,
        "release only the audience control, not the masked trust controls"
    );
}

#[test]
fn no_applicable_remedy_is_terminal() {
    let engine = engine_with([email_contract()]);
    let mut trajectory = Trajectory::new();
    let raw = ingress(&mut trajectory, &["alice", "bob"], Trust::SUSPICIOUS, "raw");
    let request = email_request(&mut trajectory, raw, "bob");

    let Some(block) = terminal_block_of(engine.evaluate(&mut trajectory, request)) else {
        panic!("expected terminal block");
    };
    assert_eq!(block.reason, BlockReason::NoRemedy);
    assert!(matches!(
        block.violations.as_slice(),
        [Violation::Breach(crate::contract::Breach::TrustBelow { .. })]
    ));
    assert!(trajectory.pending_action().is_none());
}

#[test]
fn transform_step_applies_and_flow_permits() {
    let mut engine = engine_with([email_contract()]);
    engine.register_transformer(redact_transformer()).unwrap();
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let raw = ingress(&mut trajectory, &["alice", "bob"], Trust::SUSPICIOUS, "raw secrets");
    let request = email_request(&mut trajectory, raw, "bob");

    let plans = remediable(&engine, &mut trajectory, request);
    let plan = plans
        .iter()
        .find(|p| p.steps.len() == 1 && derive_step(p.steps.first()).is_some())
        .expect("transform plan");

    let outcome = apply_first_step(&engine, &mut trajectory, plan.id);
    let Some(token) = advanced_execution(outcome) else {
        panic!("expected the transform to advance to a permit");
    };
    // The raw value keeps its label; the derived value took its slot.
    assert_eq!(trajectory.value(raw).unwrap().label().trust, Trust::SUSPICIOUS);
    assert!(matches!(
        trajectory.audit(),
        [
            AuditEvent::EffectsCommitted { .. },
            AuditEvent::DispatchFailed { .. },
            AuditEvent::ValueTransition {
                outcome: crate::audit::TransitionOutcome::Applied,
                ..
            },
        ]
    ));

    let (canonical, receipt) = trajectory.release(token).unwrap();
    assert!(canonical.rendered.contains("[redacted]"));
    assert!(!canonical.rendered.contains("raw secrets"));
    trajectory.record_output(receipt, OpaqueValue::new("sent")).unwrap();
}

#[test]
fn rule_approved_endorse_permits_inline() {
    let mut engine = engine_with([email_contract()]);
    engine
        .register_authority(inline_authority("auto-approve", human().mandate, approve_all))
        .unwrap();
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let doc = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "private");
    let request = email_request(&mut trajectory, doc, "charlie");

    let plans = remediable(&engine, &mut trajectory, request);
    assert!(raise_step(plans.first().steps.first()).is_some());
    let outcome = apply_first_step(&engine, &mut trajectory, plans.first().id);
    let Some(_token) = advanced_execution(outcome) else {
        panic!("expected inline endorse permit");
    };
    assert!(trajectory.audit().iter().any(|e| applied_raise(e).is_some()));
}

#[test]
fn inline_abstention_falls_through_to_the_next_authority() {
    let mut engine = engine_with([email_contract()]);
    engine
        .register_authority(inline_authority("first", human().mandate, abstain_all))
        .unwrap();
    engine
        .register_authority(inline_authority("second", human().mandate, approve_all))
        .unwrap();
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let doc = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "private");
    let request = email_request(&mut trajectory, doc, "charlie");
    let plans = remediable(&engine, &mut trajectory, request);
    let Some(_) = advanced_execution(apply_first_step(&engine, &mut trajectory, plans.first().id)) else {
        panic!("expected the second authority to approve after the first abstained");
    };
    // The applied endorse is attributed to the authority that actually ruled.
    assert!(
        trajectory
            .audit()
            .iter()
            .any(|e| applied_raise(e).is_some_and(|(_, authority)| authority.as_str() == "second"))
    );
}

#[test]
fn inline_authority_is_consulted_before_external() {
    let mut engine = engine_with([email_contract()]);
    // External registered first; the inline authority must still win.
    engine.register_authority(human()).unwrap();
    engine
        .register_authority(inline_authority("inline", human().mandate, approve_all))
        .unwrap();
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let doc = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "private");
    let request = email_request(&mut trajectory, doc, "charlie");
    let plans = remediable(&engine, &mut trajectory, request);
    // Inline resolves synchronously — no round-trip to the external human.
    let Some(_) = advanced_execution(apply_first_step(&engine, &mut trajectory, plans.first().id)) else {
        panic!("expected the inline authority to decide before the external one");
    };
}

#[test]
fn all_inline_abstentions_block_with_no_ruling() {
    let mut engine = engine_with([email_contract()]);
    engine
        .register_authority(inline_authority("only", human().mandate, abstain_all))
        .unwrap();
    let mut trajectory = Trajectory::new();
    let doc = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "private");
    let request = email_request(&mut trajectory, doc, "charlie");
    let plans = remediable(&engine, &mut trajectory, request);
    let Some(block) = advanced_terminal(apply_first_step(&engine, &mut trajectory, plans.first().id)) else {
        panic!("expected a terminal block when every authority abstains");
    };
    assert_eq!(block.reason, BlockReason::NoAuthorityRuled);
}

#[test]
fn inline_denial_is_decisive_and_does_not_fall_through() {
    let mut engine = engine_with([email_contract()]);
    engine
        .register_authority(inline_authority("denier", human().mandate, deny_all))
        .unwrap();
    engine
        .register_authority(inline_authority("approver", human().mandate, approve_all))
        .unwrap();
    let mut trajectory = Trajectory::new();
    let doc = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "private");
    let request = email_request(&mut trajectory, doc, "charlie");
    let plans = remediable(&engine, &mut trajectory, request);
    let Some(block) = advanced_terminal(apply_first_step(&engine, &mut trajectory, plans.first().id)) else {
        panic!("a denial must terminate, not fall through to the approver");
    };
    assert!(matches!(block.reason, BlockReason::DeniedByAuthority { .. }));
    assert!(
        trajectory
            .audit()
            .iter()
            .any(|e| denied_delta(e).is_some_and(delta_raises))
    );
}

#[test]
fn control_release_only_authority_cannot_acknowledge_an_unknown() {
    let mut engine = engine_with([]);
    engine
        .register_authority(inline_authority("control-only", releaser_mandate(), approve_all))
        .unwrap();
    let mut trajectory = Trajectory::new();
    let body = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "x");
    let request = ToolRequest::new(
        ToolName::new("mystery.tool"),
        ArgumentTree::Value(body),
        BTreeSet::new(),
    );
    let Some(block) = terminal_block_of(engine.evaluate(&mut trajectory, request)) else {
        panic!("a control-release-only authority must not clear an unknown");
    };
    assert_eq!(block.reason, BlockReason::NoRemedy);
}

#[test]
fn mixed_residual_needs_acknowledge_competence_not_just_the_lift() {
    let mut engine = engine_with([]);
    // A tool with unknown effects; dispatching it makes past-effects UNKNOWN.
    engine
        .register(ToolContract {
            name: ToolName::new("fetch"),
            requires: Some(Requirements::default()),
            output_label: ValueLabel::unknown(),
            effects: Effects::UNKNOWN,
            arguments: ArgumentSchema::opaque(),
        })
        .unwrap();
    // A sink that both demands Trusted and forbids a prior Egress.
    engine
        .register(ToolContract {
            name: ToolName::new("email.send"),
            requires: Some(Requirements {
                trust: Some(KnownTrust::Trusted),
                audience: crate::contract::AudienceRule::FromRecipients,
                forbid_prior_effects: BTreeSet::from([Effect::Egress]),
                ..Requirements::default()
            }),
            output_label: ValueLabel::identity(),
            effects: Effects::declared([Effect::Egress]),
            arguments: ArgumentSchema::with_recipients(ArgumentName::new("to")),
        })
        .unwrap();
    // Trust-competent, but NOT competent to acknowledge unknowns.
    engine
        .register_authority(inline_authority(
            "trust-only",
            crate::transition::AuthorityMandate {
                trust: Some(KnownTrust::Trusted),
                audience: Some(BTreeSet::from([user("alice"), user("bob")])),
                ..crate::transition::AuthorityMandate::none()
            },
            approve_all,
        ))
        .unwrap();

    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::UNKNOWN);
    let doc = ingress(&mut trajectory, &["alice", "bob"], Trust::UNKNOWN, "doc");
    // Dispatch fetch to drive past-effects to UNKNOWN.
    let Ok(FlowOutcome::AllowedNow(token)) = engine.evaluate(
        &mut trajectory,
        ToolRequest::new(ToolName::new("fetch"), ArgumentTree::Value(doc), BTreeSet::new()),
    ) else {
        panic!("fetch should permit");
    };
    dispatch(&mut trajectory, token, "page").unwrap();

    let request = email_request(&mut trajectory, doc, "bob");
    let decision = engine.evaluate(&mut trajectory, request);
    let Some(block) = terminal_block_of(decision) else {
        panic!("trust-only must not clear the unknown effect");
    };
    assert_eq!(block.reason, BlockReason::NoRemedy);
}

// ---- S6: criterion (1) + Accept ----

fn egress_tool() -> ToolContract {
    ToolContract {
        name: ToolName::new("net.ping"),
        requires: Some(Requirements::default()),
        output_label: ValueLabel::identity(),
        effects: Effects::declared([Effect::Egress]),
        arguments: ArgumentSchema::opaque(),
    }
}

fn ping_request(body: ValueId) -> ToolRequest {
    ToolRequest::new(ToolName::new("net.ping"), ArgumentTree::Value(body), BTreeSet::new())
}

// ---- S7: route categorization + cap fairness ----

fn tref(id: &str) -> crate::value::TransformerRef {
    crate::value::TransformerRef {
        id: id.into(),
        version: 1,
    }
}

fn plan_steps(steps: Vec<PlannedRemedy>) -> NonEmptyVec<PlannedRemedy> {
    NonEmptyVec::from_vec(steps).expect("non-empty")
}

fn derive_remedy(source: u64) -> PlannedRemedy {
    PlannedRemedy::Reduce(ReductionTarget::DeriveValue {
        source: ValueId::new(source),
        transformer: tref("s"),
    })
}

fn authorize_remedy(coordinate: DeltaCoordinate, scope: AuthorizationScope) -> PlannedRemedy {
    PlannedRemedy::Authorize {
        authorization: Authorization::new(crate::remedy::AuthorizationDelta::single(coordinate), scope).unwrap(),
        routes: NonEmptyVec::new(AuthorityName::new("x"), Vec::new()),
        targets: Vec::new(),
    }
}

fn confirm_remedy() -> PlannedRemedy {
    authorize_remedy(
        DeltaCoordinate::StandInConfirmation,
        AuthorizationScope::PolicyCheck {
            flow: crate::revision::FlowId::new(0),
        },
    )
}

fn acknowledge_remedy() -> PlannedRemedy {
    authorize_remedy(
        DeltaCoordinate::AcknowledgeUnknown(Vec::new()),
        AuthorizationScope::PolicyCheck {
            flow: crate::revision::FlowId::new(0),
        },
    )
}

#[test]
fn ask_order_ranks_by_authorization_magnitude() {
    use std::cmp::Ordering;
    let release = |ids: &[u64]| {
        plan_steps(vec![authorize_remedy(
            DeltaCoordinate::ReleaseControl(ids.iter().copied().map(ValueId::new).collect()),
            AuthorizationScope::PolicyCheck {
                flow: crate::revision::FlowId::new(0),
            },
        )])
    };
    // A subset release asks strictly less.
    assert_eq!(
        ask_cmp(&AskVector::of(&release(&[1])), &AskVector::of(&release(&[1, 2]))),
        Some(Ordering::Less)
    );
    // Overlapping but non-nested sets are incomparable.
    assert_eq!(
        ask_cmp(&AskVector::of(&release(&[1])), &AskVector::of(&release(&[2]))),
        None
    );

    let raise = |trust: KnownTrust| {
        plan_steps(vec![authorize_remedy(
            DeltaCoordinate::RaiseLabel(LabelRaise {
                trust: Some(trust),
                audience: None,
            }),
            AuthorizationScope::DerivedValue {
                source: ValueId::new(0),
            },
        )])
    };
    // A raise to Suspicious asks less than a raise to Trusted.
    assert_eq!(
        ask_cmp(
            &AskVector::of(&raise(KnownTrust::Suspicious)),
            &AskVector::of(&raise(KnownTrust::Trusted))
        ),
        Some(Ordering::Less)
    );
    // A raise and a release are different kinds: incomparable.
    assert_eq!(
        ask_cmp(
            &AskVector::of(&raise(KnownTrust::Suspicious)),
            &AskVector::of(&release(&[1]))
        ),
        None
    );

    // Asking for nothing (a pure reduce plan) dominates any authorization.
    let reduce_only = plan_steps(vec![derive_remedy(0)]);
    assert_eq!(
        ask_cmp(&AskVector::of(&reduce_only), &AskVector::of(&release(&[1]))),
        Some(Ordering::Less)
    );
    assert_eq!(
        ask_cmp(
            &AskVector::of(&reduce_only),
            &AskVector::of(&plan_steps(vec![derive_remedy(0), confirm_remedy()]))
        ),
        Some(Ordering::Less)
    );
    assert_eq!(
        ask_cmp(
            &AskVector::of(&reduce_only),
            &AskVector::of(&plan_steps(vec![acknowledge_remedy()]))
        ),
        Some(Ordering::Less)
    );
}

#[test]
fn two_tainted_leaves_each_get_their_own_transform() {
    let sink = ToolContract {
        name: ToolName::new("report.save"),
        requires: Some(Requirements {
            trust: Some(KnownTrust::Trusted),
            ..Requirements::default()
        }),
        output_label: ValueLabel::identity(),
        effects: Effects::none(),
        arguments: ArgumentSchema::opaque(),
    };
    let mut engine = engine_with([sink]);
    engine.register_transformer(redact_transformer()).unwrap();
    let mut trajectory = Trajectory::new();
    let notes = ingress(&mut trajectory, &["alice"], Trust::SUSPICIOUS, "notes");
    let draft = ingress(&mut trajectory, &["alice"], Trust::SUSPICIOUS, "draft");
    let request = ToolRequest::new(
        ToolName::new("report.save"),
        ArgumentTree::Object(std::collections::BTreeMap::from([
            (ArgumentName::new("notes"), ArgumentTree::Value(notes)),
            (ArgumentName::new("draft"), ArgumentTree::Value(draft)),
        ])),
        BTreeSet::new(),
    );

    let plans = remediable(&engine, &mut trajectory, request.clone());
    let orders: Vec<Vec<ValueId>> = plans
        .iter()
        .map(|plan| plan.steps.iter().filter_map(derive_step).collect())
        .collect();
    assert_eq!(plans.len(), 2);
    assert!(orders.contains(&vec![notes, draft]));
    assert!(orders.contains(&vec![draft, notes]));

    // The first serialized ordering walks to a permit through pursue…
    let token = walk_to_permit(&engine, &mut trajectory, request.clone());
    dispatch(&mut trajectory, token, "saved").unwrap();

    // …and the draft-first ordering walks too, driven head by head.
    let mut trajectory = Trajectory::new();
    let notes = ingress(&mut trajectory, &["alice"], Trust::SUSPICIOUS, "notes");
    let draft = ingress(&mut trajectory, &["alice"], Trust::SUSPICIOUS, "draft");
    let retry = ToolRequest::new(
        ToolName::new("report.save"),
        ArgumentTree::Object(std::collections::BTreeMap::from([
            (ArgumentName::new("notes"), ArgumentTree::Value(notes)),
            (ArgumentName::new("draft"), ArgumentTree::Value(draft)),
        ])),
        BTreeSet::new(),
    );
    let mut plans = remediable(&engine, &mut trajectory, retry);
    let mut first_sources = Vec::new();
    let token = loop {
        let plan = plans
            .iter()
            .find(|plan| {
                let sources: Vec<ValueId> = plan.steps.iter().filter_map(derive_step).collect();
                first_sources.is_empty() && sources == vec![draft, notes]
                    || !first_sources.is_empty() && sources == vec![notes]
            })
            .expect("the draft-first ordering stays walkable");
        first_sources.push(derive_step(plan.steps.first()).expect("a derive head"));
        match apply_first_step(&engine, &mut trajectory, plan.id) {
            StepOutcome::Advanced(FlowOutcome::AllowedNow(FlowPermit::Execute(token))) => break token,
            StepOutcome::Advanced(FlowOutcome::Blocked {
                plans: next,
                terminal: None,
                ..
            }) => plans = NonEmptyVec::from_vec(next).expect("a remediable block carries at least one plan"),
            other => panic!("unexpected outcome: {other:?}"),
        }
    };
    assert_eq!(first_sources, vec![draft, notes]);
    dispatch(&mut trajectory, token, "saved").unwrap();
}

#[test]
fn order_sensitive_derivation_chains_are_not_pruned() {
    fn body(_: &OpaqueValue) -> Result<OpaqueValue, crate::transition::TransformerError> {
        Ok(OpaqueValue::new("[derived]"))
    }
    let alice_only = || Audience::readers([user("alice")]);
    let taint = RegisteredTransformer {
        descriptor: crate::transition::TransformerDescriptor {
            transformer: crate::value::TransformerRef {
                id: "draft.taint".into(),
                version: 1,
            },
            precondition: crate::transition::LabelPredicate {
                trust: Some(Trust::TRUSTED),
                audience: Some(alice_only()),
            },
            output: ValueLabel {
                trust: Trust::SUSPICIOUS,
                audience: alice_only(),
            },
        },
        run: body,
    };
    let restore = RegisteredTransformer {
        descriptor: crate::transition::TransformerDescriptor {
            transformer: crate::value::TransformerRef {
                id: "draft.restore".into(),
                version: 1,
            },
            precondition: crate::transition::LabelPredicate {
                trust: Some(Trust::SUSPICIOUS),
                audience: Some(alice_only()),
            },
            output: ValueLabel {
                trust: Trust::TRUSTED,
                audience: alice_only(),
            },
        },
        run: body,
    };
    let sink = ToolContract {
        name: ToolName::new("report.save"),
        requires: Some(Requirements {
            trust: Some(KnownTrust::Trusted),
            ..Requirements::default()
        }),
        output_label: ValueLabel::identity(),
        effects: Effects::none(),
        arguments: ArgumentSchema::opaque(),
    };
    let mut engine = engine_with([sink]);
    engine.register_transformer(redact_transformer()).unwrap();
    engine.register_transformer(taint).unwrap();
    engine.register_transformer(restore).unwrap();
    let mut trajectory = Trajectory::new();
    let notes = ingress(&mut trajectory, &["alice", "bob"], Trust::SUSPICIOUS, "notes");
    let draft = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "draft");
    let request = ToolRequest::new(
        ToolName::new("report.save"),
        ArgumentTree::Object(std::collections::BTreeMap::from([
            (ArgumentName::new("notes"), ArgumentTree::Value(notes)),
            (ArgumentName::new("draft"), ArgumentTree::Value(draft)),
        ])),
        BTreeSet::new(),
    );

    let plans = remediable(&engine, &mut trajectory, request);
    let chain_of = |plan: &crate::plan::RemedyPlan| -> Vec<(ValueId, String)> {
        plan.steps
            .iter()
            .filter_map(|step| match step {
                PlannedRemedy::Reduce(ReductionTarget::DeriveValue { source, transformer }) => {
                    Some((*source, transformer.id.clone()))
                }
                _ => None,
            })
            .collect()
    };
    let chains: Vec<Vec<(ValueId, String)>> = plans.iter().map(chain_of).collect();
    // The direct plan (redact the suspicious leaf) is present…
    assert!(chains.contains(&vec![(notes, "pii.redact".to_owned())]));
    let detour = vec![
        (draft, "draft.taint".to_owned()),
        (notes, "pii.redact".to_owned()),
        (draft, "draft.restore".to_owned()),
    ];
    assert_eq!(chains.iter().filter(|chain| **chain == detour).count(), 1);

    let mut plans = plans;
    let mut remaining: Vec<String> = detour.iter().map(|(_, transformer)| transformer.clone()).collect();
    let token = loop {
        let plan = plans
            .iter()
            .find(|plan| {
                chain_of(plan)
                    .iter()
                    .map(|(_, transformer)| transformer.clone())
                    .collect::<Vec<_>>()
                    == remaining
            })
            .expect("the detour's remainder stays predicted after each recheck");
        remaining.remove(0);
        match apply_first_step(&engine, &mut trajectory, plan.id) {
            StepOutcome::Advanced(FlowOutcome::AllowedNow(FlowPermit::Execute(token))) => {
                assert!(remaining.is_empty(), "permitted before the restore step");
                break token;
            }
            StepOutcome::Advanced(FlowOutcome::Blocked {
                plans: next,
                terminal: None,
                ..
            }) => {
                assert!(!remaining.is_empty(), "still blocked after the final step");
                plans = NonEmptyVec::from_vec(next).expect("a remediable block carries at least one plan");
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
    };
    dispatch(&mut trajectory, token, "saved").unwrap();
}

fn step_label(step: &PlannedRemedy) -> &'static str {
    match step {
        PlannedRemedy::Reduce(ReductionTarget::DeriveValue { .. }) => "sanitize",
        PlannedRemedy::Authorize { authorization, .. } => match &authorization.scope() {
            AuthorizationScope::DerivedValue { .. } => "endorse",
            AuthorizationScope::PolicyCheck { .. } => "waiver",
        },
    }
}

#[test]
fn full_composition_reduces_then_authorizes_the_irreducible_residual() {
    fn launder(_: &OpaqueValue) -> Result<OpaqueValue, crate::transition::TransformerError> {
        Ok(OpaqueValue::new("[laundered]"))
    }
    let dispatch_tool = ToolContract {
        name: ToolName::new("dispatch"),
        requires: Some(Requirements {
            trust: Some(KnownTrust::Trusted),
            audience: crate::contract::AudienceRule::FromRecipients,
            ..Requirements::default()
        }),
        output_label: ValueLabel::identity(),
        effects: Effects::declared([Effect::Egress, Effect::Mutation]),
        arguments: ArgumentSchema::with_recipients(ArgumentName::new("to")),
    };
    let mut engine = engine_with([dispatch_tool]);
    engine
        .register_transformer(RegisteredTransformer {
            descriptor: crate::transition::TransformerDescriptor {
                transformer: crate::value::TransformerRef {
                    id: "detox".to_owned(),
                    version: 1,
                },
                precondition: crate::transition::LabelPredicate {
                    trust: Some(Trust::SUSPICIOUS),
                    audience: None,
                },
                output: ValueLabel {
                    audience: Audience::readers([user("alice")]),
                    trust: Trust::TRUSTED,
                },
            },
            run: launder,
        })
        .unwrap();
    engine
        .register_authority(inline_authority(
            "voucher",
            crate::transition::AuthorityMandate {
                audience: Some(BTreeSet::from([user("alice"), user("charlie")])),
                ..crate::transition::AuthorityMandate::none()
            },
            approve_all,
        ))
        .unwrap();

    let mut trajectory = Trajectory::new();
    let body = ingress(&mut trajectory, &["alice"], Trust::SUSPICIOUS, "raw");
    let to = identity_ingress(&mut trajectory, "charlie");
    let request = ToolRequest::new(
        ToolName::new("dispatch"),
        ArgumentTree::Object(std::collections::BTreeMap::from([
            (ArgumentName::new("to"), ArgumentTree::Value(to)),
            (ArgumentName::new("body"), ArgumentTree::Value(body)),
        ])),
        BTreeSet::new(),
    );

    let plans = remediable(&engine, &mut trajectory, request);

    let expected = ["sanitize", "endorse"];
    let composite = plans
        .iter()
        .find(|p| p.steps.iter().map(step_label).collect::<Vec<_>>() == expected)
        .expect("the sanitize-first composite is present");
    // Endorse signs off only the audience — trust was reduced by Sanitize.
    let endorse = composite
        .steps
        .iter()
        .find_map(|s| raise_step(s).map(|(_, raise)| raise))
        .expect("an endorse step");
    assert_eq!(endorse.trust, None);
    assert_eq!(endorse.audience.as_ref().unwrap(), &BTreeSet::from([user("charlie")]));

    let mut applied: Vec<&str> = Vec::new();
    let mut plans = plans;
    let token = loop {
        let suffix = &expected[applied.len()..];
        let plan = plans
            .iter()
            .find(|p| p.steps.iter().map(step_label).collect::<Vec<_>>() == suffix)
            .expect("the composite's remainder stays predicted");
        applied.push(step_label(plan.steps.first()));
        match apply_first_step(&engine, &mut trajectory, plan.id) {
            StepOutcome::Advanced(FlowOutcome::AllowedNow(FlowPermit::Execute(token))) => break token,
            StepOutcome::Advanced(FlowOutcome::Blocked {
                plans: next,
                terminal: None,
                ..
            }) => plans = NonEmptyVec::from_vec(next).expect("a remediable block carries at least one plan"),
            other => panic!("unexpected outcome: {other:?}"),
        }
    };
    assert_eq!(applied, ["sanitize", "endorse"]);
    dispatch(&mut trajectory, token, "sent").unwrap();
    assert_eq!(
        trajectory.past_effects(),
        &Effects::declared([Effect::Egress, Effect::Mutation])
    );
}

#[test]
fn frontier_is_deterministic() {
    let run = || {
        let mut engine = engine_with([email_contract()]);
        engine.register_authority(human()).unwrap();
        engine.register_transformer(redact_transformer()).unwrap();
        let mut trajectory = Trajectory::new();
        let doc = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "private");
        let request = email_request(&mut trajectory, doc, "charlie");
        let plans = remediable(&engine, &mut trajectory, request);
        plans.iter().map(|plan| plan.steps.clone()).collect::<Vec<_>>()
    };
    assert_eq!(run(), run());
}

#[test]
fn frontier_returns_every_route_uncapped() {
    let sink = ToolContract {
        name: ToolName::new("sink"),
        requires: Some(Requirements {
            trust: Some(KnownTrust::Trusted),
            ..Requirements::default()
        }),
        output_label: ValueLabel::identity(),
        effects: Effects::none(),
        arguments: ArgumentSchema::opaque(),
    };
    let variants = 10;
    let mut engine = engine_with([sink]);
    fn scrub(_: &OpaqueValue) -> Result<OpaqueValue, crate::transition::TransformerError> {
        Ok(OpaqueValue::new("[scrubbed]"))
    }
    for i in 0..variants {
        engine
            .register_transformer(RegisteredTransformer {
                descriptor: crate::transition::TransformerDescriptor {
                    transformer: tref(&format!("scrub{i}")),
                    precondition: crate::transition::LabelPredicate {
                        trust: Some(Trust::SUSPICIOUS),
                        audience: None,
                    },
                    output: ValueLabel::identity(),
                },
                run: scrub,
            })
            .unwrap();
    }

    let mut trajectory = Trajectory::new();
    let payload = ingress(&mut trajectory, &["alice"], Trust::SUSPICIOUS, "raw");
    let request = ToolRequest::new(ToolName::new("sink"), ArgumentTree::Value(payload), BTreeSet::new());
    let plans = remediable(&engine, &mut trajectory, request);
    assert_eq!(plans.len(), variants);
    assert!(plans.iter().all(|p| p.steps.len() == 1));
    assert!(plans.iter().all(|p| derive_step(p.steps.first()).is_some()));
}

#[test]
fn external_waiver_approval_roundtrip() {
    let mut engine = engine_with([email_contract()]);
    engine.register_authority(human()).unwrap();
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let body = ingress(&mut trajectory, &["alice", "bob"], Trust::TRUSTED, "doc");
    let secret = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "selector");
    let to = identity_ingress(&mut trajectory, "bob");
    let request = ToolRequest::new(
        ToolName::new("email.send"),
        ArgumentTree::Object(std::collections::BTreeMap::from([
            (ArgumentName::new("to"), ArgumentTree::Value(to)),
            (ArgumentName::new("body"), ArgumentTree::Value(body)),
        ])),
        BTreeSet::from([secret]),
    );

    let plans = remediable(&engine, &mut trajectory, request);
    let StepOutcome::NeedsApproval(pending) = apply_first_step(&engine, &mut trajectory, plans.first().id) else {
        panic!("expected pending approval");
    };
    assert_eq!(pending.authority().as_str(), "human");

    let decision = engine
        .apply_approval(
            &mut trajectory,
            pending,
            crate::approval::Ruling::Approve {
                reason: "reviewed".to_owned(),
            },
        )
        .unwrap();
    assert!(matches!(decision, FlowOutcome::AllowedNow(_)));
    assert!(
        trajectory
            .audit()
            .iter()
            .any(|e| matches!(e, AuditEvent::ApprovalRequested { .. }))
    );
    assert!(trajectory.audit().iter().any(|e| applied_lift(e).is_some()));

    let flow = trajectory
        .events()
        .events()
        .iter()
        .rev()
        .find_map(|event| match &event.fact {
            crate::event::Fact::ActionProposed { flow, .. } => Some(*flow),
            _ => None,
        })
        .expect("the flow proposed an action");
    let applied = trajectory
        .events()
        .events()
        .iter()
        .find_map(|event| match &event.fact {
            crate::event::Fact::AuthorizationApplied { authorization, .. } => Some(authorization.clone()),
            _ => None,
        })
        .expect("the applied lift landed as a scoped fact");
    assert_eq!(
        applied.scope(),
        &crate::remedy::AuthorizationScope::PolicyCheck { flow }
    );
}

#[test]
fn a_denied_authorization_applies_nothing() {
    let mut engine = engine_with([email_contract()]);
    engine.register_authority(human()).unwrap();
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let body = ingress(&mut trajectory, &["alice", "bob"], Trust::TRUSTED, "doc");
    let secret = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "selector");
    let to = identity_ingress(&mut trajectory, "bob");
    let request = ToolRequest::new(
        ToolName::new("email.send"),
        ArgumentTree::Object(std::collections::BTreeMap::from([
            (ArgumentName::new("to"), ArgumentTree::Value(to)),
            (ArgumentName::new("body"), ArgumentTree::Value(body)),
        ])),
        BTreeSet::from([secret]),
    );
    let plans = remediable(&engine, &mut trajectory, request);
    let StepOutcome::NeedsApproval(pending) = apply_first_step(&engine, &mut trajectory, plans.first().id) else {
        panic!("expected pending approval");
    };
    let denied = engine
        .apply_approval(
            &mut trajectory,
            pending,
            crate::approval::Ruling::Deny {
                reason: "not on my watch".to_owned(),
            },
        )
        .unwrap();
    assert!(matches!(denied, FlowOutcome::Blocked { terminal: Some(_), .. }));
    assert!(
        !trajectory
            .events()
            .events()
            .iter()
            .any(|event| matches!(&event.fact, crate::event::Fact::AuthorizationApplied { .. }))
    );
}

#[test]
fn inline_authority_inspects_the_view_and_violations() {
    fn vouch_trusted_source(
        grant: &crate::remedy::Authorization,
        violations: &[Violation],
        view: &crate::approval::TrajectoryView,
    ) -> Option<crate::approval::Ruling> {
        let audience_breach = violations
            .iter()
            .any(|v| matches!(v, Violation::Breach(crate::contract::Breach::AudienceExceeds { .. })));
        let source_trusted = view
            .label(crate::revision::ValueId::new(0))
            .is_some_and(|label| label.trust == Trust::TRUSTED);
        if audience_breach
            && source_trusted
            && matches!(grant.scope(), crate::remedy::AuthorizationScope::DerivedValue { .. })
        {
            Some(crate::approval::Ruling::Approve {
                reason: "source document is trusted".to_owned(),
            })
        } else {
            None
        }
    }
    let mut engine = engine_with([email_contract()]);
    engine
        .register_authority(inline_authority("vouch", human().mandate, vouch_trusted_source))
        .unwrap();

    // Trusted source (value#0): the view read passes, the authority approves.
    let mut trusted = Trajectory::new();
    trusted.seed_committed_effects(Effects::declared([Effect::Egress]));
    let doc = ingress(&mut trusted, &["alice"], Trust::TRUSTED, "private");
    let request = email_request(&mut trusted, doc, "charlie");
    let plans = remediable(&engine, &mut trusted, request);
    let Some(_) = advanced_execution(apply_first_step(&engine, &mut trusted, plans.first().id)) else {
        panic!("expected approval when the view shows a trusted source");
    };

    let mut suspicious = Trajectory::new();
    suspicious.seed_committed_effects(Effects::declared([Effect::Egress]));
    let doc = ingress(&mut suspicious, &["alice"], Trust::SUSPICIOUS, "private");
    let request = email_request(&mut suspicious, doc, "charlie");
    let plans = remediable(&engine, &mut suspicious, request);
    let Some(block) = advanced_terminal(apply_first_step(&engine, &mut suspicious, plans.first().id)) else {
        panic!("expected abstention when the view shows a suspicious source");
    };
    assert_eq!(block.reason, BlockReason::NoAuthorityRuled);
}

#[test]
fn external_pending_carries_a_transitive_ancestry_snapshot() {
    let mut engine = engine_with([email_contract()]);
    engine.register_authority(human()).unwrap();
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let root = ingress(&mut trajectory, &["alice"], Trust::SUSPICIOUS, "raw");
    let trusted = ValueLabel {
        audience: Audience::readers([user("alice")]),
        trust: Trust::TRUSTED,
    };
    let mid = trajectory.seed_transformed(root, trusted.clone());
    let doc = trajectory.seed_transformed(mid, trusted);
    let request = email_request(&mut trajectory, doc, "charlie");

    let plans = remediable(&engine, &mut trajectory, request);
    let StepOutcome::NeedsApproval(pending) = apply_first_step(&engine, &mut trajectory, plans.first().id) else {
        panic!("expected pending approval");
    };
    // The direct endorsed value and its transitive root are both in scope.
    let doc_view = pending.ancestry().get(doc).expect("the endorsed value is in scope");
    assert_eq!(doc_view.label.trust, Trust::TRUSTED);
    let root_view = pending
        .ancestry()
        .get(root)
        .expect("the transitive root is in the snapshot");
    assert_eq!(root_view.label.trust, Trust::SUSPICIOUS);
    assert!(matches!(root_view.provenance, crate::value::Provenance::Ingress { .. }));

    let decision = engine
        .apply_approval(
            &mut trajectory,
            pending,
            crate::approval::Ruling::Approve {
                reason: "reviewed the ancestry".to_owned(),
            },
        )
        .unwrap();
    assert!(matches!(decision, FlowOutcome::AllowedNow(_)));
}

#[test]
fn external_waiver_denial_blocks_terminally() {
    let mut engine = engine_with([email_contract()]);
    engine.register_authority(human()).unwrap();
    let mut trajectory = Trajectory::new();
    let doc = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "private");
    let request = email_request(&mut trajectory, doc, "charlie");

    let plans = remediable(&engine, &mut trajectory, request.clone());
    let StepOutcome::NeedsApproval(pending) = apply_first_step(&engine, &mut trajectory, plans.first().id) else {
        panic!("expected pending approval");
    };
    let decision = engine
        .apply_approval(
            &mut trajectory,
            pending,
            crate::approval::Ruling::Deny {
                reason: "not comfortable".to_owned(),
            },
        )
        .unwrap();
    let Some(block) = terminal_block(decision) else {
        panic!("expected terminal block");
    };
    assert!(matches!(block.reason, BlockReason::DeniedByAuthority { .. }));
    assert!(
        trajectory
            .audit()
            .iter()
            .any(|e| denied_delta(e).is_some_and(delta_raises))
    );
    assert!(trajectory.pending_action().is_none());

    assert!(matches!(
        engine.evaluate(&mut trajectory, request),
        Ok(FlowOutcome::Blocked { terminal: None, .. })
    ));
}

#[test]
fn stale_step_capabilities_and_approvals_are_refused() {
    let mut engine = engine_with([email_contract()]);
    engine.register_authority(human()).unwrap();
    let mut trajectory = Trajectory::new();
    let doc = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "private");
    let request = email_request(&mut trajectory, doc, "charlie");

    let plans = remediable(&engine, &mut trajectory, request);
    let plan = plans.first().id;
    let capability = engine.mint_step(&trajectory, plan, 0).unwrap();

    // Any state change stales the capability (and the plan itself).
    trajectory
        .admit_model_output(OpaqueValue::new("thinking"), BTreeSet::from([doc]), BTreeSet::new())
        .unwrap();
    let revision_before = trajectory.revision();
    assert!(matches!(
        engine.apply_step(&mut trajectory, capability),
        Err(StepRefused::StalePlan { .. })
    ));
    assert!(matches!(
        engine.mint_step(&trajectory, plan, 0),
        Err(StepRefused::StalePlan { .. })
    ));
    // Refusal touched nothing.
    assert_eq!(trajectory.revision(), revision_before);

    // A stale approval is likewise refused.
    trajectory.abandon_pending().unwrap();
    let retry = email_request(&mut trajectory, doc, "charlie");
    let plans = remediable(&engine, &mut trajectory, retry);
    let StepOutcome::NeedsApproval(pending) = apply_first_step(&engine, &mut trajectory, plans.first().id) else {
        panic!("expected pending approval");
    };
    trajectory
        .admit_model_output(OpaqueValue::new("more"), BTreeSet::from([doc]), BTreeSet::new())
        .unwrap();
    assert!(matches!(
        engine.apply_approval(
            &mut trajectory,
            pending,
            crate::approval::Ruling::Approve {
                reason: "late".to_owned()
            }
        ),
        Err(StepRefused::StalePlan { .. })
    ));
}

#[test]
fn foreign_trajectory_step_capability_is_refused() {
    let mut engine = engine_with([email_contract()]);
    engine.register_authority(human()).unwrap();
    let mut trajectory = Trajectory::new();
    let doc = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "private");
    let request = email_request(&mut trajectory, doc, "charlie");
    let plans = remediable(&engine, &mut trajectory, request);
    let capability = engine.mint_step(&trajectory, plans.first().id, 0).unwrap();

    let mut other = Trajectory::new();
    let revision_before = other.revision();
    assert!(matches!(
        engine.apply_step(&mut other, capability),
        Err(StepRefused::ForeignTrajectory { .. })
    ));
    // Refusal touched nothing on the foreign trajectory.
    assert_eq!(other.revision(), revision_before);
    assert!(other.turns().is_empty());
    assert!(other.audit().is_empty());
}

#[test]
fn foreign_trajectory_approval_is_refused() {
    let mut engine = engine_with([email_contract()]);
    engine.register_authority(human()).unwrap();
    let mut trajectory = Trajectory::new();
    let doc = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "private");
    let request = email_request(&mut trajectory, doc, "charlie");
    let plans = remediable(&engine, &mut trajectory, request);
    let StepOutcome::NeedsApproval(pending) = apply_first_step(&engine, &mut trajectory, plans.first().id) else {
        panic!("expected pending approval");
    };

    let mut other = Trajectory::new();
    let revision_before = other.revision();
    assert!(matches!(
        engine.apply_approval(
            &mut other,
            pending,
            crate::approval::Ruling::Approve {
                reason: "misrouted".to_owned()
            }
        ),
        Err(StepRefused::ForeignTrajectory { .. })
    ));
    assert_eq!(other.revision(), revision_before);
    assert!(other.turns().is_empty());
    assert!(other.audit().is_empty());
}

fn sanitize_then_endorse_fixture() -> (PolicyEngine, Trajectory, ToolRequest) {
    fn launder(_: &OpaqueValue) -> Result<OpaqueValue, crate::transition::TransformerError> {
        Ok(OpaqueValue::new("[laundered]"))
    }
    let mut engine = engine_with([email_contract()]);
    engine
        .register_transformer(RegisteredTransformer {
            descriptor: crate::transition::TransformerDescriptor {
                transformer: tref("detox"),
                precondition: crate::transition::LabelPredicate {
                    trust: Some(Trust::SUSPICIOUS),
                    audience: None,
                },
                output: ValueLabel {
                    audience: Audience::readers([user("alice")]),
                    trust: Trust::TRUSTED,
                },
            },
            run: launder,
        })
        .unwrap();
    engine
        .register_authority(inline_authority(
            "voucher",
            crate::transition::AuthorityMandate {
                audience: Some(BTreeSet::from([user("charlie")])),
                ..crate::transition::AuthorityMandate::none()
            },
            approve_all,
        ))
        .unwrap();
    let mut trajectory = Trajectory::new();
    let body = ingress(&mut trajectory, &["alice"], Trust::SUSPICIOUS, "raw");
    let request = email_request(&mut trajectory, body, "charlie");
    (engine, trajectory, request)
}

#[test]
fn non_head_plan_steps_are_refused_without_touching_state() {
    let (engine, mut trajectory, request) = sanitize_then_endorse_fixture();
    let plans = remediable(&engine, &mut trajectory, request);
    let expected = ["sanitize", "endorse"];
    let composite = plans
        .iter()
        .find(|p| p.steps.iter().map(step_label).collect::<Vec<_>>() == expected)
        .expect("the sanitize-endorse route");

    let revision_before = trajectory.revision();
    assert!(matches!(
        engine.mint_step(&trajectory, composite.id, 1),
        Err(StepRefused::NotNextStep { step: 1 })
    ));
    // Out of range is still its own refusal.
    assert!(matches!(
        engine.mint_step(&trajectory, composite.id, 9),
        Err(StepRefused::NoSuchStep { .. })
    ));
    assert_eq!(trajectory.revision(), revision_before);
    assert!(trajectory.pending_action().is_some());
    let mut plans = plans;
    let mut applied = 0;
    loop {
        let suffix = &expected[applied..];
        let plan = plans
            .iter()
            .find(|p| p.steps.iter().map(step_label).collect::<Vec<_>>() == suffix)
            .expect("the composite's remainder stays predicted");
        applied += 1;
        match apply_first_step(&engine, &mut trajectory, plan.id) {
            StepOutcome::Advanced(FlowOutcome::AllowedNow(_)) => break,
            StepOutcome::Advanced(FlowOutcome::Blocked {
                plans: next,
                terminal: None,
                ..
            }) => plans = NonEmptyVec::from_vec(next).expect("a remediable block carries at least one plan"),
            other => panic!("unexpected outcome: {other:?}"),
        }
    }
    assert_eq!(applied, expected.len());
}

#[test]
fn a_released_action_cannot_be_abandoned() {
    let engine = engine_with([email_contract()]);
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let body = ingress(&mut trajectory, &["alice", "bob"], Trust::TRUSTED, "doc");
    let request = email_request(&mut trajectory, body, "bob");
    let token = walk_to_permit(&engine, &mut trajectory, request.clone());
    let (_canonical, receipt) = trajectory.release(token).unwrap();

    let revision_before = trajectory.revision();
    assert!(matches!(
        trajectory.abandon_pending(),
        Err(crate::turn::DispatchInFlight { .. })
    ));
    assert_eq!(trajectory.revision(), revision_before);
    assert!(matches!(
        trajectory.pending_action().map(crate::request::PendingAction::state),
        Some(crate::request::ActionState::Released)
    ));
    assert!(matches!(
        engine.evaluate(&mut trajectory, request),
        Err(FlowRefusal::ActionAlreadyPending { .. })
    ));
    // The receipt still closes the action normally.
    trajectory.record_output(receipt, OpaqueValue::new("sent")).unwrap();
    assert!(trajectory.pending_action().is_none());
}

#[test]
fn registries_freeze_at_the_first_evaluation() {
    let mut engine = engine_with([email_contract()]);
    engine.register_authority(human()).unwrap();
    let mut trajectory = Trajectory::new();
    let doc = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "private");
    let request = email_request(&mut trajectory, doc, "charlie");
    let plans = remediable(&engine, &mut trajectory, request);

    assert!(matches!(
        engine.register_authority(inline_authority("late-voucher", human().mandate, approve_all)),
        Err(crate::engine::RegistrationRefused::Frozen(_))
    ));
    assert!(matches!(
        engine.register_transformer(redact_transformer()),
        Err(crate::engine::RegistrationRefused::Frozen(_))
    ));
    assert!(matches!(
        engine.register(email_contract()),
        Err(crate::engine::ContractRefused::Frozen(_))
    ));
    let capability = engine.mint_step(&trajectory, plans.first().id, 0).unwrap();
    assert!(engine.apply_step(&mut trajectory, capability).is_ok());
}

#[test]
fn transformer_error_fails_the_step_and_audits() {
    fn broken(_: &OpaqueValue) -> Result<OpaqueValue, crate::transition::TransformerError> {
        Err(crate::transition::TransformerError {
            message: "redactor crashed".to_owned(),
        })
    }
    let mut engine = engine_with([email_contract()]);
    let mut transformer = redact_transformer();
    transformer.run = broken;
    engine.register_transformer(transformer).unwrap();
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let raw = ingress(&mut trajectory, &["alice", "bob"], Trust::SUSPICIOUS, "raw");
    let request = email_request(&mut trajectory, raw, "bob");

    let plans = remediable(&engine, &mut trajectory, request);
    let values_before = trajectory.store().len();
    let revision_before = trajectory.revision();
    let outcome = apply_first_step(&engine, &mut trajectory, plans.first().id);
    assert!(matches!(
        outcome,
        StepOutcome::Failed(crate::audit::TransitionFailure::TransformerError { .. })
    ));
    assert_eq!(trajectory.store().len(), values_before);
    assert!(trajectory.revision() > revision_before);
    assert!(matches!(
        trajectory.audit(),
        [
            AuditEvent::EffectsCommitted { .. },
            AuditEvent::DispatchFailed { .. },
            AuditEvent::ValueTransition {
                derived: None,
                outcome: crate::audit::TransitionOutcome::Failed(_),
                ..
            },
        ]
    ));
}

#[test]
fn multi_step_composition_transform_then_waiver() {
    fn redact(_: &OpaqueValue) -> Result<OpaqueValue, crate::transition::TransformerError> {
        Ok(OpaqueValue::new("[redacted]"))
    }
    let mut engine = engine_with([email_contract()]);
    engine
        .register_transformer(RegisteredTransformer {
            descriptor: crate::transition::TransformerDescriptor {
                transformer: crate::value::TransformerRef {
                    id: "pii.redact.private".into(),
                    version: 1,
                },
                precondition: crate::transition::LabelPredicate {
                    trust: Some(Trust::SUSPICIOUS),
                    audience: None,
                },
                output: ValueLabel {
                    audience: Audience::readers([user("alice")]),
                    trust: Trust::TRUSTED,
                },
            },
            run: redact,
        })
        .unwrap();
    engine.register_authority(human()).unwrap();
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let raw = ingress(&mut trajectory, &["alice"], Trust::SUSPICIOUS, "raw");
    let request = email_request(&mut trajectory, raw, "charlie");

    let plans = remediable(&engine, &mut trajectory, request);
    // A two-step plan predicting the full route exists...
    assert!(plans.iter().any(|p| p.steps.len() == 2));
    // ...and application goes step by step, re-planning in between.
    let transform_plan = plans
        .iter()
        .find(|p| derive_step(p.steps.first()).is_some())
        .expect("plan starting with a transform");
    let StepOutcome::Advanced(FlowOutcome::Blocked {
        plans,
        violations,
        terminal: None,
    }) = apply_first_step(&engine, &mut trajectory, transform_plan.id)
    else {
        panic!("expected the transform to advance to a re-planned block");
    };
    let plans = NonEmptyVec::from_vec(plans).expect("a remediable block carries at least one plan");
    // Only the audience breach remains.
    assert!(matches!(
        violations.as_slice(),
        [Violation::Breach(crate::contract::Breach::AudienceExceeds { .. })]
    ));
    let StepOutcome::NeedsApproval(pending) = apply_first_step(&engine, &mut trajectory, plans.first().id) else {
        panic!("expected pending approval");
    };
    let decision = engine
        .apply_approval(
            &mut trajectory,
            pending,
            crate::approval::Ruling::Approve {
                reason: "redacted version may go out".to_owned(),
            },
        )
        .unwrap();
    let Some(token) = execution(decision) else {
        panic!("expected permit after the full composition");
    };
    let (canonical, receipt) = trajectory.release(token).unwrap();
    assert!(canonical.rendered.contains("[redacted]"));
    trajectory.record_output(receipt, OpaqueValue::new("sent")).unwrap();
}

#[test]
fn authorities_share_one_name_space() {
    let none = crate::transition::AuthorityMandate::none;
    let mut engine = PolicyEngine::new();
    engine
        .register_authority(inline_authority("gate", none(), approve_all))
        .unwrap();
    // The same name is refused regardless of mode.
    assert!(
        engine
            .register_authority(inline_authority("gate", none(), approve_all))
            .is_err()
    );
    assert!(engine.register_authority(external_authority("gate", none())).is_err());
}

#[test]
fn capabilities_are_bound_to_their_engine() {
    let mut engine_a = engine_with([email_contract()]);
    engine_a.register_authority(human()).unwrap();
    // Engine B registers the same names — a different trust domain.
    let mut engine_b = engine_with([email_contract()]);
    engine_b.register_authority(human()).unwrap();

    let mut trajectory = Trajectory::new();
    let doc = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "private");
    let request = email_request(&mut trajectory, doc, "charlie");
    let plans = remediable(&engine_a, &mut trajectory, request);

    // B can neither mint nor apply against A's stored plan.
    assert!(matches!(
        engine_b.mint_step(&trajectory, plans.first().id, 0),
        Err(StepRefused::ForeignEngine { .. })
    ));
    let capability = engine_a.mint_step(&trajectory, plans.first().id, 0).unwrap();
    assert!(matches!(
        engine_b.apply_step(&mut trajectory, capability),
        Err(StepRefused::ForeignEngine { .. })
    ));

    // Nor can B consume A's pending approval.
    let StepOutcome::NeedsApproval(pending) = apply_first_step(&engine_a, &mut trajectory, plans.first().id) else {
        panic!("expected pending approval");
    };
    assert!(matches!(
        engine_b.apply_approval(
            &mut trajectory,
            pending,
            crate::approval::Ruling::Approve {
                reason: "cross-domain".to_owned()
            }
        ),
        Err(StepRefused::ForeignEngine { .. })
    ));
}

#[test]
fn released_action_cannot_be_re_permitted_or_re_released() {
    let engine = engine_with([email_contract()]);
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let doc = ingress(&mut trajectory, &["alice", "bob"], Trust::TRUSTED, "doc");
    let request = email_request(&mut trajectory, doc, "bob");

    let Ok(FlowOutcome::AllowedNow(token1)) = engine.evaluate(&mut trajectory, request.clone()) else {
        panic!("expected permit");
    };
    let (_, receipt) = trajectory.release(token1).unwrap();

    // Re-entry while the dispatch is in flight is refused, not re-permitted.
    let Err(refusal) = engine.evaluate(&mut trajectory, request) else {
        panic!("expected the released action to refuse re-entry");
    };
    assert!(matches!(refusal, FlowRefusal::ActionAlreadyPending { .. }));

    // The outstanding receipt still closes the action normally.
    trajectory.record_output(receipt, OpaqueValue::new("sent")).unwrap();
    assert!(trajectory.pending_action().is_none());
}

#[test]
fn unprovable_re_entry_writes_no_audit() {
    let mut engine = engine_with([]);
    engine
        .register_authority(inline_authority(
            "accept-unknowns",
            crate::transition::AuthorityMandate {
                acknowledge_unknown: true,
                ..crate::transition::AuthorityMandate::none()
            },
            approve_all,
        ))
        .unwrap();
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::UNKNOWN);
    let body = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "x");
    let request = ToolRequest::new(
        ToolName::new("mystery.tool"),
        ArgumentTree::Value(body),
        BTreeSet::new(),
    );

    let waiver_audits =
        |trajectory: &Trajectory| trajectory.audit().iter().filter(|e| applied_lift(e).is_some()).count();

    let Ok(FlowOutcome::Blocked { terminal: None, .. }) = engine.evaluate(&mut trajectory, request.clone()) else {
        panic!("expected a remediable block");
    };
    assert_eq!(waiver_audits(&trajectory), 0);
    // Re-evaluate the same original request: still remediable, still no audit.
    let Ok(FlowOutcome::Blocked { terminal: None, .. }) = engine.evaluate(&mut trajectory, request) else {
        panic!("expected a remediable block on re-entry");
    };
    assert_eq!(waiver_audits(&trajectory), 0);
}

// ---- Denial audit attribution per grant kind ----

fn deny_all(
    _: &crate::remedy::Authorization,
    _: &[Violation],
    _: &crate::approval::TrajectoryView,
) -> Option<crate::approval::Ruling> {
    Some(crate::approval::Ruling::Deny {
        reason: "denied".to_owned(),
    })
}

fn releaser_mandate() -> crate::transition::AuthorityMandate {
    crate::transition::AuthorityMandate {
        may_release_control: true,
        ..crate::transition::AuthorityMandate::none()
    }
}

fn control_release_scenario(trajectory: &mut Trajectory) -> ToolRequest {
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let secret = ingress(trajectory, &["alice"], Trust::TRUSTED, "secret");
    let body = ingress(trajectory, &["alice", "bob"], Trust::TRUSTED, "harmless");
    let to = identity_ingress(trajectory, "bob");
    ToolRequest::new(
        ToolName::new("email.send"),
        ArgumentTree::Object(std::collections::BTreeMap::from([
            (ArgumentName::new("to"), ArgumentTree::Value(to)),
            (ArgumentName::new("body"), ArgumentTree::Value(body)),
        ])),
        BTreeSet::from([secret]),
    )
}

#[test]
fn an_inline_control_release_denial_audits_waiver_denied() {
    let mut engine = engine_with([email_contract()]);
    engine
        .register_authority(inline_authority("release-denier", releaser_mandate(), deny_all))
        .unwrap();
    let mut trajectory = Trajectory::new();
    let request = control_release_scenario(&mut trajectory);
    let plans = remediable(&engine, &mut trajectory, request);
    let plan = plans
        .iter()
        .find(|p| release_step(p.steps.first()).is_some())
        .expect("a control-release route");
    let Some(block) = advanced_terminal(apply_first_step(&engine, &mut trajectory, plan.id)) else {
        panic!("expected terminal denial");
    };
    assert!(matches!(block.reason, BlockReason::DeniedByAuthority { .. }));
    assert!(
        trajectory
            .audit()
            .iter()
            .any(|e| denied_delta(e).is_some_and(|d| !delta_raises(d)))
    );
}

#[test]
fn an_external_control_release_denial_audits_waiver_denied() {
    let mut engine = engine_with([email_contract()]);
    engine
        .register_authority(external_authority("remote-releaser", releaser_mandate()))
        .unwrap();
    let mut trajectory = Trajectory::new();
    let request = control_release_scenario(&mut trajectory);
    let plans = remediable(&engine, &mut trajectory, request);
    let plan = plans
        .iter()
        .find(|p| release_step(p.steps.first()).is_some())
        .expect("a control-release route");
    let StepOutcome::NeedsApproval(pending) = apply_first_step(&engine, &mut trajectory, plan.id) else {
        panic!("expected pending approval");
    };
    let decision = engine
        .apply_approval(
            &mut trajectory,
            pending,
            crate::approval::Ruling::Deny {
                reason: "denied".to_owned(),
            },
        )
        .unwrap();
    assert!(matches!(decision, FlowOutcome::Blocked { terminal: Some(_), .. }));
    assert!(
        trajectory
            .audit()
            .iter()
            .any(|e| denied_delta(e).is_some_and(|d| !delta_raises(d)))
    );
}

// ---- Exact violation vectors ----

// ---- Response sink parameters ----

#[test]
fn a_response_is_independent_of_the_pending_tool_action() {
    let engine = engine_with([email_contract()])
        .with_response_policy(ResponsePolicy {
            requires: Requirements {
                audience: crate::contract::AudienceRule::FromRecipients,
                ..Requirements::default()
            },
            readers: BTreeSet::from([user("alice")]),
        })
        .unwrap();
    let mut trajectory = Trajectory::new();
    let body = ingress(&mut trajectory, &["alice", "bob"], Trust::TRUSTED, "the doc");
    let request = email_request(&mut trajectory, body, "bob");
    let _token = walk_to_permit(&engine, &mut trajectory, request);
    assert!(trajectory.pending_action().is_some());

    let note = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "sending it now");
    let response = EmissionRequest {
        body: ArgumentTree::Value(note),
        control: BTreeSet::new(),
        basis: trajectory.revision(),
    };
    let Ok(FlowOutcome::AllowedNow(Emitted { .. })) = engine.evaluate_emission(&mut trajectory, response) else {
        panic!("expected emission despite the pending accepted egress");
    };
    // The emission settled without touching the in-flight action.
    assert!(trajectory.pending_action().is_some());
}

#[test]
fn response_attention_fails_closed_without_a_competent_authority() {
    let engine = PolicyEngine::new()
        .with_response_policy(ResponsePolicy {
            requires: Requirements {
                attention: crate::contract::AttentionRule::ExplicitConfirmation,
                ..Requirements::default()
            },
            readers: BTreeSet::from([user("alice")]),
        })
        .unwrap();
    let mut trajectory = Trajectory::new();
    let note = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "hi");

    let response = EmissionRequest {
        body: ArgumentTree::Value(note),
        control: BTreeSet::new(),
        basis: trajectory.revision(),
    };
    let Some(block) = terminal_block_of(engine.evaluate_emission(&mut trajectory, response)) else {
        panic!("expected block");
    };
    assert!(matches!(
        block.violations.as_slice(),
        [Violation::Breach(crate::contract::Breach::ConfirmationMissing { tool })]
            if *tool == ToolName::new(RESPONSE_SINK)
    ));
}

#[test]
fn a_response_checks_committed_past_effects() {
    let engine = PolicyEngine::new()
        .with_response_policy(ResponsePolicy {
            requires: Requirements {
                forbid_prior_effects: BTreeSet::from([Effect::Egress]),
                ..Requirements::default()
            },
            readers: BTreeSet::from([user("alice")]),
        })
        .unwrap();
    let mut trajectory = Trajectory::new();
    let note = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "quiet so far");
    let response = EmissionRequest {
        body: ArgumentTree::Value(note),
        control: BTreeSet::new(),
        basis: trajectory.revision(),
    };
    let Ok(FlowOutcome::AllowedNow(Emitted { .. })) = engine.evaluate_emission(&mut trajectory, response) else {
        panic!("expected emission before any egress");
    };

    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let response = EmissionRequest {
        body: ArgumentTree::Value(note),
        control: BTreeSet::new(),
        basis: trajectory.revision(),
    };
    let Some(block) = terminal_block_of(engine.evaluate_emission(&mut trajectory, response)) else {
        panic!("expected block after the committed egress");
    };
    assert!(matches!(
        block.violations.as_slice(),
        [Violation::Breach(crate::contract::Breach::ForbiddenPriorEffects { .. })]
    ));
}

#[test]
fn remediable_emission_walks_to_an_emit_via_pursue() {
    let mut engine = PolicyEngine::new()
        .with_response_policy(ResponsePolicy {
            requires: Requirements {
                audience: crate::contract::AudienceRule::FromRecipients,
                ..Requirements::default()
            },
            readers: BTreeSet::from([user("charlie")]),
        })
        .unwrap();
    engine
        .register_authority(inline_authority("auto-voucher", human().mandate, approve_all))
        .unwrap();
    let mut trajectory = Trajectory::new();
    // Only alice may read the note, but the conversation reader is charlie.
    let note = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "the note");
    let request = EmissionRequest {
        body: ArgumentTree::Value(note),
        control: BTreeSet::new(),
        basis: trajectory.revision(),
    };

    let EmissionPursuit::Emitted(emitted) = engine.pursue_emission(&mut trajectory, request, 8) else {
        panic!("expected the endorse walk to settle in an emit");
    };
    assert_eq!(emitted.rendered, "\"the note\"");
    assert_eq!(
        trajectory.value(note).unwrap().label().audience,
        Audience::readers([user("alice")])
    );
    assert!(matches!(
        trajectory.turns().last(),
        Some(crate::turn::Turn {
            actor: crate::turn::Actor::Assistant,
            ..
        })
    ));
    // The emission settled: its slot is free.
    assert!(trajectory.pending_emission().is_none());
}

#[test]
fn emission_slot_discipline_is_per_kind() {
    let mut engine = engine_with([email_contract()])
        .with_response_policy(ResponsePolicy {
            requires: Requirements {
                audience: crate::contract::AudienceRule::FromRecipients,
                ..Requirements::default()
            },
            readers: BTreeSet::from([user("charlie")]),
        })
        .unwrap();
    engine.register_authority(human()).unwrap();
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let note = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "private note");
    let bland = ingress(&mut trajectory, &["alice", "bob", "charlie"], Trust::TRUSTED, "ok");

    let first = EmissionRequest {
        body: ArgumentTree::Value(note),
        control: BTreeSet::new(),
        basis: trajectory.revision(),
    };
    let Ok(FlowOutcome::Blocked { terminal: None, .. }) = engine.evaluate_emission(&mut trajectory, first.clone())
    else {
        panic!("expected a remediable emission");
    };
    let pending_flow = trajectory.pending_emission().unwrap().flow();

    // A different emission proposal is refused without touching anything.
    let second = EmissionRequest {
        body: ArgumentTree::Value(bland),
        control: BTreeSet::new(),
        basis: trajectory.revision(),
    };
    let revision_before = trajectory.revision();
    let Err(refusal) = engine.evaluate_emission(&mut trajectory, second) else {
        panic!("expected refusal while an emission is pending");
    };
    assert_eq!(refusal, FlowRefusal::EmissionAlreadyPending { flow: pending_flow });
    assert_eq!(trajectory.revision(), revision_before);
    assert_eq!(trajectory.pending_emission().unwrap().flow(), pending_flow);

    let body = ingress(&mut trajectory, &["alice", "bob"], Trust::TRUSTED, "doc");
    let email = email_request(&mut trajectory, body, "bob");
    let Ok(FlowOutcome::AllowedNow(_token)) = engine.evaluate(&mut trajectory, email) else {
        panic!("expected the tool flow to permit alongside the pending emission");
    };
    assert!(trajectory.pending_emission().is_some());
    assert!(trajectory.pending_action().is_some());
}

#[test]
fn a_blocked_emission_never_clears_a_pending_action() {
    let engine = engine_with([email_contract()])
        .with_response_policy(ResponsePolicy {
            requires: Requirements {
                audience: crate::contract::AudienceRule::FromRecipients,
                ..Requirements::default()
            },
            readers: BTreeSet::from([user("charlie")]),
        })
        .unwrap();
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let body = ingress(&mut trajectory, &["alice", "bob"], Trust::TRUSTED, "doc");
    let email = email_request(&mut trajectory, body, "bob");
    let Ok(FlowOutcome::AllowedNow(token)) = engine.evaluate(&mut trajectory, email) else {
        panic!("expected the tool flow to permit");
    };
    // No transformer and no authority: the leaking emission is terminal.
    let note = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "private");
    let response = EmissionRequest {
        body: ArgumentTree::Value(note),
        control: BTreeSet::new(),
        basis: trajectory.revision(),
    };
    let Some(block) = terminal_block_of(engine.evaluate_emission(&mut trajectory, response)) else {
        panic!("expected a terminal emission block");
    };
    assert_eq!(block.reason, BlockReason::NoRemedy);
    assert!(trajectory.pending_emission().is_none());
    assert!(trajectory.pending_action().is_some());
    drop(token);
}

// ---- Terminal rescue: joint Endorse x control-release ----

fn masked_contract() -> ToolContract {
    ToolContract {
        name: ToolName::new("post.publish"),
        requires: Some(Requirements {
            trust: Some(KnownTrust::Suspicious),
            audience: crate::contract::AudienceRule::FromRecipients,
            ..Requirements::default()
        }),
        output_label: ValueLabel::identity(),
        effects: Effects::declared([Effect::Egress]),
        arguments: ArgumentSchema::with_recipients(ArgumentName::new("to")),
    }
}

fn masked_flow(trajectory: &mut Trajectory) -> (ValueId, ValueId, ToolRequest) {
    let body = ingress(trajectory, &["alice", "bob"], Trust::UNKNOWN, "draft");
    let secret = ingress(trajectory, &["alice"], Trust::SUSPICIOUS, "selection basis");
    let to = identity_ingress(trajectory, "bob");
    let request = ToolRequest::new(
        ToolName::new("post.publish"),
        ArgumentTree::Object(std::collections::BTreeMap::from([
            (ArgumentName::new("to"), ArgumentTree::Value(to)),
            (ArgumentName::new("body"), ArgumentTree::Value(body)),
        ])),
        BTreeSet::from([secret]),
    );
    (body, secret, request)
}

fn endorser_mandate() -> crate::transition::AuthorityMandate {
    crate::transition::AuthorityMandate {
        trust: Some(KnownTrust::Suspicious),
        ..crate::transition::AuthorityMandate::none()
    }
}

#[test]
fn rescue_composes_endorse_then_release_for_a_masked_flow() {
    let mut engine = engine_with([masked_contract()]);
    engine
        .register_authority(inline_authority("endorser", endorser_mandate(), approve_all))
        .unwrap();
    engine
        .register_authority(inline_authority("releaser", releaser_mandate(), approve_all))
        .unwrap();
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let (body, secret, request) = masked_flow(&mut trajectory);

    let plans = remediable(&engine, &mut trajectory, request.clone());
    assert_eq!(plans.len(), 1);
    let steps = &plans.first().steps;
    assert!(
        raise_step(steps.first())
            .is_some_and(|(source, raise)| source == body && raise.trust == Some(KnownTrust::Suspicious))
    );
    assert_eq!(
        step_targets(steps.first()),
        Some(&[Violation::Unprovable(Unprovable::TrustUnknown)][..])
    );
    assert_eq!(
        release_step(steps.get(steps.len() - 1).unwrap()),
        Some(BTreeSet::from([secret]))
    );

    let token = walk_to_permit(&engine, &mut trajectory, request);
    dispatch(&mut trajectory, token, "published").unwrap();
    let audit = trajectory.audit();
    assert!(audit.iter().any(|e| applied_raise(e).is_some()));
    assert!(audit.iter().any(|e| applied_lift(e).is_some()));
}

#[test]
fn rescue_steps_pin_their_routes_and_target_vectors() {
    let mut engine = engine_with([masked_contract()]);
    engine
        .register_authority(inline_authority("endorser", endorser_mandate(), approve_all))
        .unwrap();
    engine
        .register_authority(inline_authority("releaser", releaser_mandate(), approve_all))
        .unwrap();
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let (_, secret, request) = masked_flow(&mut trajectory);

    let plans = remediable(&engine, &mut trajectory, request);
    assert_eq!(plans.len(), 1);
    let steps = &plans.first().steps;
    assert_eq!(steps.len(), 2);

    assert_eq!(step_routes(steps.first()), Some(vec!["endorser"]));
    assert_eq!(
        step_targets(steps.first()),
        Some(&[Violation::Unprovable(Unprovable::TrustUnknown)][..])
    );

    let waiver = steps.get(1).unwrap();
    assert_eq!(step_routes(waiver), Some(vec!["releaser"]));
    assert_eq!(release_step(waiver), Some(BTreeSet::from([secret])));
    assert_eq!(
        step_targets(waiver),
        Some(
            &[Violation::Breach(crate::contract::Breach::AudienceExceeds {
                outside: BTreeSet::from([user("bob")]),
            })][..]
        )
    );
}

#[test]
fn rescue_without_an_endorse_authority_stays_terminal() {
    let mut engine = engine_with([masked_contract()]);
    engine
        .register_authority(inline_authority("releaser", releaser_mandate(), approve_all))
        .unwrap();
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let (_, _, request) = masked_flow(&mut trajectory);

    let Some(block) = terminal_block_of(engine.evaluate(&mut trajectory, request)) else {
        panic!("expected terminal block");
    };
    assert_eq!(block.reason, BlockReason::NoRemedy);
}

#[test]
fn rescue_without_a_release_authority_stays_terminal() {
    let mut engine = engine_with([masked_contract()]);
    engine
        .register_authority(inline_authority("endorser", endorser_mandate(), approve_all))
        .unwrap();
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let (_, _, request) = masked_flow(&mut trajectory);

    let Some(block) = terminal_block_of(engine.evaluate(&mut trajectory, request)) else {
        panic!("expected terminal block");
    };
    assert_eq!(block.reason, BlockReason::NoRemedy);
}

#[test]
fn rescue_endorse_authority_sees_the_projected_target() {
    fn approve_iff_projected(
        _: &crate::remedy::Authorization,
        resolved: &[Violation],
        _: &crate::approval::TrajectoryView,
    ) -> Option<crate::approval::Ruling> {
        if resolved
            .iter()
            .any(|v| matches!(v, Violation::Unprovable(Unprovable::TrustUnknown)))
        {
            Some(crate::approval::Ruling::Approve {
                reason: "the projected residual names the unknown".to_owned(),
            })
        } else {
            Some(crate::approval::Ruling::Deny {
                reason: "asked to endorse against a vector with no trust gap".to_owned(),
            })
        }
    }
    let mut engine = engine_with([masked_contract()]);
    engine
        .register_authority(inline_authority("endorser", endorser_mandate(), approve_iff_projected))
        .unwrap();
    engine
        .register_authority(inline_authority("releaser", releaser_mandate(), approve_all))
        .unwrap();
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let (_, _, request) = masked_flow(&mut trajectory);

    let token = walk_to_permit(&engine, &mut trajectory, request);
    dispatch(&mut trajectory, token, "published").unwrap();
}

#[test]
fn rescue_release_stays_least_privilege() {
    let mut engine = engine_with([masked_contract()]);
    engine
        .register_authority(inline_authority("endorser", endorser_mandate(), approve_all))
        .unwrap();
    engine
        .register_authority(inline_authority("releaser", releaser_mandate(), approve_all))
        .unwrap();
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let (_, secret, mut request) = masked_flow(&mut trajectory);
    // A second, clean control dep must not be released alongside the dirty one.
    let clean_ctl = ingress(&mut trajectory, &["alice", "bob"], Trust::TRUSTED, "benign plan");
    request.control.insert(clean_ctl);

    let plans = remediable(&engine, &mut trajectory, request);
    let steps = &plans.first().steps;
    assert_eq!(
        release_step(steps.get(steps.len() - 1).unwrap()),
        Some(BTreeSet::from([secret]))
    );
}

#[test]
fn rescue_carries_acknowledge_only_facts() {
    let contract = ToolContract {
        requires: Some(Requirements {
            trust: Some(KnownTrust::Suspicious),
            audience: crate::contract::AudienceRule::FromRecipients,
            forbid_prior_effects: BTreeSet::from([Effect::Egress]),
            ..Requirements::default()
        }),
        ..masked_contract()
    };

    let run = |ack: bool| {
        let mut engine = engine_with([contract.clone()]);
        engine
            .register_authority(inline_authority("endorser", endorser_mandate(), approve_all))
            .unwrap();
        let releaser_mandate = crate::transition::AuthorityMandate {
            may_release_control: true,
            acknowledge_unknown: ack,
            ..crate::transition::AuthorityMandate::none()
        };
        engine
            .register_authority(inline_authority("releaser", releaser_mandate, approve_all))
            .unwrap();
        let mut trajectory = Trajectory::new();
        trajectory.seed_committed_effects(Effects::UNKNOWN);
        let (_, _, request) = masked_flow(&mut trajectory);
        (engine, trajectory, request)
    };

    let (engine, mut trajectory, request) = run(false);
    assert!(matches!(
        engine.evaluate(&mut trajectory, request),
        Ok(FlowOutcome::Blocked { terminal: Some(_), .. })
    ));

    let (engine, mut trajectory, request) = run(true);
    let token = walk_to_permit(&engine, &mut trajectory, request);
    dispatch(&mut trajectory, token, "published").unwrap();
    assert!(
        trajectory
            .audit()
            .iter()
            .any(|e| applied_lift(e).is_some_and(|d| delta_acknowledges(d) && delta_releases_control(d)))
    );
}

fn subset_release_flow(trajectory: &mut Trajectory) -> (ValueId, ToolRequest) {
    let body = ingress(trajectory, &["alice", "bob"], Trust::UNKNOWN, "draft");
    let mask = ingress(trajectory, &["alice", "bob"], Trust::SUSPICIOUS, "mask");
    let gate = ingress(trajectory, &["alice"], Trust::TRUSTED, "selection");
    let to = identity_ingress(trajectory, "bob");
    let request = ToolRequest::new(
        ToolName::new("post.publish"),
        ArgumentTree::Object(std::collections::BTreeMap::from([
            (ArgumentName::new("to"), ArgumentTree::Value(to)),
            (ArgumentName::new("body"), ArgumentTree::Value(body)),
        ])),
        BTreeSet::from([mask, gate]),
    );
    (gate, request)
}

#[test]
fn rescue_finds_a_clean_subset_release_without_an_endorser() {
    let mut engine = engine_with([masked_contract()]);
    engine
        .register_authority(inline_authority("releaser", releaser_mandate(), approve_all))
        .unwrap();
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let (gate, request) = subset_release_flow(&mut trajectory);

    let plans = remediable(&engine, &mut trajectory, request.clone());
    let steps = &plans.first().steps;
    assert_eq!(steps.len(), 1);
    assert_eq!(release_step(steps.first()), Some(BTreeSet::from([gate])));

    let token = walk_to_permit(&engine, &mut trajectory, request);
    dispatch(&mut trajectory, token, "published").unwrap();
}

#[test]
fn rescue_prefers_the_smallest_release_over_an_endorsement() {
    let mut engine = engine_with([masked_contract()]);
    engine
        .register_authority(inline_authority("endorser", endorser_mandate(), approve_all))
        .unwrap();
    engine
        .register_authority(inline_authority("releaser", releaser_mandate(), approve_all))
        .unwrap();
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let (gate, request) = subset_release_flow(&mut trajectory);

    let plans = remediable(&engine, &mut trajectory, request);
    let steps = &plans.first().steps;
    assert_eq!(steps.len(), 1);
    assert_eq!(release_step(steps.first()), Some(BTreeSet::from([gate])));
}

#[test]
fn rescue_external_approval_resolves_the_projected_residual() {
    let mut engine = engine_with([masked_contract()]);
    engine
        .register_authority(external_authority("remote-endorser", endorser_mandate()))
        .unwrap();
    engine
        .register_authority(inline_authority("releaser", releaser_mandate(), approve_all))
        .unwrap();
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let (_, _, request) = masked_flow(&mut trajectory);

    let plans = remediable(&engine, &mut trajectory, request);
    let capability = engine.mint_step(&trajectory, plans.first().id, 0).unwrap();
    let StepOutcome::NeedsApproval(pending) = engine.apply_step(&mut trajectory, capability).unwrap() else {
        panic!("expected the external endorse to defer");
    };
    assert_eq!(pending.resolves(), &[Violation::Unprovable(Unprovable::TrustUnknown)]);
    let decision = engine
        .apply_approval(
            &mut trajectory,
            pending,
            crate::approval::Ruling::Approve {
                reason: "vouched".to_owned(),
            },
        )
        .unwrap();
    assert!(matches!(decision, FlowOutcome::Blocked { terminal: None, .. }));
}

#[test]
fn release_search_streams_past_32_control_deps() {
    let mut engine = engine_with([masked_contract()]);
    engine
        .register_authority(inline_authority("endorser", endorser_mandate(), approve_all))
        .unwrap();
    engine
        .register_authority(inline_authority("releaser", releaser_mandate(), approve_all))
        .unwrap();
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let (_, secret, mut request) = masked_flow(&mut trajectory);
    // Pad with neutral identity-label controls well past 32 total deps.
    for i in 0..39 {
        request
            .control
            .insert(identity_ingress(&mut trajectory, &format!("noise-{i}")));
    }
    assert!(request.control.len() > 32);

    let plans = remediable(&engine, &mut trajectory, request);
    let steps = &plans.first().steps;
    assert!(steps.iter().any(|step| raise_step(step).is_some()));
    assert_eq!(
        release_step(steps.get(steps.len() - 1).unwrap()),
        Some(BTreeSet::from([secret])),
        "the minimum release names exactly the masking dep, none of the noise"
    );
}

#[test]
fn rescue_does_not_over_endorse_re_masked_leaves() {
    let mut engine = engine_with([masked_contract()]);
    engine
        .register_authority(inline_authority("endorser", endorser_mandate(), approve_all))
        .unwrap();
    engine
        .register_authority(inline_authority("releaser", releaser_mandate(), approve_all))
        .unwrap();
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let first = ingress(&mut trajectory, &["alice", "bob"], Trust::UNKNOWN, "summary");
    let second = ingress(&mut trajectory, &["alice", "bob"], Trust::UNKNOWN, "appendix");
    let mask = ingress(&mut trajectory, &["alice"], Trust::SUSPICIOUS, "mask");
    let to = identity_ingress(&mut trajectory, "bob");
    let request = ToolRequest::new(
        ToolName::new("post.publish"),
        ArgumentTree::Object(std::collections::BTreeMap::from([
            (ArgumentName::new("to"), ArgumentTree::Value(to)),
            (
                ArgumentName::new("body"),
                ArgumentTree::List(vec![ArgumentTree::Value(first), ArgumentTree::Value(second)]),
            ),
        ])),
        BTreeSet::from([mask]),
    );

    let plans = remediable(&engine, &mut trajectory, request.clone());
    let steps = &plans.first().steps;
    assert_eq!(steps.len(), 2);
    assert!(raise_step(steps.first()).is_some_and(|(source, _)| source == first));
    assert!(release_step(steps.get(1).unwrap()).is_some());
    let endorsed = |t: &Trajectory| t.audit().iter().filter(|e| applied_raise(e).is_some()).count();
    let token = walk_to_permit(&engine, &mut trajectory, request);
    dispatch(&mut trajectory, token, "published").unwrap();
    assert_eq!(endorsed(&trajectory), 1);
}

#[test]
fn rescue_endorse_targets_shrink_per_peel() {
    let mut engine = engine_with([masked_contract()]);
    let endorser = crate::transition::AuthorityMandate {
        trust: Some(KnownTrust::Suspicious),
        audience: Some(BTreeSet::from([user("bob")])),
        ..crate::transition::AuthorityMandate::none()
    };
    engine
        .register_authority(inline_authority("endorser", endorser, approve_all))
        .unwrap();
    engine
        .register_authority(inline_authority("releaser", releaser_mandate(), approve_all))
        .unwrap();
    let mut trajectory = Trajectory::new();
    trajectory.seed_committed_effects(Effects::declared([Effect::Egress]));
    let first = ingress(&mut trajectory, &["alice"], Trust::UNKNOWN, "summary");
    let second = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "appendix");
    let mask = ingress(&mut trajectory, &["alice"], Trust::SUSPICIOUS, "mask");
    let to = identity_ingress(&mut trajectory, "bob");
    let request = ToolRequest::new(
        ToolName::new("post.publish"),
        ArgumentTree::Object(std::collections::BTreeMap::from([
            (ArgumentName::new("to"), ArgumentTree::Value(to)),
            (
                ArgumentName::new("body"),
                ArgumentTree::List(vec![ArgumentTree::Value(first), ArgumentTree::Value(second)]),
            ),
        ])),
        BTreeSet::from([mask]),
    );

    let plans = remediable(&engine, &mut trajectory, request.clone());
    let steps = &plans.first().steps;
    assert!(raise_step(steps.first()).is_some_and(|(source, _)| source == first));
    let first_targets = step_targets(steps.first()).expect("an authorize step");
    assert!(
        first_targets
            .iter()
            .any(|v| matches!(v, Violation::Unprovable(Unprovable::TrustUnknown)))
    );
    assert!(
        first_targets
            .iter()
            .any(|v| matches!(v, Violation::Breach(crate::contract::Breach::AudienceExceeds { .. })))
    );
    assert!(raise_step(steps.get(1).unwrap()).is_some_and(|(source, _)| source == second));
    assert_eq!(
        step_targets(steps.get(1).unwrap()),
        Some(
            &[Violation::Breach(crate::contract::Breach::AudienceExceeds {
                outside: BTreeSet::from([user("bob")]),
            })][..]
        )
    );

    let token = walk_to_permit(&engine, &mut trajectory, request);
    dispatch(&mut trajectory, token, "published").unwrap();
}

#[test]
fn declared_trusted_output_cannot_launder_a_suspicious_flow() {
    let summarize = ToolContract {
        name: ToolName::new("doc.summarize"),
        requires: Some(Requirements::default()),
        output_label: ValueLabel {
            audience: Audience::PUBLIC,
            trust: Trust::TRUSTED,
        },
        effects: Effects::none(),
        arguments: ArgumentSchema::opaque(),
    };
    let mut engine = engine_with([summarize, email_contract()]);
    engine.register_authority(human()).unwrap();
    let mut trajectory = Trajectory::new();
    let page = ingress(&mut trajectory, &["alice"], Trust::SUSPICIOUS, "web page");
    let request = ToolRequest::new(
        ToolName::new("doc.summarize"),
        ArgumentTree::Object(std::collections::BTreeMap::from([(
            ArgumentName::new("doc"),
            ArgumentTree::Value(page),
        )])),
        BTreeSet::new(),
    );
    let token = match engine.evaluate(&mut trajectory, request) {
        Ok(FlowOutcome::AllowedNow(token)) => token,
        other => panic!("summarize has no requirements, expected a permit, got {other:?}"),
    };
    let summary = dispatch(&mut trajectory, token, "summary").unwrap();
    // Declared trusted+public; the conservative fold absorbed it.
    assert_eq!(trajectory.value(summary).unwrap().label().trust, Trust::SUSPICIOUS);

    let email = email_request(&mut trajectory, summary, "alice");
    let plans = remediable(&engine, &mut trajectory, email);
    let steps = &plans.first().steps;
    assert!(
        raise_step(steps.first())
            .is_some_and(|(source, raise)| source == summary && raise.trust == Some(KnownTrust::Trusted)),
        "the widening is authorizable only as an explicit durable raise"
    );
}

#[test]
fn declared_public_output_cannot_widen_a_bounded_audience() {
    let summarize = ToolContract {
        name: ToolName::new("doc.summarize"),
        requires: Some(Requirements::default()),
        output_label: ValueLabel {
            audience: Audience::PUBLIC,
            trust: Trust::TRUSTED,
        },
        effects: Effects::none(),
        arguments: ArgumentSchema::opaque(),
    };
    let mut engine = engine_with([summarize, email_contract()]);
    engine.register_authority(human()).unwrap();
    let mut trajectory = Trajectory::new();
    let doc = ingress(&mut trajectory, &["alice"], Trust::TRUSTED, "internal doc");
    let request = ToolRequest::new(
        ToolName::new("doc.summarize"),
        ArgumentTree::Object(std::collections::BTreeMap::from([(
            ArgumentName::new("doc"),
            ArgumentTree::Value(doc),
        )])),
        BTreeSet::new(),
    );
    let token = match engine.evaluate(&mut trajectory, request) {
        Ok(FlowOutcome::AllowedNow(token)) => token,
        other => panic!("summarize has no requirements, expected a permit, got {other:?}"),
    };
    let summary = dispatch(&mut trajectory, token, "summary").unwrap();
    // Declared public; the fold keeps the bounded audience.
    assert_eq!(
        trajectory.value(summary).unwrap().label().audience,
        Audience::readers([user("alice")])
    );

    let email = email_request(&mut trajectory, summary, "bob");
    let plans = remediable(&engine, &mut trajectory, email);
    let steps = &plans.first().steps;
    assert!(
        raise_step(steps.first())
            .is_some_and(|(source, raise)| source == summary && raise.audience == Some(BTreeSet::from([user("bob")]))),
        "the widening is authorizable only as an explicit audience vouch"
    );
}

// ---- Unknown requirements: fail closed as RequirementsUnknown ----

#[test]
fn unknown_requirements_escalate_as_sole_unprovable() {
    let mut engine = PolicyEngine::new();
    engine
        .register(ToolContract {
            name: ToolName::new("mystery.tool"),
            requires: None,
            output_label: ValueLabel::identity(),
            effects: Effects::none(),
            arguments: ArgumentSchema::opaque(),
        })
        .unwrap();
    for label in [
        ValueLabel::identity(),
        ValueLabel {
            trust: Trust::SUSPICIOUS,
            audience: Audience::readers([user("alice")]),
        },
    ] {
        let mut trajectory = Trajectory::new();
        let value = trajectory.ingress(Speaker::user(user("alice")), label, OpaqueValue::new("hi"));
        let request = ToolRequest::new(
            ToolName::new("mystery.tool"),
            ArgumentTree::Value(value),
            BTreeSet::new(),
        );
        let block = terminal_block_of(engine.evaluate(&mut trajectory, request)).expect("expected terminal block");
        assert_eq!(
            block.violations,
            vec![Violation::Unprovable(Unprovable::RequirementsUnknown)]
        );
        assert_eq!(block.reason, BlockReason::NoRemedy);
    }
}

#[test]
fn allow_authority_acknowledges_unknown_requirements() {
    fn always_allow(
        _authorization: &Authorization,
        _violations: &[Violation],
        _view: &TrajectoryView<'_>,
    ) -> Option<Ruling> {
        Some(Ruling::Approve {
            reason: "policy allow".into(),
        })
    }
    let mut engine = PolicyEngine::new();
    engine
        .register(ToolContract {
            name: ToolName::new("mystery.tool"),
            requires: None,
            output_label: ValueLabel::identity(),
            effects: Effects::none(),
            arguments: ArgumentSchema::opaque(),
        })
        .unwrap();
    engine
        .register_authority(Authority::inline(
            "default-allow",
            AuthorityMandate::none().acknowledge_unknown(),
            always_allow,
        ))
        .unwrap();
    let mut trajectory = Trajectory::new();
    let value = trajectory.ingress(
        Speaker::user(user("alice")),
        ValueLabel::identity(),
        OpaqueValue::new("hi"),
    );
    let request = ToolRequest::new(
        ToolName::new("mystery.tool"),
        ArgumentTree::Value(value),
        BTreeSet::new(),
    );
    match engine.pursue(&mut trajectory, request, 8) {
        Pursuit::Permitted(_token) => {}
        other => panic!("expected permitted via acknowledgment, got {other:?}"),
    }
    let acknowledged = trajectory.audit().iter().any(|e| {
        matches!(
            e,
            AuditEvent::AuthorizationApplied { authority, resolved, .. }
                if authority.as_str() == "default-allow"
                    && resolved == &[Violation::Unprovable(Unprovable::RequirementsUnknown)]
        )
    });
    assert!(
        acknowledged,
        "expected default-allow to record acknowledging RequirementsUnknown"
    );
}

/// A contract that considers its requirements and declares none (`Some(default)`)
/// is unconditionally different from one that never states them (`None`): the
/// former is a deliberate "nothing required" and stays ungated, no escalation
/// at all.
#[test]
fn considered_empty_requirements_stay_ungated() {
    let mut engine = PolicyEngine::new();
    engine
        .register(ToolContract {
            name: ToolName::new("open.tool"),
            requires: Some(Requirements::default()),
            output_label: ValueLabel::identity(),
            effects: Effects::none(),
            arguments: ArgumentSchema::opaque(),
        })
        .unwrap();
    let mut trajectory = Trajectory::new();
    let value = trajectory.ingress(
        Speaker::user(user("alice")),
        ValueLabel::identity(),
        OpaqueValue::new("hi"),
    );
    let request = ToolRequest::new(ToolName::new("open.tool"), ArgumentTree::Value(value), BTreeSet::new());
    assert!(matches!(
        engine.evaluate(&mut trajectory, request),
        Ok(FlowOutcome::AllowedNow(_))
    ));
}
