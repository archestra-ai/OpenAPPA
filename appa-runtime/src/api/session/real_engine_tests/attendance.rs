use super::*;
use crate::api::{RemedyOutcome, RemedyRefusal};
use appa_engine::fact::Fact;
use appa_runtime_api::{Attendance, CONTROL_TOOL, HookDecision, HookEvent, SessionSettings};

/// A family run unattended: a stock `approve` person approver that answers only unattended
/// families, beside a `hitl` desk that asks a person. Both cover the `wire` block.
const UNATTENDED_APPROVER: &str = r#"
[policy]
version = 2

[[policy.tool]]
name = "wire"
parameters = { type = "object", properties = { amount = { type = "integer" } } }
requires = { attention = ["irreversible"] }
delta = {}

[[policy.authority]]
name = "person-approval"
unattended_only = true
[policy.authority.permits]
attention = ["irreversible"]

[[policy.authority]]
name = "desk"
[policy.authority.permits]
attention = ["irreversible"]

[externals]
timeout_ms = 2000
max_body_bytes = 65536

[externals.authorities.person-approval]
builtin = "approve"

[externals.authorities.desk]
builtin = "hitl"
"#;

const PERSON_APPROVAL: &str = "person-approval";

fn opened_at(db: &std::path::Path) -> Runtime {
    Runtime::open(config_from(UNATTENDED_APPROVER), db.to_path_buf(), None).expect("the deployment opens")
}

fn family(dir: &tempfile::TempDir) -> (Runtime, Session) {
    let runtime = opened_at(&dir.path().join("appa.db"));
    let session = runtime
        .create_pinned_session(root(), None, SessionSettings::default())
        .expect("a fresh id opens");
    (runtime, session)
}

fn actor() -> Actor {
    Actor {
        root: root(),
        child: None,
    }
}

async fn attend(runtime: &Runtime, state: Attendance) {
    let decision = crate::hooks::handle(runtime, HookEvent::Attendance { actor: actor(), state }).await;
    assert_eq!(decision, HookDecision::Ack);
}

/// The authorities each offer opened on `root` consults, by its quoted id, in opening order.
fn opened(runtime: &Runtime) -> Vec<(OfferId, Vec<String>)> {
    let plans = runtime.log_facts(&root()).into_iter().filter_map(|fact| match fact {
        Fact::OfferOpened { trajectory, plan, .. } if trajectory == root() => Some(
            plan.required
                .iter()
                .map(|required| required.authority.as_str().to_string())
                .collect(),
        ),
        _ => None,
    });
    runtime.minted_offers(&root(), &root()).into_iter().zip(plans).collect()
}

fn rulings_by(runtime: &Runtime, authority: &str) -> usize {
    runtime
        .log_facts(&root())
        .iter()
        .filter(|fact| matches!(fact, Fact::Ruling { authority: ruled, .. } if ruled.as_str() == authority))
        .count()
}

fn host_records(runtime: &Runtime) -> usize {
    runtime.inner.log(&root()).expect("the log reads").host_records().len()
}

async fn proposed(session: &Session, amount: u64) -> ToolCallDecision {
    session
        .on_tool_call(wire(amount), false)
        .await
        .expect("the call is judged")
}

/// The block's offers stand, and the person approver's plan is one of them: the block commits
/// and the approval the runtime applies to it does not.
async fn retained(runtime: &Runtime, session: &Session) -> OfferId {
    runtime.store().fail_commit_after(1);
    let failed = session.on_tool_call(wire(500), false).await;
    assert!(matches!(failed, Err(EventError::Storage(_))), "{failed:?}");
    opened(runtime)
        .into_iter()
        .find(|(_, plan)| plan == &[PERSON_APPROVAL.to_string()])
        .map(|(offer, _)| offer)
        .expect("the block offers the person approver's plan")
}

/// Quote `offer` on the control call, as an embedded host's model does.
async fn vouch(runtime: &Runtime, offer: &OfferId) {
    let control = crate::hooks::handle(
        runtime,
        HookEvent::ToolCall {
            actor: actor(),
            call: ProposedCall {
                tool: CONTROL_TOOL.to_string(),
                arguments: raw(serde_json::json!({ "offer_id": offer.0 })),
                cwd: None,
            },
            call_id: None,
            spawn: None,
            prompt: None,
            ruling: None,
            declares: None,
        },
    )
    .await;
    assert_eq!(control, HookDecision::PassControl);
}

async fn execute(runtime: &Runtime, offer: &OfferId) -> RemedyOutcome {
    let args = serde_json::from_value(serde_json::json!({ "offer_id": offer.0 })).expect("the arguments parse");
    runtime.execute_embedded_remedy(&actor(), args).await
}

fn refused_to_attended() -> RemedyOutcome {
    RemedyOutcome::Refused {
        reason: RemedyRefusal::UnattendedOnly {
            authority: PERSON_APPROVAL.to_string(),
        },
    }
}

/// While the window is open no call is auto-applied, whoever sent it, and its block plans
/// only the desk; the status read says the window is open, and opening it again records
/// nothing. Once closed, the next call is auto-applied through the person approver.
#[tokio::test]
async fn an_open_window_plans_no_unattended_approver_and_auto_applies_nothing() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (runtime, session) = family(&dir);
    attend(&runtime, Attendance::Open).await;
    attend(&runtime, Attendance::Open).await;
    let moves = |runtime: &Runtime| {
        runtime
            .log_facts(&root())
            .iter()
            .filter(|fact| matches!(fact, Fact::Attendance { .. }))
            .count()
    };
    assert_eq!(moves(&runtime), 1);
    assert_eq!(
        runtime.status(&root()).map(|status| status.attendance),
        Some(Attendance::Open)
    );

    for amount in [500, 600] {
        let decision = proposed(&session, amount).await;
        assert!(matches!(decision, ToolCallDecision::Deny { .. }), "{decision:?}");
    }
    let consulted: Vec<Vec<String>> = opened(&runtime).into_iter().map(|(_, plan)| plan).collect();
    assert_eq!(consulted, [vec!["desk".to_string()], vec!["desk".to_string()]]);
    assert_eq!(rulings_by(&runtime, PERSON_APPROVAL), 0);

    attend(&runtime, Attendance::Closed).await;
    assert_eq!(moves(&runtime), 2);
    let decision = proposed(&session, 700).await;
    assert!(matches!(decision, ToolCallDecision::Allow { .. }), "{decision:?}");
    assert_eq!(rulings_by(&runtime, PERSON_APPROVAL), 1);
}

/// An approval the person approver granted while the family ran unattended, and that no call
/// spent, does not survive the window opening: the same call is blocked for the person, and
/// after the window closes it is approved afresh.
#[tokio::test]
async fn opening_the_window_supersedes_a_standing_unattended_approval() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (runtime, session) = family(&dir);
    let offer = retained(&runtime, &session).await;
    vouch(&runtime, &offer).await;
    assert!(matches!(
        execute(&runtime, &offer).await,
        RemedyOutcome::Authorized { .. }
    ));

    attend(&runtime, Attendance::Open).await;
    let decision = proposed(&session, 500).await;
    assert!(matches!(decision, ToolCallDecision::Deny { .. }), "{decision:?}");

    attend(&runtime, Attendance::Closed).await;
    let decision = proposed(&session, 500).await;
    assert!(matches!(decision, ToolCallDecision::Allow { .. }), "{decision:?}");
    let approvals = runtime
        .log_facts(&root())
        .iter()
        .filter(|fact| matches!(fact, Fact::CallApproved { .. }))
        .count();
    assert_eq!(approvals, 2, "the released call spent a fresh approval");
}

/// A retained offer that consults the person approver is refused with its own class while
/// the window is open, even on a vouch recorded before it opened, and nothing is claimed or
/// consulted. After the window closes, the same offer executes.
#[tokio::test]
async fn a_retained_unattended_offer_is_refused_while_the_window_is_open() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (runtime, session) = family(&dir);
    let offer = retained(&runtime, &session).await;
    vouch(&runtime, &offer).await;
    attend(&runtime, Attendance::Open).await;
    let before = host_records(&runtime);
    assert_eq!(execute(&runtime, &offer).await, refused_to_attended());
    assert_eq!(host_records(&runtime), before, "a refused claim records nothing");
    vouch(&runtime, &offer).await;
    assert_eq!(execute(&runtime, &offer).await, refused_to_attended());
    assert_eq!(rulings_by(&runtime, PERSON_APPROVAL), 0);

    attend(&runtime, Attendance::Closed).await;
    vouch(&runtime, &offer).await;
    assert!(matches!(
        execute(&runtime, &offer).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert!(matches!(proposed(&session, 500).await, ToolCallDecision::Allow { .. }));
    assert_eq!(rulings_by(&runtime, PERSON_APPROVAL), 1);
}

/// A reopened deployment replays the window from its records and plans as the open window
/// does; a log whose record moves the window to where it already stood is refused.
#[tokio::test]
async fn the_window_replays_from_its_records() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let db = dir.path().join("appa.db");
    let moved = {
        let (runtime, _) = family(&dir);
        attend(&runtime, Attendance::Open).await;
        runtime
            .log_facts(&root())
            .into_iter()
            .skip_while(|fact| matches!(fact, Fact::TrajectoryOpened(_)))
            .map(|fact| serde_json::to_value(fact).expect("a fact serializes"))
            .collect::<Vec<_>>()
    };
    let runtime = opened_at(&db);
    assert_eq!(
        runtime.status(&root()).map(|status| status.attendance),
        Some(Attendance::Open)
    );
    let session = runtime.session(&root(), &root()).expect("the trajectory reopens");
    assert!(matches!(proposed(&session, 500).await, ToolCallDecision::Deny { .. }));
    assert_eq!(rulings_by(&runtime, PERSON_APPROVAL), 0);

    let closed = serde_json::to_string(&moved)
        .expect("the batch serializes")
        .replace("\"open\"", "\"closed\"");
    runtime.store().corrupt_batch(&root(), 1, closed.as_bytes());
    assert!(matches!(
        session.on_tool_call(wire(600), false).await,
        Err(EventError::UntrustedLog(_))
    ));
}

/// A family no host ever attended records no attendance, and a log written before the window
/// existed reads back closed: its blocks are auto-applied as they were.
#[tokio::test]
async fn a_log_without_attendance_replays_unattended() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let db = dir.path().join("appa.db");
    {
        let (runtime, session) = family(&dir);
        assert!(matches!(proposed(&session, 500).await, ToolCallDecision::Allow { .. }));
        assert!(
            !runtime
                .log_facts(&root())
                .iter()
                .any(|fact| matches!(fact, Fact::Attendance { .. }))
        );
    }
    let runtime = opened_at(&db);
    assert_eq!(
        runtime.status(&root()).map(|status| status.attendance),
        Some(Attendance::Closed)
    );
    let session = runtime.session(&root(), &root()).expect("the trajectory reopens");
    session
        .on_tool_result(
            wire(500),
            ToolOutcome::Success {
                body: OutcomeBody::Available("sent".to_string()),
            },
        )
        .await
        .expect("the replayed dispatch takes its result");
    assert!(matches!(proposed(&session, 600).await, ToolCallDecision::Allow { .. }));
}
