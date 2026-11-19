use super::super::Runtime;
use super::*;
use crate::api::host::AheadState;
use crate::api::{Actor, RemedyAttempt, RemedyOutcome, SpawnBinding, ToolCallDecision};
use crate::config::Config;
use crate::engine::LabelSpelling;
use appa_engine::fact::{Fact, ReturnPolicy};
use appa_eventlog::{AheadAttempt, AheadOutcome};

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
            child.on_child_return("the answer".to_string()).await,
            Ok(ChildReturnDecision::Deferred)
        ),
        "and so is a return made without ending"
    );
    assert!(matches!(
        child.on_child_end(None).await,
        Ok(ChildReturnDecision::NoValue)
    ));
    assert_eq!(deferred_returns(&runtime), 1);
    assert_eq!(crossings(&runtime), 0);
}

#[tokio::test]
async fn a_deferred_child_that_narrowed_nothing_returns_at_its_stop() {
    let Deployed { runtime, .. } = deployed(false);
    let child = deferred_child(&runtime).await;
    assert!(matches!(
        child.on_child_end(Some("done".to_string())).await,
        Ok(ChildReturnDecision::Returned { value }) if value == "done"
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
    assert_eq!(
        accepted,
        RemedyOutcome::Returned {
            value: "the answer".to_string()
        }
    );
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
        settling(&runtime, &parent(), &notice.accept).await,
        RemedyOutcome::Returned {
            value: "the answer".to_string()
        },
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
