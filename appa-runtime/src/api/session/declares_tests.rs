use super::super::Runtime;
use super::*;
use crate::api::host::AheadState;
use crate::api::{Actor, RemedyAttempt, RemedyOutcome, SpawnBinding, ToolCallDecision};
use crate::config::Config;
use crate::engine::LabelSpelling;
use appa_engine::fact::{Fact, ReturnPolicy};
use appa_eventlog::{AheadAttempt, AheadOutcome};
use appa_runtime_api::SpawnLaunch;

const TOOLS: &str = r#"
[policy]
version = 2

[[policy.tool]]
name = "fetch"
parameters = { type = "object", properties = { a = { type = "integer" } } }
delta = {}

[[policy.tool]]
name = "taint"
parameters = { type = "object", properties = { a = { type = "integer" } } }
delta = { trust = "suspicious" }
"#;

/// The reserved sanitizer a return declared with a schema crosses through.
const ATTEST: &str = r#"
[[policy.sanitizer]]
name = "attest-schema"
on = ["tool_output"]
[policy.sanitizer.permits]
trust = { from = "suspicious", to = "trusted" }
"#;

const DEPLOYMENT: &str = r#"
[policy.deployment]
context_control = true

[externals]
timeout_ms = 2000
max_body_bytes = 65536
"#;

/// A runtime whose root is open, serving a policy that does or does not register
/// `attest-schema`. The directory lives as long as the runtime.
struct Deployed {
    dir: tempfile::TempDir,
    runtime: Runtime,
}

impl Deployed {
    fn config(&self, text: &str) -> Config {
        let path = self.dir.path().join("appa.toml");
        std::fs::write(&path, text).expect("the fixture writes");
        Config::load(&path).expect("the fixture validates")
    }

    /// Another runtime process serving the same store, as a host's second replica does.
    fn replica(&self) -> Runtime {
        let config = Config::load(&self.dir.path().join("appa.toml")).expect("the fixture validates");
        Runtime::open(config, self.dir.path().join("appa.db"), None).expect("the replica opens")
    }
}

fn policy(attesting: bool) -> String {
    let attest = if attesting { ATTEST } else { "" };
    format!("{TOOLS}{attest}{DEPLOYMENT}")
}

fn deployed(attesting: bool) -> Deployed {
    deployed_serving(&policy(attesting))
}

fn deployed_serving(text: &str) -> Deployed {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, text).expect("the fixture writes");
    let runtime = Runtime::open(
        Config::load(&path).expect("the fixture validates"),
        dir.path().join("appa.db"),
        None,
    )
    .expect("the runtime opens");
    runtime.create_session(root(), None).expect("a fresh root opens");
    Deployed { dir, runtime }
}

fn root() -> TrajectoryId {
    TrajectoryId("cc:root".to_string())
}

fn session(runtime: &Runtime) -> Session {
    runtime.session(&root(), &root()).expect("the session reattaches")
}

fn call(tool: &str, a: i64) -> ProposedCall {
    ProposedCall {
        tool: tool.to_string(),
        arguments: raw(serde_json::json!({ "a": a })),
        cwd: None,
    }
}

/// The bare declaration: the return crosses as spoken, floored at this session's label.
fn as_spoken() -> RemedyArguments {
    RemedyArguments {
        label: Some(LabelSpelling::default()),
        return_schema: None,
    }
}

fn attested() -> RemedyArguments {
    RemedyArguments {
        label: Some(LabelSpelling::default()),
        return_schema: Some(serde_json::json!({
            "type": "object",
            "properties": { "days": { "type": "integer", "minimum": 0, "maximum": 30 } },
            "required": ["days"],
        })),
    }
}

async fn declaring(
    runtime: &Runtime,
    spawn: ProposedCall,
    call_id: &str,
    declares: RemedyArguments,
) -> Result<ToolCallDecision, EventError> {
    session(runtime)
        .on_spawn_declaring(
            spawn,
            Some(call_id.to_string()),
            None,
            DeclaredReturn::Arguments(declares),
        )
        .await
}

fn facts(runtime: &Runtime, kind: impl Fn(&Fact) -> bool) -> usize {
    let log = runtime.inner.log(&root()).expect("the log reads");
    log.facts().iter().filter(|fact| kind(fact)).count()
}

fn dispatches(runtime: &Runtime) -> usize {
    facts(runtime, |fact| matches!(fact, Fact::DispatchOpened { .. }))
}

fn forks(runtime: &Runtime) -> usize {
    facts(runtime, |fact| matches!(fact, Fact::ForkPrepared { .. }))
}

fn approvals(runtime: &Runtime) -> usize {
    facts(runtime, |fact| matches!(fact, Fact::CallApproved { .. }))
}

fn declarations(runtime: &Runtime) -> usize {
    facts(runtime, |fact| {
        matches!(
            fact,
            Fact::CallApproved {
                acceptance: None,
                output: None,
                return_policy: Some(_),
                rulings,
                ..
            } if rulings.is_empty()
        )
    })
}

/// Every approval spent, by the offer that approved it: each one at most once.
fn spent(runtime: &Runtime) -> Vec<appa_engine::value::OfferId> {
    let log = runtime.inner.log(&root()).expect("the log reads");
    log.facts()
        .iter()
        .filter_map(|fact| match fact {
            Fact::CallApprovalConsumed { offer, .. } => Some(*offer),
            _ => None,
        })
        .collect()
}

/// The return policy each prepared fork carries.
fn fork_returns(runtime: &Runtime) -> Vec<ReturnPolicy> {
    let log = runtime.inner.log(&root()).expect("the log reads");
    log.facts()
        .iter()
        .filter_map(|fact| match fact {
            Fact::ForkPrepared { return_policy, .. } => Some(return_policy.clone()),
            _ => None,
        })
        .collect()
}

fn crossing_through(policy: &ReturnPolicy) -> Option<Option<String>> {
    match policy {
        ReturnPolicy::Crossing(crossing) => Some(
            crossing
                .sanitizer
                .as_ref()
                .map(|sanitizer| sanitizer.name().as_str().to_string()),
        ),
        ReturnPolicy::Void | ReturnPolicy::Deferred => None,
    }
}

fn declares_offered(decision: &ToolCallDecision) -> bool {
    matches!(decision, ToolCallDecision::Deny { offers, .. } if offers.iter().any(|offer| offer.returns.is_some()))
}

fn binding(decision: Result<ToolCallDecision, EventError>) -> SpawnBinding {
    match decision {
        Ok(ToolCallDecision::Allow {
            spawn: Some(binding), ..
        }) => binding,
        other => panic!("the spawn releases with its fork, got {other:?}"),
    }
}

#[tokio::test]
async fn a_spawn_with_nothing_else_to_settle_is_released_by_the_proposal_that_declares_its_return() {
    let Deployed { runtime, .. } = deployed(false);
    binding(declaring(&runtime, call("fetch", 1), "toolu-1", as_spoken()).await);
    assert_eq!(dispatches(&runtime), 1);
    assert_eq!(forks(&runtime), 1);
    assert_eq!(declarations(&runtime), 1, "the return was declared once");
    assert_eq!(spent(&runtime).len(), 1, "the declaration was spent by the release");
    assert_eq!(
        fork_returns(&runtime).iter().map(crossing_through).collect::<Vec<_>>(),
        vec![Some(None)],
        "the return crosses as spoken"
    );
}

/// The probe this replaces spent the approval a narrowing left standing and prepared a fork
/// nothing could take back. The declaring proposal stands at the narrowing's block instead,
/// and the call proposed again after it releases once.
#[tokio::test]
async fn a_spawn_that_also_narrows_declares_then_releases_once_after_its_narrowing_is_accepted() {
    let Deployed { runtime, .. } = deployed(false);
    let blocked = declaring(&runtime, call("taint", 1), "toolu-1", as_spoken())
        .await
        .expect("the spawn is judged");
    let ToolCallDecision::Deny { offers, .. } = &blocked else {
        panic!("the narrowing still blocks the spawn: {blocked:?}");
    };
    assert!(!offers.is_empty(), "the narrowing is offered");
    assert!(
        !declares_offered(&blocked),
        "the declaration stands, so no plan declares"
    );
    assert_eq!(declarations(&runtime), 1);
    assert_eq!(dispatches(&runtime), 0);

    let log = runtime.inner.log(&root()).expect("the log reads");
    let narrowing = crate::engine::resolve_rendered(&log, &OfferId(offers[0].id.clone())).expect("the offer resolves");
    assert!(matches!(
        session(&runtime)
            .on_remedy(narrowing, RemedyArguments::default(), None, None)
            .await,
        Ok(RemedyDecision::Authorized { .. })
    ));

    let child_binding = binding(declaring(&runtime, call("taint", 1), "toolu-2", as_spoken()).await);
    assert_eq!(dispatches(&runtime), 1);
    assert_eq!(forks(&runtime), 1);
    assert_eq!(declarations(&runtime), 1, "the retry declared nothing again");
    let spent = spent(&runtime);
    let distinct: std::collections::BTreeSet<_> = spent.iter().collect();
    assert_eq!(distinct.len(), spent.len(), "no approval was spent twice: {spent:?}");
    assert_eq!(spent.len(), 1, "the release spent the narrowing's approval once");

    let child = session(&runtime)
        .on_child_start(
            TrajectoryId("cc:root:child".to_string()),
            SpawnRef::Binding(child_binding),
        )
        .expect("the fork binds and the child opens");
    assert!(matches!(
        child.on_child_end(Some("done".to_string())).await,
        Ok(ChildReturnDecision::Returned { .. })
    ));
}

/// The probe this replaces was released while the first spawn waited to start, and failed
/// the second spawn's fork. The declaring proposal is refused as outstanding instead, spends
/// nothing, and the call releases once proposed again after the first child starts.
#[tokio::test]
async fn a_spawn_proposed_while_another_waits_spends_nothing_and_releases_when_proposed_again() {
    let Deployed { runtime, .. } = deployed(false);
    let first = binding(declaring(&runtime, call("fetch", 1), "toolu-1", as_spoken()).await);
    assert!(matches!(
        declaring(&runtime, call("fetch", 2), "toolu-2", as_spoken()).await,
        Err(EventError::SpawnOutstanding)
    ));
    assert_eq!(dispatches(&runtime), 1);
    assert_eq!(forks(&runtime), 1);
    assert_eq!(spent(&runtime).len(), 1, "only the first spawn spent its declaration");

    session(&runtime)
        .on_child_start(TrajectoryId("cc:root:first".to_string()), SpawnRef::Binding(first))
        .expect("the first child starts");
    binding(declaring(&runtime, call("fetch", 2), "toolu-2", as_spoken()).await);
    assert_eq!(dispatches(&runtime), 2);
    assert_eq!(forks(&runtime), 2);
    assert_eq!(declarations(&runtime), 2, "each spawn declared once");
    assert_eq!(spent(&runtime).len(), 2);
}

#[tokio::test]
async fn an_attested_declaration_releases_the_spawn_behind_attest_schema() {
    let Deployed { runtime, .. } = deployed(true);
    binding(declaring(&runtime, call("fetch", 1), "toolu-1", attested()).await);
    assert_eq!(
        fork_returns(&runtime).iter().map(crossing_through).collect::<Vec<_>>(),
        vec![Some(Some(appa_engine::names::SanitizerName::ATTEST_SCHEMA.to_string()))],
    );
}

#[tokio::test]
async fn an_attested_declaration_no_plan_fits_is_not_applied_and_the_spawn_stays_blocked() {
    let Deployed { runtime, .. } = deployed(false);
    let decision = declaring(&runtime, call("fetch", 1), "toolu-1", attested())
        .await
        .expect("the spawn is judged");
    assert!(
        declares_offered(&decision),
        "the block still offers its own declarations: {decision:?}"
    );
    assert_eq!(approvals(&runtime), 0, "nothing was applied");
    assert_eq!(dispatches(&runtime), 0);
}

#[tokio::test]
async fn a_repeat_of_a_released_spawn_answers_its_release_without_another_dispatch() {
    let Deployed { runtime, .. } = deployed(false);
    let released = declaring(&runtime, call("fetch", 1), "toolu-1", as_spoken())
        .await
        .expect("the spawn is judged");
    let repeated = declaring(&runtime, call("fetch", 1), "toolu-1", as_spoken())
        .await
        .expect("the repeat is answered");
    assert_eq!(released, repeated);
    assert_eq!(dispatches(&runtime), 1);
    assert_eq!(forks(&runtime), 1);
    assert_eq!(declarations(&runtime), 1);
}

#[tokio::test]
async fn a_repeat_of_a_spawn_still_blocked_declares_nothing_again() {
    let Deployed { runtime, .. } = deployed(false);
    let blocked = declaring(&runtime, call("taint", 1), "toolu-1", as_spoken())
        .await
        .expect("the spawn is judged");
    let repeated = declaring(&runtime, call("taint", 1), "toolu-1", as_spoken())
        .await
        .expect("the repeat is answered");
    assert_eq!(blocked, repeated, "the repeat stands at the same block");
    assert_eq!(declarations(&runtime), 1);
    assert_eq!(approvals(&runtime), 1);
    assert_eq!(dispatches(&runtime), 0);
}

#[tokio::test]
async fn a_return_declared_ahead_is_taken_before_the_one_the_proposal_declares() {
    let Deployed { runtime, .. } = deployed(true);
    let acting = Actor {
        root: root(),
        child: None,
    };
    assert!(matches!(
        runtime
            .declare_return_ahead(&acting, as_spoken(), RemedyAttempt("r1".to_string()))
            .await,
        RemedyOutcome::Returned { .. }
    ));
    binding(declaring(&runtime, call("fetch", 1), "toolu-1", attested()).await);
    let ahead = AheadState::fold(runtime.inner.log(&root()).expect("the log reads").host_records());
    assert_eq!(
        ahead.settlement(&root(), &AheadAttempt::Host("r1".to_string())),
        Some(AheadOutcome::Applied)
    );
    assert_eq!(
        fork_returns(&runtime).iter().map(crossing_through).collect::<Vec<_>>(),
        vec![Some(None)],
        "the return crosses as declared ahead, as spoken"
    );
    assert_eq!(declarations(&runtime), 1);
}

/// The canonical ids of the offers a block rendered.
fn canonical(runtime: &Runtime, offers: &[appa_runtime_api::OfferedRemedy]) -> Vec<String> {
    let log = runtime.inner.log(&root()).expect("the log reads");
    offers
        .iter()
        .filter_map(|offer| crate::engine::resolve_rendered(&log, &OfferId(offer.id.clone())))
        .map(|offer| offer.0)
        .collect()
}

/// The canonical ids of every offer the family accepted.
fn accepted(runtime: &Runtime) -> Vec<String> {
    let log = runtime.inner.log(&root()).expect("the log reads");
    log.facts()
        .iter()
        .filter_map(|fact| match fact {
            Fact::OfferAccepted { offer, .. } => Some(offer.to_hex()),
            _ => None,
        })
        .collect()
}

async fn undeclared(runtime: &Runtime, spawn: ProposedCall, call_id: &str) -> ToolCallDecision {
    session(runtime)
        .on_tool_call_identified(spawn, Some(call_id.to_string()), Some(SpawnKind::Single), None)
        .await
        .expect("the spawn is judged")
}

/// A reload and another call's denial leave the block the call stood at stale: the hook's
/// proposal is decided afresh and applies the fitting offer of the new block.
#[tokio::test]
async fn a_fitting_offer_gone_stale_is_replaced_and_applied_in_the_same_hook() {
    let deployed = deployed(true);
    let runtime = &deployed.runtime;
    let ToolCallDecision::Deny { offers, .. } = undeclared(runtime, call("fetch", 1), "toolu-x").await else {
        panic!("an undeclared spawn is blocked");
    };
    let first = canonical(runtime, &offers);
    runtime
        .reload(deployed.config(&policy(false)))
        .expect("the runtime reloads");
    assert!(matches!(
        session(runtime).on_tool_call(call("taint", 1), false).await,
        Ok(ToolCallDecision::Deny { .. })
    ));
    let stale = session(runtime)
        .with_view(|context| Ok(context.fitting_offer(&offers, &as_spoken())))
        .expect("the view reads");
    assert_eq!(stale, Fit::Stale);
    binding(declaring(runtime, call("fetch", 1), "toolu-x", as_spoken()).await);
    let applied = accepted(runtime);
    assert_eq!(applied.len(), 1, "{applied:?}");
    assert!(!first.contains(&applied[0]), "the stale offer was not applied");
    assert_eq!(dispatches(runtime), 1);
    assert_eq!(forks(runtime), 1);
    assert_eq!(declarations(runtime), 1);
}

/// Another delivery of the call executed the fitting offer: the block it was rendered in reads
/// it as taken, not as a declaration no plan fits, and the call proposed again is released by
/// that declaration.
#[tokio::test]
async fn a_fitting_offer_another_delivery_took_is_not_unfit() {
    let Deployed { runtime, .. } = deployed(false);
    let ToolCallDecision::Deny { offers, .. } = undeclared(&runtime, call("fetch", 1), "toolu-x").await else {
        panic!("an undeclared spawn is blocked");
    };
    let log = runtime.inner.log(&root()).expect("the log reads");
    let as_spoken_offer = offers
        .iter()
        .find(|offer| offer.returns == Some(appa_runtime_api::OfferedReturn::AsSpoken))
        .and_then(|offer| crate::engine::resolve_rendered(&log, &OfferId(offer.id.clone())))
        .expect("the bare declaration is offered");
    assert!(matches!(
        session(&runtime)
            .on_remedy(as_spoken_offer, as_spoken(), None, None)
            .await,
        Ok(RemedyDecision::Authorized { .. })
    ));
    let fit = session(&runtime)
        .with_view(|context| Ok(context.fitting_offer(&offers, &as_spoken())))
        .expect("the view reads");
    assert_eq!(fit, Fit::Ended);
    binding(declaring(&runtime, call("fetch", 1), "toolu-x", as_spoken()).await);
    assert_eq!(declarations(&runtime), 1);
    assert_eq!(dispatches(&runtime), 1);
}

/// Two deliveries of one call, on two replicas at once: whichever applies the declaration,
/// the other reads the release, and the call is released once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_deliveries_of_one_declaring_spawn_release_it_once() {
    for _ in 0..8 {
        let deployed = deployed(false);
        let replica = deployed.replica();
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let tasks: Vec<_> = [&deployed.runtime, &replica]
            .into_iter()
            .map(|runtime| {
                let session = session(runtime);
                let barrier = std::sync::Arc::clone(&barrier);
                tokio::spawn(async move {
                    barrier.wait().await;
                    session
                        .on_spawn_declaring(
                            call("fetch", 1),
                            Some("toolu-x".to_string()),
                            None,
                            DeclaredReturn::Arguments(as_spoken()),
                        )
                        .await
                })
            })
            .collect();
        let mut decisions = Vec::new();
        for task in tasks {
            decisions.push(task.await.expect("the task joins"));
        }
        let [a, b] = <[_; 2]>::try_from(decisions).expect("two deliveries were decided");
        let shown = format!("{a:?} {b:?}");
        assert_eq!(binding(a), binding(b), "{shown}");
        assert_eq!(dispatches(&deployed.runtime), 1);
        assert_eq!(forks(&deployed.runtime), 1);
        assert_eq!(declarations(&deployed.runtime), 1);
    }
}

/// The block a declaration no plan fits stands as the undeclared call's, with the notice.
#[tokio::test]
async fn a_declaration_no_plan_fits_adds_its_notice_to_the_undeclared_block() {
    let Deployed { runtime, .. } = deployed(false);
    let ToolCallDecision::Deny {
        feedback,
        outcomes,
        offers,
        display,
        review,
    } = undeclared(&runtime, call("fetch", 1), "toolu-x").await
    else {
        panic!("an undeclared spawn is blocked");
    };
    let decision = declaring(&runtime, call("fetch", 1), "toolu-x", attested())
        .await
        .expect("the spawn is judged");
    assert_eq!(
        decision,
        ToolCallDecision::Deny {
            feedback: format!(
                "{}{}\n\n{feedback}",
                super::call::UNFIT_DECLARED,
                super::call::APPLY_BELOW
            ),
            outcomes,
            offers,
            display,
            review,
        }
    );
    assert_eq!(approvals(&runtime), 0);
}

#[tokio::test]
async fn the_prepared_fork_carries_the_declared_floor_and_the_attested_shape() {
    let Deployed { runtime, .. } = deployed(true);
    let schema = serde_json::json!({
        "type": "object",
        "properties": { "days": { "type": "integer", "minimum": 0, "maximum": 30 } },
        "required": ["days"],
    });
    let declares = RemedyArguments {
        label: Some(LabelSpelling {
            trust: Some("suspicious".to_string()),
            audience: None,
        }),
        return_schema: Some(schema.clone()),
    };
    binding(declaring(&runtime, call("fetch", 1), "toolu-1", declares).await);
    let suspicious = session(&runtime)
        .with_view(|context| Ok(context.engine().trust_chain().rank_of("suspicious")))
        .expect("the view reads")
        .expect("the chain names the rank");
    let expected = ReturnPolicy::Crossing(appa_engine::fact::CrossingReturn {
        floor: appa_engine::label::Label::new(suspicious, appa_engine::label::Audience::public()),
        sanitizer: Some(appa_engine::fact::ReturnSanitizer::Attest(
            appa_engine::shape::ReturnShape::compile(&schema).expect("the schema compiles"),
        )),
        below_floor: appa_engine::fact::BelowFloor::Refuse,
    });
    assert_eq!(fork_returns(&runtime), vec![expected]);
}

#[tokio::test]
async fn a_declaration_whose_application_fails_to_commit_refuses_and_leaves_no_approval() {
    let Deployed { runtime, .. } = deployed(false);
    // The proposal's denial commits; the application's commit fails.
    runtime.store().fail_commit_after(1);
    assert!(matches!(
        declaring(&runtime, call("fetch", 1), "toolu-1", as_spoken()).await,
        Err(EventError::Storage(_))
    ));
    assert_eq!(
        facts(&runtime, |fact| matches!(fact, Fact::ProposalBatchDecided { .. })),
        1,
        "the denial committed"
    );
    assert_eq!(approvals(&runtime), 0);
    assert_eq!(accepted(&runtime), Vec::<String>::new());
    assert_eq!(dispatches(&runtime), 0);
    binding(declaring(&runtime, call("fetch", 1), "toolu-1", as_spoken()).await);
    assert_eq!(declarations(&runtime), 1);
    assert_eq!(dispatches(&runtime), 1);
}

#[tokio::test]
async fn a_deployment_without_context_control_releases_a_declaring_spawn_unmarked_and_declares_nothing() {
    let Deployed { runtime, .. } = deployed_serving(&format!(
        "{TOOLS}{}",
        DEPLOYMENT.replace("context_control = true", "context_control = false")
    ));
    assert!(
        matches!(
            declaring(&runtime, call("fetch", 1), "toolu-1", as_spoken()).await,
            Ok(ToolCallDecision::Allow { spawn: None, .. })
        ),
        "the deployment releases the spawn unmarked, so there is no fork for a declaration to bind"
    );
    assert_eq!(declarations(&runtime), 0);
    assert_eq!(approvals(&runtime), 0);
    assert_eq!(forks(&runtime), 0);
    assert_eq!(dispatches(&runtime), 1);
}

async fn deferring(runtime: &Runtime, spawn: ProposedCall, call_id: &str) -> Result<ToolCallDecision, EventError> {
    session(runtime)
        .on_spawn_declaring(spawn, Some(call_id.to_string()), None, DeclaredReturn::Deferred)
        .await
}

#[tokio::test]
async fn a_deferred_return_declared_on_a_spawn_without_a_call_id_is_refused_and_writes_nothing() {
    let Deployed { runtime, .. } = deployed(false);
    let refused = session(&runtime)
        .on_spawn_declaring(call("fetch", 1), None, None, DeclaredReturn::Deferred)
        .await;
    assert!(matches!(refused, Err(EventError::DeferralUnkeyed)), "{refused:?}");
    assert_eq!(dispatches(&runtime), 0);
    assert_eq!(declarations(&runtime), 0);
}

#[tokio::test]
async fn a_declaring_spawn_with_an_empty_call_id_is_refused_and_writes_nothing() {
    let Deployed { runtime, .. } = deployed(false);
    let opened = runtime.inner.log(&root()).expect("the log reads");
    let refused = declaring(&runtime, call("fetch", 1), "", as_spoken()).await;
    assert!(matches!(refused, Err(EventError::CallIdReused)), "{refused:?}");
    let after = runtime.inner.log(&root()).expect("the log reads");
    assert_eq!(after.basis(), opened.basis());
    assert_eq!(after.host_records(), opened.host_records());
}

#[tokio::test]
async fn a_spawn_proposed_with_a_deferred_return_is_released_under_it() {
    let Deployed { runtime, .. } = deployed(false);
    binding(deferring(&runtime, call("fetch", 1), "toolu-1").await);
    assert_eq!(declarations(&runtime), 1, "the return was declared once");
    assert_eq!(spent(&runtime).len(), 1, "the declaration was spent by the release");
    assert_eq!(fork_returns(&runtime), vec![ReturnPolicy::Deferred]);
}

#[tokio::test]
async fn a_deferred_return_is_declared_for_its_own_proposal_and_not_for_the_same_call_without_it() {
    let Deployed { runtime, .. } = deployed(false);
    // The narrowing keeps the deferral declared and unspent.
    let blocked = deferring(&runtime, call("taint", 1), "toolu-1")
        .await
        .expect("the spawn is judged");
    assert!(matches!(blocked, ToolCallDecision::Deny { .. }));
    assert_eq!(declarations(&runtime), 1);

    // The same call proposed without the deferral declares for itself.
    let plain = declaring(&runtime, call("taint", 1), "toolu-2", as_spoken())
        .await
        .expect("the spawn is judged");
    assert!(matches!(plain, ToolCallDecision::Deny { .. }));
    assert_eq!(
        declarations(&runtime),
        2,
        "each class of proposal holds its own declaration"
    );
    assert_eq!(forks(&runtime), 0);
}

#[tokio::test]
async fn a_call_id_decided_without_the_deferral_is_not_answered_for_a_proposal_that_carries_it() {
    let Deployed { runtime, .. } = deployed(false);
    binding(declaring(&runtime, call("fetch", 1), "toolu-1", as_spoken()).await);
    assert!(matches!(
        deferring(&runtime, call("fetch", 1), "toolu-1").await,
        Err(EventError::CallIdReused)
    ));
}

#[tokio::test]
async fn a_deployment_that_does_not_control_context_releases_a_deferring_spawn_unmarked() {
    let Deployed { runtime, .. } =
        deployed_serving(&policy(false).replace("context_control = true", "context_control = false"));
    assert!(matches!(
        deferring(&runtime, call("fetch", 1), "toolu-1").await,
        Ok(ToolCallDecision::Allow { spawn: None, .. })
    ));
    assert_eq!(forks(&runtime), 0);
    assert_eq!(declarations(&runtime), 0);
}

/// A child opened under a deferred return.
async fn deferred_child(runtime: &Runtime) -> Session {
    let fork = binding(deferring(runtime, call("fetch", 1), "toolu-1").await);
    session(runtime)
        .on_child_start(TrajectoryId("cc:root:child".to_string()), SpawnRef::Binding(fork))
        .expect("the fork binds and the child opens")
}

/// Run `taint` in `child`, accepting the narrowing its block offers.
async fn tainted(runtime: &Runtime, child: &Session) {
    let taint = call("taint", 1);
    let blocked = child
        .on_tool_call_identified(taint.clone(), Some("toolu-c1".to_string()), None, None)
        .await
        .expect("the child's call is judged");
    let ToolCallDecision::Deny { offers, .. } = &blocked else {
        panic!("the narrowing blocks the child's call: {blocked:?}");
    };
    let log = runtime.inner.log(&root()).expect("the log reads");
    let narrowing = crate::engine::resolve_rendered(&log, &OfferId(offers[0].id.clone())).expect("the offer resolves");
    assert!(matches!(
        child.on_remedy(narrowing, RemedyArguments::default(), None, None).await,
        Ok(RemedyDecision::Authorized { .. })
    ));
    assert!(matches!(
        child
            .on_tool_call_identified(taint.clone(), Some("toolu-c2".to_string()), None, None)
            .await,
        Ok(ToolCallDecision::Allow { .. })
    ));
    child
        .on_tool_result_identified(
            taint,
            Some("toolu-c2".to_string()),
            ToolOutcome::Success {
                body: OutcomeBody::Available("tainted".to_string()),
            },
        )
        .await
        .expect("the result is admitted");
}

fn deferred_returns(runtime: &Runtime) -> usize {
    facts(runtime, |fact| matches!(fact, Fact::ReturnDeferred { .. }))
}

fn crossings(runtime: &Runtime) -> usize {
    facts(runtime, |fact| matches!(fact, Fact::ChildReturn { .. }))
}

#[tokio::test]
async fn a_deferred_child_that_narrowed_is_answered_deferred_at_its_stop_and_nothing_crosses() {
    let Deployed { runtime, .. } = deployed(false);
    let child = deferred_child(&runtime).await;
    tainted(&runtime, &child).await;
    assert!(matches!(
        child.on_child_end(Some("the answer".to_string())).await,
        Ok(ChildReturnDecision::Deferred)
    ));
    assert_eq!(deferred_returns(&runtime), 1);
    assert_eq!(crossings(&runtime), 0);
    assert!(
        matches!(
            child.on_child_end(Some("the answer".to_string())).await,
            Ok(ChildReturnDecision::Deferred)
        ),
        "a repeated stop is answered as the deferral that ended the errand"
    );
    assert!(
        matches!(
            child.on_child_return("the answer".to_string(), None).await,
            Err(EventError::TrajectoryEnded)
        ),
        "a return made without ending is a turn the ended child cannot take"
    );
    assert!(matches!(
        child.on_child_end(None).await,
        Ok(ChildReturnDecision::NoValue)
    ));
    assert_eq!(deferred_returns(&runtime), 1);
    assert_eq!(crossings(&runtime), 0);
}

/// A held return crossed nothing, so it has no receipt, and its record is no released content:
/// a receipt forged to name it does not release.
#[tokio::test]
async fn a_held_return_has_no_receipt_and_its_record_releases_nothing() {
    use appa_runtime_api::{ReleaseDigest, ReleaseReceipt, ReleaseRefusal};
    let Deployed { runtime, .. } = deployed(false);
    let child = deferred_child(&runtime).await;
    tainted(&runtime, &child).await;
    let held = child
        .on_child_end(Some("the answer".to_string()))
        .await
        .expect("the stop is judged");
    assert!(matches!(held, ChildReturnDecision::Deferred));

    let log = runtime.inner.log(&root()).expect("the log reads");
    let position = |held: fn(&Fact) -> bool| log.facts().iter().position(held).expect("recorded") as u64;
    let deferred = position(|fact| matches!(fact, Fact::ReturnDeferred { .. }));
    let tainted = position(|fact| {
        matches!(
            fact,
            Fact::ValueAdmitted {
                provenance: appa_engine::value::Provenance::ToolResult { .. },
                ..
            }
        )
    });
    let sink = raw(serde_json::json!({ "a": 1 }));
    let forged = ReleaseReceipt {
        session: root(),
        fact_position: deferred,
        digest: ReleaseDigest::of(b"the answer"),
    };
    assert_eq!(
        runtime.judge_release(&[forged], "fetch", &sink).await,
        Err(ReleaseRefusal::Integrity)
    );
    let admitted = ReleaseReceipt {
        session: root(),
        fact_position: tainted,
        digest: ReleaseDigest::of(b"tainted"),
    };
    assert_eq!(runtime.judge_release(&[admitted], "fetch", &sink).await, Ok(()));
}

#[tokio::test]
async fn a_deferred_child_that_narrowed_nothing_returns_at_its_stop() {
    let Deployed { runtime, .. } = deployed(false);
    let child = deferred_child(&runtime).await;
    assert!(matches!(
        child.on_child_end(Some("done".to_string())).await,
        Ok(ChildReturnDecision::Returned { value, .. }) if value == "done"
    ));
    assert_eq!(deferred_returns(&runtime), 0);
    assert_eq!(crossings(&runtime), 1);
}

fn parent() -> Actor {
    Actor {
        root: root(),
        child: None,
    }
}

/// A child that narrowed and stopped under a deferred return, and what its parent is told.
async fn deferred_answer(runtime: &Runtime) -> (Session, crate::api::DeferredReturnNotice) {
    let child = deferred_child(runtime).await;
    tainted(runtime, &child).await;
    assert!(matches!(
        child.on_child_end(Some("the answer".to_string())).await,
        Ok(ChildReturnDecision::Deferred)
    ));
    let notice = runtime
        .deferred_return(&parent(), "toolu-1", &EmbeddedPresentationOptions::default())
        .expect("the family reads")
        .expect("the spawn's return was deferred");
    (child, notice)
}

/// The value a control call returned, where the return crossed as a record it names.
fn crossed_value(outcome: &RemedyOutcome) -> Option<&str> {
    match outcome {
        RemedyOutcome::Returned {
            value,
            receipt: Some(_),
        } => Some(value),
        _ => None,
    }
}

async fn settling(runtime: &Runtime, actor: &Actor, reference: &crate::api::ReturnReference) -> RemedyOutcome {
    let args =
        serde_json::from_value(serde_json::json!({ "offer_id": reference.as_str() })).expect("the arguments parse");
    runtime.execute_embedded_remedy(actor, args).await
}

#[tokio::test]
async fn accepting_a_deferred_return_crosses_it_once_as_the_control_calls_result() {
    let Deployed { runtime, .. } = deployed(false);
    let (_, notice) = deferred_answer(&runtime).await;
    assert!(runtime.owns_return(&parent(), &notice.accept));

    let accepted = settling(&runtime, &parent(), &notice.accept).await;
    assert_eq!(crossed_value(&accepted), Some("the answer"));
    assert_eq!(crossings(&runtime), 1);

    // A settled return answers its recorded outcome, whichever decision the repeat names.
    assert_eq!(settling(&runtime, &parent(), &notice.accept).await, accepted);
    assert_eq!(settling(&runtime, &parent(), &notice.discard).await, accepted);
    assert_eq!(crossings(&runtime), 1);
    assert_eq!(
        runtime
            .deferred_return(&parent(), "toolu-1", &EmbeddedPresentationOptions::default())
            .expect("the family reads"),
        Some(notice),
        "the notice reads the same after the decision"
    );
}

/// An accepted held return crossed as a record of its own, so its acceptance names that record
/// and a sink judged on it releases, as one judged on a crossed return of the same bytes does.
#[tokio::test]
async fn an_accepted_held_returns_receipt_releases_to_a_sink() {
    let Deployed { runtime, .. } = deployed(false);
    let (_, notice) = deferred_answer(&runtime).await;
    let accepted = settling(&runtime, &parent(), &notice.accept).await;
    let RemedyOutcome::Returned {
        value,
        receipt: Some(receipt),
    } = accepted
    else {
        panic!("the return is accepted with its receipt: {accepted:?}");
    };
    assert_eq!(value, "the answer");
    assert_eq!(receipt.digest, appa_runtime_api::ReleaseDigest::of(b"the answer"));
    assert_eq!(
        runtime
            .judge_release(&[receipt], "fetch", &raw(serde_json::json!({ "a": 1 })))
            .await,
        Ok(())
    );
}

#[tokio::test]
async fn discarding_a_deferred_return_crosses_nothing_and_cannot_be_undone() {
    let Deployed { runtime, .. } = deployed(false);
    let (_, notice) = deferred_answer(&runtime).await;

    let discarded = settling(&runtime, &parent(), &notice.discard).await;
    assert!(matches!(discarded, RemedyOutcome::Returned { .. }), "{discarded:?}");
    assert_eq!(facts(&runtime, |fact| matches!(fact, Fact::ReturnDiscarded { .. })), 1);
    assert_eq!(settling(&runtime, &parent(), &notice.accept).await, discarded);
    assert_eq!(crossings(&runtime), 0);
}

#[tokio::test]
async fn a_return_reference_settles_nothing_for_a_trajectory_that_does_not_own_the_return() {
    let Deployed { runtime, .. } = deployed(false);
    let (child, notice) = deferred_answer(&runtime).await;
    let stranger = Actor {
        root: root(),
        child: Some(child.trajectory.clone()),
    };
    let unknown = RemedyOutcome::Refused {
        reason: crate::api::RemedyRefusal::UnknownOffer,
    };
    assert!(!runtime.owns_return(&stranger, &notice.accept));
    let refused = settling(&runtime, &stranger, &notice.accept).await;
    assert!(matches!(refused, RemedyOutcome::Refused { .. }), "{refused:?}");

    let unminted = crate::api::ReturnReference::parse("ret-0123456789abcdef").expect("the spelling is a reference");
    assert!(!runtime.owns_return(&parent(), &unminted));
    assert_eq!(settling(&runtime, &parent(), &unminted).await, unknown);
    assert_eq!(crossings(&runtime), 0);
    assert_eq!(deferred_returns(&runtime), 1);
}

#[tokio::test]
async fn a_spawn_whose_return_crossed_at_the_stop_has_no_deferral_notice() {
    let Deployed { runtime, .. } = deployed(false);
    let child = deferred_child(&runtime).await;
    assert!(matches!(
        child.on_child_end(Some("done".to_string())).await,
        Ok(ChildReturnDecision::Returned { .. })
    ));
    assert_eq!(
        runtime
            .deferred_return(&parent(), "toolu-1", &EmbeddedPresentationOptions::default())
            .expect("the family reads"),
        None
    );
    assert!(runtime.defers_return(&parent(), "toolu-1").expect("the family reads"));
    assert!(
        !runtime
            .defers_return(&parent(), "toolu-unknown")
            .expect("the family reads")
    );
}

fn admitted(runtime: &Runtime) -> usize {
    facts(runtime, |fact| matches!(fact, Fact::ValueAdmitted { .. }))
}

fn spoken(body: &str) -> ToolOutcome {
    ToolOutcome::Success {
        body: OutcomeBody::Available(body.to_string()),
    }
}

#[tokio::test]
async fn a_deferred_spawns_result_carries_the_notice_and_admits_nothing_the_host_reports() {
    let Deployed { runtime, .. } = deployed(false);
    let (child, notice) = deferred_answer(&runtime).await;
    let before = admitted(&runtime);
    let decided = session(&runtime)
        .on_spawn_result_identified(
            call("fetch", 1),
            Some("toolu-1".to_string()),
            spoken("the answer"),
            Some(child.trajectory.clone()),
            Some("the answer".to_string()),
            SpawnLaunch::Undeclared,
        )
        .await
        .expect("the spawn's result is decided");
    assert_eq!(
        decided,
        SpawnResultDecision::Outcome(ToolResultDecision::Replace {
            placeholder: notice.feedback.clone(),
            presentation: None,
        })
    );
    assert_eq!(admitted(&runtime), before);
    assert_eq!(crossings(&runtime), 0);
    assert_eq!(
        crossed_value(&settling(&runtime, &parent(), &notice.accept).await),
        Some("the answer"),
        "the closed dispatch leaves the decision open"
    );
}

#[tokio::test]
async fn a_deferred_spawns_result_reported_as_an_ordinary_result_is_withheld() {
    let Deployed { runtime, .. } = deployed(false);
    let (_, notice) = deferred_answer(&runtime).await;
    let before = admitted(&runtime);
    let decided = session(&runtime)
        .on_tool_result_identified(call("fetch", 1), Some("toolu-1".to_string()), spoken("the answer"))
        .await
        .expect("the result is decided");
    assert_eq!(
        decided,
        ToolResultDecision::Replace {
            placeholder: notice.feedback,
            presentation: None,
        }
    );
    assert_eq!(admitted(&runtime), before);
}

#[tokio::test]
async fn a_deferred_spawns_result_before_its_childs_stop_is_withheld() {
    let Deployed { runtime, .. } = deployed(false);
    let _child = deferred_child(&runtime).await;
    let decided = session(&runtime)
        .on_tool_result_identified(call("fetch", 1), Some("toolu-1".to_string()), spoken("early words"))
        .await
        .expect("the result is decided");
    assert!(matches!(decided, ToolResultDecision::Replace { .. }), "{decided:?}");
    assert_eq!(admitted(&runtime), 0);
}

#[tokio::test]
async fn a_deferred_spawns_result_reported_after_its_acceptance_still_carries_the_notice() {
    let Deployed { runtime, .. } = deployed(false);
    let (child, notice) = deferred_answer(&runtime).await;
    settling(&runtime, &parent(), &notice.accept).await;
    let before = admitted(&runtime);
    let decided = session(&runtime)
        .on_spawn_result_identified(
            call("fetch", 1),
            Some("toolu-1".to_string()),
            spoken("the answer"),
            Some(child.trajectory.clone()),
            Some("the answer".to_string()),
            SpawnLaunch::Undeclared,
        )
        .await
        .expect("the spawn's result is decided");
    assert_eq!(
        decided,
        SpawnResultDecision::Outcome(ToolResultDecision::Replace {
            placeholder: notice.feedback,
            presentation: None,
        })
    );
    assert_eq!(admitted(&runtime), before);
}

#[tokio::test]
async fn a_deferred_spawns_reported_failure_closes_its_dispatch_as_failed() {
    let Deployed { runtime, .. } = deployed(false);
    let (child, notice) = deferred_answer(&runtime).await;
    let before = admitted(&runtime);
    let failed = || ToolOutcome::Failure {
        message: "boom".to_string(),
    };
    let decided = session(&runtime)
        .on_spawn_result_identified(
            call("fetch", 1),
            Some("toolu-1".to_string()),
            failed(),
            Some(child.trajectory.clone()),
            Some("the answer".to_string()),
            SpawnLaunch::Undeclared,
        )
        .await
        .expect("the spawn's result is decided");
    assert_eq!(
        decided,
        SpawnResultDecision::Outcome(ToolResultDecision::Replace {
            placeholder: notice.feedback,
            presentation: None,
        })
    );
    assert_eq!(
        facts(&runtime, |fact| matches!(
            fact,
            Fact::DispatchClosed {
                outcome: appa_engine::fact::CloseOutcome::Failure
                    | appa_engine::fact::CloseOutcome::FailureWithBody { .. },
                ..
            }
        )),
        1
    );
    session(&runtime)
        .on_tool_result_identified(call("fetch", 1), Some("toolu-1".to_string()), failed())
        .await
        .expect("the same failure reported again does not contradict the close");
    assert_eq!(admitted(&runtime), before);
}

fn operation(name: &str) -> Option<appa_runtime_api::ReturnOperation> {
    Some(appa_runtime_api::ReturnOperation::new(name).expect("an operation is named"))
}

fn held_records(runtime: &Runtime) -> usize {
    let log = runtime.inner.log(&root()).expect("the log reads");
    log.host_records()
        .iter()
        .filter(|record| matches!(record.observation, appa_eventlog::HostObservation::ReturnHeld { .. }))
        .count()
}

fn occurrences(
    runtime: &Runtime,
) -> Vec<(
    u32,
    crate::api::OccurrenceState,
    Option<appa_runtime_api::ReturnOperation>,
)> {
    runtime
        .child_occurrences(&parent(), "toolu-1", &EmbeddedPresentationOptions::default())
        .expect("the family reads")
        .into_iter()
        .map(|occurrence| (occurrence.occurrence, occurrence.state, occurrence.operation))
        .collect()
}

fn notices(runtime: &Runtime) -> Vec<crate::api::DeferredReturnNotice> {
    runtime
        .child_occurrences(&parent(), "toolu-1", &EmbeddedPresentationOptions::default())
        .expect("the family reads")
        .into_iter()
        .map(|occurrence| occurrence.notice.expect("a live parent is told of each occurrence"))
        .collect()
}

/// A background child under a deferred return that narrowed and returned twice without ending,
/// steered between its turns.
async fn two_held_turns(runtime: &Runtime) -> Session {
    let fork = binding(deferring(runtime, call("fetch", 1), "toolu-1").await);
    let child_id = TrajectoryId("cc:root:child".to_string());
    let child = session(runtime)
        .on_child_start(child_id.clone(), SpawnRef::Binding(fork.clone()))
        .expect("the fork binds and the child opens");
    tainted(runtime, &child).await;
    assert!(matches!(
        child.on_child_return("turn one".to_string(), operation("op-1")).await,
        Ok(ChildReturnDecision::Deferred)
    ));
    assert_eq!(child.liveness_of(&child_id).expect("the log reads"), Liveness::Live);
    session(runtime)
        .on_child_start(child_id, SpawnRef::Binding(fork))
        .expect("the parent steers its child");
    assert!(matches!(
        child.on_child_return("turn two".to_string(), operation("op-2")).await,
        Ok(ChildReturnDecision::Deferred)
    ));
    child
}

#[tokio::test]
async fn each_return_a_background_child_makes_without_ending_is_held_at_its_own_occurrence() {
    use crate::api::OccurrenceState::Pending;
    let Deployed { runtime, .. } = deployed(false);
    let child = two_held_turns(&runtime).await;
    assert_eq!(
        facts(&runtime, |fact| matches!(fact, Fact::ReturnDeferred { held: true, .. })),
        2
    );
    assert_eq!(held_records(&runtime), 2);
    assert_eq!(crossings(&runtime), 0);
    assert_eq!(
        occurrences(&runtime),
        vec![(0, Pending, operation("op-1")), (1, Pending, operation("op-2"))]
    );
    let [_, latest] = notices(&runtime).try_into().expect("two occurrences");
    assert_eq!(
        runtime
            .deferred_return(&parent(), "toolu-1", &EmbeddedPresentationOptions::default())
            .expect("the family reads"),
        Some(latest),
        "the spawn's notice is the latest return awaiting the parent"
    );
    assert!(matches!(child.liveness_of(&child.trajectory), Ok(Liveness::Live)));
}

#[tokio::test]
async fn a_retried_return_answers_its_own_occurrence_and_refuses_other_words() {
    let Deployed { runtime, .. } = deployed(false);
    let child = two_held_turns(&runtime).await;
    let before = (deferred_returns(&runtime), held_records(&runtime));

    // The host lost the first answer after its append and retries: the operation names the
    // occurrence it froze, though the child has returned again since.
    assert!(matches!(
        child.on_child_return("turn one".to_string(), operation("op-1")).await,
        Ok(ChildReturnDecision::Deferred)
    ));
    assert!(matches!(
        child.on_child_return("turn two".to_string(), operation("op-2")).await,
        Ok(ChildReturnDecision::Deferred)
    ));
    assert!(matches!(
        child
            .on_child_return("turn one, revised".to_string(), operation("op-1"))
            .await,
        Err(EventError::ReturnRetryChanged)
    ));
    assert_eq!((deferred_returns(&runtime), held_records(&runtime)), before);
}

#[tokio::test]
async fn held_returns_settle_out_of_order_and_repeated_settlements_replay() {
    use crate::api::OccurrenceState::{Accepted, Discarded};
    let Deployed { runtime, .. } = deployed(false);
    two_held_turns(&runtime).await;
    let [first, second] = notices(&runtime).try_into().expect("two occurrences");

    let accepted = settling(&runtime, &parent(), &second.accept).await;
    assert_eq!(crossed_value(&accepted), Some("turn two"));
    let discarded = settling(&runtime, &parent(), &first.discard).await;
    assert!(matches!(discarded, RemedyOutcome::Returned { .. }), "{discarded:?}");
    assert_eq!(
        occurrences(&runtime),
        vec![(0, Discarded, operation("op-1")), (1, Accepted, operation("op-2"))]
    );
    assert_eq!(crossings(&runtime), 1);
    assert_eq!(facts(&runtime, |fact| matches!(fact, Fact::ReturnAccepted { .. })), 1);

    assert_eq!(settling(&runtime, &parent(), &second.discard).await, accepted);
    assert_eq!(settling(&runtime, &parent(), &first.accept).await, discarded);
    assert_eq!(crossings(&runtime), 1);
    assert_eq!(
        notices(&runtime),
        vec![first, second],
        "the notices read the same after the decisions"
    );
}

/// Only a retry of the stop repeats the deferral that ended the child: a turn on the ended
/// child is refused as ended, whatever its words.
#[tokio::test]
async fn a_child_ended_by_its_deferral_repeats_it_at_its_stop_and_refuses_anything_else_as_ended() {
    let Deployed { runtime, .. } = deployed(false);
    let (child, _) = deferred_answer(&runtime).await;
    assert!(matches!(
        child.on_child_end(Some("the answer".to_string())).await,
        Ok(ChildReturnDecision::Deferred)
    ));
    for refused in [
        child.on_child_return("the answer".to_string(), None).await,
        child.on_child_end(Some("another answer".to_string())).await,
        child.on_child_return("another answer".to_string(), None).await,
        child
            .on_child_return("the answer".to_string(), operation("op-late"))
            .await,
    ] {
        assert!(matches!(refused, Err(EventError::TrajectoryEnded)), "{refused:?}");
    }
    assert_eq!(deferred_returns(&runtime), 1);
    assert_eq!(held_records(&runtime), 0);
}

#[tokio::test]
async fn a_spawn_whose_parent_can_decide_records_a_floor_that_holds_what_falls_below_it() {
    let Deployed { runtime, .. } = deployed(false);
    let decides = || DeclaredReturn::Decides(LabelSpelling::default());
    assert!(matches!(
        session(&runtime)
            .on_spawn_declaring(call("fetch", 1), None, None, decides())
            .await,
        Err(EventError::DeferralUnkeyed)
    ));
    binding(
        session(&runtime)
            .on_spawn_declaring(call("fetch", 1), Some("toolu-1".to_string()), None, decides())
            .await,
    );
    assert!(matches!(
        fork_returns(&runtime).as_slice(),
        [ReturnPolicy::Crossing(appa_engine::fact::CrossingReturn {
            sanitizer: None,
            below_floor: appa_engine::fact::BelowFloor::Hold,
            ..
        })]
    ));
}

#[tokio::test]
async fn a_stop_in_a_held_returns_words_freezes_its_own_occurrence_and_ends_the_child() {
    use crate::api::OccurrenceState::Pending;
    let Deployed { runtime, .. } = deployed(false);
    let child = deferred_child(&runtime).await;
    tainted(&runtime, &child).await;
    for answered in [
        child.on_child_return("the answer".to_string(), None).await,
        child.on_child_end(Some("the answer".to_string())).await,
    ] {
        assert!(matches!(answered, Ok(ChildReturnDecision::Deferred)), "{answered:?}");
    }
    assert_eq!(
        child.liveness_of(&child.trajectory).expect("the log reads"),
        Liveness::Ended
    );
    assert_eq!(occurrences(&runtime), vec![(0, Pending, None), (1, Pending, None)]);
    assert_eq!(
        facts(&runtime, |fact| matches!(
            fact,
            Fact::ReturnDeferred { held: false, .. }
        )),
        1
    );
    let [_, terminal] = notices(&runtime).try_into().expect("two occurrences");
    assert_eq!(
        runtime
            .deferred_return(&parent(), "toolu-1", &EmbeddedPresentationOptions::default())
            .expect("the family reads"),
        Some(terminal)
    );
}

#[tokio::test]
async fn an_operation_no_held_return_bound_is_decided_afresh_and_binds_its_own_occurrence() {
    use crate::api::OccurrenceState::Pending;
    let Deployed { runtime, .. } = deployed(false);
    let child = deferred_child(&runtime).await;
    tainted(&runtime, &child).await;
    for op in ["op-1", "op-2"] {
        let held = child.on_child_return("done".to_string(), operation(op)).await;
        assert!(matches!(held, Ok(ChildReturnDecision::Deferred)), "{op}: {held:?}");
    }
    assert_eq!(
        occurrences(&runtime),
        vec![(0, Pending, operation("op-1")), (1, Pending, operation("op-2"))],
        "the same words under a new operation are a new return"
    );
    assert!(matches!(
        child
            .on_child_return("done, revised".to_string(), operation("op-2"))
            .await,
        Err(EventError::ReturnRetryChanged)
    ));
    // At the hook, the changed retry is refused by its class, not blocked as an integrity
    // failure: nothing is in the model's way.
    let hooked = crate::hooks::handle(
        &runtime,
        appa_runtime_api::HookEvent::ChildReturn {
            root: root(),
            child: child.trajectory.clone(),
            value: "done, revised".to_string(),
            operation: operation("op-2"),
        },
    )
    .await;
    assert!(
        matches!(
            hooked,
            appa_runtime_api::HookDecision::Refuse {
                class: appa_runtime_api::RefusalClass::ReturnRetryChanged,
                ..
            }
        ),
        "{hooked:?}"
    );
    assert_eq!(held_records(&runtime), 2);
}

/// A retry of a held return is answered from the family's log on a replica that still serves
/// the revision the family moved past: the same words as the record, other words refused,
/// while a return the replica would have to judge refuses as the policy move.
#[tokio::test]
async fn a_held_returns_retry_is_answered_on_a_replica_behind_the_familys_revision() {
    let deployed = deployed(false);
    let runtime = &deployed.runtime;
    let behind = deployed.replica();
    let child = deferred_child(runtime).await;
    tainted(runtime, &child).await;
    assert!(matches!(
        child.on_child_return("draft".to_string(), operation("op-1")).await,
        Ok(ChildReturnDecision::Deferred)
    ));
    assert!(
        runtime
            .reload(deployed.config(&policy(true)))
            .expect("the policy reloads")
            .changed
    );
    session(runtime)
        .on_tool_call_identified(call("fetch", 2), Some("toolu-2".to_string()), None, None)
        .await
        .expect("the family is stamped at the newer revision");
    let retrying = behind
        .session(&root(), &child.trajectory)
        .expect("the child reattaches on the replica");
    assert!(matches!(
        retrying.on_child_return("draft".to_string(), operation("op-1")).await,
        Ok(ChildReturnDecision::Deferred)
    ));
    assert!(matches!(
        retrying
            .on_child_return("redrafted".to_string(), operation("op-1"))
            .await,
        Err(EventError::ReturnRetryChanged)
    ));
    let judged = retrying.on_child_return("draft".to_string(), operation("op-2")).await;
    assert!(matches!(judged, Err(EventError::PolicyMoved { .. })), "{judged:?}");
    assert_eq!(held_records(runtime), 1);
}

#[tokio::test]
async fn a_held_returns_retry_answers_its_occurrence_after_the_child_ended() {
    let Deployed { runtime, .. } = deployed(false);
    let child = deferred_child(&runtime).await;
    tainted(&runtime, &child).await;
    assert!(matches!(
        child.on_child_return("found".to_string(), operation("op-1")).await,
        Ok(ChildReturnDecision::Deferred)
    ));
    assert!(matches!(
        child.on_child_end(Some("final".to_string())).await,
        Ok(ChildReturnDecision::Deferred)
    ));
    assert_eq!(
        child.liveness_of(&child.trajectory).expect("the log reads"),
        Liveness::Ended
    );
    let before = (deferred_returns(&runtime), held_records(&runtime));
    assert!(matches!(
        child.on_child_return("found".to_string(), operation("op-1")).await,
        Ok(ChildReturnDecision::Deferred)
    ));
    assert!(matches!(
        child
            .on_child_return("found, revised".to_string(), operation("op-1"))
            .await,
        Err(EventError::ReturnRetryChanged)
    ));
    assert_eq!((deferred_returns(&runtime), held_records(&runtime)), before);
}

#[tokio::test]
async fn a_crossed_keyed_return_binds_no_operation_and_its_retry_crosses_again() {
    let Deployed { runtime, .. } = deployed(false);
    let child = deferred_child(&runtime).await;
    // Untainted, each return leaves the parent where it is and crosses at once.
    for (said, op) in [("turn one", "op-1"), ("turn two", "op-2"), ("turn one", "op-1")] {
        assert!(matches!(
            &child
                .on_child_return(said.to_string(), operation(op))
                .await
                .expect("the return crosses"),
            ChildReturnDecision::Returned { value, .. } if value == said
        ));
    }
    assert_eq!((crossings(&runtime), held_records(&runtime)), (3, 0));
}

/// Returns named by their words alone: after a steer, the answer that repeats the words of a
/// held return the parent already decided on is a return of its own. It crosses once the
/// parent took the child's label, and is held again once the parent discarded it.
#[tokio::test]
async fn the_answer_after_a_steer_and_a_decision_is_a_return_of_its_own() {
    for accept in [true, false] {
        let Deployed { runtime, .. } = deployed(false);
        let fork = binding(deferring(&runtime, call("fetch", 1), "toolu-1").await);
        let child_id = TrajectoryId("cc:root:child".to_string());
        let child = session(&runtime)
            .on_child_start(child_id.clone(), SpawnRef::Binding(fork.clone()))
            .expect("the fork binds and the child opens");
        tainted(&runtime, &child).await;
        assert!(matches!(
            child.on_child_return("done".to_string(), None).await,
            Ok(ChildReturnDecision::Deferred)
        ));
        session(&runtime)
            .on_child_start(child_id, SpawnRef::Binding(fork))
            .expect("the parent steers its child");
        let [first] = notices(&runtime).try_into().expect("one occurrence");
        settling(&runtime, &parent(), if accept { &first.accept } else { &first.discard }).await;
        let second = child.on_child_return("done".to_string(), None).await;
        match accept {
            true => {
                assert!(
                    matches!(&second, Ok(ChildReturnDecision::Returned { value, .. }) if value == "done"),
                    "{second:?}"
                );
                assert_eq!(crossings(&runtime), 2);
            }
            false => {
                assert!(matches!(second, Ok(ChildReturnDecision::Deferred)), "{second:?}");
                assert_eq!(occurrences(&runtime).len(), 2);
            }
        }
    }
}

async fn spawn_result_saying(runtime: &Runtime, child: &Session, said: &str) -> SpawnResultDecision {
    session(runtime)
        .on_spawn_result_identified(
            call("fetch", 1),
            Some("toolu-1".to_string()),
            spoken(said),
            Some(child.trajectory.clone()),
            Some(said.to_string()),
            SpawnLaunch::Undeclared,
        )
        .await
        .expect("the spawn's result is decided")
}

/// A held return the parent accepted, then a stop that crosses: the spawn's result repeating
/// the crossing delivers it, and no notice of the earlier decision stands in for it.
#[tokio::test]
async fn a_crossing_after_an_accepted_held_return_is_the_spawns_result() {
    let deferred = deployed(false);
    let child = deferred_child(&deferred.runtime).await;
    tainted(&deferred.runtime, &child).await;
    let deciding = below_its_floor(DeclaredReturn::Decides(middle_floor())).await;
    for (runtime, child) in [(&deferred.runtime, &child), (&deciding.0.runtime, &deciding.1)] {
        assert!(matches!(
            child.on_child_return("one".to_string(), operation("op-1")).await,
            Ok(ChildReturnDecision::Deferred)
        ));
        let [held] = notices(runtime).try_into().expect("one occurrence");
        settling(runtime, &parent(), &held.accept).await;
        let Ok(ChildReturnDecision::Returned { receipt, .. }) = child.on_child_end(Some("two".to_string())).await
        else {
            panic!("the stop crosses");
        };
        assert_eq!(
            runtime
                .deferred_return(&parent(), "toolu-1", &EmbeddedPresentationOptions::default())
                .expect("the family reads"),
            None,
            "a later crossing supersedes the decided return's notice"
        );
        assert_eq!(
            spawn_result_saying(runtime, child, "two").await,
            SpawnResultDecision::Return(ChildReturnDecision::Returned {
                value: "two".to_string(),
                receipt,
            })
        );
    }
}

/// The spawn's notice is its latest return still awaiting the parent's decision, however the
/// parent decided on later ones; once none awaits, the child's latest occurrence.
#[tokio::test]
async fn the_spawns_notice_is_its_latest_pending_return_before_any_decided_one() {
    let Deployed { runtime, .. } = deployed(false);
    let child = deferred_child(&runtime).await;
    tainted(&runtime, &child).await;
    for op in ["op-1", "op-2", "op-3"] {
        assert!(matches!(
            child.on_child_return(format!("answer {op}"), operation(op)).await,
            Ok(ChildReturnDecision::Deferred)
        ));
    }
    let [first, second, third] = notices(&runtime).try_into().expect("three occurrences");
    let notice = || {
        runtime
            .deferred_return(&parent(), "toolu-1", &EmbeddedPresentationOptions::default())
            .expect("the family reads")
    };
    settling(&runtime, &parent(), &third.discard).await;
    settling(&runtime, &parent(), &second.discard).await;
    assert_eq!(notice(), Some(first.clone()));
    settling(&runtime, &parent(), &first.discard).await;
    assert_eq!(notice(), Some(third));
}

/// A parent that ended before deciding on its child's return is quoted no settlement for it
/// by either reader: the spawn's notice and the occurrences read alike.
#[tokio::test]
async fn a_parent_that_ended_is_quoted_no_settlement_for_a_return_it_never_decided() {
    let Deployed { runtime, .. } = deployed(false);
    let child = deferred_child(&runtime).await;
    let fork = binding(
        child
            .on_spawn_declaring(
                call("fetch", 2),
                Some("toolu-g".to_string()),
                None,
                DeclaredReturn::Deferred,
            )
            .await,
    );
    let grandchild = child
        .on_child_start(TrajectoryId("cc:root:grandchild".to_string()), SpawnRef::Binding(fork))
        .expect("the grandchild opens");
    tainted(&runtime, &grandchild).await;
    assert!(matches!(
        grandchild.on_child_return("found".to_string(), None).await,
        Ok(ChildReturnDecision::Deferred)
    ));
    assert!(matches!(
        child.on_child_end(None).await,
        Ok(ChildReturnDecision::NoValue)
    ));
    let acting = Actor {
        root: root(),
        child: Some(child.trajectory.clone()),
    };
    let presentation = EmbeddedPresentationOptions::default();
    assert_eq!(
        runtime
            .deferred_return(&acting, "toolu-g", &presentation)
            .expect("the family reads"),
        None
    );
    let [occurrence] = runtime
        .child_occurrences(&acting, "toolu-g", &presentation)
        .expect("the family reads")
        .try_into()
        .expect("one occurrence");
    assert_eq!(
        (occurrence.state, occurrence.notice),
        (crate::api::OccurrenceState::ParentEnded, None)
    );
}

/// A return the model declared ahead covers the next spawn whatever the host declares on it: a
/// spawn whose parent decides takes it as a floor that holds what falls below it.
#[tokio::test]
async fn a_deciding_spawn_takes_a_return_declared_ahead_as_a_floor_its_parent_decides_below() {
    let Deployed { runtime, .. } = deployed(true);
    let acting = Actor {
        root: root(),
        child: None,
    };
    assert!(matches!(
        runtime
            .declare_return_ahead(&acting, as_spoken(), RemedyAttempt("r1".to_string()))
            .await,
        RemedyOutcome::Returned { .. }
    ));
    binding(
        session(&runtime)
            .on_spawn_declaring(
                call("fetch", 1),
                Some("toolu-1".to_string()),
                None,
                DeclaredReturn::Decides(LabelSpelling::default()),
            )
            .await,
    );
    let ahead = AheadState::fold(runtime.inner.log(&root()).expect("the log reads").host_records());
    assert_eq!(
        ahead.settlement(&root(), &AheadAttempt::Host("r1".to_string())),
        Some(AheadOutcome::Applied)
    );
    let [ReturnPolicy::Crossing(crossing)] = <[_; 1]>::try_from(fork_returns(&runtime)).expect("one fork") else {
        panic!("the fork takes a crossing");
    };
    assert_eq!(crossing.below_floor, appa_engine::fact::BelowFloor::Hold);
}

#[tokio::test]
async fn a_return_made_through_child_return_reads_back_as_the_notice_through_the_spawn_result() {
    let Deployed { runtime, .. } = deployed(false);
    let child = deferred_child(&runtime).await;
    tainted(&runtime, &child).await;
    // An in-process subagent offers its final answer through child_return, and retries it.
    for _ in 0..2 {
        assert!(matches!(
            child.on_child_return("the answer".to_string(), None).await,
            Ok(ChildReturnDecision::Deferred)
        ));
    }
    let presentation = EmbeddedPresentationOptions::default();
    let notice = runtime
        .deferred_return(&parent(), "toolu-1", &presentation)
        .expect("the family reads")
        .expect("the parent is told its subagent's answer waits for its decision");
    let before = admitted(&runtime);
    let decided = session(&runtime)
        .on_spawn_result_identified(
            call("fetch", 1),
            Some("toolu-1".to_string()),
            spoken("the answer"),
            Some(child.trajectory.clone()),
            Some("the answer".to_string()),
            SpawnLaunch::Undeclared,
        )
        .await
        .expect("the spawn's result is decided");
    assert_eq!(
        decided,
        SpawnResultDecision::Outcome(ToolResultDecision::Replace {
            placeholder: notice.feedback.clone(),
            presentation: None,
        })
    );
    assert_eq!(admitted(&runtime), before);
    assert_eq!(
        crossed_value(&settling(&runtime, &parent(), &notice.accept).await),
        Some("the answer")
    );
    assert_eq!(
        runtime
            .deferred_return(&parent(), "toolu-1", &presentation)
            .expect("the family reads"),
        Some(notice),
        "the spawn's result keeps the notice after the decision"
    );
}

#[tokio::test]
async fn a_held_return_whose_parent_ended_can_never_be_settled() {
    use crate::api::OccurrenceState::ParentEnded;
    let Deployed { runtime, .. } = deployed(false);
    let child = deferred_child(&runtime).await;
    let fork = binding(
        child
            .on_spawn_declaring(
                call("fetch", 2),
                Some("toolu-g".to_string()),
                None,
                DeclaredReturn::Deferred,
            )
            .await,
    );
    let grandchild = child
        .on_child_start(TrajectoryId("cc:root:grandchild".to_string()), SpawnRef::Binding(fork))
        .expect("the grandchild opens");
    tainted(&runtime, &grandchild).await;
    assert!(matches!(
        grandchild.on_child_return("found".to_string(), None).await,
        Ok(ChildReturnDecision::Deferred)
    ));
    let acting = Actor {
        root: root(),
        child: Some(child.trajectory.clone()),
    };
    let presentation = EmbeddedPresentationOptions::default();
    let pending = runtime
        .child_occurrences(&acting, "toolu-g", &presentation)
        .expect("the family reads");
    let notice = pending[0].notice.clone().expect("a live parent is told of its return");
    assert!(matches!(
        child.on_child_end(None).await,
        Ok(ChildReturnDecision::NoValue)
    ));
    let ended = runtime
        .child_occurrences(&acting, "toolu-g", &presentation)
        .expect("the family reads");
    assert_eq!(
        ended
            .iter()
            .map(|occurrence| (occurrence.state, occurrence.notice.is_none()))
            .collect::<Vec<_>>(),
        vec![(ParentEnded, true)]
    );
    assert!(!matches!(
        settling(&runtime, &acting, &notice.accept).await,
        RemedyOutcome::Returned { .. }
    ));
}

/// A policy whose chain names a middle rank, and a read that lowers trust to it.
const MIDDLE: &str = r#"
[policy]
version = 2
trust_chain = ["suspicious", "middle", "trusted"]

[[policy.tool]]
name = "fetch"
parameters = { type = "object", properties = { a = { type = "integer" } } }
delta = {}

[[policy.tool]]
name = "middling"
parameters = { type = "object", properties = { a = { type = "integer" } } }
delta = { trust = "middle" }
"#;

/// [`MIDDLE`] without its middle rank: a child at it falls to the lowest rank, and a floor at
/// it rises to the highest.
const NO_MIDDLE: &str = r#"
[policy]
version = 2
trust_chain = ["suspicious", "trusted"]

[[policy.tool]]
name = "fetch"
parameters = { type = "object", properties = { a = { type = "integer" } } }
delta = {}

[[policy.tool]]
name = "middling"
parameters = { type = "object", properties = { a = { type = "integer" } } }
delta = { trust = "suspicious" }
"#;

/// A child spawned under `declares`, floored at the middle rank, which read down to it; then
/// the policy drops that rank, so whatever the child returns stands below its floor.
async fn below_its_floor(declares: DeclaredReturn) -> (Deployed, Session) {
    let deployed = deployed_serving(&format!("{MIDDLE}{DEPLOYMENT}"));
    let fork = binding(
        session(&deployed.runtime)
            .on_spawn_declaring(call("fetch", 1), Some("toolu-1".to_string()), None, declares)
            .await,
    );
    let child = session(&deployed.runtime)
        .on_child_start(TrajectoryId("cc:root:child".to_string()), SpawnRef::Binding(fork))
        .expect("the fork binds and the child opens");
    let read = call("middling", 1);
    let blocked = child
        .on_tool_call_identified(read.clone(), Some("toolu-c1".to_string()), None, None)
        .await
        .expect("the child's call is judged");
    let ToolCallDecision::Deny { offers, .. } = &blocked else {
        panic!("the narrowing blocks the child's call: {blocked:?}");
    };
    let log = deployed.runtime.inner.log(&root()).expect("the log reads");
    let narrowing = crate::engine::resolve_rendered(&log, &OfferId(offers[0].id.clone())).expect("the offer resolves");
    assert!(matches!(
        child.on_remedy(narrowing, RemedyArguments::default(), None, None).await,
        Ok(RemedyDecision::Authorized { .. })
    ));
    assert!(matches!(
        child
            .on_tool_call_identified(read.clone(), Some("toolu-c2".to_string()), None, None)
            .await,
        Ok(ToolCallDecision::Allow { .. })
    ));
    child
        .on_tool_result_identified(read, Some("toolu-c2".to_string()), spoken("middling"))
        .await
        .expect("the result is admitted");
    deployed
        .runtime
        .reload(deployed.config(&format!("{NO_MIDDLE}{DEPLOYMENT}")))
        .expect("the policy reloads");
    (deployed, child)
}

fn middle_floor() -> LabelSpelling {
    LabelSpelling {
        trust: Some("middle".to_string()),
        audience: None,
    }
}

#[tokio::test]
async fn a_return_below_a_deciding_spawns_floor_is_held_accepted_and_its_stop_ends_the_child() {
    use crate::api::OccurrenceState::{Accepted, Pending};
    let (deployed, child) = below_its_floor(DeclaredReturn::Decides(middle_floor())).await;
    let runtime = &deployed.runtime;
    assert!(matches!(
        child.on_child_return("found".to_string(), operation("op-1")).await,
        Ok(ChildReturnDecision::Deferred)
    ));
    assert_eq!((crossings(runtime), held_records(runtime)), (0, 1));
    let [held] = notices(runtime).try_into().expect("one occurrence");
    assert_eq!(
        crossed_value(&settling(runtime, &parent(), &held.accept).await),
        Some("found")
    );
    // The parent took the child's label: its stop crosses, and the child ends.
    assert!(matches!(
        &child
            .on_child_end(Some("done".to_string()))
            .await
            .expect("the stop crosses"),
        ChildReturnDecision::Returned { value, .. } if value == "done"
    ));
    assert_eq!(occurrences(runtime), vec![(0, Accepted, operation("op-1"))]);
    assert_eq!(crossings(runtime), 2);

    let (deployed, child) = below_its_floor(DeclaredReturn::Decides(middle_floor())).await;
    let runtime = &deployed.runtime;
    assert!(matches!(
        child.on_child_end(Some("final".to_string())).await,
        Ok(ChildReturnDecision::Deferred)
    ));
    assert_eq!(
        child.liveness_of(&child.trajectory).expect("the log reads"),
        Liveness::Ended
    );
    assert_eq!(occurrences(runtime), vec![(0, Pending, None)]);
    assert!(
        runtime
            .deferred_return(&parent(), "toolu-1", &EmbeddedPresentationOptions::default())
            .expect("the family reads")
            .is_some(),
        "the stop's deferral is the spawn's notice"
    );
}

#[tokio::test]
async fn a_return_below_a_plain_spawns_floor_is_refused_and_nothing_is_held() {
    let (deployed, child) = below_its_floor(DeclaredReturn::Arguments(RemedyArguments {
        label: Some(middle_floor()),
        return_schema: None,
    }))
    .await;
    let refused = child.on_child_return("found".to_string(), operation("op-1")).await;
    assert!(
        matches!(refused, Ok(ChildReturnDecision::Blocked { .. })),
        "{refused:?}"
    );
    assert_eq!(deferred_returns(&deployed.runtime), 0);
}

#[tokio::test]
async fn a_deciding_spawn_whose_return_crossed_delivers_its_result_as_a_plain_spawn_does() {
    let delivered = |declares: DeclaredReturn| async move {
        let Deployed { runtime, .. } = deployed(false);
        let fork = binding(
            session(&runtime)
                .on_spawn_declaring(call("fetch", 1), Some("toolu-1".to_string()), None, declares)
                .await,
        );
        let child = session(&runtime)
            .on_child_start(TrajectoryId("cc:root:child".to_string()), SpawnRef::Binding(fork))
            .expect("the fork binds and the child opens");
        let stopped = child
            .on_child_end(Some("done".to_string()))
            .await
            .expect("the stop is judged");
        let result = session(&runtime)
            .on_spawn_result_identified(
                call("fetch", 1),
                Some("toolu-1".to_string()),
                spoken("done"),
                Some(child.trajectory.clone()),
                Some("done".to_string()),
                SpawnLaunch::Undeclared,
            )
            .await;
        // Each log places its records apart, so the deliveries compare without their receipts;
        // within one log the spawn result replays the stop's crossing.
        let ChildReturnDecision::Returned { value, receipt } = stopped else {
            panic!("the stop crosses: {stopped:?}");
        };
        let replayed = result.map(|decided| match decided {
            SpawnResultDecision::Return(ChildReturnDecision::Returned {
                value,
                receipt: replayed,
            }) => (replayed == receipt).then_some(value),
            _ => None,
        });
        (value, replayed.map_err(|error| error.to_string()), admitted(&runtime))
    };
    let plain = delivered(DeclaredReturn::Arguments(as_spoken())).await;
    assert_eq!((plain.0.as_str(), &plain.1), ("done", &Ok(Some("done".to_string()))));
    assert_eq!(
        delivered(DeclaredReturn::Decides(LabelSpelling::default())).await,
        plain
    );
}

#[tokio::test]
async fn a_child_under_a_floor_of_its_own_cannot_spawn_a_deciding_subagent() {
    let Deployed { runtime, .. } = deployed(false);
    let fork = binding(declaring(&runtime, call("fetch", 1), "toolu-1", as_spoken()).await);
    let child = session(&runtime)
        .on_child_start(TrajectoryId("cc:root:child".to_string()), SpawnRef::Binding(fork))
        .expect("the fork binds and the child opens");
    let deciding = child
        .on_spawn_declaring(
            call("fetch", 3),
            Some("toolu-3".to_string()),
            None,
            DeclaredReturn::Decides(LabelSpelling::default()),
        )
        .await;
    assert!(matches!(deciding, Ok(ToolCallDecision::Deny { .. })), "{deciding:?}");
    assert!(
        !fork_returns(&runtime).iter().any(ReturnPolicy::holds_below_floor),
        "no fork holds what falls below its floor"
    );
}

/// A spawn reported with no message, kagent's `result` empty beside keys nothing checked,
/// by a host whose launch reply has the fields `launch` keeps.
async fn reported_without_a_message(runtime: &Runtime, child: &Session, launch: SpawnLaunch) -> SpawnResultDecision {
    let reply = serde_json::json!({"result": "", "subagent_session_id": "child-ctx", "note": "the child's bytes"});
    reported(runtime, child, &reply, launch).await
}

async fn reported(
    runtime: &Runtime,
    child: &Session,
    reply: &serde_json::Value,
    launch: SpawnLaunch,
) -> SpawnResultDecision {
    session(runtime)
        .on_spawn_result_identified(
            call("fetch", 1),
            Some("toolu-1".to_string()),
            spoken(&reply.to_string()),
            Some(child.trajectory.clone()),
            None,
            launch,
        )
        .await
        .expect("the spawn's result is decided")
}

fn kagent() -> SpawnLaunch {
    appa_adapter_kagent::adapter().spawn_launch
}

/// The bodies of every value the root admitted, in log order.
fn admitted_bodies(runtime: &Runtime) -> Vec<String> {
    let log = runtime.inner.log(&root()).expect("the log reads");
    log.facts()
        .iter()
        .filter_map(|fact| match fact {
            Fact::ValueAdmitted { value, .. } => Some(value.body.as_str().to_string()),
            _ => None,
        })
        .collect()
}

/// A spawn reply with no message from a child that already returned closes with no body and
/// admits none of its bytes: the harness is answered with the runtime's words in its place.
#[tokio::test]
async fn a_returned_childs_spawn_reply_without_a_message_admits_nothing() {
    let Deployed { runtime, .. } = deployed(false);
    let child = bound_child(&runtime, 1).await;
    crossing(&child, "the answer", "op-1").await;
    let before = admitted(&runtime);
    assert_eq!(
        reported_without_a_message(&runtime, &child, kagent()).await,
        SpawnResultDecision::NoReturn {
            output: super::result::NO_RETURN.to_string(),
        }
    );
    assert_eq!(admitted(&runtime), before);
    assert!(
        runtime.open_dispatches(&root(), &root()).is_empty(),
        "the reply closed the spawn"
    );
}

/// A spawn reply with no message from a child that still runs with nothing returned is its
/// launch: it closes on the host's launch fields alone, so nothing beside them — the child's
/// bytes in `note` included — reaches the parent, and a repeat of it once the child returned
/// answers from that close.
#[tokio::test]
async fn a_launch_admits_only_the_hosts_launch_fields_and_its_repeat_answers_from_the_close() {
    let Deployed { runtime, .. } = deployed(false);
    let child = bound_child(&runtime, 1).await;
    let before = admitted_bodies(&runtime);
    let fields = serde_json::json!({"subagent_session_id": "child-1"}).to_string();
    let launched = reported_without_a_message(&runtime, &child, kagent()).await;
    assert!(
        matches!(&launched, SpawnResultDecision::Launched { output, receipt: Some(_) } if *output == fields),
        "{launched:?}"
    );
    assert_eq!(admitted_bodies(&runtime)[before.len()..], [fields]);
    crossing(&child, "the answer", "op-1").await;
    let crossed = admitted(&runtime);
    assert_eq!(reported_without_a_message(&runtime, &child, kagent()).await, launched);
    assert_eq!(admitted(&runtime), crossed, "the repeat admits nothing anew");
}

/// A host that declares no launch fields has its launch admit nothing: the runtime's words
/// stand in for the reply.
#[tokio::test]
async fn a_launch_under_a_host_declaring_no_launch_fields_admits_nothing() {
    let Deployed { runtime, .. } = deployed(false);
    let child = bound_child(&runtime, 1).await;
    let before = admitted(&runtime);
    assert_eq!(
        reported_without_a_message(&runtime, &child, SpawnLaunch::Undeclared).await,
        SpawnResultDecision::Launched {
            output: super::result::LAUNCHED.to_string(),
            receipt: None,
        }
    );
    assert_eq!(admitted(&runtime), before);
    assert!(
        runtime.open_dispatches(&root(), &root()).is_empty(),
        "the reply closed the spawn"
    );
}

/// A launch whose fields are past the size limit closes on nothing and is answered with the
/// limit's notice, and so is its repeat once the child returned.
#[tokio::test]
async fn a_launch_past_the_size_limit_answers_with_the_limit_and_so_does_its_repeat() {
    let Deployed { runtime, .. } = deployed(false);
    let child = bound_child(&runtime, 1).await;
    let reply = serde_json::json!({
        "isAsync": true,
        "status": "async_launched",
        "agentId": "child-1",
        "prompt": "p".repeat(70_000),
    });
    let launch = appa_adapter_claude_code::adapter().spawn_launch;
    let before = admitted(&runtime);
    let launched = reported(&runtime, &child, &reply, launch).await;
    assert!(
        matches!(
            &launched,
            SpawnResultDecision::Outcome(ToolResultDecision::Replace { .. })
        ),
        "{launched:?}"
    );
    crossing(&child, "the answer", "op-1").await;
    let crossed = admitted(&runtime);
    assert_eq!(reported(&runtime, &child, &reply, launch).await, launched);
    assert_eq!(admitted(&runtime), crossed);
    assert_eq!(crossed, before + 1, "only the child's return was admitted");
}

/// [`MIDDLE`]'s `fetch` behind a `signoff` the authority at `url` rules on.
fn signed_off(url: &str) -> String {
    format!(
        r#"
[policy]
version = 2
trust_chain = ["suspicious", "middle", "trusted"]

[[policy.tool]]
name = "fetch"
parameters = {{ type = "object", properties = {{ a = {{ type = "integer" }} }} }}
requires = {{ attention = ["signoff"] }}
delta = {{}}

[[policy.authority]]
name = "signoff"
[policy.authority.permits]
attention = ["signoff"]
{DEPLOYMENT}
[externals.authorities.signoff]
url = "{url}"
"#
    )
}

fn deciding_at(trust: &str) -> DeclaredReturn {
    DeclaredReturn::Decides(LabelSpelling {
        trust: Some(trust.to_string()),
        audience: None,
    })
}

/// A deciding spawn of `fetch` declared under `declares`, blocked on its signoff, which is then
/// approved: the call stays unreleased.
async fn signed_off_unreleased(runtime: &Runtime, call_id: &str, declares: DeclaredReturn) {
    let blocked = session(runtime)
        .on_spawn_declaring(call("fetch", 1), Some(call_id.to_string()), None, declares)
        .await
        .expect("the spawn is judged");
    let ToolCallDecision::Deny { offers, .. } = &blocked else {
        panic!("the signoff blocks the spawn: {blocked:?}");
    };
    let [signoff] = offers.as_slice() else {
        panic!("the declaration stands and the signoff alone is offered: {offers:?}");
    };
    let log = runtime.inner.log(&root()).expect("the log reads");
    let signoff = crate::engine::resolve_rendered(&log, &OfferId(signoff.id.clone())).expect("the offer resolves");
    assert!(matches!(
        session(runtime)
            .on_remedy(signoff, RemedyArguments::default(), None, None)
            .await,
        Ok(RemedyDecision::Authorized { .. })
    ));
}

fn fork_floors(runtime: &Runtime) -> Vec<appa_engine::label::Trust> {
    fork_returns(runtime)
        .iter()
        .map(|policy| match policy {
            ReturnPolicy::Crossing(crossing) if crossing.below_floor == appa_engine::fact::BelowFloor::Hold => {
                crossing.floor.trust
            }
            other => panic!("a deciding spawn's fork holds below its floor: {other:?}"),
        })
        .collect()
}

/// A deciding spawn's signoff approved under the `suspicious` floor is not spent by the same call
/// proposed under the `middle` floor: that proposal declares for itself, and its own signoff
/// releases it under the `middle` floor. The call proposed again under the `suspicious` floor
/// spends the approval made under it, and a replica answers each release's retry from the log.
#[tokio::test]
async fn a_deciding_spawns_approval_under_one_floor_is_not_spent_under_another() {
    let url = super::real_engine_tests::stub(serde_json::json!({"ruling": "approve"})).await;
    let deployed = deployed_serving(&signed_off(&url));
    let runtime = &deployed.runtime;
    let rank = |name: &str| {
        session(runtime)
            .with_view(|context| Ok(context.engine().trust_chain().rank_of(name)))
            .expect("the chain reads")
            .expect("the chain names the rank")
    };
    signed_off_unreleased(runtime, "toolu-1", deciding_at("suspicious")).await;
    signed_off_unreleased(runtime, "toolu-2", deciding_at("middle")).await;
    assert_eq!((spent(runtime).len(), dispatches(runtime)), (0, 0));
    assert_eq!(declarations(runtime), 2, "each floor declared for itself");

    let middle = binding(
        session(runtime)
            .on_spawn_declaring(
                call("fetch", 1),
                Some("toolu-3".to_string()),
                None,
                deciding_at("middle"),
            )
            .await,
    );
    assert_eq!(fork_floors(runtime), vec![rank("middle")]);
    session(runtime)
        .on_child_start(
            TrajectoryId("cc:root:middle".to_string()),
            SpawnRef::Binding(middle.clone()),
        )
        .expect("the middle child starts");

    let suspicious = binding(
        session(runtime)
            .on_spawn_declaring(
                call("fetch", 1),
                Some("toolu-4".to_string()),
                None,
                deciding_at("suspicious"),
            )
            .await,
    );
    assert_eq!(fork_floors(runtime), vec![rank("middle"), rank("suspicious")]);
    let spent = spent(runtime);
    assert_eq!(
        spent.iter().collect::<std::collections::BTreeSet<_>>().len(),
        spent.len(),
        "no approval was spent twice: {spent:?}"
    );

    // A replica answers each release's retry from the log, and a retry under the other floor
    // is another act.
    let replica = deployed.replica();
    let retried = |call_id: &'static str, floor: &'static str| {
        let session = replica.session(&root(), &root()).expect("the session reattaches");
        async move {
            session
                .on_spawn_declaring(call("fetch", 1), Some(call_id.to_string()), None, deciding_at(floor))
                .await
        }
    };
    assert!(matches!(
        retried("toolu-3", "middle").await,
        Ok(ToolCallDecision::Allow { .. })
    ));
    assert_eq!(binding(retried("toolu-4", "suspicious").await), suspicious);
    assert!(matches!(
        retried("toolu-4", "middle").await,
        Err(EventError::CallIdReused)
    ));
    assert_eq!(dispatches(runtime), 2);
}

#[tokio::test]
async fn a_deciding_spawn_whose_floor_the_policy_does_not_spell_is_refused_and_writes_nothing() {
    let Deployed { runtime, .. } = deployed(false);
    let refused = session(&runtime)
        .on_spawn_declaring(
            call("fetch", 1),
            Some("toolu-1".to_string()),
            None,
            deciding_at("middle"),
        )
        .await;
    assert!(
        matches!(&refused, Err(error @ EventError::FloorUnspelled { .. }) if error.is_operational()),
        "{refused:?}"
    );
    assert_eq!((approvals(&runtime), dispatches(&runtime)), (0, 0));
}

/// Run `taint` in the root, accepting the narrowing its block offers.
async fn tainted_root(runtime: &Runtime) {
    let parent = session(runtime);
    let taint = call("taint", 7);
    let blocked = parent
        .on_tool_call_identified(taint.clone(), Some("toolu-p1".to_string()), None, None)
        .await
        .expect("the root's call is judged");
    let ToolCallDecision::Deny { offers, .. } = &blocked else {
        panic!("the narrowing blocks the root's call: {blocked:?}");
    };
    let log = runtime.inner.log(&root()).expect("the log reads");
    let narrowing = crate::engine::resolve_rendered(&log, &OfferId(offers[0].id.clone())).expect("the offer resolves");
    assert!(matches!(
        parent
            .on_remedy(narrowing, RemedyArguments::default(), None, None)
            .await,
        Ok(RemedyDecision::Authorized { .. })
    ));
    assert!(matches!(
        parent
            .on_tool_call_identified(taint.clone(), Some("toolu-p2".to_string()), None, None)
            .await,
        Ok(ToolCallDecision::Allow { .. })
    ));
    parent
        .on_tool_result_identified(
            taint,
            Some("toolu-p2".to_string()),
            ToolOutcome::Success {
                body: OutcomeBody::Available("tainted too".to_string()),
            },
        )
        .await
        .expect("the result is admitted");
}

/// A stop replaying the child's latest crossing carries that crossing's receipt, even where the
/// parent accepted an earlier held occurrence after it: the receipt names the occurrence the
/// decision replayed, never the child's latest admission in log order.
#[tokio::test]
async fn a_replayed_return_carries_the_receipt_of_the_occurrence_it_replays() {
    use appa_runtime_api::ReleaseDigest;
    let Deployed { runtime, .. } = deployed(false);
    let child = deferred_child(&runtime).await;
    tainted(&runtime, &child).await;
    assert!(matches!(
        child.on_child_return("turn one".to_string(), operation("op-1")).await,
        Ok(ChildReturnDecision::Deferred)
    ));
    // The parent narrows itself to where the child stands, so the child's next return moves
    // nothing and crosses at once.
    tainted_root(&runtime).await;
    let second = child
        .on_child_return("turn two".to_string(), operation("op-2"))
        .await
        .expect("the return is judged");
    let ChildReturnDecision::Returned {
        value,
        receipt: crossed,
    } = second
    else {
        panic!("the return crosses at once: {second:?}");
    };
    assert_eq!(value, "turn two");
    assert_eq!(crossed.digest, ReleaseDigest::of(b"turn two"));

    // The parent now accepts the earlier held occurrence.
    let [first] = notices(&runtime).try_into().expect("one held occurrence");
    assert_eq!(
        crossed_value(&settling(&runtime, &parent(), &first.accept).await),
        Some("turn one")
    );

    // The child stops repeating its latest return: the engine replays "turn two".
    let stopped = child
        .on_child_end(Some("turn two".to_string()))
        .await
        .expect("the stop is judged");
    assert_eq!(
        stopped,
        ChildReturnDecision::Returned {
            value: "turn two".to_string(),
            receipt: crossed,
        },
        "the replay names the record that crossed the bytes it returns"
    );
}

/// A crossed return's receipt is judged at the label the child's value crossed at, not
/// above it.
#[tokio::test]
async fn a_crossed_returns_receipt_is_judged_at_the_label_it_crossed_at() {
    const SINK: &str = r#"
[[policy.tool]]
name = "post"
requires = { trust = "trusted" }
delta = {}
"#;
    let Deployed { runtime, dir: _dir } = deployed_serving(&format!("{TOOLS}{SINK}{DEPLOYMENT}"));
    let child = deferred_child(&runtime).await;
    tainted(&runtime, &child).await;
    assert!(matches!(
        child.on_child_return("turn one".to_string(), operation("op-1")).await,
        Ok(ChildReturnDecision::Deferred)
    ));
    tainted_root(&runtime).await;
    let second = child
        .on_child_return("turn two".to_string(), operation("op-2"))
        .await
        .expect("the return is judged");
    let ChildReturnDecision::Returned { receipt: crossed, .. } = second else {
        panic!("the return crosses at once: {second:?}");
    };
    let no_args = raw(serde_json::json!({}));
    assert_eq!(
        runtime.judge_release(&[crossed], "post", &no_args).await,
        Err(appa_runtime_api::ReleaseRefusal::HardBlock),
        "a suspicious child's return does not reach a sink that requires trusted content"
    );
}

/// A bound child of a fresh `fetch(a)` spawn released under the call id `toolu-<a>`.
async fn bound_child(runtime: &Runtime, a: i64) -> Session {
    let call_id = format!("toolu-{a}");
    let fork = binding(declaring(runtime, call("fetch", a), &call_id, as_spoken()).await);
    session(runtime)
        .on_child_start(TrajectoryId(format!("cc:root:child-{a}")), SpawnRef::Binding(fork))
        .expect("the fork binds and the child opens")
}

async fn crossing(child: &Session, said: &str, op: &str) -> appa_runtime_api::ReleaseReceipt {
    match child.on_child_return(said.to_string(), operation(op)).await {
        Ok(ChildReturnDecision::Returned { receipt, .. }) => receipt,
        other => panic!("the return crosses: {other:?}"),
    }
}

async fn spawn_reported(runtime: &Runtime, child: &Session, a: i64, said: &str) -> SpawnResultDecision {
    session(runtime)
        .on_spawn_result_identified(
            call("fetch", a),
            Some(format!("toolu-{a}")),
            spoken(said),
            Some(child.trajectory.clone()),
            Some(said.to_string()),
            SpawnLaunch::Undeclared,
        )
        .await
        .expect("the spawn result is decided")
}

/// A repeated spawn result answers with the crossing its first report replayed, though the
/// child crossed the same bytes again since.
#[tokio::test]
async fn a_repeated_spawn_result_replays_the_crossing_its_first_report_replayed() {
    let Deployed { runtime, .. } = deployed(false);
    let child = bound_child(&runtime, 1).await;
    let first = crossing(&child, "x", "op-1").await;
    let replayed = spawn_reported(&runtime, &child, 1, "x").await;
    assert_eq!(
        replayed,
        SpawnResultDecision::Return(ChildReturnDecision::Returned {
            value: "x".to_string(),
            receipt: first.clone(),
        })
    );
    let again = crossing(&child, "x", "op-2").await;
    assert_ne!(again, first, "the child's second crossing is another record");
    assert_eq!(spawn_reported(&runtime, &child, 1, "x").await, replayed);
}

/// A spawn result whose message no crossing stood for stays withheld when repeated, though
/// the child crossed those bytes since.
#[tokio::test]
async fn a_repeated_withheld_spawn_result_stays_withheld_after_the_child_crosses_its_bytes() {
    let Deployed { runtime, .. } = deployed(false);
    let child = bound_child(&runtime, 1).await;
    let withheld = spawn_reported(&runtime, &child, 1, "y").await;
    assert!(
        matches!(
            withheld,
            SpawnResultDecision::Return(ChildReturnDecision::Blocked { .. })
        ),
        "{withheld:?}"
    );
    crossing(&child, "y", "op-1").await;
    assert_eq!(spawn_reported(&runtime, &child, 1, "y").await, withheld);
}

/// A spawn result repeating its child's crossing answers with that crossing. A body the report
/// carries beyond the crossed value was never checked, so the value is delivered in its place;
/// a report of the value alone is the crossing itself.
#[tokio::test]
async fn a_spawn_result_carrying_more_than_its_crossed_value_delivers_the_value_in_its_place() {
    for (body, delivers) in [
        (OutcomeBody::Available("x".to_string()), false),
        (OutcomeBody::Unavailable, false),
        (OutcomeBody::Available("x, and bytes no stop checked".to_string()), true),
        (OutcomeBody::Available("x".repeat(70_000)), true),
    ] {
        let Deployed { runtime, .. } = deployed(false);
        let child = bound_child(&runtime, 1).await;
        let receipt = crossing(&child, "x", "op-1").await;
        let reported = session(&runtime)
            .on_spawn_result_identified(
                call("fetch", 1),
                Some("toolu-1".to_string()),
                ToolOutcome::Success { body: body.clone() },
                Some(child.trajectory.clone()),
                Some("x".to_string()),
                SpawnLaunch::Undeclared,
            )
            .await
            .expect("the spawn result is decided");
        let value = "x".to_string();
        let expected = match delivers {
            true => SpawnResultDecision::Outcome(ToolResultDecision::Deliver { value, receipt }),
            false => SpawnResultDecision::Return(ChildReturnDecision::Returned { value, receipt }),
        };
        assert_eq!(reported, expected, "{body:?}");
    }
}

fn deciding_here() -> DeclaredReturn {
    DeclaredReturn::Decides(LabelSpelling::default())
}

async fn retried_spawn(
    runtime: &Runtime,
    call_id: &str,
    declares: DeclaredReturn,
) -> Result<ToolCallDecision, EventError> {
    session(runtime)
        .on_spawn_declaring(call("fetch", 1), Some(call_id.to_string()), None, declares)
        .await
}

/// A released deciding spawn's floor was resolved at the parent's label when it was proposed: a
/// retry after the parent narrowed answers that release, and a retry naming another floor
/// there is another act.
#[tokio::test]
async fn a_released_deciding_spawns_retry_answers_its_release_after_the_parents_label_moved() {
    let Deployed { runtime, .. } = deployed(false);
    let released = binding(retried_spawn(&runtime, "toolu-1", deciding_here()).await);
    tainted(&runtime, &session(&runtime)).await;
    assert_eq!(
        binding(retried_spawn(&runtime, "toolu-1", deciding_here()).await),
        released
    );
    assert!(matches!(
        retried_spawn(&runtime, "toolu-1", deciding_at("suspicious")).await,
        Err(EventError::CallIdReused)
    ));
    assert_eq!(forks(&runtime), 1);
}

/// A spawn released under a chain a reload renumbers, or drops its floor's rank from, answers
/// its retry with the release, whatever return it declares.
#[tokio::test]
async fn a_released_spawns_retry_after_a_reload_that_renumbers_the_chain_answers_its_release() {
    for declares in [
        DeclaredReturn::Arguments(as_spoken()),
        DeclaredReturn::Deferred,
        deciding_here(),
        deciding_at("trusted"),
        deciding_at("middle"),
        deciding_at("suspicious"),
    ] {
        let deployed = deployed_serving(&format!("{MIDDLE}{DEPLOYMENT}"));
        let runtime = &deployed.runtime;
        let released = binding(retried_spawn(runtime, "toolu-1", declares.clone()).await);
        runtime
            .reload(deployed.config(&format!("{NO_MIDDLE}{DEPLOYMENT}")))
            .expect("the policy reloads");
        assert_eq!(
            binding(retried_spawn(runtime, "toolu-1", declares.clone()).await),
            released,
            "{declares:?}"
        );
        assert_eq!(forks(runtime), 1, "{declares:?}");
    }
}

/// A denied deciding spawn's retry under the floor it was denied under is answered from the
/// denial; under another floor, the same call id is another act.
#[tokio::test]
async fn a_denied_deciding_spawns_retry_under_another_floor_reuses_its_call_id() {
    let url = super::real_engine_tests::stub(serde_json::json!({"ruling": "approve"})).await;
    let deployed = deployed_serving(&signed_off(&url));
    let runtime = &deployed.runtime;
    let denied = |decision: Result<ToolCallDecision, EventError>| match decision {
        Ok(ToolCallDecision::Deny { offers, .. }) => offers,
        other => panic!("the signoff blocks the spawn: {other:?}"),
    };
    let offers = denied(retried_spawn(runtime, "toolu-1", deciding_at("suspicious")).await);
    assert_eq!(
        denied(retried_spawn(runtime, "toolu-1", deciding_at("suspicious")).await),
        offers
    );
    assert!(matches!(
        retried_spawn(runtime, "toolu-1", deciding_at("middle")).await,
        Err(EventError::CallIdReused)
    ));
    assert_eq!(declarations(runtime), 1);
}

/// A deployment that does not control subagent context releases a deciding spawn unmarked,
/// whatever floor its host names: no floor is resolved there.
#[tokio::test]
async fn a_deployment_without_context_control_releases_a_deciding_spawn_whatever_floor_it_names() {
    let Deployed { runtime, .. } = deployed_serving(&format!(
        "{TOOLS}{}",
        DEPLOYMENT.replace("context_control = true", "context_control = false")
    ));
    let released = retried_spawn(&runtime, "toolu-1", deciding_at("middle")).await;
    assert!(
        matches!(released, Ok(ToolCallDecision::Allow { spawn: None, .. })),
        "{released:?}"
    );
    assert_eq!(
        (forks(&runtime), declarations(&runtime), dispatches(&runtime)),
        (0, 0, 1)
    );
}

/// A return declared ahead at a floor other than the one a deciding spawn names is not applied
/// to it; the spawn takes the floor its own proposal declares.
#[tokio::test]
async fn a_deciding_spawn_unfit_for_the_return_declared_ahead_takes_its_own_declaration() {
    let Deployed { runtime, .. } = deployed_serving(&format!("{MIDDLE}{DEPLOYMENT}"));
    let acting = Actor {
        root: root(),
        child: None,
    };
    let ahead = RemedyArguments {
        label: Some(middle_floor()),
        return_schema: None,
    };
    assert!(matches!(
        runtime
            .declare_return_ahead(&acting, ahead, RemedyAttempt("r1".to_string()))
            .await,
        RemedyOutcome::Returned { .. }
    ));
    binding(retried_spawn(&runtime, "toolu-1", deciding_here()).await);
    let state = AheadState::fold(runtime.inner.log(&root()).expect("the log reads").host_records());
    assert_eq!(
        state.settlement(&root(), &AheadAttempt::Host("r1".to_string())),
        Some(AheadOutcome::Unfit)
    );
    let trusted = session(&runtime)
        .with_view(|context| Ok(context.engine().trust_chain().rank_of("trusted")))
        .expect("the chain reads")
        .expect("the chain names the rank");
    assert_eq!(fork_floors(&runtime), vec![trusted]);
}

/// A held return still awaiting its parent's decision, then a stop that crosses: the spawn's
/// result repeating the crossing carries the pending return's notice, which still settles.
#[tokio::test]
async fn a_crossing_after_a_pending_held_return_answers_with_the_pending_notice() {
    let Deployed { runtime, .. } = deployed(false);
    let child = deferred_child(&runtime).await;
    tainted(&runtime, &child).await;
    assert!(matches!(
        child.on_child_return("one".to_string(), operation("op-1")).await,
        Ok(ChildReturnDecision::Deferred)
    ));
    tainted(&runtime, &session(&runtime)).await;
    assert!(matches!(
        child.on_child_end(Some("two".to_string())).await,
        Ok(ChildReturnDecision::Returned { .. })
    ));
    assert!(
        matches!(
            spawn_result_saying(&runtime, &child, "two").await,
            SpawnResultDecision::Outcome(ToolResultDecision::Replace { .. })
        ),
        "the pending return's notice stands in for the crossing"
    );
    let [pending] = notices(&runtime).try_into().expect("one held occurrence");
    assert!(matches!(
        settling(&runtime, &parent(), &pending.accept).await,
        RemedyOutcome::Returned { .. }
    ));
}

async fn failed_saying(parent: &Session, child: &Session, said: &str) -> SpawnResultDecision {
    parent
        .on_spawn_result_identified(
            call("fetch", 1),
            Some("toolu-1".to_string()),
            ToolOutcome::Failure {
                message: "the harness lost the subagent".to_string(),
            },
            Some(child.trajectory.clone()),
            Some(said.to_string()),
            SpawnLaunch::Undeclared,
        )
        .await
        .expect("the spawn result is decided")
}

/// A held return the parent accepted, then a stop that crosses, then the spawn's result reported
/// as a failure repeating it: the report closes as a deferred spawn's whose child held nothing
/// does, with no notice.
#[tokio::test]
async fn a_failed_spawn_result_repeating_a_crossing_after_an_accepted_held_return_carries_no_notice() {
    let unheld = deployed(false);
    let crossed = deferred_child(&unheld.runtime).await;
    assert!(matches!(
        crossed.on_child_end(Some("two".to_string())).await,
        Ok(ChildReturnDecision::Returned { .. })
    ));
    let unheld = failed_saying(&session(&unheld.runtime), &crossed, "two").await;

    let Deployed { runtime, .. } = deployed(false);
    let child = deferred_child(&runtime).await;
    tainted(&runtime, &child).await;
    assert!(matches!(
        child.on_child_return("one".to_string(), operation("op-1")).await,
        Ok(ChildReturnDecision::Deferred)
    ));
    let [held] = notices(&runtime).try_into().expect("one occurrence");
    settling(&runtime, &parent(), &held.accept).await;
    assert!(matches!(
        child.on_child_end(Some("two".to_string())).await,
        Ok(ChildReturnDecision::Returned { .. })
    ));
    assert_eq!(failed_saying(&session(&runtime), &child, "two").await, unheld);
}

/// A parent that ended after deciding on its child's return is quoted no settlement for it by
/// either reader.
#[tokio::test]
async fn a_parent_that_ended_after_deciding_is_quoted_no_settlement() {
    let Deployed { runtime, .. } = deployed(false);
    let child = deferred_child(&runtime).await;
    let fork = binding(
        child
            .on_spawn_declaring(
                call("fetch", 2),
                Some("toolu-g".to_string()),
                None,
                DeclaredReturn::Deferred,
            )
            .await,
    );
    let grandchild = child
        .on_child_start(TrajectoryId("cc:root:grandchild".to_string()), SpawnRef::Binding(fork))
        .expect("the grandchild opens");
    tainted(&runtime, &grandchild).await;
    assert!(matches!(
        grandchild.on_child_return("found".to_string(), operation("op-g")).await,
        Ok(ChildReturnDecision::Deferred)
    ));
    let acting = Actor {
        root: root(),
        child: Some(child.trajectory.clone()),
    };
    let presentation = EmbeddedPresentationOptions::default();
    let notice = runtime
        .deferred_return(&acting, "toolu-g", &presentation)
        .expect("the family reads")
        .expect("the live parent is told of the held return");
    settling(&runtime, &acting, &notice.discard).await;
    assert!(matches!(
        child.on_child_end(None).await,
        Ok(ChildReturnDecision::NoValue)
    ));
    assert_eq!(
        runtime
            .deferred_return(&acting, "toolu-g", &presentation)
            .expect("the family reads"),
        None
    );
    let [occurrence] = runtime
        .child_occurrences(&acting, "toolu-g", &presentation)
        .expect("the family reads")
        .try_into()
        .expect("one occurrence");
    assert_eq!(occurrence.notice, None);
}

/// A held return's retry through a runtime pinned to another deployment line is refused: the
/// family follows its own line, and no other line's policy answers it.
#[tokio::test]
async fn a_held_returns_retry_through_another_line_is_refused() {
    let deployed = deployed(false);
    let runtime = &deployed.runtime;
    let child = deferred_child(runtime).await;
    tainted(runtime, &child).await;
    assert!(matches!(
        child.on_child_return("one".to_string(), operation("op-1")).await,
        Ok(ChildReturnDecision::Deferred)
    ));
    let published = runtime
        .publish(
            &appa_engine::profile::DeploymentName::new("other"),
            &runtime
                .prepare_deployment(deployed.config(&policy(false)))
                .expect("the deployment loads"),
        )
        .expect("the line publishes");
    let pinned = runtime.pinned(&published);
    let retried = pinned
        .session(&root(), &child.trajectory)
        .expect("the child reattaches")
        .on_child_return("one".to_string(), operation("op-1"))
        .await;
    assert!(matches!(retried, Err(EventError::PolicyUnavailable(_))), "{retried:?}");
}

/// A spawn result withheld while its child had crossed other bytes delivers no crossing:
/// reported again with those bytes, after a reload, it stays withheld rather than replaying
/// the child's crossing.
#[tokio::test]
async fn a_withheld_spawn_result_reported_again_with_the_crossed_bytes_stays_withheld() {
    let deployed = deployed(false);
    let child = bound_child(&deployed.runtime, 1).await;
    crossing(&child, "x", "op-1").await;
    let withheld = spawn_reported(&deployed.runtime, &child, 1, "y").await;
    assert!(
        matches!(
            withheld,
            SpawnResultDecision::Return(ChildReturnDecision::Blocked { .. })
        ),
        "{withheld:?}"
    );
    let replica = deployed.replica();
    let again = spawn_reported(&replica, &child, 1, "x").await;
    assert!(
        matches!(again, SpawnResultDecision::Return(ChildReturnDecision::Blocked { .. })),
        "{again:?}"
    );
}

/// A deciding spawn's retry after a reload is judged by the revision that released it: a floor
/// that revision resolves to another one is another act, never an answer from the weaker
/// release.
#[tokio::test]
async fn a_released_deciding_spawns_retry_under_another_floor_after_a_reload_reuses_its_call_id() {
    let deployed = deployed_serving(&format!("{MIDDLE}{DEPLOYMENT}"));
    let runtime = &deployed.runtime;
    let released = binding(retried_spawn(runtime, "toolu-1", deciding_at("suspicious")).await);
    runtime
        .reload(deployed.config(&format!("{NO_MIDDLE}{DEPLOYMENT}")))
        .expect("the policy reloads");
    assert!(matches!(
        retried_spawn(runtime, "toolu-1", deciding_at("trusted")).await,
        Err(EventError::CallIdReused)
    ));
    assert_eq!(
        binding(retried_spawn(runtime, "toolu-1", deciding_at("suspicious")).await),
        released
    );
    assert_eq!(forks(runtime), 1);
}

/// A spawn result that closed the spawn on the child's crossing is answered from that close
/// when repeated, though the child has since frozen a return awaiting the parent's decision.
#[tokio::test]
async fn a_repeated_spawn_result_is_answered_from_its_close_past_a_later_held_return() {
    let Deployed { runtime, .. } = deployed(false);
    let child = deferred_child(&runtime).await;
    let receipt = crossing(&child, "x", "op-1").await;
    let first = spawn_result_saying(&runtime, &child, "x").await;
    assert_eq!(
        first,
        SpawnResultDecision::Return(ChildReturnDecision::Returned {
            value: "x".to_string(),
            receipt,
        })
    );
    tainted(&runtime, &child).await;
    assert!(matches!(
        child.on_child_return("y".to_string(), operation("op-2")).await,
        Ok(ChildReturnDecision::Deferred)
    ));
    assert_eq!(spawn_result_saying(&runtime, &child, "x").await, first);
}

/// A floor the releasing revision cannot spell is not the one it released under, whatever the
/// serving revision spells.
#[tokio::test]
async fn a_deciding_spawns_retry_under_a_floor_its_releasing_revision_cannot_spell_reuses_its_call_id() {
    let deployed = deployed_serving(&format!("{NO_MIDDLE}{DEPLOYMENT}"));
    let runtime = &deployed.runtime;
    binding(retried_spawn(runtime, "toolu-1", deciding_at("trusted")).await);
    runtime
        .reload(deployed.config(&format!("{MIDDLE}{DEPLOYMENT}")))
        .expect("the policy reloads");
    assert!(matches!(
        retried_spawn(runtime, "toolu-1", deciding_at("middle")).await,
        Err(EventError::CallIdReused)
    ));
}
