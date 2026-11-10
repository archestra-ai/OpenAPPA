use super::super::Runtime;
use super::*;
use crate::api::host::AheadState;
use crate::api::{
    Actor, PermitKey, RemedyAttempt, RemedyOutcome, RemedyRefusal, SERVING_LINE, ToolCallDecision, host_actor,
};
use crate::config::Config;
use crate::engine::LabelSpelling;
use appa_eventlog::{AheadAttempt, AheadOutcome, EndKind, HostObservation};

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

/// Which policy a replica serves: whether it registers `attest-schema`, and a revision tag
/// that keeps two otherwise equal files apart.
#[derive(Clone, Copy)]
enum Revision {
    Plain,
    Attesting(u8),
    Unattesting,
    /// Only `fetch`, under a trust chain with or without the intermediate `suspicious` rank.
    Ranked(Ranks),
}

#[derive(Clone, Copy)]
enum Ranks {
    WithIntermediate,
    WithoutIntermediate,
}

impl Revision {
    fn text(self) -> String {
        match self {
            Revision::Plain => format!("{TOOLS}{DEPLOYMENT}"),
            Revision::Attesting(tag) => format!("# revision {tag}\n{TOOLS}{ATTEST}{DEPLOYMENT}"),
            Revision::Unattesting => format!("# unattesting\n{TOOLS}{DEPLOYMENT}"),
            Revision::Ranked(ranks) => {
                let chain = match ranks {
                    Ranks::WithIntermediate => r#"["untrusted", "suspicious", "trusted"]"#,
                    Ranks::WithoutIntermediate => r#"["untrusted", "trusted"]"#,
                };
                format!(
                    "[policy]\nversion = 2\ntrust_chain = {chain}\n\n[[policy.tool]]\nname = \"fetch\"\n\
                     parameters = {{ type = \"object\", properties = {{ a = {{ type = \"integer\" }} }} }}\n\
                     delta = {{}}\n{DEPLOYMENT}"
                )
            }
        }
    }
}

/// Two runtime processes serving one SQLite store, as a host with two replicas does.
struct Replicas {
    dir: tempfile::TempDir,
    a: Runtime,
    b: Runtime,
}

impl Replicas {
    fn config(&self, revision: Revision) -> Config {
        let path = self.dir.path().join("appa.toml");
        std::fs::write(&path, revision.text()).expect("the fixture writes");
        Config::load(&path).expect("the fixture validates")
    }

    /// Both replicas serve `revision`, `b` first.
    fn reload(&self, revision: Revision) {
        for runtime in [&self.b, &self.a] {
            runtime.reload(self.config(revision)).expect("the replica reloads");
        }
    }
}

fn replicas() -> Replicas {
    replicas_serving(Revision::Plain)
}

fn replicas_serving(revision: Revision) -> Replicas {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, revision.text()).expect("the fixture writes");
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
    Replicas { dir, a, b }
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_spawns_on_two_replicas_take_one_declaration() {
    let replicas = replicas();
    let Replicas { a, b, .. } = &replicas;
    assert!(matches!(declare(a, "r1", bare()).await, RemedyOutcome::Returned { .. }));
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let tasks: Vec<_> = [(a, "toolu-x"), (b, "toolu-y")]
        .into_iter()
        .map(|(runtime, call_id)| {
            let session = runtime.session(&root(), &root()).expect("the session reattaches");
            let barrier = std::sync::Arc::clone(&barrier);
            tokio::spawn(async move {
                barrier.wait().await;
                session
                    .on_tool_call_identified(call("fetch"), Some(call_id.to_string()), Some(SpawnKind::Single), None)
                    .await
            })
        })
        .collect();
    let mut decisions = Vec::new();
    for task in tasks {
        decisions.push(task.await.expect("the task joins"));
    }
    let [x, y] = <[_; 2]>::try_from(decisions).expect("two spawns were decided");
    assert_eq!(
        [&x, &y].into_iter().filter(|decision| released(decision)).count(),
        1,
        "{x:?} {y:?}"
    );
    assert_eq!([&x, &y].into_iter().filter(|decision| blocked(decision)).count(), 1);
    let state = ahead(a);
    let took = ["toolu-x", "toolu-y"]
        .into_iter()
        .filter(|call_id| state.assignment(&root(), call_id) == Some(Some(&host("r1"))))
        .count();
    assert_eq!(took, 1, "exactly one first record names the declaration");
    assert!(settlements(a) <= 1);
}

#[tokio::test]
async fn a_call_first_recorded_undeclared_never_takes_a_later_declaration() {
    let replicas = replicas();
    let Replicas { a, b, .. } = &replicas;
    assert!(blocked(&spawn(a, Some("toolu-x")).await));
    assert!(matches!(declare(b, "r1", bare()).await, RemedyOutcome::Returned { .. }));
    assert!(
        blocked(&spawn(a, Some("toolu-x")).await),
        "the retry keeps its plain block"
    );
    let state = ahead(a);
    assert_eq!(state.assignment(&root(), "toolu-x"), Some(None));
    assert_eq!(state.live(&root()), Some(&host("r1")), "the declaration stays live");
    assert!(released(&spawn(b, Some("toolu-y")).await), "a fresh spawn takes it");
}

#[tokio::test]
async fn a_retried_spawn_that_applied_leaves_a_later_declaration_alone() {
    let replicas = replicas();
    let Replicas { a, b, .. } = &replicas;
    declare(a, "r1", bare()).await;
    assert!(released(&spawn(a, Some("toolu-x")).await));
    declare(b, "r2", bare()).await;
    assert!(released(&spawn(b, Some("toolu-x")).await), "the retry re-proposes");
    let state = ahead(a);
    assert_eq!(state.settlement(&root(), &host("r1")), Some(AheadOutcome::Applied));
    assert_eq!(
        state.live(&root()),
        Some(&host("r2")),
        "the later declaration is untouched"
    );
    // X's fork is still outstanding, so the next spawn waits on it, holding the declaration.
    assert!(matches!(
        spawn(a, Some("toolu-y")).await,
        Err(EventError::SpawnOutstanding)
    ));
    assert_eq!(ahead(a).assignment(&root(), "toolu-y"), Some(Some(&host("r2"))));
}

#[tokio::test]
async fn a_lapsed_settlement_is_read_back_by_its_retry_on_another_replica() {
    let replicas = replicas();
    let Replicas { a, b, .. } = &replicas;
    declare(a, "r1", bare()).await;
    narrow(b).await;
    assert!(blocked(&spawn(b, Some("toolu-x")).await));
    assert_eq!(ahead(a).settlement(&root(), &host("r1")), Some(AheadOutcome::Lapsed));
    declare(b, "r2", bare()).await;
    assert!(blocked(&spawn(a, Some("toolu-x")).await));
    let state = ahead(a);
    assert_eq!(state.settlement(&root(), &host("r1")), Some(AheadOutcome::Lapsed));
    assert_eq!(
        state.live(&root()),
        Some(&host("r2")),
        "the retry left the later declaration"
    );
    assert_eq!(settlements(a), 1);
}

#[tokio::test]
async fn one_attempt_declares_once_across_replicas() {
    let replicas = replicas();
    let Replicas { a, b, .. } = &replicas;
    assert!(matches!(declare(a, "r1", bare()).await, RemedyOutcome::Returned { .. }));
    assert!(matches!(declare(b, "r1", bare()).await, RemedyOutcome::Returned { .. }));
    let other = RemedyArguments {
        label: Some(LabelSpelling {
            trust: Some("suspicious".to_string()),
            audience: None,
        }),
        return_schema: None,
    };
    assert!(matches!(
        declare(b, "r1", other).await,
        RemedyOutcome::Refused {
            reason: RemedyRefusal::AttemptReused,
            ..
        }
    ));
    assert_eq!(declarations(a), 1);
}

#[tokio::test]
async fn a_replayed_attempt_does_not_rearm_its_spent_declaration() {
    let replicas = replicas();
    let Replicas { a, b, .. } = &replicas;
    declare(a, "r1", bare()).await;
    assert!(released(&spawn(a, Some("toolu-x")).await));
    assert!(matches!(declare(b, "r1", bare()).await, RemedyOutcome::Returned { .. }));
    assert!(
        blocked(&spawn(b, Some("toolu-y")).await),
        "the next spawn gets its plain block"
    );
    assert_eq!(declarations(a), 1);
}

#[tokio::test]
async fn an_unidentified_spawn_reserves_and_applies_its_declaration() {
    let replicas = replicas();
    let Replicas { a, b, .. } = &replicas;
    declare(a, "r1", bare()).await;
    assert!(released(&spawn(b, None).await));
    assert_eq!(
        count(a, |observation| matches!(
            observation,
            HostObservation::AheadReserved { .. }
        )),
        1
    );
    assert_eq!(settlements(a), 1);
    assert_eq!(ahead(a).settlement(&root(), &host("r1")), Some(AheadOutcome::Applied));
}

#[tokio::test]
async fn a_reserved_declaration_a_boundary_cleared_settles_ended() {
    let replicas = replicas();
    let Replicas { a, b, .. } = &replicas;
    a.record_prompt(&acting()).expect("the prompt records");
    declare(a, "r1", bare()).await;
    a.store().fail_commit_after(1);
    assert!(matches!(spawn(a, Some("toolu-x")).await, Err(EventError::Storage(_))));
    assert_eq!(ahead(a).assignment(&root(), "toolu-x"), Some(Some(&host("r1"))));
    b.record_turn_end(&acting(), EndKind::Boundary).expect("the turn ends");
    assert!(blocked(&spawn(b, Some("toolu-x")).await));
    assert_eq!(ahead(a).settlement(&root(), &host("r1")), Some(AheadOutcome::Ended));
}

#[tokio::test]
async fn a_housekeeping_end_leaves_the_declaration_live() {
    let replicas = replicas();
    let Replicas { a, b, .. } = &replicas;
    a.record_prompt(&acting()).expect("the prompt records");
    declare(a, "r1", bare()).await;
    b.record_turn_end(&acting(), EndKind::Housekeeping)
        .expect("the housekeeping end records");
    assert!(released(&spawn(b, Some("toolu-x")).await));
}

#[tokio::test]
async fn an_apply_whose_commit_fails_settles_nothing_and_its_retry_applies() {
    let replicas = replicas();
    let Replicas { a, b, .. } = &replicas;
    declare(a, "r1", bare()).await;
    // The proposal commits its first record; the application's commit fails.
    a.store().fail_commit_after(1);
    assert!(matches!(spawn(a, Some("toolu-x")).await, Err(EventError::Storage(_))));
    assert_eq!(ahead(a).settlement(&root(), &host("r1")), None, "unsettled");
    assert!(released(&spawn(b, Some("toolu-x")).await));
    assert_eq!(ahead(a).settlement(&root(), &host("r1")), Some(AheadOutcome::Applied));
}

/// The application's own replays run out first: the exhaustion is operational, so it is
/// answered at once rather than spent on another round.
#[tokio::test]
async fn an_application_contended_past_its_replays_refuses_and_settles_nothing() {
    let replicas = replicas();
    let Replicas { a, .. } = &replicas;
    declare(a, "r1", bare()).await;
    a.store().fail_commit_after(1);
    assert!(spawn(a, Some("toolu-x")).await.is_err());
    a.store().contend_next_appends(u64::MAX);
    assert!(matches!(
        spawn(a, Some("toolu-x")).await,
        Err(EventError::Contended {
            attempts: super::drive::REPLAY_LIMIT
        })
    ));
    assert_eq!(settlements(a), 0);
    a.store().contend_next_appends(0);
    assert!(released(&spawn(a, Some("toolu-x")).await));
    assert_eq!(ahead(a).settlement(&root(), &host("r1")), Some(AheadOutcome::Applied));
}

/// A turn end lands against the application, which then refuses as moved; the next round
/// commits the settlement and answers with it.
#[tokio::test]
async fn a_settlement_a_round_commits_is_its_answer() {
    let replicas = replicas();
    let Replicas { a, .. } = &replicas;
    declare(a, "r1", bare()).await;
    reserved(a, "toolu-x").await;
    a.store().contend_next_append_with(
        &root(),
        &root(),
        &HostObservation::TurnEnded {
            actor: host_actor(&acting()),
            end_kind: EndKind::Boundary,
        },
    );
    assert!(
        blocked(&spawn(a, Some("toolu-x")).await),
        "the plain block, not a refusal"
    );
    assert_eq!(ahead(a).settlement(&root(), &host("r1")), Some(AheadOutcome::Ended));
    assert_eq!(settlements(a), 1);
}

fn schema_bound() -> RemedyArguments {
    RemedyArguments {
        label: Some(LabelSpelling::default()),
        return_schema: Some(serde_json::json!({
            "type": "object",
            "properties": { "ok": { "type": "boolean" } },
            "required": ["ok"],
        })),
    }
}

fn line() -> appa_eventlog::DeploymentName {
    appa_eventlog::DeploymentName::new(SERVING_LINE)
}

/// A declaration of the child's trajectory under `attempt`, copied from one the root made:
/// a state no admission writes, since a second admission of an attempt replays the first.
fn declared_as(runtime: &Runtime, actor: &Actor, attempt: AheadAttempt) -> HostObservation {
    let log = runtime.inner.log(&root()).expect("the log reads");
    let Some(HostObservation::AheadDeclared { arguments, label, .. }) = log
        .host_records()
        .iter()
        .map(|record| record.observation.clone())
        .find(|observation| matches!(observation, HostObservation::AheadDeclared { .. }))
    else {
        panic!("the root declared ahead");
    };
    HostObservation::AheadDeclared {
        actor: host_actor(actor),
        attempt,
        arguments,
        label,
        spends: None,
    }
}

fn child() -> Actor {
    Actor {
        root: root(),
        child: Some(TrajectoryId("cc:child".to_string())),
    }
}

/// Reserve the declaration for X: its first decision commits, and the settlement or
/// application after it does not.
async fn reserved(runtime: &Runtime, call_id: &str) {
    runtime.store().fail_commit_after(1);
    assert!(matches!(
        spawn(runtime, Some(call_id)).await,
        Err(EventError::Storage(_))
    ));
    assert!(ahead(runtime).assignment(&root(), call_id).flatten().is_some());
}

#[tokio::test]
async fn a_reservation_a_later_turn_ended_keeps_its_ended_settlement() {
    let replicas = replicas();
    let Replicas { a, b, .. } = &replicas;
    a.record_prompt(&acting()).expect("the prompt records");
    declare(a, "r1", bare()).await;
    reserved(a, "toolu-x").await;
    b.record_turn_end(&acting(), EndKind::Boundary).expect("the turn ends");
    b.record_prompt(&acting()).expect("the prompt records");
    declare(b, "r3", bare()).await;
    assert!(blocked(&spawn(a, Some("toolu-x")).await));
    let state = ahead(a);
    assert_eq!(state.settlement(&root(), &host("r1")), Some(AheadOutcome::Ended));
    assert_eq!(
        state.live(&root()),
        Some(&host("r3")),
        "the next turn's declaration is untouched"
    );
}

/// The closest deterministic schedule to a duplicate applying from a read before the first
/// application committed: the duplicate reads the committed application and re-proposes.
#[tokio::test]
async fn a_duplicate_of_an_applied_spawn_re_proposes_without_settling_again() {
    let replicas = replicas();
    let Replicas { a, b, .. } = &replicas;
    declare(a, "r1", bare()).await;
    reserved(b, "toolu-x").await;
    assert!(released(&spawn(a, Some("toolu-x")).await));
    assert!(released(&spawn(b, Some("toolu-x")).await));
    assert_eq!(settlements(a), 1);
    assert_eq!(ahead(a).settlement(&root(), &host("r1")), Some(AheadOutcome::Applied));
}

#[tokio::test]
async fn a_declaration_held_by_another_actor_is_not_replayed_to_this_one() {
    let replicas = replicas();
    let Replicas { a, .. } = &replicas;
    declare(a, "seed", bare()).await;
    a.inner
        .append_host(&root(), &declared_as(a, &child(), host("op")))
        .expect("the record lands");
    assert!(matches!(
        declare(a, "op", bare()).await,
        RemedyOutcome::Refused {
            reason: RemedyRefusal::ActorMismatch
        }
    ));
    assert_eq!(declarations(a), 2, "nothing was written");
}

#[tokio::test]
async fn a_declaration_after_a_boundary_is_live_into_the_next_turn() {
    let replicas = replicas();
    let Replicas { a, b, .. } = &replicas;
    a.record_turn_end(&acting(), EndKind::Boundary).expect("the turn ends");
    declare(b, "r1", bare()).await;
    a.record_prompt(&acting()).expect("the prompt records");
    assert!(released(&spawn(a, Some("toolu-x")).await));
}

#[tokio::test]
async fn an_unfit_settlement_is_read_back_by_its_retry_on_another_replica() {
    let replicas = replicas_serving(Revision::Attesting(1));
    let Replicas { a, b, .. } = &replicas;
    assert!(matches!(
        declare(a, "r1", schema_bound()).await,
        RemedyOutcome::Returned { .. }
    ));
    // The schema's route is gone, so no offer fits the declaration.
    replicas.reload(Revision::Unattesting);
    assert!(blocked(&spawn(b, Some("toolu-x")).await));
    assert_eq!(ahead(a).settlement(&root(), &host("r1")), Some(AheadOutcome::Unfit));
    assert!(blocked(&spawn(a, Some("toolu-x")).await));
    assert_eq!(settlements(a), 1);
    assert!(
        blocked(&spawn(a, Some("toolu-y")).await),
        "a new spawn gets its plain block"
    );
}

/// The route still stands, but the declared floor names a rank the new policy dropped: the
/// declaration is unfit rather than applied and refused round after round.
#[tokio::test]
async fn a_declaration_whose_floor_the_new_policy_cannot_read_settles_unfit() {
    let replicas = replicas_serving(Revision::Ranked(Ranks::WithIntermediate));
    let Replicas { a, b, .. } = &replicas;
    let floored = RemedyArguments {
        label: Some(LabelSpelling {
            trust: Some("suspicious".to_string()),
            audience: None,
        }),
        return_schema: None,
    };
    let declared = declare(a, "r1", floored).await;
    assert!(matches!(declared, RemedyOutcome::Returned { .. }), "{declared:?}");
    replicas.reload(Revision::Ranked(Ranks::WithoutIntermediate));
    let decision = spawn(b, Some("toolu-x")).await;
    assert!(blocked(&decision), "the ordinary block: {decision:?}");
    assert_eq!(ahead(a).settlement(&root(), &host("r1")), Some(AheadOutcome::Unfit));
}

#[tokio::test]
async fn a_boundary_that_lands_against_a_terminal_settlement_is_judged_at_commit() {
    let replicas = replicas_serving(Revision::Attesting(1));
    let Replicas { a, .. } = &replicas;
    declare(a, "r1", schema_bound()).await;
    replicas.reload(Revision::Unattesting);
    reserved(a, "toolu-x").await;
    a.store().contend_next_append_with(
        &root(),
        &root(),
        &HostObservation::TurnEnded {
            actor: host_actor(&acting()),
            end_kind: EndKind::Boundary,
        },
    );
    assert!(blocked(&spawn(a, Some("toolu-x")).await));
    assert_eq!(
        ahead(a).settlement(&root(), &host("r1")),
        Some(AheadOutcome::Ended),
        "re-judged after the end, not Unfit"
    );
}

#[tokio::test]
async fn a_publish_between_judgment_and_settlement_writes_nothing_and_the_reload_applies() {
    let replicas = replicas_serving(Revision::Attesting(1));
    let Replicas { a, .. } = &replicas;
    declare(a, "r1", schema_bound()).await;
    replicas.reload(Revision::Unattesting);
    reserved(a, "toolu-x").await;
    let basis = a.log_basis(&root());
    let head = a.store().head(&line()).expect("the head reads").expect("a head stands");
    let next = replicas.config(Revision::Attesting(2));
    a.store()
        .publish_before_next_append(&line(), next.policy_file().bytes());
    // Judged Unfit under the served revision; the next one would fit it.
    assert!(matches!(
        spawn(a, Some("toolu-x")).await,
        Err(EventError::PolicyMoved { .. })
    ));
    assert_eq!(settlements(a), 0);
    assert_eq!(a.log_basis(&root()), basis, "the family did not move");
    let moved = a.store().head(&line()).expect("the head reads").expect("a head stands");
    assert_eq!(moved.revision, head.revision.next(), "the publish is durable");
    replicas.reload(Revision::Attesting(2));
    assert!(released(&spawn(a, Some("toolu-x")).await));
    assert_eq!(ahead(a).settlement(&root(), &host("r1")), Some(AheadOutcome::Applied));
}

#[tokio::test]
async fn a_declaration_that_spends_a_vouch_folds_as_its_release() {
    let key = PermitKey::call("execute_remedy_plan", &serde_json::json!({}));
    let vouched = |actor: &Actor, seq| appa_eventlog::HostRecord {
        seq,
        observation: HostObservation::Vouched {
            actor: host_actor(actor),
            key: key.wire(),
            ruling: None,
        },
    };
    let replicas = replicas();
    let seeded = declare(&replicas.a, "seed", bare()).await;
    assert!(matches!(seeded, RemedyOutcome::Returned { .. }));
    let HostObservation::AheadDeclared { arguments, label, .. } = declared_as(&replicas.a, &acting(), host("r1"))
    else {
        unreachable!("declared_as answers a declaration");
    };
    let spending = HostObservation::AheadDeclared {
        actor: host_actor(&acting()),
        attempt: host("r1"),
        arguments,
        label,
        spends: Some(appa_eventlog::SpentVouch { key: key.wire() }),
    };
    let released = HostObservation::Released {
        actor: host_actor(&acting()),
        key: key.wire(),
    };
    let standing = |last: HostObservation, others: bool| {
        let mut records = vec![vouched(&acting(), 1)];
        if others {
            records.push(vouched(&child(), 2));
        }
        records.push(appa_eventlog::HostRecord {
            seq: 3,
            observation: last,
        });
        crate::api::host::HostState::fold(&records, std::time::SystemTime::now())
            .standing(&key)
            .ok()
    };
    assert_eq!(standing(spending.clone(), false), None);
    assert_eq!(standing(released.clone(), false), None);
    let kept = standing(spending, true);
    assert_eq!(kept, standing(released, true));
    assert_eq!(
        kept.map(|vouch| vouch.actor),
        Some(child()),
        "another actor's vouch is untouched"
    );
}

/// The daemon's declaration: authorized by the vouch its control hook recorded.
#[cfg(feature = "daemon")]
mod daemon {
    use super::*;
    use crate::api::{ExecuteRemedyPlanArgs, Holder, LabelArgs, Scan, declaration_key, standing_in};

    fn args() -> ExecuteRemedyPlanArgs {
        ExecuteRemedyPlanArgs {
            offer_id: None,
            label: Some(LabelArgs {
                trust: None,
                audience: None,
            }),
            return_schema: None,
        }
    }

    fn other_args() -> ExecuteRemedyPlanArgs {
        ExecuteRemedyPlanArgs {
            label: Some(LabelArgs {
                trust: Some("suspicious".to_string()),
                audience: None,
            }),
            ..args()
        }
    }

    fn key(call: Option<&str>) -> PermitKey {
        declaration_key(&args(), call)
    }

    fn vouched(runtime: &Runtime, family: &TrajectoryId, call: Option<&str>) -> bool {
        let log = runtime.inner.log(family).expect("the log reads");
        standing_in(&log, &key(call)).is_ok()
    }

    fn operational(outcome: &RemedyOutcome) -> bool {
        matches!(
            outcome,
            RemedyOutcome::Refused {
                reason: RemedyRefusal::Runtime { .. }
            }
        )
    }

    fn unvouched(outcome: &RemedyOutcome) -> bool {
        matches!(
            outcome,
            RemedyOutcome::Refused {
                reason: RemedyRefusal::UnvouchedDeclaration
            }
        )
    }

    fn returned(outcome: &RemedyOutcome) -> bool {
        matches!(outcome, RemedyOutcome::Returned { .. })
    }

    fn spending(runtime: &Runtime) -> usize {
        count(runtime, |observation| {
            matches!(observation, HostObservation::AheadDeclared { spends: Some(_), .. })
        })
    }

    fn sibling() -> TrajectoryId {
        TrajectoryId("cc:sibling".to_string())
    }

    #[tokio::test]
    async fn a_retried_mcp_call_replays_its_declaration_after_the_vouch_is_spent() {
        let replicas = replicas();
        let Replicas { a, b, .. } = &replicas;
        a.vouch(&key(Some("c")), &acting(), None);
        assert!(returned(&a.declare_vouched(args(), Some("c"))));
        assert!(!vouched(a, &root(), Some("c")), "the declaration spent the vouch");
        assert!(returned(&b.declare_vouched(args(), Some("c"))));
        assert!(matches!(
            b.declare_vouched(other_args(), Some("c")),
            RemedyOutcome::Refused {
                reason: RemedyRefusal::AttemptReused
            }
        ));
        assert_eq!(declarations(a), 1);
    }

    /// With no expected actor on the daemon path, a call two actors declared under is
    /// ambiguous to replay.
    #[tokio::test]
    async fn a_call_two_actors_declared_under_is_ambiguous_to_replay() {
        let replicas = replicas();
        let Replicas { a, .. } = &replicas;
        declare(a, "seed", bare()).await;
        let call = AheadAttempt::McpCall("c".to_string());
        for actor in [acting(), child()] {
            a.inner
                .append_host(&root(), &declared_as(a, &actor, call.clone()))
                .expect("the record lands");
        }
        assert!(matches!(
            a.replay_named("c", &RemedyArguments::from(args())),
            Ok(Some(RemedyOutcome::Refused {
                reason: RemedyRefusal::AmbiguousReplay
            }))
        ));
    }

    #[tokio::test]
    async fn a_vouch_the_turn_ended_declares_nothing() {
        let replicas = replicas();
        let Replicas { a, .. } = &replicas;
        a.vouch(&key(Some("c")), &acting(), None);
        a.record_turn_end(&acting(), EndKind::Boundary).expect("the turn ends");
        assert!(unvouched(&a.declare_vouched(args(), Some("c"))));
        assert_eq!(declarations(a), 0);
        assert!(!vouched(a, &root(), Some("c")));
    }

    #[tokio::test]
    async fn an_admission_that_fails_to_commit_spends_nothing_and_its_redelivery_declares() {
        let replicas = replicas();
        let Replicas { a, .. } = &replicas;
        a.vouch(&key(Some("c")), &acting(), None);
        a.store().contend_next_appends(u64::MAX);
        assert!(operational(&a.declare_vouched(args(), Some("c"))));
        a.store().contend_next_appends(0);
        a.store().fail_commit_after(0);
        assert!(operational(&a.declare_vouched(args(), Some("c"))));
        assert_eq!(declarations(a), 0);
        assert!(vouched(a, &root(), Some("c")), "the vouch is live");
        assert!(returned(&a.declare_vouched(args(), Some("c"))));
        assert_eq!(spending(a), 1);
        assert!(!vouched(a, &root(), Some("c")));
    }

    #[tokio::test]
    async fn a_publish_the_replica_cannot_follow_refuses_admission_and_keeps_the_vouch() {
        let replicas = replicas_serving(Revision::Attesting(1));
        let Replicas { a, .. } = &replicas;
        let schema = ExecuteRemedyPlanArgs {
            return_schema: schema_bound().return_schema,
            ..args()
        };
        let schema_key = declaration_key(&schema, Some("c"));
        a.vouch(&schema_key, &acting(), None);
        a.vouch(&key(Some("d")), &acting(), None);
        let next = replicas.config(Revision::Unattesting);
        a.store()
            .publish_before_next_append(&line(), next.policy_file().bytes());
        let schema_args = || ExecuteRemedyPlanArgs {
            return_schema: schema_bound().return_schema,
            ..args()
        };
        assert!(operational(&a.declare_vouched(schema_args(), Some("c"))));
        assert_eq!(declarations(a), 0);
        let log = a.inner.log(&root()).expect("the log reads");
        assert!(standing_in(&log, &schema_key).is_ok(), "the vouch is live");
        assert!(a.store().head(&line()).expect("the head reads").is_some());
        replicas.reload(Revision::Unattesting);
        // The schema's route is gone under the new head: the redelivery is refused.
        assert!(operational(&a.declare_vouched(schema_args(), Some("c"))));
        let log = a.inner.log(&root()).expect("the log reads");
        assert!(standing_in(&log, &schema_key).is_ok(), "the vouch is still live");
        // A label valid under the new head declares once.
        let declared = a.declare_vouched(args(), Some("d"));
        assert!(returned(&declared), "{declared:?}");
        assert_eq!(declarations(a), 1);
    }

    #[tokio::test]
    async fn one_attempt_through_the_admission_stages_declares_once() {
        let replicas = replicas();
        let Replicas { a, b, .. } = &replicas;
        let arguments = RemedyArguments::from(args());
        a.vouch(&key(Some("c")), &acting(), None);
        assert!(matches!(b.replay_named("c", &arguments), Ok(None)));
        assert!(returned(&a.declare_vouched(args(), Some("c"))));
        assert_eq!(
            b.discover_holder(&key(Some("c"))).expect("the discovery reads"),
            Holder::Nobody
        );
        b.store().fail_next_reads(1);
        let scan = || Scan::NoHolder { call: "c".to_string() };
        assert!(operational(&b.admit(scan(), &key(Some("c")), &arguments)));
        assert!(returned(&b.admit(scan(), &key(Some("c")), &arguments)));
        assert!(matches!(
            b.admit(scan(), &key(Some("c")), &RemedyArguments::from(other_args())),
            RemedyOutcome::Refused {
                reason: RemedyRefusal::AttemptReused
            }
        ));
        assert_eq!(declarations(a), 1);
        assert_eq!(spending(a), 1);
        assert_eq!(
            count(a, |observation| matches!(observation, HostObservation::Released { .. })),
            0
        );
    }

    #[tokio::test]
    async fn two_families_vouching_one_unnamed_key_spend_neither() {
        let replicas = replicas();
        let Replicas { a, .. } = &replicas;
        a.create_session(sibling(), None).expect("the sibling opens");
        let sibling_actor = Actor {
            root: sibling(),
            child: None,
        };
        a.vouch(&key(None), &acting(), None);
        a.vouch(&key(None), &sibling_actor, None);
        assert!(unvouched(&a.declare_vouched(args(), None)));
        assert!(vouched(a, &root(), None));
        assert!(vouched(a, &sibling(), None));
    }

    #[tokio::test]
    async fn a_discovery_whose_index_does_not_read_is_operational() {
        let replicas = replicas();
        let Replicas { a, .. } = &replicas;
        a.vouch(&key(Some("c")), &acting(), None);
        assert!(matches!(a.replay_named("c", &RemedyArguments::from(args())), Ok(None)));
        a.store().fail_next_reads(1);
        assert!(a.discover_holder(&key(Some("c"))).is_err());
        a.store().fail_next_reads(1);
        assert!(operational(&a.declare_vouched(args(), Some("c"))));
        assert_eq!(declarations(a), 0);
        assert!(vouched(a, &root(), Some("c")));
        assert!(returned(&a.declare_vouched(args(), Some("c"))));
        assert_eq!(declarations(a), 1);
    }

    /// A sibling that vouched and released is a candidate, not a holder: its log must read.
    fn released_sibling(runtime: &Runtime) {
        runtime.create_session(sibling(), None).expect("the sibling opens");
        let sibling_actor = Actor {
            root: sibling(),
            child: None,
        };
        runtime.vouch(&key(Some("c")), &sibling_actor, None);
        runtime
            .inner
            .append_host(
                &sibling(),
                &HostObservation::Released {
                    actor: host_actor(&sibling_actor),
                    key: key(Some("c")).wire(),
                },
            )
            .expect("the release lands");
    }

    #[tokio::test]
    async fn a_candidate_family_that_does_not_read_fails_discovery_operationally() {
        let replicas = replicas();
        let Replicas { a, .. } = &replicas;
        released_sibling(a);
        a.vouch(&key(Some("c")), &acting(), None);
        a.store().fail_reads_of(&sibling(), 1);
        assert!(a.discover_holder(&key(Some("c"))).is_err());
        a.store().fail_reads_of(&sibling(), 1);
        assert!(operational(&a.declare_vouched(args(), Some("c"))));
        assert_eq!(declarations(a), 0);
        assert!(vouched(a, &root(), Some("c")));
        assert!(returned(&a.declare_vouched(args(), Some("c"))));
    }

    #[tokio::test]
    async fn a_sibling_recheck_that_does_not_read_aborts_the_admission() {
        let replicas = replicas();
        let Replicas { a, .. } = &replicas;
        released_sibling(a);
        a.vouch(&key(Some("c")), &acting(), None);
        assert_eq!(
            a.discover_holder(&key(Some("c"))).expect("the discovery reads"),
            Holder::One(root())
        );
        a.store().fail_reads_of(&sibling(), 1);
        let scan = Scan::Holder {
            root: root(),
            call: Some("c".to_string()),
        };
        assert!(operational(&a.admit(
            scan,
            &key(Some("c")),
            &RemedyArguments::from(args())
        )));
        assert_eq!(declarations(a), 0);
        assert!(vouched(a, &root(), Some("c")));
        assert!(returned(&a.declare_vouched(args(), Some("c"))));
    }

    #[tokio::test]
    async fn a_revouch_names_the_vouch_the_declaration_spends() {
        let replicas = replicas();
        let Replicas { a, .. } = &replicas;
        a.vouch(&key(None), &acting(), None);
        a.vouch(&key(None), &acting(), None);
        let log = a.inner.log(&root()).expect("the log reads");
        let seqs: Vec<u64> = log
            .host_records()
            .iter()
            .filter(|record| matches!(record.observation, HostObservation::Vouched { .. }))
            .map(|record| record.seq)
            .collect();
        let [first, second] = seqs[..] else {
            panic!("two vouches: {seqs:?}");
        };
        assert_eq!(standing_in(&log, &key(None)).expect("the vouch stands").seq, second);
        assert!(returned(&a.declare_vouched(args(), None)));
        let state = ahead(a);
        assert_eq!(state.declared_by_attempt(&AheadAttempt::Vouch(second)).len(), 1);
        assert!(state.declared_by_attempt(&AheadAttempt::Vouch(first)).is_empty());
    }
}
