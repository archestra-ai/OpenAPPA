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
    _dir: tempfile::TempDir,
    runtime: Runtime,
}

fn deployed(attesting: bool) -> Deployed {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let path = dir.path().join("appa.toml");
    let attest = if attesting { ATTEST } else { "" };
    std::fs::write(&path, format!("{TOOLS}{attest}{DEPLOYMENT}")).expect("the fixture writes");
    let runtime = Runtime::open(
        Config::load(&path).expect("the fixture validates"),
        dir.path().join("appa.db"),
        None,
    )
    .expect("the runtime opens");
    runtime.create_session(root(), None).expect("a fresh root opens");
    Deployed { _dir: dir, runtime }
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
        .on_spawn_declaring(spawn, Some(call_id.to_string()), None, declares)
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
        ReturnPolicy::Void => None,
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
