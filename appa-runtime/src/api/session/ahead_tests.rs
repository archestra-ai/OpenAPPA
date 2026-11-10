use super::super::Runtime;
use super::*;
use crate::api::host::AheadState;
use crate::api::{Actor, RemedyAttempt, RemedyOutcome, RemedyRefusal, ToolCallDecision};
use crate::config::Config;
use crate::engine::LabelSpelling;
use appa_eventlog::{AheadAttempt, AheadOutcome, EndKind, HostObservation};

const POLICY: &str = r#"
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

[policy.deployment]
context_control = true

[externals]
timeout_ms = 2000
max_body_bytes = 65536
"#;

/// Two runtime processes serving one SQLite store, as a host with two replicas does.
struct Replicas {
    _dir: tempfile::TempDir,
    a: Runtime,
    b: Runtime,
}

fn replicas() -> Replicas {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, POLICY).expect("the fixture writes");
    let open = || {
        Runtime::open(
            Config::load(&path).expect("the fixture validates"),
            dir.path().join("appa.db"),
            None,
        )
        .expect("the replica opens")
    };
    let (a, b) = (open(), open());
    a.create_session(root(), None).expect("a fresh root opens");
    Replicas { _dir: dir, a, b }
}

fn root() -> TrajectoryId {
    TrajectoryId("cc:root".to_string())
}

fn acting() -> Actor {
    Actor {
        root: root(),
        child: None,
    }
}

fn bare() -> RemedyArguments {
    RemedyArguments {
        label: Some(LabelSpelling::default()),
        return_schema: None,
    }
}

fn host(attempt: &str) -> AheadAttempt {
    AheadAttempt::Host(attempt.to_string())
}

fn call(tool: &str) -> ProposedCall {
    ProposedCall {
        tool: tool.to_string(),
        arguments: raw(serde_json::json!({ "a": 1 })),
        cwd: None,
    }
}

async fn declare(runtime: &Runtime, attempt: &str, arguments: RemedyArguments) -> RemedyOutcome {
    runtime
        .declare_return_ahead(&acting(), arguments, RemedyAttempt(attempt.to_string()))
        .await
}

async fn spawn(runtime: &Runtime, call_id: Option<&str>) -> Result<ToolCallDecision, EventError> {
    runtime
        .session(&root(), &root())
        .expect("the session reattaches")
        .on_tool_call_identified(
            call("fetch"),
            call_id.map(str::to_string),
            Some(SpawnKind::Single),
            None,
        )
        .await
}

fn released(decision: &Result<ToolCallDecision, EventError>) -> bool {
    matches!(decision, Ok(ToolCallDecision::Allow { spawn: Some(_), .. }))
}

fn blocked(decision: &Result<ToolCallDecision, EventError>) -> bool {
    matches!(decision, Ok(ToolCallDecision::Deny { .. }))
}

fn ahead(runtime: &Runtime) -> AheadState {
    AheadState::fold(runtime.inner.log(&root()).expect("the log reads").host_records())
}

fn count(runtime: &Runtime, kind: impl Fn(&HostObservation) -> bool) -> usize {
    let log = runtime.inner.log(&root()).expect("the log reads");
    log.host_records()
        .iter()
        .filter(|record| kind(&record.observation))
        .count()
}

fn declarations(runtime: &Runtime) -> usize {
    count(runtime, |observation| {
        matches!(observation, HostObservation::AheadDeclared { .. })
    })
}

fn settlements(runtime: &Runtime) -> usize {
    count(runtime, |observation| {
        matches!(observation, HostObservation::AheadSettled { .. })
    })
}

/// Admit a narrowing result at the root, accepting the narrowing its block offers.
async fn narrow(runtime: &Runtime) {
    let session = runtime.session(&root(), &root()).expect("the session reattaches");
    if matches!(
        session.on_tool_call(call("taint"), false).await,
        Ok(ToolCallDecision::Deny { .. })
    ) {
        let quoted = runtime
            .minted_offers(&root(), &root())
            .first()
            .expect("the narrowing block surfaced its acceptance")
            .clone();
        let offer = runtime.resolve_in(&root(), &quoted).expect("the quoted id resolves").0;
        assert!(matches!(
            session.on_remedy(offer, RemedyArguments::default(), None, None).await,
            Ok(RemedyDecision::Authorized { .. })
        ));
        assert!(matches!(
            session.on_tool_call(call("taint"), false).await,
            Ok(ToolCallDecision::Allow { spawn: None, .. })
        ));
    }
    assert_eq!(
        session
            .on_tool_result(
                call("taint"),
                ToolOutcome::Success {
                    body: OutcomeBody::Available("outside content".to_string()),
                },
            )
            .await
            .expect("the result is admitted"),
        ToolResultDecision::Keep
    );
}

#[tokio::test]
async fn two_spawns_on_two_replicas_take_one_declaration() {
    let Replicas { a, b, .. } = replicas();
    assert!(matches!(
        declare(&a, "r1", bare()).await,
        RemedyOutcome::Returned { .. }
    ));
    let (x, y) = tokio::join!(spawn(&a, Some("toolu-x")), spawn(&b, Some("toolu-y")));
    assert_eq!(
        [&x, &y].into_iter().filter(|decision| released(decision)).count(),
        1,
        "{x:?} {y:?}"
    );
    assert_eq!([&x, &y].into_iter().filter(|decision| blocked(decision)).count(), 1);
    let state = ahead(&a);
    let took = ["toolu-x", "toolu-y"]
        .into_iter()
        .filter(|call_id| state.assignment(&root(), call_id) == Some(Some(&host("r1"))))
        .count();
    assert_eq!(took, 1, "exactly one first record names the declaration");
    assert!(settlements(&a) <= 1);
}

#[tokio::test]
async fn a_call_first_recorded_undeclared_never_takes_a_later_declaration() {
    let Replicas { a, b, .. } = replicas();
    assert!(blocked(&spawn(&a, Some("toolu-x")).await));
    assert!(matches!(
        declare(&b, "r1", bare()).await,
        RemedyOutcome::Returned { .. }
    ));
    assert!(
        blocked(&spawn(&a, Some("toolu-x")).await),
        "the retry keeps its plain block"
    );
    let state = ahead(&a);
    assert_eq!(state.assignment(&root(), "toolu-x"), Some(None));
    assert_eq!(state.live(&root()), Some(&host("r1")), "the declaration stays live");
    assert!(released(&spawn(&b, Some("toolu-y")).await), "a fresh spawn takes it");
}

#[tokio::test]
async fn a_retried_spawn_that_applied_leaves_a_later_declaration_alone() {
    let Replicas { a, b, .. } = replicas();
    declare(&a, "r1", bare()).await;
    assert!(released(&spawn(&a, Some("toolu-x")).await));
    declare(&b, "r2", bare()).await;
    assert!(released(&spawn(&b, Some("toolu-x")).await), "the retry re-proposes");
    let state = ahead(&a);
    assert_eq!(state.settlement(&root(), &host("r1")), Some(AheadOutcome::Applied));
    assert_eq!(
        state.live(&root()),
        Some(&host("r2")),
        "the later declaration is untouched"
    );
    // X's fork is still outstanding, so the next spawn waits on it, holding the declaration.
    assert!(matches!(
        spawn(&a, Some("toolu-y")).await,
        Err(EventError::SpawnOutstanding)
    ));
    assert_eq!(ahead(&a).assignment(&root(), "toolu-y"), Some(Some(&host("r2"))));
}

#[tokio::test]
async fn a_lapsed_settlement_is_read_back_by_its_retry_on_another_replica() {
    let Replicas { a, b, .. } = replicas();
    declare(&a, "r1", bare()).await;
    narrow(&b).await;
    assert!(blocked(&spawn(&b, Some("toolu-x")).await));
    assert_eq!(ahead(&a).settlement(&root(), &host("r1")), Some(AheadOutcome::Lapsed));
    declare(&b, "r2", bare()).await;
    assert!(blocked(&spawn(&a, Some("toolu-x")).await));
    let state = ahead(&a);
    assert_eq!(state.settlement(&root(), &host("r1")), Some(AheadOutcome::Lapsed));
    assert_eq!(
        state.live(&root()),
        Some(&host("r2")),
        "the retry left the later declaration"
    );
    assert_eq!(settlements(&a), 1);
}

#[tokio::test]
async fn one_attempt_declares_once_across_replicas() {
    let Replicas { a, b, .. } = replicas();
    assert!(matches!(
        declare(&a, "r1", bare()).await,
        RemedyOutcome::Returned { .. }
    ));
    assert!(matches!(
        declare(&b, "r1", bare()).await,
        RemedyOutcome::Returned { .. }
    ));
    let other = RemedyArguments {
        label: Some(LabelSpelling {
            trust: Some("suspicious".to_string()),
            audience: None,
        }),
        return_schema: None,
    };
    assert!(matches!(
        declare(&b, "r1", other).await,
        RemedyOutcome::Refused {
            reason: RemedyRefusal::AttemptReused,
            ..
        }
    ));
    assert_eq!(declarations(&a), 1);
}

#[tokio::test]
async fn a_replayed_attempt_does_not_rearm_its_spent_declaration() {
    let Replicas { a, b, .. } = replicas();
    declare(&a, "r1", bare()).await;
    assert!(released(&spawn(&a, Some("toolu-x")).await));
    assert!(matches!(
        declare(&b, "r1", bare()).await,
        RemedyOutcome::Returned { .. }
    ));
    assert!(
        blocked(&spawn(&b, Some("toolu-y")).await),
        "the next spawn gets its plain block"
    );
    assert_eq!(declarations(&a), 1);
}

#[tokio::test]
async fn an_unidentified_spawn_reserves_and_applies_its_declaration() {
    let Replicas { a, b, .. } = replicas();
    declare(&a, "r1", bare()).await;
    assert!(released(&spawn(&b, None).await));
    assert_eq!(
        count(&a, |observation| matches!(
            observation,
            HostObservation::AheadReserved { .. }
        )),
        1
    );
    assert_eq!(settlements(&a), 1);
    assert_eq!(ahead(&a).settlement(&root(), &host("r1")), Some(AheadOutcome::Applied));
}

#[tokio::test]
async fn a_reserved_declaration_a_boundary_cleared_settles_ended() {
    let Replicas { a, b, .. } = replicas();
    a.record_prompt(&acting()).expect("the prompt records");
    declare(&a, "r1", bare()).await;
    a.store().fail_commit_after(1);
    assert!(matches!(spawn(&a, Some("toolu-x")).await, Err(EventError::Storage(_))));
    assert_eq!(ahead(&a).assignment(&root(), "toolu-x"), Some(Some(&host("r1"))));
    b.record_turn_end(&acting(), EndKind::Boundary).expect("the turn ends");
    assert!(blocked(&spawn(&b, Some("toolu-x")).await));
    assert_eq!(ahead(&a).settlement(&root(), &host("r1")), Some(AheadOutcome::Ended));
}

#[tokio::test]
async fn a_housekeeping_end_leaves_the_declaration_live() {
    let Replicas { a, b, .. } = replicas();
    a.record_prompt(&acting()).expect("the prompt records");
    declare(&a, "r1", bare()).await;
    b.record_turn_end(&acting(), EndKind::Housekeeping)
        .expect("the housekeeping end records");
    assert!(released(&spawn(&b, Some("toolu-x")).await));
}

#[tokio::test]
async fn an_apply_whose_commit_fails_settles_nothing_and_its_retry_applies() {
    let Replicas { a, b, .. } = replicas();
    declare(&a, "r1", bare()).await;
    // The proposal commits its first record; the application's commit fails.
    a.store().fail_commit_after(1);
    assert!(matches!(spawn(&a, Some("toolu-x")).await, Err(EventError::Storage(_))));
    assert_eq!(ahead(&a).settlement(&root(), &host("r1")), None, "unsettled");
    assert!(released(&spawn(&b, Some("toolu-x")).await));
    assert_eq!(ahead(&a).settlement(&root(), &host("r1")), Some(AheadOutcome::Applied));
}

#[tokio::test]
async fn contention_through_every_round_refuses_and_settles_nothing() {
    let Replicas { a, .. } = replicas();
    declare(&a, "r1", bare()).await;
    a.store().fail_commit_after(1);
    assert!(spawn(&a, Some("toolu-x")).await.is_err());
    a.store().contend_next_appends(u64::MAX);
    assert!(matches!(
        spawn(&a, Some("toolu-x")).await,
        Err(EventError::Contended { .. })
    ));
    assert_eq!(settlements(&a), 0);
    a.store().contend_next_appends(0);
    assert!(released(&spawn(&a, Some("toolu-x")).await));
    assert_eq!(ahead(&a).settlement(&root(), &host("r1")), Some(AheadOutcome::Applied));
}
