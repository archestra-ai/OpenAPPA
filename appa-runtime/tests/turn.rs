use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use appa_engine::fact::{BoundaryKind, CloseOutcome, Fact, Revision};
use appa_engine::label::{Audience, Dim, Label, ReaderId, Trust};
use appa_engine::projection::Projection;
use appa_engine::value::{Provenance, RawResultDigest, ResolvedCall, ToolName, TrajectoryId};
use appa_runtime::store::TenantId;
use appa_runtime::tool::{BuiltinTool, EXECUTE_REMEDY_PLAN, FORK, SUBMIT_RESULT};
use appa_runtime::{
    Completion, Config, Limits, Mediator, RunBudget, Step, StopReason, Turn, TurnError, WireFunctionCall, WireMessage,
    WireToolCall,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

fn mediator(policy: &str, tools: &[(&str, BuiltinTool)]) -> Arc<Mediator> {
    let config = Config::from_toml_str(policy).expect("policy parses");
    let tools = tools
        .iter()
        .map(|(name, backend)| (ToolName::new(*name), backend.clone()))
        .collect::<BTreeMap<_, _>>();
    Arc::new(Mediator::new(config, tools).expect("mediator assembles"))
}

fn call(id: &str, name: &str, arguments: &str) -> WireToolCall {
    WireToolCall {
        id: id.to_string(),
        kind: "function".to_string(),
        function: WireFunctionCall {
            name: name.to_string(),
            arguments: arguments.to_string(),
        },
    }
}

fn calls(tool_calls: Vec<WireToolCall>) -> Completion {
    Completion {
        content: None,
        tool_calls,
    }
}

fn final_answer(content: &str) -> Completion {
    Completion {
        content: Some(content.to_string()),
        tool_calls: Vec::new(),
    }
}

async fn begin(mediator: &Arc<Mediator>, tenant: &TenantId, session: &TrajectoryId, text: &str) -> Turn {
    mediator
        .begin_turn(tenant.clone(), session.clone(), text, CancellationToken::new())
        .await
        .expect("turn begins")
}

fn facts(mediator: &Mediator, tenant: &TenantId, session: &TrajectoryId) -> Vec<Fact> {
    mediator.snapshot(tenant, session).expect("snapshot").0
}

fn offered_plan(
    mediator: &Mediator,
    tenant: &TenantId,
    session: &TrajectoryId,
    call_id: &str,
    description: &str,
) -> String {
    let content = facts(mediator, tenant, session)
        .iter()
        .rev()
        .find_map(|fact| match fact {
            Fact::BlockFeedback {
                call_id: id, content, ..
            } if id.as_str() == call_id => Some(content.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{call_id} has no block feedback"));
    let at = content
        .find(description)
        .unwrap_or_else(|| panic!("no plan matching {description:?} in: {content}"));
    let head = &content[..at];
    let open = head
        .rfind("\"remedy-")
        .unwrap_or_else(|| panic!("no quoted handle precedes {description:?} in: {content}"));
    let rest = &head[open + 1..];
    let close = rest.find('"').expect("the handle's closing quote");
    rest[..close].to_string()
}

/// `execute_remedy_plan(plan_id)` arguments for a handle resolved by [`offered_plan`].
fn remedy_args(handle: &str) -> String {
    format!(r#"{{"plan_id":"{handle}"}}"#)
}

const RAW_ACCEPT: &str = "accept the narrowing and return the result raw";

fn tool_values(log: &[Fact]) -> Vec<(&str, &appa_engine::label::Label)> {
    log.iter()
        .filter_map(|fact| match fact {
            Fact::ValueAdmitted {
                value,
                provenance: Provenance::ToolResult { .. },
                ..
            } => Some((value.body.as_str(), &value.label)),
            _ => None,
        })
        .collect()
}

async fn parent_with_completed_turn(
    mediator: &Arc<Mediator>,
    tenant: &TenantId,
    budget: &mut RunBudget,
) -> TrajectoryId {
    let parent = mediator.create_session(tenant.clone());
    let mut turn = begin(mediator, tenant, &parent, "parent").await;
    assert!(matches!(
        turn.mediate(final_answer("ready"), budget).await.unwrap(),
        Step::Final(_)
    ));
    parent
}

async fn child_with_internal_read(
    mediator: &Arc<Mediator>,
    tenant: &TenantId,
    parent: &TrajectoryId,
    budget: &mut RunBudget,
) -> Turn {
    let child = mediator.fork_session_reserved(tenant, parent).unwrap();
    let mut turn = mediator
        .begin_forked_turn(tenant.clone(), child, "inspect", CancellationToken::new())
        .unwrap();
    assert!(matches!(
        turn.mediate(calls(vec![call("read", "read", "{}")]), budget)
            .await
            .unwrap(),
        Step::Continue
    ));
    assert!(matches!(
        turn.mediate(
            calls(vec![call(
                "accept-read",
                EXECUTE_REMEDY_PLAN,
                r#"{"plan_id":"remedy-0"}"#,
            )]),
            budget,
        )
        .await
        .unwrap(),
        Step::Continue
    ));
    turn
}

#[tokio::test]
async fn ordinary_calls_are_serial_and_only_admitted_results_enter_the_transcript() {
    let mediator = mediator(
        r#"
version = 1
[[tool]]
name = "first"
delta = {}
[[tool]]
name = "second"
delta = {}
"#,
        &[
            ("first", BuiltinTool::Echo("one".to_string())),
            ("second", BuiltinTool::Echo("two".to_string())),
        ],
    );
    let tenant = TenantId::new("tenant");
    let session = mediator.create_session(tenant.clone());
    let mut turn = begin(&mediator, &tenant, &session, "run both").await;
    let mut budget = RunBudget::default();

    assert!(matches!(
        turn.mediate(
            calls(vec![call("a", "first", "{}"), call("b", "second", "{}")]),
            &mut budget,
        )
        .await
        .unwrap(),
        Step::Continue
    ));
    let log = facts(&mediator, &tenant, &session);
    let opened: Vec<_> = log
        .iter()
        .filter_map(|fact| match fact {
            Fact::DispatchOpened { dispatch, .. } => Some(dispatch),
            _ => None,
        })
        .collect();
    let closed: Vec<_> = log
        .iter()
        .filter_map(|fact| match fact {
            Fact::DispatchClosed { dispatch, .. } => Some(dispatch),
            _ => None,
        })
        .collect();
    assert_eq!(opened, closed);
    assert_eq!(
        tool_values(&log).iter().map(|(body, _)| *body).collect::<Vec<_>>(),
        ["one", "two"]
    );

    let transcript = turn.transcript().unwrap();
    assert_eq!(transcript.len(), 4);
    assert_eq!(transcript[0], WireMessage::user("run both"));
    assert_eq!(transcript[1].tool_calls.as_ref().unwrap().len(), 2);
    assert_eq!(transcript[2], WireMessage::tool_result("a", "one"));
    assert_eq!(transcript[3], WireMessage::tool_result("b", "two"));

    assert!(matches!(
        turn.mediate(final_answer("finished"), &mut budget).await.unwrap(),
        Step::Final(answer) if answer == "finished"
    ));
}

#[tokio::test]
async fn a_pending_cast_output_keeps_its_raw_body_confined() {
    let tenant = TenantId::new("tenant");
    let mut budget = RunBudget::default();
    let pending_cast = mediator(
        r#"
version = 1
[[tool]]
name = "scan"
effects = ["read"]
delta = { trust = "unknown" }
"#,
        &[("scan", BuiltinTool::Echo("secret mailbox".to_string()))],
    );
    let session = pending_cast.create_session(tenant.clone());
    let mut turn = begin(&pending_cast, &tenant, &session, "scan").await;
    assert!(matches!(
        turn.mediate(calls(vec![call("c", "scan", "{}")]), &mut budget)
            .await
            .unwrap(),
        Step::Continue
    ));
    let log = facts(&pending_cast, &tenant, &session);
    assert!(tool_values(&log).is_empty());
    assert!(log.iter().any(|fact| matches!(
        fact,
        Fact::DispatchClosed {
            outcome: CloseOutcome::Success { effects },
            ..
        } if effects.iter().any(|effect| effect.as_str() == "read")
    )));
    assert_eq!(
        log.iter()
            .filter(|fact| matches!(fact, Fact::BlockFeedback { .. }))
            .count(),
        1
    );
    turn.stop(StopReason::Cancelled).unwrap();
}

#[tokio::test]
async fn authority_remedies_allow_or_deny_before_any_south_dispatch() {
    let allow = mediator(
        r#"
version = 1
[[tool]]
name = "wire"
effects = ["spend"]
[tool.requires]
attention = ["signoff"]

[[authority]]
name = "officer"
mandate = { attends = ["signoff"] }
implementation = { builtin = "approve" }
"#,
        &[("wire", BuiltinTool::Echo("paid".to_string()))],
    );
    let tenant = TenantId::new("tenant");
    let session = allow.create_session(tenant.clone());
    let mut turn = begin(&allow, &tenant, &session, "pay").await;
    let mut budget = RunBudget::default();
    turn.mediate(calls(vec![call("blocked", "wire", "{}")]), &mut budget)
        .await
        .unwrap();
    assert!(
        !facts(&allow, &tenant, &session)
            .iter()
            .any(|fact| matches!(fact, Fact::DispatchOpened { .. }))
    );
    assert!(matches!(
        turn.mediate(
            calls(vec![call("remedy", EXECUTE_REMEDY_PLAN, r#"{"plan_id":"remedy-0"}"#,)]),
            &mut budget,
        )
        .await
        .unwrap(),
        Step::Continue
    ));
    let log = facts(&allow, &tenant, &session);
    assert_eq!(log.iter().filter(|fact| matches!(fact, Fact::Ruling { .. })).count(), 1);
    assert!(log.iter().any(|fact| matches!(
        fact,
        Fact::DispatchClosed {
            outcome: CloseOutcome::Success { effects },
            ..
        } if effects.iter().any(|effect| effect.as_str() == "spend")
    )));
    turn.stop(StopReason::Cancelled).unwrap();

    let (url, server) = spawn_authority("deny").await;
    let deny_policy = format!(
        r#"
version = 1
[[tool]]
name = "wire"
effects = ["spend"]
[tool.requires]
attention = ["signoff"]

[[authority]]
name = "officer"
mandate = {{ attends = ["signoff"] }}
implementation = {{ resolver = {{ url = "{url}" }} }}
"#
    );
    let deny = mediator(&deny_policy, &[("wire", BuiltinTool::Echo("must not run".to_string()))]);
    let session = deny.create_session(tenant.clone());
    let mut turn = begin(&deny, &tenant, &session, "pay").await;
    turn.mediate(calls(vec![call("blocked", "wire", "{}")]), &mut budget)
        .await
        .unwrap();
    assert!(matches!(
        turn.mediate(
            calls(vec![call("denied", EXECUTE_REMEDY_PLAN, r#"{"plan_id":"remedy-0"}"#,)]),
            &mut budget,
        )
        .await
        .unwrap(),
        Step::Continue
    ));
    server.await.unwrap();
    let log = facts(&deny, &tenant, &session);
    assert!(!log.iter().any(|fact| matches!(fact, Fact::Ruling { .. })));
    assert!(!log.iter().any(|fact| matches!(fact, Fact::DispatchOpened { .. })));
    turn.stop(StopReason::Cancelled).unwrap();
}

#[tokio::test]
async fn a_denial_consumes_every_offer_naming_the_denying_authority() {
    // `RMD-6`: a denial consumes every offered plan naming the denying authority for this rendered
    // call, so an advertised alternative is always executable. `broad` covers both gaps, so it is
    // named by both offered plans; denying it must retire both, not just the one consulted.
    // (An abstention is the complement and stays plan-local — the sibling test above covers it.)
    let (deny_url, consults, deny_server) = spawn_counting_authority("deny");
    let policy = format!(
        r#"
version = 1
trust_chain = ["suspicious", "trusted"]
[boundary]
trust = "suspicious"
[[tool]]
name = "wire"
effects = ["spend"]
delta = {{}}
[tool.requires]
trust = "trusted"
attention = ["signoff"]

[[authority]]
name = "broad"
mandate = {{ can_raise_trust_to = "trusted", attends = ["signoff"] }}
implementation = {{ resolver = {{ url = "{deny_url}" }} }}

[[authority]]
name = "mark-only"
mandate = {{ attends = ["signoff"] }}
implementation = {{ builtin = "approve" }}
"#
    );
    let mediated = mediator(&policy, &[("wire", BuiltinTool::Echo("paid".to_string()))]);
    let tenant = TenantId::new("tenant");
    let session = mediated.create_session(tenant.clone());
    let mut turn = begin(&mediated, &tenant, &session, "pay").await;
    let mut budget = RunBudget::default();

    // The block offers two plans: {broad: floor+mark} and {broad: floor, mark-only: mark}.
    assert!(matches!(
        turn.mediate(calls(vec![call("blocked", "wire", "{}")]), &mut budget)
            .await
            .unwrap(),
        Step::Continue
    ));

    // Executing the first consults `broad`, which denies.
    assert!(matches!(
        turn.mediate(
            calls(vec![call("denied", EXECUTE_REMEDY_PLAN, r#"{"plan_id":"remedy-0"}"#)]),
            &mut budget,
        )
        .await
        .unwrap(),
        Step::Continue
    ));
    assert_eq!(consults.load(Ordering::SeqCst), 1);

    // remedy-1 also names `broad`, so the denial retired it too: the handle no longer resolves.
    assert!(matches!(
        turn.mediate(
            calls(vec![call("stale", EXECUTE_REMEDY_PLAN, r#"{"plan_id":"remedy-1"}"#)]),
            &mut budget,
        )
        .await
        .unwrap(),
        Step::Continue
    ));
    // The denier is never consulted again — which is the whole of `RMD-6`. Retiring only the
    // consulted offer would have left remedy-1 live and sent a second request here.
    // Reverting the Deny arm takes this count from 1 to 2, which is what makes the assertion
    // discriminating rather than merely true.
    assert_eq!(consults.load(Ordering::SeqCst), 1);
    let log = facts(&mediated, &tenant, &session);
    assert!(!log.iter().any(|fact| matches!(fact, Fact::DispatchOpened { .. })));
    assert!(!log.iter().any(|fact| matches!(fact, Fact::Ruling { .. })));
    turn.stop(StopReason::Cancelled).unwrap();
    deny_server.abort();
}

#[tokio::test]
async fn a_denied_authority_leaves_its_sibling_offer_and_the_approval_records_its_review() {
    let (deny_url, deny_server) = spawn_authority("deny").await;
    let policy = format!(
        r#"
version = 1
trust_chain = ["suspicious", "trusted"]
[[tool]]
name = "wire"
effects = ["spend"]
[tool.requires]
attention = ["signoff"]

[[authority]]
name = "no-officer"
mandate = {{ attends = ["signoff"] }}
implementation = {{ resolver = {{ url = "{deny_url}" }} }}

[[authority]]
name = "yes-officer"
mandate = {{ attends = ["signoff"] }}
implementation = {{ builtin = "approve" }}
"#
    );
    let mediated = mediator(&policy, &[("wire", BuiltinTool::Echo("paid".to_string()))]);
    let tenant = TenantId::new("tenant");
    let session = mediated.create_session(tenant.clone());
    let mut turn = begin(&mediated, &tenant, &session, "pay").await;
    let mut budget = RunBudget::default();

    assert!(matches!(
        turn.mediate(calls(vec![call("blocked", "wire", "{}")]), &mut budget)
            .await
            .unwrap(),
        Step::Continue
    ));
    assert!(matches!(
        turn.mediate(
            calls(vec![call("denied", EXECUTE_REMEDY_PLAN, r#"{"plan_id":"remedy-0"}"#)]),
            &mut budget,
        )
        .await
        .unwrap(),
        Step::Continue
    ));
    assert!(
        !facts(&mediated, &tenant, &session)
            .iter()
            .any(|fact| matches!(fact, Fact::DispatchOpened { .. }))
    );

    assert!(matches!(
        turn.mediate(
            calls(vec![
                call("approved", EXECUTE_REMEDY_PLAN, r#"{"plan_id":"remedy-1"}"#,)
            ]),
            &mut budget,
        )
        .await
        .unwrap(),
        Step::Continue
    ));
    deny_server.await.unwrap();

    let log = facts(&mediated, &tenant, &session);
    let (authority, reviewed) = log
        .iter()
        .find_map(|fact| match fact {
            Fact::Ruling {
                authority, reviewed, ..
            } => Some((authority, reviewed)),
            _ => None,
        })
        .expect("the sibling approval lands one ruling");
    assert_eq!(authority.as_str(), "yes-officer");
    assert_eq!(reviewed.tool, ToolName::new("wire"));
    assert_eq!(
        reviewed.trajectory_label,
        Label::new(Dim::Known(Trust::new(1)), Dim::Known(Audience::Public))
    );
    assert!(reviewed.arg_refs.is_empty());
    assert_eq!(log.iter().filter(|fact| matches!(fact, Fact::Ruling { .. })).count(), 1);
    assert!(log.iter().any(|fact| matches!(
        fact,
        Fact::DispatchClosed {
            outcome: CloseOutcome::Success { effects },
            ..
        } if effects.iter().any(|effect| effect.as_str() == "spend")
    )));
    turn.stop(StopReason::Cancelled).unwrap();
}

#[tokio::test]
async fn pending_cast_acceptance_requires_a_later_round_and_admits_once() {
    let mediated = mediator(
        r#"
version = 1
trust_chain = ["suspicious", "trusted"]
[[tool]]
name = "scan"
effects = ["read"]
delta = { trust = "unknown" }

[[cast]]
name = "paranoid"
constant = { trust = "suspicious" }
"#,
        &[("scan", BuiltinTool::Echo("mail body".to_string()))],
    );
    let tenant = TenantId::new("tenant");
    let session = mediated.create_session(tenant.clone());
    let mut turn = begin(&mediated, &tenant, &session, "scan").await;
    let mut budget = RunBudget::default();

    assert!(matches!(
        turn.mediate(
            calls(vec![
                call("scan", "scan", "{}"),
                call("early-accept", EXECUTE_REMEDY_PLAN, r#"{"plan_id":"remedy-0"}"#,),
            ]),
            &mut budget,
        )
        .await
        .unwrap(),
        Step::Continue
    ));
    let offered = facts(&mediated, &tenant, &session);
    assert!(tool_values(&offered).is_empty());
    assert!(!offered.iter().any(|fact| matches!(fact, Fact::DispatchClosed { .. })));
    assert!(offered.iter().any(|fact| matches!(
        fact,
        Fact::BlockFeedback { call_id, .. } if call_id.as_str() == "early-accept"
    )));

    assert!(matches!(
        turn.mediate(
            calls(vec![call("accept", EXECUTE_REMEDY_PLAN, r#"{"plan_id":"remedy-0"}"#,)]),
            &mut budget,
        )
        .await
        .unwrap(),
        Step::Continue
    ));
    let log = facts(&mediated, &tenant, &session);
    assert_eq!(
        tool_values(&log).iter().map(|(body, _)| *body).collect::<Vec<_>>(),
        ["mail body"]
    );
    assert_eq!(
        log.iter()
            .filter(|fact| matches!(fact, Fact::OutputCastAccepted { .. }))
            .count(),
        1
    );
    assert_eq!(
        log.iter()
            .filter(|fact| matches!(fact, Fact::DispatchClosed { .. }))
            .count(),
        1
    );

    let transcript = turn.transcript().unwrap();
    let scan_response = transcript
        .iter()
        .find(|message| message.tool_call_id.as_deref() == Some("scan"))
        .expect("the scan call has an offer response");
    assert_ne!(scan_response.content.as_deref(), Some("mail body"));
    let acceptance_response = transcript
        .iter()
        .find(|message| message.tool_call_id.as_deref() == Some("accept"))
        .expect("the acceptance call has the admitted result");
    assert_eq!(acceptance_response.content.as_deref(), Some("mail body"));

    assert!(matches!(
        turn.mediate(final_answer("done"), &mut budget).await.unwrap(),
        Step::Final(answer) if answer == "done"
    ));
    assert!(
        !facts(&mediated, &tenant, &session)
            .iter()
            .any(|fact| matches!(fact, Fact::OutputCastLapsed { .. }))
    );
}

/// Facts carrying the named committed effect — the checkpoint or a close, wherever it landed.
fn effect_carriers(log: &[Fact], kind: &str) -> usize {
    log.iter()
        .filter(|fact| match fact {
            Fact::DispatchSucceeded { effects, .. } => effects.iter().any(|effect| effect.as_str() == kind),
            Fact::DispatchClosed {
                outcome: CloseOutcome::Success { effects },
                ..
            } => effects.iter().any(|effect| effect.as_str() == kind),
            _ => false,
        })
        .count()
}

#[tokio::test]
async fn a_pending_cast_success_commits_effects_before_its_offer_resolves() {
    // The external effect happened the moment the tool succeeded; the success checkpoint commits
    // it immediately, so a later call's no_prior(read) in the SAME round sees it — the raw body
    // stays confined behind the offer. Acceptance later folds the value without a second effect.
    let mediated = mediator(
        r#"
version = 1
trust_chain = ["suspicious", "trusted"]
[[tool]]
name = "scan"
effects = ["read"]
delta = { trust = "unknown" }

[[tool]]
name = "audit"
delta = {}
[tool.requires]
effects = { has_no = ["read"] }

[[cast]]
name = "paranoid"
constant = { trust = "suspicious" }
"#,
        &[
            ("scan", BuiltinTool::Echo("mail body".to_string())),
            ("audit", BuiltinTool::Echo("must not run".to_string())),
        ],
    );
    let tenant = TenantId::new("tenant");
    let session = mediated.create_session(tenant.clone());
    let mut turn = begin(&mediated, &tenant, &session, "scan then audit").await;
    let mut budget = RunBudget::default();

    assert!(matches!(
        turn.mediate(
            calls(vec![call("scan", "scan", "{}"), call("audit", "audit", "{}")]),
            &mut budget,
        )
        .await
        .unwrap(),
        Step::Continue
    ));
    let log = facts(&mediated, &tenant, &session);
    // The checkpoint committed the effect while the dispatch stays open and the body confined...
    assert_eq!(effect_carriers(&log, "read"), 1);
    assert!(tool_values(&log).is_empty());
    // ...so the same-round audit call failed its no_prior(read) and never dispatched.
    assert_eq!(
        log.iter()
            .filter(|fact| matches!(fact, Fact::DispatchOpened { .. }))
            .count(),
        1
    );
    assert!(log.iter().any(|fact| matches!(
        fact,
        Fact::BlockFeedback { call_id, .. } if call_id.as_str() == "audit"
    )));

    assert!(matches!(
        turn.mediate(
            calls(vec![call("accept", EXECUTE_REMEDY_PLAN, r#"{"plan_id":"remedy-0"}"#)]),
            &mut budget,
        )
        .await
        .unwrap(),
        Step::Continue
    ));
    let log = facts(&mediated, &tenant, &session);
    assert_eq!(
        tool_values(&log).iter().map(|(body, _)| *body).collect::<Vec<_>>(),
        ["mail body"]
    );
    // The close contributed no duplicate: the one effect carrier is still the checkpoint.
    assert_eq!(effect_carriers(&log, "read"), 1);
}

#[tokio::test]
async fn a_lapsed_pending_cast_leaves_its_checkpointed_effects_standing_once() {
    let mediated = mediator(
        r#"
version = 1
trust_chain = ["suspicious", "trusted"]
[[tool]]
name = "scan"
effects = ["read"]
delta = { trust = "unknown" }

[[cast]]
name = "paranoid"
constant = { trust = "suspicious" }
"#,
        &[("scan", BuiltinTool::Echo("mail body".to_string()))],
    );
    let tenant = TenantId::new("tenant");
    let session = mediated.create_session(tenant.clone());
    let mut turn = begin(&mediated, &tenant, &session, "scan").await;
    let mut budget = RunBudget::default();

    turn.mediate(calls(vec![call("scan", "scan", "{}")]), &mut budget)
        .await
        .unwrap();
    assert!(matches!(
        turn.mediate(final_answer("done"), &mut budget).await.unwrap(),
        Step::Final(_)
    ));
    let log = facts(&mediated, &tenant, &session);
    assert_eq!(
        log.iter()
            .filter(|fact| matches!(fact, Fact::OutputCastLapsed { .. }))
            .count(),
        1
    );
    assert_eq!(effect_carriers(&log, "read"), 1);
    assert!(tool_values(&log).is_empty());
}

#[tokio::test]
async fn ordinary_narrowing_acceptance_requires_a_later_round() {
    // Informed acceptance holds for ordinary soft blocks as for pending casts: an acceptance
    // authored in the same assistant response that triggered the offer predates it and is
    // refused; the same offer executes in the next round.
    let mediated = mediator(
        r#"
version = 1
[[tool]]
name = "get"
delta = { audience = { exactly = ["internal"] } }
"#,
        &[("get", BuiltinTool::Echo("secret".to_string()))],
    );
    let tenant = TenantId::new("tenant");
    let session = mediated.create_session(tenant.clone());
    let mut turn = begin(&mediated, &tenant, &session, "fetch").await;
    let mut budget = RunBudget::default();

    assert!(matches!(
        turn.mediate(
            calls(vec![
                call("blocked", "get", "{}"),
                call("early-accept", EXECUTE_REMEDY_PLAN, r#"{"plan_id":"remedy-0"}"#),
            ]),
            &mut budget,
        )
        .await
        .unwrap(),
        Step::Continue
    ));
    let log = facts(&mediated, &tenant, &session);
    assert!(!log.iter().any(|fact| matches!(fact, Fact::DispatchOpened { .. })));
    assert!(!log.iter().any(|fact| matches!(fact, Fact::Acceptance { .. })));

    assert!(matches!(
        turn.mediate(
            calls(vec![call("accept", EXECUTE_REMEDY_PLAN, r#"{"plan_id":"remedy-0"}"#)]),
            &mut budget,
        )
        .await
        .unwrap(),
        Step::Continue
    ));
    let log = facts(&mediated, &tenant, &session);
    assert_eq!(
        log.iter()
            .filter(|fact| matches!(fact, Fact::Acceptance { .. }))
            .count(),
        1
    );
    assert_eq!(
        tool_values(&log).iter().map(|(body, _)| *body).collect::<Vec<_>>(),
        ["secret"]
    );
}

#[tokio::test]
async fn an_authority_only_plan_executes_in_its_offering_round() {
    // The round gate binds acceptances, not rulings: a plan with no Accept step is executable in
    // the very round that offered it.
    let mediated = mediator(
        r#"
version = 1
[[tool]]
name = "wire"
effects = ["spend"]
[tool.requires]
attention = ["signoff"]

[[authority]]
name = "officer"
mandate = { attends = ["signoff"] }
implementation = { builtin = "approve" }
"#,
        &[("wire", BuiltinTool::Echo("paid".to_string()))],
    );
    let tenant = TenantId::new("tenant");
    let session = mediated.create_session(tenant.clone());
    let mut turn = begin(&mediated, &tenant, &session, "pay").await;
    let mut budget = RunBudget::default();

    assert!(matches!(
        turn.mediate(
            calls(vec![
                call("blocked", "wire", "{}"),
                call("same-round", EXECUTE_REMEDY_PLAN, r#"{"plan_id":"remedy-0"}"#),
            ]),
            &mut budget,
        )
        .await
        .unwrap(),
        Step::Continue
    ));
    let log = facts(&mediated, &tenant, &session);
    assert_eq!(log.iter().filter(|fact| matches!(fact, Fact::Ruling { .. })).count(), 1);
    assert!(log.iter().any(|fact| matches!(
        fact,
        Fact::DispatchClosed {
            outcome: CloseOutcome::Success { effects },
            ..
        } if effects.iter().any(|effect| effect.as_str() == "spend")
    )));
}

#[tokio::test]
async fn return_offers_run_cheapest_first_and_the_free_crossing_leads_the_feedback() {
    // The menu a blocked return offers is ordered by what each plan costs the parent: a
    // residual-free sanitize crosses the value and narrows nothing, raw acceptance narrows
    // permanently. A child reads the menu as "how do I return this" and takes the first entry, so
    // the ordering is the affordance — and the prose names the free crossing rather than leading
    // with the void return, which is only the best move when no free crossing exists.
    let mediated = mediator(
        r#"
version = 1
[[tool]]
name = "read"
delta = { audience = { exactly = ["internal"] } }

[[sanitizer]]
name = "pii"
on = ["tool_output"]
[sanitizer.mandate]
audience = { from = { includes = ["internal"] }, to = { exactly = ["public"] } }
[sanitizer.implementation]
builtin = "redact-email"
"#,
        &[("read", BuiltinTool::Echo("ask eve@corp.com".to_string()))],
    );
    let tenant = TenantId::new("tenant");
    let parent = mediated.create_session(tenant.clone());
    let mut budget = RunBudget::default();
    let mut parent_turn = begin(&mediated, &tenant, &parent, "parent").await;
    parent_turn.mediate(final_answer("ready"), &mut budget).await.unwrap();
    drop(parent_turn);

    let child = mediated.fork_session(&tenant, &parent).unwrap();
    let session = child.clone();
    let mut turn = child_with_internal_read(&mediated, &tenant, &child, &mut budget).await;
    turn.mediate(
        calls(vec![call("submit", SUBMIT_RESULT, r#"{"value":"ask eve@corp.com"}"#)]),
        &mut budget,
    )
    .await
    .unwrap();

    let sanitize = offered_plan(&mediated, &tenant, &session, "submit", "derivation");
    let accept = offered_plan(&mediated, &tenant, &session, "submit", RAW_ACCEPT);
    assert!(
        sanitize < accept,
        "the free crossing must hold the lower handle: sanitize={sanitize} accept={accept}"
    );

    let content = facts(&mediated, &tenant, &session)
        .iter()
        .rev()
        .find_map(|fact| match fact {
            Fact::BlockFeedback { call_id, content, .. } if call_id.as_str() == "submit" => Some(content.clone()),
            _ => None,
        })
        .expect("the blocked return answers the submit");
    // The property, not the phrasing: the free crossing is named in the prose, ahead of the menu
    // every plan is listed in. A child that reads only the first sentences still sees it.
    let named_at = content
        .find(&format!("\"{sanitize}\""))
        .unwrap_or_else(|| panic!("the free crossing is never named: {content}"));
    let menu_at = content
        .find("Call execute_remedy_plan with plan_id")
        .unwrap_or_else(|| panic!("no menu in: {content}"));
    assert!(
        named_at < menu_at,
        "the free crossing must be named before the menu: {content}"
    );
    assert!(content.contains("submit_result null"), "{content}");
}

#[tokio::test]
async fn return_acceptance_is_round_gated_but_a_residual_free_sanitize_is_not() {
    let mediated = mediator(
        r#"
version = 1
[[tool]]
name = "read"
delta = { audience = { exactly = ["internal"] } }

[[sanitizer]]
name = "pii"
on = ["tool_output"]
[sanitizer.mandate]
audience = { from = { includes = ["internal"] }, to = { exactly = ["public"] } }
[sanitizer.implementation]
builtin = "redact-email"
"#,
        &[("read", BuiltinTool::Echo("ask eve@corp.com".to_string()))],
    );
    let tenant = TenantId::new("tenant");
    let parent = mediated.create_session(tenant.clone());
    let mut budget = RunBudget::default();
    let mut parent_turn = begin(&mediated, &tenant, &parent, "parent").await;
    parent_turn.mediate(final_answer("ready"), &mut budget).await.unwrap();
    drop(parent_turn);

    let accepting_child = mediated.fork_session(&tenant, &parent).unwrap();
    let mut accepting_turn = child_with_internal_read(&mediated, &tenant, &accepting_child, &mut budget).await;
    assert!(matches!(
        accepting_turn
            .mediate(
                calls(vec![
                    call("submit", SUBMIT_RESULT, r#"{"value":"ask eve@corp.com"}"#),
                    call("early-accept", EXECUTE_REMEDY_PLAN, r#"{"plan_id":"remedy-3"}"#),
                ]),
                &mut budget,
            )
            .await
            .unwrap(),
        Step::Continue
    ));
    assert!(
        !facts(&mediated, &tenant, &parent)
            .iter()
            .any(|fact| matches!(fact, Fact::ChildReturn { .. }))
    );
    assert!(matches!(
        accepting_turn
            .mediate(
                calls(vec![call(
                    "accept-return",
                    EXECUTE_REMEDY_PLAN,
                    r#"{"plan_id":"remedy-3"}"#
                )]),
                &mut budget,
            )
            .await
            .unwrap(),
        Step::ChildFinished
    ));
    drop(accepting_turn);

    let sanitizing_child = mediated.fork_session(&tenant, &parent).unwrap();
    let mut sanitizing_turn = child_with_internal_read(&mediated, &tenant, &sanitizing_child, &mut budget).await;
    assert!(matches!(
        sanitizing_turn
            .mediate(
                calls(vec![
                    call("submit", SUBMIT_RESULT, r#"{"value":"ask eve@corp.com"}"#),
                    call("same-round-sanitize", EXECUTE_REMEDY_PLAN, r#"{"plan_id":"remedy-2"}"#),
                ]),
                &mut budget,
            )
            .await
            .unwrap(),
        Step::ChildFinished
    ));

    let log = facts(&mediated, &tenant, &parent);
    let returned: Vec<_> = log
        .iter()
        .filter_map(|fact| match fact {
            Fact::ValueAdmitted {
                value,
                provenance: Provenance::ChildReturn { .. },
                ..
            } => Some(value.body.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(returned, ["ask eve@corp.com", "ask [redacted-email]"]);
}

#[tokio::test]
async fn every_unaccepted_pending_cast_lapses_before_the_turn_boundary() {
    let mediated = mediator(
        r#"
version = 1
trust_chain = ["suspicious", "trusted"]
[[tool]]
name = "scan"
effects = ["read"]
delta = { trust = "unknown" }

[[cast]]
name = "paranoid"
constant = { trust = "suspicious" }
"#,
        &[("scan", BuiltinTool::Echo("mail body".to_string()))],
    );
    let tenant = TenantId::new("tenant");
    let session = mediated.create_session(tenant.clone());
    let mut turn = begin(&mediated, &tenant, &session, "scan twice").await;
    let mut budget = RunBudget::default();

    assert!(matches!(
        turn.mediate(
            calls(vec![
                call("first", "scan", "{}"),
                call("second", "scan", r#"{"again":true}"#)
            ]),
            &mut budget,
        )
        .await
        .unwrap(),
        Step::Continue
    ));
    assert!(matches!(
        turn.mediate(final_answer("done"), &mut budget).await.unwrap(),
        Step::Final(_)
    ));

    let log = facts(&mediated, &tenant, &session);
    assert!(tool_values(&log).is_empty());
    assert_eq!(
        log.iter()
            .filter(|fact| matches!(fact, Fact::OutputCastLapsed { .. }))
            .count(),
        2
    );
    assert_eq!(
        log.iter()
            .filter(|fact| matches!(
                fact,
                Fact::DispatchClosed {
                    outcome: CloseOutcome::Success { .. },
                    ..
                }
            ))
            .count(),
        2
    );
    let turn_end = log
        .iter()
        .position(|fact| {
            matches!(
                fact,
                Fact::Boundary {
                    kind: BoundaryKind::TurnEnd,
                    ..
                }
            )
        })
        .expect("the turn ends");
    assert!(
        log[..turn_end]
            .iter()
            .filter(|fact| matches!(fact, Fact::OutputCastLapsed { .. }))
            .count()
            == 2
    );
}

#[tokio::test]
async fn mixed_fork_rounds_have_no_side_effects_and_requests_are_turn_bound() {
    let mediated = mediator(
        r#"
version = 1
[[tool]]
name = "write"
effects = ["mutation"]
delta = {}
"#,
        &[("write", BuiltinTool::Echo("written".to_string()))],
    );
    let tenant = TenantId::new("tenant");
    let mixed_session = mediated.create_session(tenant.clone());
    let mut mixed = begin(&mediated, &tenant, &mixed_session, "delegate").await;
    let mut budget = RunBudget::default();
    assert!(matches!(
        mixed
            .mediate(
                calls(vec![
                    call("fork", FORK, r#"{"task":"inspect"}"#),
                    call("write", "write", "{}"),
                ]),
                &mut budget,
            )
            .await
            .unwrap(),
        Step::Continue
    ));
    let log = facts(&mediated, &tenant, &mixed_session);
    assert!(!log.iter().any(|fact| matches!(fact, Fact::DispatchOpened { .. })));
    let responses: Vec<_> = log
        .iter()
        .filter_map(|fact| match fact {
            Fact::BlockFeedback { call_id, .. } => Some(call_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(responses, ["fork", "write"]);
    mixed.stop(StopReason::Cancelled).unwrap();

    let empty = mediator("version = 1\n", &[]);
    let first_session = empty.create_session(tenant.clone());
    let second_session = empty.create_session(tenant.clone());
    let mut first = begin(&empty, &tenant, &first_session, "first").await;
    let mut second = begin(&empty, &tenant, &second_session, "second").await;
    let Step::Fork(first_request) = first
        .mediate(calls(vec![call("f1", FORK, r#"{"task":"task one"}"#)]), &mut budget)
        .await
        .unwrap()
    else {
        panic!("first fork expected")
    };
    let Step::Fork(second_request) = second
        .mediate(calls(vec![call("f2", FORK, r#"{"task":"task two"}"#)]), &mut budget)
        .await
        .unwrap()
    else {
        panic!("second fork expected")
    };
    assert_eq!(first_request.task(), "task one");
    assert!(matches!(first.transcript(), Err(TurnError::Lifecycle { .. })));
    let no_child = empty.create_session(tenant.clone());
    assert!(matches!(
        first.complete_fork(second_request, no_child.clone()),
        Err(TurnError::ForkIdentity)
    ));
    assert!(matches!(
        first.complete_fork(first_request, no_child).unwrap(),
        Step::Continue
    ));
    assert!(matches!(
        first.mediate(final_answer("done"), &mut budget).await.unwrap(),
        Step::Final(_)
    ));
    drop(second);
}

#[tokio::test]
async fn a_fork_request_cannot_complete_a_later_turn_on_the_same_trajectory() {
    let mediator = mediator("version = 1\n", &[]);
    let tenant = TenantId::new("tenant");
    let session = mediator.create_session(tenant.clone());
    let mut budget = RunBudget::default();

    let mut first = begin(&mediator, &tenant, &session, "first turn").await;
    let Step::Fork(stale) = first
        .mediate(calls(vec![call("fork", FORK, r#"{"task":"first"}"#)]), &mut budget)
        .await
        .unwrap()
    else {
        panic!("first fork expected")
    };
    drop(first);

    let mut second = begin(&mediator, &tenant, &session, "second turn").await;
    let Step::Fork(current) = second
        .mediate(calls(vec![call("fork", FORK, r#"{"task":"second"}"#)]), &mut budget)
        .await
        .unwrap()
    else {
        panic!("second fork expected")
    };

    let no_child = mediator.create_session(tenant.clone());
    assert!(matches!(
        second.complete_fork(stale, no_child.clone()),
        Err(TurnError::ForkIdentity)
    ));
    assert!(matches!(
        second.complete_fork(current, no_child).unwrap(),
        Step::Continue
    ));
    second.stop(StopReason::Cancelled).unwrap();
}

#[tokio::test]
async fn child_context_stops_at_the_fork_and_raw_returns_cross_once() {
    let mediator = mediator("version = 1\n", &[]);
    let tenant = TenantId::new("tenant");
    let parent = mediator.create_session(tenant.clone());
    let mut budget = RunBudget::default();

    let mut first = begin(&mediator, &tenant, &parent, "root task").await;
    first.mediate(final_answer("prior answer"), &mut budget).await.unwrap();
    drop(first);

    let mut parent_turn = begin(&mediator, &tenant, &parent, "delegate now").await;
    let Step::Fork(request) = parent_turn
        .mediate(calls(vec![call("fork", FORK, r#"{"task":"inspect"}"#)]), &mut budget)
        .await
        .unwrap()
    else {
        panic!("fork expected")
    };
    budget.charge_fork().unwrap();
    let child = mediator.fork_session(&tenant, &parent).unwrap();
    let mut child_turn = begin(&mediator, &tenant, &child, request.task()).await;
    assert_eq!(child_turn.depth(), 1);
    assert!(child_turn.is_child());
    assert_eq!(
        child_turn.transcript().unwrap(),
        [
            WireMessage::user("root task"),
            WireMessage::assistant("prior answer"),
            WireMessage::user("delegate now"),
            WireMessage::user("inspect"),
        ]
    );

    assert!(matches!(
        child_turn
            .mediate(
                calls(vec![call("return", SUBMIT_RESULT, r#"{"value":"finding"}"#,)]),
                &mut budget,
            )
            .await
            .unwrap(),
        Step::ChildFinished
    ));
    assert!(matches!(child_turn.transcript(), Err(TurnError::Lifecycle { .. })));
    drop(child_turn);
    parent_turn.complete_fork(request, child.clone()).unwrap();
    let parent_context = parent_turn.transcript().unwrap();
    assert!(parent_context.contains(&WireMessage::user("finding")));
    assert!(!parent_context.contains(&WireMessage::assistant("child free text")));

    let (log_before, revision_before) = mediator.snapshot(&tenant, &child).unwrap();
    assert!(matches!(
        mediator
            .begin_turn(tenant.clone(), child.clone(), "return again", CancellationToken::new())
            .await,
        Err(appa_runtime::BeginTurnError::SessionReturned)
    ));
    let (log_after, revision_after) = mediator.snapshot(&tenant, &child).unwrap();
    assert_eq!(log_before.len(), log_after.len());
    assert_eq!(revision_before, revision_after);

    assert!(mediator.fork_session(&tenant, &child).is_err());
    assert!(mediator.fork_session_reserved(&tenant, &child).is_err());

    let log = facts(&mediator, &tenant, &parent);
    assert_eq!(
        log.iter()
            .filter(|fact| matches!(fact, Fact::ChildReturn { .. }))
            .count(),
        1
    );
    assert_eq!(
        log.iter()
            .filter(|fact| matches!(
                fact,
                Fact::Boundary {
                    kind: BoundaryKind::Merge { .. },
                    ..
                }
            ))
            .count(),
        1
    );
    parent_turn.stop(StopReason::Cancelled).unwrap();
}

#[tokio::test]
async fn the_join_signal_distinguishes_a_crossed_value_from_every_non_crossing_ending() {
    #[derive(Clone, Copy)]
    enum Ending {
        Crossed,
        Void,
        Prose,
    }

    async fn join_feedback(ending: Ending) -> String {
        let mediator = mediator("version = 1\n", &[]);
        let tenant = TenantId::new("tenant");
        let parent = mediator.create_session(tenant.clone());
        let mut budget = RunBudget::default();

        let mut parent_turn = begin(&mediator, &tenant, &parent, "delegate now").await;
        let Step::Fork(request) = parent_turn
            .mediate(calls(vec![call("fork", FORK, r#"{"task":"errand"}"#)]), &mut budget)
            .await
            .unwrap()
        else {
            panic!("fork expected")
        };
        budget.charge_fork().unwrap();
        let child = mediator.fork_session(&tenant, &parent).unwrap();
        let mut child_turn = begin(&mediator, &tenant, &child, request.task()).await;

        let completion = match ending {
            Ending::Crossed => calls(vec![call("return", SUBMIT_RESULT, r#"{"value":"finding"}"#)]),
            Ending::Void => calls(vec![call("return", SUBMIT_RESULT, r#"{"value":null}"#)]),
            Ending::Prose => final_answer("child-only conclusion"),
        };
        child_turn.mediate(completion, &mut budget).await.unwrap();
        drop(child_turn);
        parent_turn.complete_fork(request, child).unwrap();

        facts(&mediator, &tenant, &parent)
            .iter()
            .find_map(|fact| match fact {
                Fact::BlockFeedback { call_id, content, .. } if call_id.as_str() == "fork" => Some(content.clone()),
                _ => None,
            })
            .expect("the join answers the fork call")
    }

    let crossed = join_feedback(Ending::Crossed).await;
    let void = join_feedback(Ending::Void).await;
    let prose = join_feedback(Ending::Prose).await;

    assert_ne!(crossed, void);
    assert_eq!(void, prose);
}

#[tokio::test]
async fn child_free_final_prose_finishes_without_crossing_to_the_parent() {
    let mediator = mediator("version = 1\n", &[]);
    let tenant = TenantId::new("tenant");
    let parent = mediator.create_session(tenant.clone());
    let forked = mediator.fork_session_reserved(&tenant, &parent).unwrap();
    let child = forked.session().clone();
    let mut turn = mediator
        .begin_forked_turn(tenant.clone(), forked, "inspect", CancellationToken::new())
        .unwrap();
    let mut budget = RunBudget::default();

    assert!(matches!(
        turn.mediate(final_answer("child-only conclusion"), &mut budget)
            .await
            .unwrap(),
        Step::ChildFinished
    ));
    let log = facts(&mediator, &tenant, &parent);
    assert_eq!(
        log.iter()
            .filter(|fact| matches!(
                fact,
                Fact::AssistantMessage {
                    trajectory,
                    content: Some(_),
                    calls,
                } if trajectory == &child && calls.is_empty()
            ))
            .count(),
        1
    );
    assert!(appa_runtime::transcript::model_transcript(&[], &log, &parent).is_empty());
}

#[tokio::test]
async fn sanitized_and_void_child_returns_have_closed_crossing_semantics() {
    let sanitized = mediator(
        r#"
version = 1
[[sanitizer]]
name = "pii"
on = ["tool_output"]
[sanitizer.mandate]
audience = { from = { includes = ["internal"] }, to = { exactly = ["public"] } }
[sanitizer.implementation]
builtin = "redact-email"
[child]
return_sanitizer = "pii"
"#,
        &[],
    );
    let tenant = TenantId::new("tenant");
    let parent = sanitized.create_session(tenant.clone());
    let child = sanitized.fork_session(&tenant, &parent).unwrap();
    let mut child_turn = begin(&sanitized, &tenant, &child, "inspect").await;
    let mut budget = RunBudget::default();
    assert!(matches!(
        child_turn
            .mediate(
                calls(vec![call("return", SUBMIT_RESULT, r#"{"value":"ask eve@corp.com"}"#,)]),
                &mut budget,
            )
            .await
            .unwrap(),
        Step::ChildFinished
    ));
    let log = facts(&sanitized, &tenant, &parent);
    let returned: Vec<_> = log
        .iter()
        .filter_map(|fact| match fact {
            Fact::ValueAdmitted {
                value,
                provenance: Provenance::ChildReturn { .. },
                ..
            } => Some(value),
            _ => None,
        })
        .collect();
    assert_eq!(returned.len(), 1);
    assert_eq!(returned[0].body.as_str(), "ask [redacted-email]");
    assert_eq!(returned[0].label.audience, Dim::Known(Audience::Public));
    assert!(log.iter().any(|fact| matches!(
        fact,
        Fact::ChildReturn {
            derivation: appa_engine::fact::ReturnDerivation::Sanitized { raw_digest, .. },
            ..
        } if raw_digest == &RawResultDigest::of(b"ask eve@corp.com")
    )));
    let parent_context = appa_runtime::transcript::model_transcript(&[], &log, &parent);
    assert!(parent_context.contains(&WireMessage::user("ask [redacted-email]")));
    assert!(!parent_context.contains(&WireMessage::user("ask eve@corp.com")));

    let raw = mediator(
        "version = 1\n[[tool]]\nname = \"after\"\ndelta = {}\n",
        &[("after", BuiltinTool::Echo("must not run".to_string()))],
    );
    let parent = raw.create_session(tenant.clone());
    let child = raw.fork_session(&tenant, &parent).unwrap();
    let mut child_turn = begin(&raw, &tenant, &child, "inspect").await;
    assert!(matches!(
        child_turn
            .mediate(
                calls(vec![
                    call("void", SUBMIT_RESULT, r#"{"value":null}"#),
                    call("after", "after", "{}"),
                ]),
                &mut budget,
            )
            .await
            .unwrap(),
        Step::ChildFinished
    ));
    let log = facts(&raw, &tenant, &parent);
    assert!(!log.iter().any(|fact| matches!(fact, Fact::ChildReturn { .. })));
    assert!(!log.iter().any(|fact| matches!(fact, Fact::DispatchOpened { .. })));
    assert_eq!(
        log.iter()
            .filter(|fact| matches!(fact, Fact::BlockFeedback { .. }))
            .count(),
        2
    );
    assert!(!log.iter().any(|fact| matches!(
        fact,
        Fact::ValueAdmitted {
            provenance: Provenance::ChildReturn { .. },
            ..
        }
    )));
    assert!(!log.iter().any(|fact| matches!(
        fact,
        Fact::Boundary {
            kind: BoundaryKind::Merge { .. },
            ..
        }
    )));
}

#[tokio::test]
async fn a_residual_bearing_sanitize_return_plan_is_round_gated() {
    let mediated = mediator(
        r#"
version = 1
trust_chain = ["suspicious", "trusted"]
[[tool]]
name = "read"
delta = { trust = "suspicious", audience = { exactly = ["internal"] } }

[[sanitizer]]
name = "pii"
on = ["tool_output"]
[sanitizer.mandate]
audience = { from = { includes = ["internal"] }, to = { exactly = ["public"] } }
[sanitizer.implementation]
builtin = "redact-email"
"#,
        &[("read", BuiltinTool::Echo("ask eve@corp.com".to_string()))],
    );
    let tenant = TenantId::new("tenant");
    let parent = mediated.create_session(tenant.clone());
    let mut budget = RunBudget::default();
    let mut parent_turn = begin(&mediated, &tenant, &parent, "parent").await;
    parent_turn.mediate(final_answer("ready"), &mut budget).await.unwrap();
    drop(parent_turn);

    let child = mediated.fork_session(&tenant, &parent).unwrap();
    let mut turn = child_with_internal_read(&mediated, &tenant, &child, &mut budget).await;
    assert!(matches!(
        turn.mediate(
            calls(vec![
                call("submit", SUBMIT_RESULT, r#"{"value":"ask eve@corp.com"}"#),
                call("early-sanitize", EXECUTE_REMEDY_PLAN, r#"{"plan_id":"remedy-2"}"#),
            ]),
            &mut budget,
        )
        .await
        .unwrap(),
        Step::Continue
    ));
    assert!(
        !facts(&mediated, &tenant, &parent)
            .iter()
            .any(|fact| matches!(fact, Fact::ChildReturn { .. }))
    );

    assert!(matches!(
        turn.mediate(
            calls(vec![call(
                "sanitize-return",
                EXECUTE_REMEDY_PLAN,
                r#"{"plan_id":"remedy-2"}"#
            )]),
            &mut budget,
        )
        .await
        .unwrap(),
        Step::ChildFinished
    ));
    let log = facts(&mediated, &tenant, &parent);
    assert_eq!(
        log.iter()
            .filter(|fact| matches!(fact, Fact::ChildReturnAcceptance { .. }))
            .count(),
        1
    );
    assert!(log.iter().any(|fact| matches!(
        fact,
        Fact::ValueAdmitted {
            value,
            provenance: Provenance::ChildReturn { .. },
            ..
        } if value.body.as_str() == "ask [redacted-email]"
    )));
}

#[tokio::test]
async fn a_voided_child_is_re_drivable_and_may_still_cross_its_one_value() {
    let mediated = mediator("version = 1\n", &[]);
    let tenant = TenantId::new("tenant");
    let parent = mediated.create_session(tenant.clone());
    let mut budget = RunBudget::default();
    let mut parent_turn = begin(&mediated, &tenant, &parent, "parent").await;
    parent_turn.mediate(final_answer("ready"), &mut budget).await.unwrap();
    drop(parent_turn);

    let child = mediated.fork_session(&tenant, &parent).unwrap();
    let mut first = begin(&mediated, &tenant, &child, "look").await;
    assert!(matches!(
        first
            .mediate(
                calls(vec![call("void", SUBMIT_RESULT, r#"{"value":null}"#)]),
                &mut budget
            )
            .await
            .unwrap(),
        Step::ChildFinished
    ));
    drop(first);

    let mut second = begin(&mediated, &tenant, &child, "look again").await;
    assert!(matches!(
        second
            .mediate(
                calls(vec![call("return", SUBMIT_RESULT, r#"{"value":"finding"}"#)]),
                &mut budget,
            )
            .await
            .unwrap(),
        Step::ChildFinished
    ));
    let log = facts(&mediated, &tenant, &parent);
    assert_eq!(
        log.iter()
            .filter(|fact| matches!(fact, Fact::ChildReturn { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn narrowing_return_plans_cross_only_the_selected_raw_or_sanitized_value() {
    let mediator = mediator(
        r#"
version = 1
[[tool]]
name = "read"
delta = { audience = { exactly = ["internal"] } }

[[sanitizer]]
name = "pii"
on = ["tool_output"]
[sanitizer.mandate]
audience = { from = { includes = ["internal"] }, to = { exactly = ["public"] } }
[sanitizer.implementation]
builtin = "redact-email"
"#,
        &[("read", BuiltinTool::Echo("ask eve@corp.com".to_string()))],
    );
    let tenant = TenantId::new("tenant");
    let parent = mediator.create_session(tenant.clone());
    let mut budget = RunBudget::default();
    let mut parent_turn = begin(&mediator, &tenant, &parent, "parent").await;
    parent_turn.mediate(final_answer("ready"), &mut budget).await.unwrap();
    drop(parent_turn);

    let sanitized_child = mediator.fork_session(&tenant, &parent).unwrap();
    let mut sanitized_turn = begin(&mediator, &tenant, &sanitized_child, "read").await;
    sanitized_turn
        .mediate(calls(vec![call("read", "read", "{}")]), &mut budget)
        .await
        .unwrap();
    sanitized_turn
        .mediate(
            calls(vec![call(
                "accept-read",
                EXECUTE_REMEDY_PLAN,
                r#"{"plan_id":"remedy-0"}"#,
            )]),
            &mut budget,
        )
        .await
        .unwrap();
    sanitized_turn
        .mediate(
            calls(vec![call("submit", SUBMIT_RESULT, r#"{"value":"ask eve@corp.com"}"#)]),
            &mut budget,
        )
        .await
        .unwrap();
    let sanitize = offered_plan(&mediator, &tenant, &sanitized_child, "submit", "derivation");
    assert!(matches!(
        sanitized_turn
            .mediate(
                calls(vec![call(
                    "sanitize-return",
                    EXECUTE_REMEDY_PLAN,
                    &remedy_args(&sanitize)
                )]),
                &mut budget,
            )
            .await
            .unwrap(),
        Step::ChildFinished
    ));
    drop(sanitized_turn);

    let raw_child = mediator.fork_session(&tenant, &parent).unwrap();
    let mut raw_turn = begin(&mediator, &tenant, &raw_child, "read").await;
    raw_turn
        .mediate(calls(vec![call("read", "read", "{}")]), &mut budget)
        .await
        .unwrap();
    raw_turn
        .mediate(
            calls(vec![call(
                "accept-read",
                EXECUTE_REMEDY_PLAN,
                r#"{"plan_id":"remedy-0"}"#,
            )]),
            &mut budget,
        )
        .await
        .unwrap();
    raw_turn
        .mediate(
            calls(vec![call("submit", SUBMIT_RESULT, r#"{"value":"ask eve@corp.com"}"#)]),
            &mut budget,
        )
        .await
        .unwrap();
    let accept = offered_plan(&mediator, &tenant, &raw_child, "submit", RAW_ACCEPT);
    assert!(matches!(
        raw_turn
            .mediate(
                calls(vec![call("accept-return", EXECUTE_REMEDY_PLAN, &remedy_args(&accept))]),
                &mut budget,
            )
            .await
            .unwrap(),
        Step::ChildFinished
    ));

    let log = facts(&mediator, &tenant, &parent);
    let returned: Vec<_> = log
        .iter()
        .filter_map(|fact| match fact {
            Fact::ValueAdmitted {
                value,
                provenance: Provenance::ChildReturn { .. },
                ..
            } => Some(value.body.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(returned, ["ask [redacted-email]", "ask eve@corp.com"]);
    assert_eq!(
        log.iter()
            .filter(|fact| matches!(fact, Fact::ChildReturnAcceptance { .. }))
            .count(),
        1
    );
    assert_eq!(
        log.iter()
            .filter(|fact| matches!(
                fact,
                Fact::ChildReturn {
                    derivation: appa_engine::fact::ReturnDerivation::Sanitized { .. },
                    ..
                }
            ))
            .count(),
        1
    );
}

#[tokio::test]
async fn stale_sibling_child_return_offer_cannot_cross_or_replay() {
    let mediator = mediator(
        r#"
version = 1
[[tool]]
name = "read"
delta = { audience = { exactly = ["internal"] } }

[[sanitizer]]
name = "pii"
on = ["tool_output"]
[sanitizer.mandate]
audience = { from = { includes = ["internal"] }, to = { exactly = ["public"] } }
[sanitizer.implementation]
builtin = "redact-email"
"#,
        &[("read", BuiltinTool::Echo("internal".to_string()))],
    );
    let tenant = TenantId::new("tenant");
    let mut budget = RunBudget::default();
    let parent = parent_with_completed_turn(&mediator, &tenant, &mut budget).await;

    let mut first = child_with_internal_read(&mediator, &tenant, &parent, &mut budget).await;
    assert!(matches!(
        first
            .mediate(
                calls(vec![call("first-offer", SUBMIT_RESULT, r#"{"value":"first"}"#)]),
                &mut budget,
            )
            .await
            .unwrap(),
        Step::Continue
    ));

    let mut sibling = child_with_internal_read(&mediator, &tenant, &parent, &mut budget).await;
    assert!(matches!(
        sibling
            .mediate(
                calls(vec![call("sibling-offer", SUBMIT_RESULT, r#"{"value":"second"}"#,)]),
                &mut budget,
            )
            .await
            .unwrap(),
        Step::Continue
    ));
    assert!(matches!(
        sibling
            .mediate(
                calls(vec![call(
                    "sibling-cross",
                    EXECUTE_REMEDY_PLAN,
                    r#"{"plan_id":"remedy-3"}"#,
                )]),
                &mut budget,
            )
            .await
            .unwrap(),
        Step::ChildFinished
    ));

    assert!(matches!(
        first
            .mediate(
                calls(vec![call(
                    "stale-offer",
                    EXECUTE_REMEDY_PLAN,
                    r#"{"plan_id":"remedy-3"}"#,
                )]),
                &mut budget,
            )
            .await
            .unwrap(),
        Step::Continue
    ));
    assert!(matches!(
        first
            .mediate(
                calls(vec![call(
                    "replayed-offer",
                    EXECUTE_REMEDY_PLAN,
                    r#"{"plan_id":"remedy-3"}"#,
                )]),
                &mut budget,
            )
            .await
            .unwrap(),
        Step::Continue
    ));
    first.stop(StopReason::Cancelled).unwrap();

    let log = facts(&mediator, &tenant, &parent);
    let crossings: Vec<_> = log
        .iter()
        .filter_map(|fact| match fact {
            Fact::ChildReturn { value, .. } => Some(value),
            _ => None,
        })
        .collect();
    assert_eq!(crossings.len(), 1);
    assert_eq!(crossings[0].body.as_str(), "second");
    assert_eq!(
        crossings[0].label.audience,
        Dim::Known(Audience::restricted([ReaderId::new("internal")]))
    );
    assert_eq!(
        log.iter()
            .filter(|fact| matches!(
                fact,
                Fact::Boundary {
                    kind: BoundaryKind::Merge { .. },
                    ..
                }
            ))
            .count(),
        1
    );
    for call_id in ["stale-offer", "replayed-offer"] {
        assert_eq!(
            log.iter()
                .filter(|fact| matches!(
                    fact,
                    Fact::BlockFeedback { call_id: answered, .. } if answered.as_str() == call_id
                ))
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn failed_return_sanitizers_are_charged_per_raw_digest_and_sanitizer() {
    let (alpha_url, alpha_requests, alpha_server) = spawn_repeating_response("not json").await;
    let (beta_url, beta_requests, beta_server) = spawn_repeating_response("not json").await;
    let policy = format!(
        r#"
version = 1
[[tool]]
name = "read"
delta = {{ audience = {{ exactly = ["internal"] }} }}

[[sanitizer]]
name = "alpha"
on = ["tool_output"]
[sanitizer.mandate]
audience = {{ from = {{ includes = ["internal"] }}, to = {{ exactly = ["public"] }} }}
[sanitizer.implementation]
resolver = {{ url = "{alpha_url}", timeout_ms = 1000 }}

[[sanitizer]]
name = "beta"
on = ["tool_output"]
[sanitizer.mandate]
audience = {{ from = {{ includes = ["internal"] }}, to = {{ exactly = ["public"] }} }}
[sanitizer.implementation]
resolver = {{ url = "{beta_url}", timeout_ms = 1000 }}
"#
    );
    let mediator = mediator(&policy, &[("read", BuiltinTool::Echo("internal".to_string()))]);
    let tenant = TenantId::new("tenant");
    let mut budget = RunBudget::new(Limits {
        max_blocked_proposals_per_call: 2,
        ..Limits::default()
    });
    let parent = parent_with_completed_turn(&mediator, &tenant, &mut budget).await;
    let mut child = child_with_internal_read(&mediator, &tenant, &parent, &mut budget).await;

    assert!(matches!(
        child
            .mediate(
                calls(vec![call("first-offer", SUBMIT_RESULT, r#"{"value":"first"}"#)]),
                &mut budget,
            )
            .await
            .unwrap(),
        Step::Continue
    ));
    for call_id in ["alpha-1", "alpha-2", "alpha-over-limit"] {
        assert!(matches!(
            child
                .mediate(
                    calls(vec![call(call_id, EXECUTE_REMEDY_PLAN, r#"{"plan_id":"remedy-3"}"#,)]),
                    &mut budget,
                )
                .await
                .unwrap(),
            Step::Continue
        ));
    }
    assert_eq!(alpha_requests.load(Ordering::SeqCst), 2);

    assert!(matches!(
        child
            .mediate(
                calls(vec![call("beta-1", EXECUTE_REMEDY_PLAN, r#"{"plan_id":"remedy-4"}"#,)]),
                &mut budget,
            )
            .await
            .unwrap(),
        Step::Continue
    ));
    assert_eq!(beta_requests.load(Ordering::SeqCst), 1);

    assert!(matches!(
        child
            .mediate(
                calls(vec![call("second-offer", SUBMIT_RESULT, r#"{"value":"second"}"#)]),
                &mut budget,
            )
            .await
            .unwrap(),
        Step::Continue
    ));
    assert!(matches!(
        child
            .mediate(
                calls(vec![call(
                    "second-alpha-1",
                    EXECUTE_REMEDY_PLAN,
                    r#"{"plan_id":"remedy-6"}"#,
                )]),
                &mut budget,
            )
            .await
            .unwrap(),
        Step::Continue
    ));
    assert_eq!(alpha_requests.load(Ordering::SeqCst), 3);

    assert!(matches!(
        child
            .mediate(
                calls(vec![call(
                    "accept-raw",
                    EXECUTE_REMEDY_PLAN,
                    r#"{"plan_id":"remedy-5"}"#,
                )]),
                &mut budget,
            )
            .await
            .unwrap(),
        Step::ChildFinished
    ));
    alpha_server.abort();
    beta_server.abort();

    let log = facts(&mediator, &tenant, &parent);
    for call_id in [
        "alpha-1",
        "alpha-2",
        "alpha-over-limit",
        "beta-1",
        "second-alpha-1",
        "accept-raw",
    ] {
        assert_eq!(
            log.iter()
                .filter(|fact| matches!(
                    fact,
                    Fact::BlockFeedback { call_id: answered, .. } if answered.as_str() == call_id
                ))
                .count(),
            1
        );
    }
    let returned: Vec<_> = log
        .iter()
        .filter_map(|fact| match fact {
            Fact::ValueAdmitted {
                value,
                provenance: Provenance::ChildReturn { .. },
                ..
            } => Some(value),
            _ => None,
        })
        .collect();
    assert_eq!(returned.len(), 1);
    assert_eq!(returned[0].body.as_str(), "first");
    assert_eq!(
        returned[0].label.audience,
        Dim::Known(Audience::restricted([ReaderId::new("internal")]))
    );
    assert_eq!(
        log.iter()
            .filter(|fact| matches!(fact, Fact::ChildReturnAcceptance { .. }))
            .count(),
        1
    );
    assert!(!log.iter().any(|fact| matches!(
        fact,
        Fact::ChildReturn {
            derivation: appa_engine::fact::ReturnDerivation::Sanitized { .. },
            ..
        }
    )));
}

#[tokio::test]
async fn hostile_cast_resolver_answer_above_may_cast_is_discarded() {
    let (resolver_url, requests, server) = spawn_repeating_response(r#"{"trust":"trusted"}"#).await;
    let policy = format!(
        r#"
version = 1
trust_chain = ["suspicious", "trusted"]

[[tool]]
name = "scan"
effects = ["read"]
delta = {{ trust = "unknown" }}

[[cast]]
name = "classifier"
resolver = {{ url = "{resolver_url}", may_cast = {{ trust = ["suspicious"] }} }}
"#
    );
    let mediator = mediator(&policy, &[("scan", BuiltinTool::Echo("mailbox".to_string()))]);
    let tenant = TenantId::new("tenant");
    let session = mediator.create_session(tenant.clone());
    let mut turn = begin(&mediator, &tenant, &session, "scan").await;
    let mut budget = RunBudget::default();

    assert!(matches!(
        turn.mediate(calls(vec![call("scan-call", "scan", "{}")]), &mut budget)
            .await
            .unwrap(),
        Step::Continue
    ));
    server.abort();
    assert_eq!(requests.load(Ordering::SeqCst), 1);

    let log = facts(&mediator, &tenant, &session);
    assert!(log.iter().any(|fact| matches!(
        fact,
        Fact::DispatchClosed {
            outcome: CloseOutcome::Success { effects },
            ..
        } if effects.len() == 1 && effects[0].as_str() == "read"
    )));
    assert!(
        !log.iter()
            .any(|fact| matches!(fact, Fact::CastApplied { .. } | Fact::OutputCastApplied { .. }))
    );
    assert!(tool_values(&log).is_empty());
    assert_eq!(
        log.iter()
            .filter(|fact| matches!(
                fact,
                Fact::BlockFeedback { call_id, .. } if call_id.as_str() == "scan-call"
            ))
            .count(),
        1
    );
    turn.stop(StopReason::Cancelled).unwrap();
}

#[tokio::test]
async fn cancellation_mid_round_closes_open_call_and_answers_all_remaining_calls() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        accepted_tx.send(()).ok();
        let _socket = socket;
        tokio::time::sleep(Duration::from_secs(30)).await;
    });
    let policy = format!(
        r#"
version = 1
[[tool]]
name = "slow"
[tool.implementation.http]
url = "http://{address}/run"

[[tool]]
name = "fast-a"
effects = ["later.a"]
delta = {{}}

[[tool]]
name = "fast-b"
effects = ["later.b"]
delta = {{}}
"#
    );
    let mediator = mediator(
        &policy,
        &[
            ("fast-a", BuiltinTool::Echo("a".to_string())),
            ("fast-b", BuiltinTool::Echo("b".to_string())),
        ],
    );
    let tenant = TenantId::new("tenant");
    let session = mediator.create_session(tenant.clone());
    let token = CancellationToken::new();
    let cancel = token.clone();
    tokio::spawn(async move {
        accepted_rx.await.ok();
        cancel.cancel();
    });
    let mut turn = mediator
        .begin_turn(tenant.clone(), session.clone(), "run", token)
        .await
        .unwrap();
    let mut budget = RunBudget::default();
    assert!(matches!(
        turn.mediate(
            calls(vec![
                call("slow-call", "slow", "{}"),
                call("fast-a-call", "fast-a", "{}"),
                call("fast-b-call", "fast-b", "{}"),
            ]),
            &mut budget,
        )
        .await
        .unwrap(),
        Step::PolicyStop(_)
    ));
    server.abort();

    let log = facts(&mediator, &tenant, &session);
    let opened: Vec<_> = log
        .iter()
        .filter_map(|fact| match fact {
            Fact::DispatchOpened { dispatch, .. } => Some(dispatch),
            _ => None,
        })
        .collect();
    let closed: Vec<_> = log
        .iter()
        .filter_map(|fact| match fact {
            Fact::DispatchClosed {
                dispatch,
                outcome: CloseOutcome::Indeterminate,
                ..
            } => Some(dispatch),
            _ => None,
        })
        .collect();
    assert_eq!(opened, closed);
    assert_eq!(opened.len(), 1);
    let slow = ResolvedCall::new(ToolName::new("slow"), serde_json::json!({}), Vec::new());
    assert_eq!(opened[0].digest(), &slow.digest());
    assert!(!log.iter().any(|fact| matches!(
        fact,
        Fact::DispatchClosed {
            outcome: CloseOutcome::Success { .. },
            ..
        }
    )));
    let answered: Vec<_> = log
        .iter()
        .filter_map(|fact| match fact {
            Fact::BlockFeedback { call_id, .. } => Some(call_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(answered, ["slow-call", "fast-a-call", "fast-b-call"]);
    assert_eq!(
        log.iter()
            .filter(|fact| matches!(
                fact,
                Fact::AssistantMessage { calls, .. } if calls.is_empty()
            ))
            .count(),
        1
    );
    assert_eq!(
        log.iter()
            .filter(|fact| matches!(
                fact,
                Fact::Boundary {
                    kind: BoundaryKind::TurnEnd,
                    ..
                }
            ))
            .count(),
        1
    );
}

#[tokio::test]
async fn one_budget_is_shared_across_root_and_descendant_turns() {
    let mediator = mediator(
        r#"
version = 1
[[tool]]
name = "read"
delta = {}
"#,
        &[("read", BuiltinTool::Echo("value".to_string()))],
    );
    let tenant = TenantId::new("tenant");
    let parent = mediator.create_session(tenant.clone());
    let mut parent_turn = begin(&mediator, &tenant, &parent, "root").await;
    let mut budget = RunBudget::new(Limits {
        max_tool_invocations: 1,
        ..Limits::default()
    });
    parent_turn
        .mediate(calls(vec![call("root-read", "read", "{}")]), &mut budget)
        .await
        .unwrap();
    assert!(budget.is_exhausted());

    let Step::Fork(request) = parent_turn
        .mediate(calls(vec![call("fork", FORK, r#"{"task":"child"}"#)]), &mut budget)
        .await
        .unwrap()
    else {
        panic!("fork expected")
    };
    budget.charge_fork().unwrap();
    let child = mediator.fork_session(&tenant, &parent).unwrap();
    let mut child_turn = begin(&mediator, &tenant, &child, request.task()).await;
    assert!(matches!(
        child_turn
            .mediate(calls(vec![call("child-read", "read", "{}")]), &mut budget)
            .await
            .unwrap(),
        Step::PolicyStop(_)
    ));
    parent_turn.complete_fork(request, child.clone()).unwrap();
    let log = facts(&mediator, &tenant, &parent);
    assert_eq!(
        log.iter()
            .filter(|fact| matches!(fact, Fact::DispatchOpened { .. }))
            .count(),
        1
    );
    parent_turn.stop(StopReason::BudgetExhausted).unwrap();

    let mut fork_budget = RunBudget::new(Limits {
        max_forks: 1,
        max_fork_depth: 2,
        ..Limits::default()
    });
    assert!(fork_budget.allows_fork_from_depth(0));
    assert!(fork_budget.allows_fork_from_depth(1));
    assert!(!fork_budget.allows_fork_from_depth(2));
    fork_budget.charge_fork().unwrap();
    assert_eq!(fork_budget.charge_fork(), Err(appa_runtime::BudgetExhausted));
}

async fn spawn_repeating_response(body: &'static str) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let server_requests = requests.clone();
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            read_request_headers(&mut socket).await;
            server_requests.fetch_add(1, Ordering::SeqCst);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    (format!("http://{address}/resolve"), requests, handle)
}

async fn read_request_headers(socket: &mut tokio::net::TcpStream) {
    let mut received = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let count = socket.read(&mut buffer).await.unwrap();
        if count == 0 {
            return;
        }
        received.extend_from_slice(&buffer[..count]);
        if received.windows(4).any(|window| window == b"\r\n\r\n") {
            return;
        }
    }
}

fn spawn_counting_authority(
    ruling: &'static str,
) -> (String, std::sync::Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let consults = std::sync::Arc::new(AtomicUsize::new(0));
    let counter = consults.clone();
    let handle = tokio::spawn(async move {
        let listener = TcpListener::from_std(listener).unwrap();
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            counter.fetch_add(1, Ordering::SeqCst);
            read_request_headers(&mut socket).await;
            let body = format!(r#"{{"ruling":"{ruling}"}}"#);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    (format!("http://{address}/rule"), consults, handle)
}

async fn spawn_authority(ruling: &'static str) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request_headers(&mut socket).await;
        let body = format!(r#"{{"ruling":"{ruling}"}}"#);
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });
    (format!("http://{address}/rule"), handle)
}

fn feedback_payload(log: &[Fact], call_id: &str) -> serde_json::Value {
    let content = log
        .iter()
        .rev()
        .find_map(|fact| match fact {
            Fact::BlockFeedback {
                call_id: id, content, ..
            } if id.as_str() == call_id => Some(content.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{call_id} has no block feedback"));
    let (_, json) = content
        .split_once('\n')
        .unwrap_or_else(|| panic!("{call_id}'s feedback has no payload line: {content}"));
    serde_json::from_str(json).unwrap_or_else(|error| panic!("{call_id}'s payload is not JSON ({error}): {json}"))
}

#[tokio::test]
async fn a_landed_cast_clears_the_check_and_the_call_dispatches() {
    let policy = r#"
version = 1
trust_chain = ["suspicious", "trusted"]

[[tool]]
name = "scan"

[[tool]]
name = "send"
delta = {}
[tool.requires]
trust = "suspicious"

[[cast]]
name = "assume-suspicious"
constant = { trust = "suspicious" }
"#;
    let mediated = mediator(
        policy,
        &[
            ("scan", BuiltinTool::Echo("mail body".to_string())),
            ("send", BuiltinTool::Echo("sent".to_string())),
        ],
    );
    let tenant = TenantId::new("tenant");
    let session = mediated.create_session(tenant.clone());
    let mut turn = begin(&mediated, &tenant, &session, "go").await;
    let mut budget = RunBudget::default();
    assert!(matches!(
        turn.mediate(calls(vec![call("scan-call", "scan", "{}")]), &mut budget)
            .await
            .unwrap(),
        Step::Continue
    ));
    assert!(matches!(
        turn.mediate(calls(vec![call("send-call", "send", "{}")]), &mut budget)
            .await
            .unwrap(),
        Step::Continue
    ));

    let log = facts(&mediated, &tenant, &session);
    assert_eq!(
        log.iter()
            .filter(|fact| matches!(fact, Fact::CastApplied { .. }))
            .count(),
        1
    );
    assert!(
        tool_values(&log).iter().any(|(body, _)| *body == "sent"),
        "send dispatched after the cast landed"
    );
    assert!(
        !log.iter()
            .any(|fact| matches!(fact, Fact::BlockFeedback { call_id, .. } if call_id.as_str() == "send-call")),
        "no residual block reached the model"
    );
    turn.stop(StopReason::Cancelled).unwrap();
}

#[tokio::test]
async fn every_unestablished_fact_is_attempted_and_the_residual_is_named() {
    let policy = r#"
version = 1
trust_chain = ["suspicious", "trusted"]

[[tool]]
name = "scan"

[[tool]]
name = "send"
delta = {}
[tool.requires]
trust = "suspicious"
audience = { includes = ["alice"] }

[[cast]]
name = "assume-suspicious"
constant = { trust = "suspicious" }
"#;
    let mediated = mediator(
        policy,
        &[
            ("scan", BuiltinTool::Echo("mail body".to_string())),
            ("send", BuiltinTool::Echo("must not run".to_string())),
        ],
    );
    let tenant = TenantId::new("tenant");
    let session = mediated.create_session(tenant.clone());
    let mut turn = begin(&mediated, &tenant, &session, "go").await;
    let mut budget = RunBudget::default();
    assert!(matches!(
        turn.mediate(
            calls(vec![call("scan-1", "scan", "{}"), call("scan-2", "scan", "{}")]),
            &mut budget
        )
        .await
        .unwrap(),
        Step::Continue
    ));
    assert!(matches!(
        turn.mediate(calls(vec![call("send-call", "send", "{}")]), &mut budget)
            .await
            .unwrap(),
        Step::Continue
    ));

    let log = facts(&mediated, &tenant, &session);
    assert_eq!(
        log.iter()
            .filter(|fact| matches!(fact, Fact::CastApplied { .. }))
            .count(),
        2,
        "the cast lands on both values, not only the first fact"
    );
    assert!(!tool_values(&log).iter().any(|(body, _)| *body == "must not run"));
    let payload = feedback_payload(&log, "send-call");
    let residual = payload["unestablished"].as_array().expect("unestablished entries");
    assert_eq!(residual.len(), 2, "both audience residuals are named");
    for entry in residual {
        assert_eq!(entry["dimension"], "Audience");
        assert_eq!(entry["source_kind"], "tool_result");
        assert!(entry.get("value").is_none(), "no internal id crosses to the model");
    }
    turn.stop(StopReason::Cancelled).unwrap();
}

#[tokio::test]
async fn a_declining_resolver_leaves_the_residual_named_not_blanket() {
    let (resolver_url, requests, server) = spawn_repeating_response(r#"{}"#).await;
    let policy = format!(
        r#"
version = 1
trust_chain = ["suspicious", "trusted"]

[[tool]]
name = "scan"

[[tool]]
name = "send"
delta = {{}}
[tool.requires]
trust = "trusted"

[[cast]]
name = "classifier"
resolver = {{ url = "{resolver_url}", may_cast = {{ trust = ["trusted"] }} }}
"#
    );
    let mediated = mediator(
        &policy,
        &[
            ("scan", BuiltinTool::Echo("mail body".to_string())),
            ("send", BuiltinTool::Echo("must not run".to_string())),
        ],
    );
    let tenant = TenantId::new("tenant");
    let session = mediated.create_session(tenant.clone());
    let mut turn = begin(&mediated, &tenant, &session, "go").await;
    let mut budget = RunBudget::default();
    assert!(matches!(
        turn.mediate(calls(vec![call("scan-call", "scan", "{}")]), &mut budget)
            .await
            .unwrap(),
        Step::Continue
    ));
    assert!(matches!(
        turn.mediate(calls(vec![call("send-call", "send", "{}")]), &mut budget)
            .await
            .unwrap(),
        Step::Continue
    ));
    server.abort();
    assert_eq!(requests.load(Ordering::SeqCst), 1, "one resolution pass per proposal");

    let log = facts(&mediated, &tenant, &session);
    assert!(!log.iter().any(|fact| matches!(fact, Fact::CastApplied { .. })));
    assert!(!tool_values(&log).iter().any(|(body, _)| *body == "must not run"));
    let payload = feedback_payload(&log, "send-call");
    let residual = payload["unestablished"].as_array().expect("unestablished entries");
    assert_eq!(residual.len(), 1);
    assert_eq!(residual[0]["dimension"], "Trust");
    assert_eq!(residual[0]["source_kind"], "tool_result");
    turn.stop(StopReason::Cancelled).unwrap();
}

#[tokio::test]
async fn an_offer_stays_gated_on_missing_facts_before_any_gate_or_authority() {
    let (officer_url, consults, officer_server) = spawn_counting_authority("approve");
    let policy = format!(
        r#"
version = 1
trust_chain = ["suspicious", "trusted"]

[[tool]]
name = "scan"

[[tool]]
name = "restrict"
delta = {{ trust = "suspicious" }}
[tool.requires]
trust = "suspicious"
audience = {{ includes = ["alice"] }}
attention = ["signoff"]

[[authority]]
name = "officer"
mandate = {{ attends = ["signoff"] }}
implementation = {{ resolver = {{ url = "{officer_url}" }} }}

[[cast]]
name = "assume-trusted"
constant = {{ trust = "trusted" }}
"#
    );
    let mediated = mediator(
        &policy,
        &[
            ("scan", BuiltinTool::Echo("mail body".to_string())),
            ("restrict", BuiltinTool::Echo("must not run".to_string())),
        ],
    );
    let tenant = TenantId::new("tenant");
    let session = mediated.create_session(tenant.clone());
    let mut turn = begin(&mediated, &tenant, &session, "go").await;
    let mut budget = RunBudget::default();
    assert!(matches!(
        turn.mediate(calls(vec![call("scan-call", "scan", "{}")]), &mut budget)
            .await
            .unwrap(),
        Step::Continue
    ));
    assert!(matches!(
        turn.mediate(
            calls(vec![
                call("restrict-call", "restrict", "{}"),
                call("exec-call", EXECUTE_REMEDY_PLAN, r#"{"plan_id":"remedy-0"}"#),
            ]),
            &mut budget
        )
        .await
        .unwrap(),
        Step::Continue
    ));

    let log = facts(&mediated, &tenant, &session);
    let offer = feedback_payload(&log, "restrict-call");
    assert!(offer["narrowing"].is_object(), "the trust narrowing is offered");
    assert!(!offer["unestablished"].as_array().unwrap().is_empty());

    let gated = feedback_payload(&log, "exec-call");
    assert_eq!(gated["unestablished"].as_array().unwrap()[0]["dimension"], "Audience");
    assert!(
        !log.iter()
            .any(|fact| matches!(fact, Fact::Acceptance { .. } | Fact::Ruling { .. })),
        "nothing was accepted or ruled"
    );
    assert_eq!(
        log.iter()
            .filter(|fact| matches!(fact, Fact::DispatchOpened { .. }))
            .count(),
        1,
        "only scan's own dispatch opened — restrict never did"
    );
    officer_server.abort();
    assert_eq!(
        consults.load(Ordering::SeqCst),
        0,
        "no authority heard of the gated plan"
    );
    turn.stop(StopReason::Cancelled).unwrap();
}

#[tokio::test]
async fn a_sanitizer_bound_return_resolves_its_fold_before_crossing() {
    let resolved = r#"
version = 1
trust_chain = ["suspicious", "trusted"]

[[tool]]
name = "scan"

[[sanitizer]]
name = "pii"
on = ["tool_output"]
[sanitizer.mandate]
audience = { from = { includes = ["internal"] }, to = { exactly = ["public"] } }
[sanitizer.implementation]
builtin = "redact-email"

[child]
return_sanitizer = "pii"

[[cast]]
name = "assume-suspicious"
constant = { trust = "suspicious" }

[[cast]]
name = "assume-internal"
constant = { audience = { exactly = ["internal"] } }
"#;
    let mediated = mediator(resolved, &[("scan", BuiltinTool::Echo("ask eve@corp.com".to_string()))]);
    let tenant = TenantId::new("tenant");
    let parent = mediated.create_session(tenant.clone());
    let child = mediated.fork_session(&tenant, &parent).unwrap();
    let mut child_turn = begin(&mediated, &tenant, &child, "inspect").await;
    let mut budget = RunBudget::default();
    assert!(matches!(
        child_turn
            .mediate(calls(vec![call("scan-call", "scan", "{}")]), &mut budget)
            .await
            .unwrap(),
        Step::Continue
    ));
    assert!(matches!(
        child_turn
            .mediate(
                calls(vec![call("return", SUBMIT_RESULT, r#"{"value":"ask eve@corp.com"}"#)]),
                &mut budget
            )
            .await
            .unwrap(),
        Step::ChildFinished
    ));
    let log = facts(&mediated, &tenant, &parent);
    assert_eq!(
        log.iter()
            .filter(|fact| matches!(fact, Fact::CastApplied { .. }))
            .count(),
        2,
        "both fold dimensions were established before the crossing"
    );
    assert!(log.iter().any(|fact| matches!(
        fact,
        Fact::ChildReturn {
            derivation: appa_engine::fact::ReturnDerivation::Sanitized { .. },
            ..
        }
    )));

    let unresolvable = r#"
version = 1
trust_chain = ["suspicious", "trusted"]

[[tool]]
name = "scan"

[[sanitizer]]
name = "pii"
on = ["tool_output"]
[sanitizer.mandate]
audience = { from = { includes = ["internal"] }, to = { exactly = ["public"] } }
[sanitizer.implementation]
builtin = "redact-email"

[child]
return_sanitizer = "pii"
"#;
    let mediated = mediator(
        unresolvable,
        &[("scan", BuiltinTool::Echo("ask eve@corp.com".to_string()))],
    );
    let parent = mediated.create_session(tenant.clone());
    let child = mediated.fork_session(&tenant, &parent).unwrap();
    let mut child_turn = begin(&mediated, &tenant, &child, "inspect").await;
    assert!(matches!(
        child_turn
            .mediate(calls(vec![call("scan-call", "scan", "{}")]), &mut budget)
            .await
            .unwrap(),
        Step::Continue
    ));
    assert!(matches!(
        child_turn
            .mediate(
                calls(vec![call("return", SUBMIT_RESULT, r#"{"value":"ask eve@corp.com"}"#)]),
                &mut budget
            )
            .await
            .unwrap(),
        Step::Continue
    ));
    let log = facts(&mediated, &tenant, &child);
    assert!(!log.iter().any(|fact| matches!(fact, Fact::ChildReturn { .. })));
    let payload = feedback_payload(&log, "return");
    let residual = payload["unestablished"].as_array().expect("unestablished entries");
    assert_eq!(residual.len(), 2, "both fold dimensions are named");
    assert!(residual.iter().all(|entry| entry["source_kind"] == "tool_result"));
    child_turn.stop(StopReason::Cancelled).unwrap();
}

#[tokio::test]
async fn a_tool_output_sanitize_plan_admits_the_derivation_and_withholds_the_raw() {
    let mediated = mediator(
        r#"
version = 1
[[tool]]
name = "read"
delta = { audience = { exactly = ["internal"] } }

[[sanitizer]]
name = "pii"
on = ["tool_output"]
hint = "Drops email addresses so a record can leave the org."
[sanitizer.mandate]
audience = { from = { includes = ["internal"] }, to = { exactly = ["public"] } }
[sanitizer.implementation]
builtin = "redact-email"
"#,
        &[("read", BuiltinTool::Echo("ask eve@corp.com".to_string()))],
    );
    let tenant = TenantId::new("tenant");
    let session = mediated.create_session(tenant.clone());
    let mut budget = RunBudget::default();
    let mut turn = begin(&mediated, &tenant, &session, "go").await;

    assert!(matches!(
        turn.mediate(calls(vec![call("read-call", "read", "{}")]), &mut budget)
            .await
            .unwrap(),
        Step::Continue
    ));
    let payload = feedback_payload(&facts(&mediated, &tenant, &session), "read-call");
    let plans = payload["remedy_plans"].as_array().expect("remedy plans");
    assert_eq!(plans.len(), 2, "acceptance and the applicable sanitizer");
    let sanitize = plans
        .iter()
        .find(|plan| plan["sanitizes"].is_object())
        .expect("a sanitize plan is offered");
    assert_eq!(sanitize["sanitizes"]["sanitizer"], "pii");
    assert_eq!(
        sanitize["sanitizes"]["hint"],
        "Drops email addresses so a record can leave the org."
    );
    assert_eq!(sanitize["accepts_narrowing"], false);
    let handle = sanitize["plan_id"]
        .as_str()
        .expect("the plan carries a handle")
        .to_string();

    assert!(matches!(
        turn.mediate(
            calls(vec![call("sanitize-call", EXECUTE_REMEDY_PLAN, &remedy_args(&handle))]),
            &mut budget,
        )
        .await
        .unwrap(),
        Step::Continue
    ));

    let log = facts(&mediated, &tenant, &session);
    let admitted: Vec<_> = log
        .iter()
        .filter_map(|fact| match fact {
            Fact::ValueAdmitted {
                value,
                provenance: Provenance::ToolResult { .. },
                ..
            } => Some((value.body.as_str(), value.label.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        admitted,
        [(
            "ask [redacted-email]",
            Label::new(Dim::Known(Trust::new(u8::MAX)), Dim::Known(Audience::Public))
        )]
    );
    let projection = Projection::build(&log, Revision::new(log.len() as u64));
    assert_eq!(
        projection.view(&session).current_label(),
        Label::new(Dim::Known(Trust::new(1)), Dim::Known(Audience::Public))
    );
    assert!(log.iter().any(|fact| matches!(
        fact,
        Fact::OutputSanitizerApplied { sanitizer, raw_digest, .. }
            if sanitizer.as_str() == "pii" && raw_digest == &RawResultDigest::of(b"ask eve@corp.com")
    )));
    assert!(!log.iter().any(|fact| matches!(fact, Fact::Acceptance { .. })));
    assert!(!log.iter().any(|fact| match fact {
        Fact::ValueAdmitted { value, .. } => value.body.as_str().contains("eve@corp.com"),
        Fact::BlockFeedback { content, .. } => content.contains("eve@corp.com"),
        _ => false,
    }));
    turn.stop(StopReason::Cancelled).unwrap();
}
