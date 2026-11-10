use super::super::{OpenError, OutcomeBody, Runtime};
use super::*;
use crate::api::{Actor, RemedyDecision, SpawnBinding, ToolCallDecision, ToolOutcome, ToolResultDecision};
use crate::config::Config;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The events without a host call id, as a harness that names none reports them.
impl Session {
    pub(crate) fn trajectory(&self) -> &TrajectoryId {
        &self.trajectory
    }

    pub async fn on_tool_call(&self, call: ProposedCall, spawn: bool) -> Result<ToolCallDecision, EventError> {
        self.on_tool_call_identified(call, None, spawn.then_some(SpawnKind::Single), None)
            .await
    }

    pub async fn on_tool_result(&self, call: ProposedCall, o: ToolOutcome) -> Result<ToolResultDecision, EventError> {
        self.on_tool_result_identified(call, None, o).await
    }

    pub async fn on_spawn_result(
        &self,
        call: ProposedCall,
        outcome: ToolOutcome,
        child: Option<TrajectoryId>,
        value: Option<String>,
    ) -> Result<SpawnResultDecision, EventError> {
        self.on_spawn_result_identified(call, None, outcome, child, value).await
    }

    pub fn on_child_start(&self, id: TrajectoryId, spawn: SpawnRef) -> Result<Session, EventError> {
        self.start_child(id, spawn).map(|(child, _)| child)
    }
}

fn classify_report(
    call: &ProposedCall,
    canonical: impl FnOnce() -> Option<Vec<u8>>,
    open: &[OpenDispatch],
) -> Result<appa_engine::value::DispatchId, UnreportableOutcome> {
    classify_report_identified(
        call,
        None,
        canonical,
        open,
        std::iter::empty(),
        &appa_engine::value::TrajectoryId::new("test"),
    )
}

/// One fixture configuration, from its whole TOML text. The file has
/// to exist on disk because `Config::load` reads the policy file's
/// bytes, which the opening record keys the deployment by.
fn config_from(text: &str) -> Config {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, text).expect("the fixture writes");
    Config::load(&path).expect("the fixture validates")
}

fn config_with(policy: &str, authority_url: Option<&str>) -> Config {
    let binding = match authority_url {
        Some(url) => format!("[externals.authorities.approver]\nurl = \"{url}\"\n"),
        None => String::new(),
    };
    let text = format!("[policy]\n{policy}\n[externals]\ntimeout_ms = 2000\nmax_body_bytes = 65536\n{binding}");
    config_from(&text)
}

const FETCH_AND_SEND: &str = r#"
version = 2

# A neutral fetch: its result folds at the trajectory's own label, so the
# lifecycle tests release it freely. `taint` brings outside content in at the
# low rank; releasing it takes the narrowing acceptance its block offers.
[[policy.tool]]
name = "fetch"
parameters = { type = "object", properties = { b = { type = "integer" }, a = { type = "integer" } } }
delta = {}

[[policy.tool]]
name = "taint"
parameters = { type = "object", properties = { a = { type = "integer" } } }
delta = { trust = "suspicious" }

[[policy.tool]]
name = "send"
requires = { trust = "trusted" }
delta = {}

# The child tests fork under this fixture; branching takes declared context control.
[policy.deployment]
context_control = true
"#;

fn root() -> TrajectoryId {
    TrajectoryId("cc:root".to_string())
}

fn taint(spelling: serde_json::Value) -> ProposedCall {
    ProposedCall {
        tool: "taint".to_string(),
        arguments: raw(spelling),
        cwd: None,
    }
}

fn fetch(spelling: serde_json::Value) -> ProposedCall {
    ProposedCall {
        tool: "fetch".to_string(),
        arguments: raw(spelling),
        cwd: None,
    }
}

/// Spawn with the child's return declared: the marked spawn blocks on the return
/// menu, the parent picks the plan routing the return through `sanitizer` (none: as
/// spoken) floored at `label`, and the approved spawn releases with its fork.
async fn declare_spawn(
    session: &Session,
    spawn: ProposedCall,
    sanitizer: Option<&str>,
    label: crate::engine::LabelSpelling,
) -> SpawnBinding {
    let call = authorize_spawn(session, spawn, sanitizer, label).await;
    let ToolCallDecision::Allow {
        spawn: Some(binding), ..
    } = session
        .on_tool_call(call.proposed(), true)
        .await
        .expect("the approved spawn releases")
    else {
        panic!("a context-controlled spawn releases a fork binding");
    };
    binding
}

async fn authorize_spawn(
    session: &Session,
    spawn: ProposedCall,
    sanitizer: Option<&str>,
    label: crate::engine::LabelSpelling,
) -> ExactCall {
    let route = match sanitizer {
        None => appa_runtime_api::OfferedReturn::AsSpoken,
        Some(sanitizer) => appa_runtime_api::OfferedReturn::Sanitized {
            sanitizer: sanitizer.to_string(),
        },
    };
    authorize_spawn_as(session, spawn, SpawnKind::Single, route, Some(label)).await
}

async fn authorize_spawn_as(
    session: &Session,
    spawn: ProposedCall,
    kind: SpawnKind,
    route: appa_runtime_api::OfferedReturn,
    label: Option<crate::engine::LabelSpelling>,
) -> ExactCall {
    let ToolCallDecision::Deny { offers, .. } = session
        .on_tool_call_identified(spawn, None, Some(kind), None)
        .await
        .expect("the spawn is judged")
    else {
        panic!("a marked spawn blocks until its return is declared");
    };
    let quoted = offers
        .iter()
        .find(|offer| offer.returns.as_ref() == Some(&route))
        .map(|offer| OfferId(offer.id.clone()))
        .expect("the menu offers the requested return route");
    let log = session.inner.log(&session.root).expect("the log reads");
    let offer = crate::engine::resolve_rendered(&log, &quoted).expect("the quoted id resolves");
    let RemedyDecision::Authorized { call } = session
        .on_remedy(
            offer,
            RemedyArguments {
                label,
                return_schema: None,
            },
            None,
            None,
        )
        .await
        .expect("the declaration executes")
    else {
        panic!("a return declaration approves the spawn");
    };
    call
}

/// The bare declaration: the return crosses as spoken, floored at the parent's
/// current label.
async fn declared_spawn(session: &Session, spawn: ProposedCall) -> SpawnBinding {
    declare_spawn(session, spawn, None, crate::engine::LabelSpelling::default()).await
}

async fn open_child(session: &mut Session, spawn: ProposedCall, child: TrajectoryId) -> Session {
    let binding = declared_spawn(session, spawn).await;
    session
        .on_child_start(child, SpawnRef::Binding(binding))
        .expect("the fork binds and the child opens")
}

fn floor_trust(trust: &str) -> crate::engine::LabelSpelling {
    crate::engine::LabelSpelling {
        trust: Some(trust.to_string()),
        audience: None,
    }
}

fn floor_audience(audience: &[&str]) -> crate::engine::LabelSpelling {
    crate::engine::LabelSpelling {
        trust: None,
        audience: Some(audience.iter().map(|reader| reader.to_string()).collect()),
    }
}

/// Open a child whose return the parent floored at `floor`, crossing as spoken.
async fn open_child_floored(
    session: &mut Session,
    spawn: ProposedCall,
    child: TrajectoryId,
    floor: crate::engine::LabelSpelling,
) -> Session {
    let binding = declare_spawn(session, spawn, None, floor).await;
    session
        .on_child_start(child, SpawnRef::Binding(binding))
        .expect("the fork binds and the child opens")
}

/// Open a child whose return the parent routed through `sanitizer`, floored at `label`.
async fn open_child_via(
    session: &mut Session,
    spawn: ProposedCall,
    child: TrajectoryId,
    sanitizer: &str,
    label: crate::engine::LabelSpelling,
) -> Session {
    let binding = declare_spawn(session, spawn, Some(sanitizer), label).await;
    session
        .on_child_start(child, SpawnRef::Binding(binding))
        .expect("the fork binds and the child opens")
}

async fn stub(answer: serde_json::Value) -> String {
    use axum::routing::post;
    let app = axum::Router::new().route(
        "/",
        post(move || {
            let answer = answer.clone();
            async move { axum::Json(serde_json::json!({"version": 1, "answer": answer})) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback stub binds");
    let addr = listener.local_addr().expect("the stub has an address");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("the stub serves");
    });
    format!("http://{addr}/")
}

#[test]
fn a_policy_in_the_documented_dialect_builds_the_engine() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    assert!(Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None).is_ok());
}

#[test]
fn an_undialectal_policy_refuses_open() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let config = config_with("version = 2\nstray_key = true\n", None);
    assert!(matches!(
        Runtime::open(config, dir.path().join("appa.db"), None),
        Err(OpenError::Policy(_)),
    ));
}

#[test]
fn an_inline_impl_binding_is_refused() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let policy = r#"
version = 2
[[policy.authority]]
name = "approver"
[policy.authority.permits]
attention = ["irreversible"]
[policy.authority.implementation]
builtin = "approve"
"#;
    assert!(matches!(
        Runtime::open(config_with(policy, None), dir.path().join("appa.db"), None),
        Err(OpenError::Policy(error))
            if matches!(*error, appa_policy::ConfigError::ForbiddenInlineBinding { .. }),
    ));
}

#[test]
fn a_policy_naming_an_unbound_authority_opens() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let policy = r#"
version = 2
[[policy.authority]]
name = "approver"
[policy.authority.permits]
attention = ["irreversible"]
"#;
    assert!(Runtime::open(config_with(policy, None), dir.path().join("appa.db"), None).is_ok());
}

#[test]
fn a_non_neutral_starting_label_seeds_the_root_and_survives_a_restart() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let db = dir.path().join("appa.db");
    let policy = r#"
version = 2
[[policy.tool]]
name = "fetch"
[policy.deployment]
starting_label = { trust = "suspicious" }
"#;
    let runtime = Runtime::open(config_with(policy, None), db.clone(), None).expect("the deployment opens");
    runtime.create_session(root(), None).expect("a fresh id opens");
    let live = runtime.status(&root()).expect("a fresh root answers");
    assert_eq!((live.trust.as_str(), live.audience.as_str()), ("suspicious", "public"));
    drop(runtime);
    let reopened = Runtime::open(config_with(policy, None), db, None).expect("the deployment reopens");
    let restarted = reopened.status(&root()).expect("the persisted root answers");
    assert_eq!((restarted.trust, restarted.audience), (live.trust, live.audience));
}

#[test]
fn liveness_of_an_unopened_child_is_unknown() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    runtime.create_session(root(), None).expect("a fresh id opens");
    assert!(runtime.live(&root(), &root()).is_ok());
    assert!(matches!(
        runtime.live(&root(), &TrajectoryId("cc:never-bound".to_string())),
        Err(EventError::UnknownTrajectory),
    ));
}

#[test]
fn a_reserved_tool_name_refuses_open() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let policy = r#"
version = 2
[[policy.tool]]
name = "appa/execute_remedy_plan"
"#;
    assert!(matches!(
        Runtime::open(config_with(policy, None), dir.path().join("appa.db"), None),
        Err(OpenError::ReservedTool(_)),
    ));
}

#[tokio::test]
async fn an_allowed_call_is_released_with_canonical_bytes() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    let decision = session
        .on_tool_call(fetch(serde_json::json!({"b": 1, "a": 2})), false)
        .await
        .expect("the call is decided");
    assert!(matches!(decision, ToolCallDecision::Allow { spawn: None, .. }));
    let open = runtime
        .open_dispatches(&root(), &root())
        .pop()
        .expect("the released call opened a dispatch");
    assert_eq!(open.bytes, br#"{"a":2,"b":1}"#.to_vec());
    let log = runtime.log_facts(&root());

    let facts: Vec<appa_engine::fact::Fact> = log.clone();
    assert!(matches!(
        facts.last(),
        Some(appa_engine::fact::Fact::DispatchOpened { .. })
    ));
}

#[tokio::test]
async fn identified_calls_run_in_parallel_and_report_after_restart_in_any_order() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let db = dir.path().join("appa.db");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), db.clone(), None).expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    let call = fetch(serde_json::json!({"a": 1}));

    for call_id in ["toolu-1", "toolu-2"] {
        assert!(matches!(
            session
                .on_tool_call_identified(call.clone(), Some(call_id.to_string()), None, None)
                .await
                .expect("the identified call releases"),
            ToolCallDecision::Allow { spawn: None, .. }
        ));
    }
    assert_eq!(runtime.open_dispatches(&root(), &root()).len(), 2);
    drop(session);
    drop(runtime);

    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), db, None).expect("the deployment reopens");
    let session = runtime.session(&root(), &root()).expect("the session reattaches");
    for (call_id, body) in [("toolu-2", "second"), ("toolu-1", "first")] {
        assert_eq!(
            session
                .on_tool_result_identified(
                    call.clone(),
                    Some(call_id.to_string()),
                    ToolOutcome::Success {
                        body: OutcomeBody::Available(body.to_string()),
                    },
                )
                .await
                .expect("the identified result is correlated"),
            ToolResultDecision::Keep,
        );
    }
    assert!(runtime.open_dispatches(&root(), &root()).is_empty());
}

/// Parallel calls need every one of them identified, not just the newest. An unidentified
/// call already open cannot be told apart from this one when its outcome arrives — the two
/// share a tool and can share bytes — so the second dispatch is refused whichever end is
/// unidentified.
#[tokio::test]
async fn a_call_beside_an_unidentified_one_is_refused_even_when_it_carries_an_identity() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    let call = fetch(serde_json::json!({"a": 1}));

    assert!(matches!(
        session
            .on_tool_call(call.clone(), false)
            .await
            .expect("the unidentified call releases"),
        ToolCallDecision::Allow { spawn: None, .. }
    ));
    assert!(matches!(
        session
            .on_tool_call_identified(
                fetch(serde_json::json!({"a": 2})),
                Some("toolu-1".to_string()),
                None,
                None
            )
            .await,
        Err(EventError::CallOutstanding),
    ));
    assert_eq!(runtime.open_dispatches(&root(), &root()).len(), 1);

    // Once the unidentified call is settled, an identified one opens beside nothing.
    session
        .on_tool_result(
            call,
            ToolOutcome::Success {
                body: OutcomeBody::Available("first".to_string()),
            },
        )
        .await
        .expect("the unidentified outcome is correlated");
    assert!(matches!(
        session
            .on_tool_call_identified(
                fetch(serde_json::json!({"a": 2})),
                Some("toolu-1".to_string()),
                None,
                None
            )
            .await
            .expect("the identified call releases"),
        ToolCallDecision::Allow { spawn: None, .. }
    ));
}

/// A fan-out wider than two drains. The outcome closing the third of five leaves four
/// dispatches open, and the count alone must not refuse it: a batch that opens nothing
/// is never the second call.
#[tokio::test]
async fn a_wide_parallel_fan_out_reports_every_call() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    let call = fetch(serde_json::json!({"a": 1}));
    let ids = ["toolu-1", "toolu-2", "toolu-3", "toolu-4", "toolu-5"];

    for call_id in ids {
        assert!(matches!(
            session
                .on_tool_call_identified(call.clone(), Some(call_id.to_string()), None, None)
                .await
                .expect("the identified call releases"),
            ToolCallDecision::Allow { spawn: None, .. }
        ));
    }
    assert_eq!(runtime.open_dispatches(&root(), &root()).len(), ids.len());

    for call_id in ids {
        assert_eq!(
            session
                .on_tool_result_identified(
                    call.clone(),
                    Some(call_id.to_string()),
                    ToolOutcome::Success {
                        body: OutcomeBody::Available(call_id.to_string()),
                    },
                )
                .await
                .expect("the identified result is correlated"),
            ToolResultDecision::Keep,
        );
    }
    assert!(runtime.open_dispatches(&root(), &root()).is_empty());
}

#[tokio::test]
async fn three_identified_calls_report_results_while_siblings_remain_open() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    let call = fetch(serde_json::json!({"a": 1}));

    for call_id in ["toolu-1", "toolu-2", "toolu-3"] {
        assert!(matches!(
            session
                .on_tool_call_identified(call.clone(), Some(call_id.to_string()), None, None)
                .await
                .expect("the identified call releases"),
            ToolCallDecision::Allow { spawn: None, .. }
        ));
    }
    assert_eq!(runtime.open_dispatches(&root(), &root()).len(), 3);

    for (call_id, body) in [("toolu-2", "second"), ("toolu-3", "third"), ("toolu-1", "first")] {
        assert_eq!(
            session
                .on_tool_result_identified(
                    call.clone(),
                    Some(call_id.to_string()),
                    ToolOutcome::Success {
                        body: OutcomeBody::Available(body.to_string()),
                    },
                )
                .await
                .expect("an identified result closes only its own dispatch"),
            ToolResultDecision::Keep,
        );
    }
    assert!(runtime.open_dispatches(&root(), &root()).is_empty());
}

#[tokio::test]
async fn a_host_call_id_cannot_be_reused() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    session
        .on_tool_call_identified(
            fetch(serde_json::json!({"a": 1})),
            Some("toolu-1".to_string()),
            None,
            None,
        )
        .await
        .expect("the first call releases");

    let reused = session
        .on_tool_call_identified(
            fetch(serde_json::json!({"a": 2})),
            Some("toolu-1".to_string()),
            None,
            None,
        )
        .await;
    assert!(matches!(reused, Err(EventError::CallIdReused)), "got {reused:?}");
    assert_eq!(runtime.open_dispatches(&root(), &root()).len(), 1);
}

#[tokio::test]
async fn ordinary_calls_overlap_an_unbound_spawn_but_a_second_spawn_does_not() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");

    let first = authorize_spawn(
        &session,
        fetch(serde_json::json!({"a": 1})),
        None,
        crate::engine::LabelSpelling::default(),
    )
    .await;
    assert!(matches!(
        session
            .on_tool_call_identified(
                first.proposed(),
                Some("spawn-1".to_string()),
                Some(SpawnKind::Single),
                None
            )
            .await
            .expect("the first spawn releases"),
        ToolCallDecision::Allow { spawn: Some(_), .. }
    ));
    assert!(matches!(
        session
            .on_tool_call_identified(
                fetch(serde_json::json!({"a": 2})),
                Some("ordinary-1".to_string()),
                None,
                None,
            )
            .await
            .expect("an ordinary call overlaps the spawn"),
        ToolCallDecision::Allow { spawn: None, .. }
    ));

    let second_proposal = session
        .on_tool_call_identified(
            fetch(serde_json::json!({"a": 3})),
            Some("spawn-2".to_string()),
            Some(SpawnKind::Single),
            None,
        )
        .await
        .expect("the second spawn is checked");
    let ToolCallDecision::Deny { offers, .. } = second_proposal else {
        panic!("the second spawn first declares its return");
    };
    let quoted = OfferId(offers[0].id.clone());
    let log = session.inner.log(&session.root).expect("the log reads");
    let offer = crate::engine::resolve_rendered(&log, &quoted).expect("the quoted id resolves");
    let RemedyDecision::Authorized { call: second } = session
        .on_remedy(
            offer,
            RemedyArguments {
                label: Some(crate::engine::LabelSpelling::default()),
                return_schema: None,
            },
            None,
            None,
        )
        .await
        .expect("the second return declaration executes")
    else {
        panic!("the return declaration authorizes the second spawn");
    };
    let second = session
        .on_tool_call_identified(
            second.proposed(),
            Some("spawn-2".to_string()),
            Some(SpawnKind::Single),
            None,
        )
        .await;
    assert!(matches!(second, Err(EventError::SpawnOutstanding)), "got {second:?}");
}

#[tokio::test]
async fn a_duplicate_argument_key_is_refused_and_opens_no_dispatch() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    let duplicated = ProposedCall {
        tool: "fetch".to_string(),
        arguments: serde_json::value::RawValue::from_string(r#"{"a":1,"a":2}"#.to_string())
            .expect("the fixture is well-formed JSON"),
        cwd: None,
    };
    let decision = session
        .on_tool_call(duplicated, false)
        .await
        .expect("the call is decided");
    assert!(
        matches!(decision, ToolCallDecision::Deny { .. }),
        "a duplicate key must be refused, not resolved by last-wins: {decision:?}"
    );
    assert!(
        runtime.open_dispatches(&root(), &root()).pop().is_none(),
        "a refused call opens no dispatch"
    );
}

#[tokio::test]
async fn a_success_admits_the_raw_result_and_closes() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    session
        .on_tool_call(fetch(serde_json::json!({"a": 1})), false)
        .await
        .expect("the call is decided");
    let kept = session
        .on_tool_result(
            fetch(serde_json::json!({"a": 1})),
            ToolOutcome::Success {
                body: OutcomeBody::Available("data".to_string()),
            },
        )
        .await
        .expect("the result is admitted");
    assert_eq!(kept, ToolResultDecision::Keep);
    assert!(
        runtime.open_dispatches(&root(), &root()).pop().is_none(),
        "the admitted result closed the dispatch",
    );
}

#[tokio::test]
async fn the_success_checkpoint_commits_once_across_a_lost_admission() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let url = stub(serde_json::json!({"body": "scrubbed"})).await;
    let runtime =
        Runtime::open(emitting_leak_config(&url), dir.path().join("appa.db"), None).expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    let outcome = || ToolOutcome::Success {
        body: OutcomeBody::Available("raw with pii".to_string()),
    };

    assert!(matches!(
        session
            .on_tool_call(leak(), false)
            .await
            .expect("the block is delivered"),
        ToolCallDecision::Deny { .. },
    ));
    let quoted = runtime
        .minted_offers(&root(), &root())
        .into_iter()
        .find(|offer| {
            matches!(
                runtime.offer_kind(&root(), offer),
                Some(crate::api::OfferKind::Sanitizer { .. })
            )
        })
        .expect("the block surfaced the sanitize plan");
    let offer = runtime.resolve_in(&root(), &quoted).expect("the quoted id resolves").0;
    assert!(matches!(
        session
            .on_remedy(offer, RemedyArguments::default(), None, None)
            .await
            .expect("the sanitize offer binds"),
        RemedyDecision::Authorized { .. },
    ));
    assert!(matches!(
        session
            .on_tool_call(leak(), false)
            .await
            .expect("the re-proposal resumes"),
        ToolCallDecision::Allow { spawn: None, .. }
    ));
    let base = runtime.log_basis(&root());

    runtime.store().fail_commit_after(1);
    assert!(matches!(
        session.on_tool_result(leak(), outcome()).await,
        Err(EventError::Storage(_)),
    ));
    let log = runtime.log_facts(&root());
    assert_eq!(
        runtime.log_basis(&root()),
        base + 1,
        "the checkpoint committed, the admission did not",
    );
    let effects = log
        .iter()
        .cloned()
        .find_map(|fact| match fact {
            appa_engine::fact::Fact::DispatchSucceeded { effects, .. } => Some(effects),
            _ => None,
        })
        .expect("the checkpoint recorded the observed success");
    assert_eq!(effects.len(), 1, "the checkpoint committed the declared effect");
    assert_eq!(
        runtime.open_dispatches(&root(), &root()).len(),
        1,
        "the checkpointed dispatch stays open for its admission",
    );

    let replaced = session
        .on_tool_result(leak(), outcome())
        .await
        .expect("the re-reported outcome admits");
    assert_eq!(
        replaced,
        ToolResultDecision::Deliver {
            value: "scrubbed".to_string()
        },
    );
    let log = runtime.log_facts(&root());
    assert_eq!(
        runtime.log_basis(&root()),
        base + 2,
        "the retry appended the admission alone — the checkpoint did not repeat",
    );
    let closed = log
        .iter()
        .cloned()
        .find_map(|fact| match fact {
            appa_engine::fact::Fact::DispatchClosed {
                outcome: appa_engine::fact::CloseOutcome::Success { effects },
                ..
            } => Some(effects),
            _ => None,
        })
        .expect("the admission closed the dispatch successfully");
    assert!(
        closed.is_empty(),
        "the close carries no effects: the checkpoint committed them once",
    );
}

#[tokio::test]
async fn a_failure_closes_with_no_effects_and_an_indeterminate_leaves_the_reservation() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    session
        .on_tool_call(fetch(serde_json::json!({"a": 1})), false)
        .await
        .expect("the call is decided");
    let kept = session
        .on_tool_result(
            fetch(serde_json::json!({"a": 1})),
            ToolOutcome::Failure {
                message: "exit 1".to_string(),
            },
        )
        .await
        .expect("the failure closes");
    assert_eq!(kept, ToolResultDecision::Keep);
    assert!(runtime.open_dispatches(&root(), &root()).pop().is_none());

    session
        .on_tool_call(fetch(serde_json::json!({"a": 1})), false)
        .await
        .expect("the second occurrence is decided");
    let kept = session
        .on_tool_result(fetch(serde_json::json!({"a": 1})), ToolOutcome::Indeterminate)
        .await
        .expect("the indeterminate closes");
    assert_eq!(kept, ToolResultDecision::Keep);
}

#[tokio::test]
async fn an_invalid_argument_call_returns_deny_feedback() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    let decision = session
        .on_tool_call(fetch(serde_json::json!({"a": "not a number"})), false)
        .await
        .expect("the refusal is delivered as feedback");
    assert!(matches!(decision, ToolCallDecision::Deny { .. }));
    assert!(runtime.open_dispatches(&root(), &root()).pop().is_none());
    let log = runtime.log_facts(&root());
    assert!(
        matches!(log.as_slice(), [appa_engine::fact::Fact::TrajectoryOpened(_)]),
        "an invalid call appends no record after the opening",
    );
}

#[tokio::test]
async fn a_tool_nothing_covers_is_refused_typed_before_it_runs() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    let refused = session
        .on_tool_call(
            ProposedCall {
                tool: "wrench".to_string(),
                arguments: raw(serde_json::json!({})),
                cwd: None,
            },
            false,
        )
        .await;
    assert!(matches!(
        refused,
        Err(EventError::UndeclaredTool { tool }) if tool == "wrench"
    ));
    assert!(only_the_opening(&runtime), "the refusal appends nothing");
}

fn latest_offer(runtime: &Runtime) -> OfferId {
    let quoted = runtime
        .minted_offers(&root(), &root())
        .into_iter()
        .next_back()
        .expect("the deny surfaced an offer");
    runtime.resolve_in(&root(), &quoted).expect("the quoted id resolves").0
}

const READ_ONLY: &str = r#"
version = 2

[[policy.tool]]
name = "read"
parameters = { type = "object", properties = { path = { type = "string" } } }
"#;

/// A root opened under one policy follows its deployment to the next: after a restart under
/// an edited policy, its earlier batches replay under the revision that decided them, and its
/// next call is decided at the head, exactly as a new root's is.
#[tokio::test]
async fn an_old_root_decides_at_the_head_its_restart_published() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let db = dir.path().join("appa.db");
    {
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), db.clone(), None).expect("the deployment opens");
        let session = runtime.create_session(root(), None).expect("a fresh id opens");
        session
            .on_tool_call(fetch(serde_json::json!({"a": 1})), false)
            .await
            .expect("the call is decided");
        session
            .on_tool_result(
                fetch(serde_json::json!({"a": 1})),
                ToolOutcome::Success {
                    body: OutcomeBody::Available("data".to_string()),
                },
            )
            .await
            .expect("the result is admitted");
    }
    let runtime = Runtime::open(config_with(READ_ONLY, None), db, None).expect("the edited deployment opens");

    let old = runtime.session(&root(), &root()).expect("the old root reopens");
    let refused = old.on_tool_call(fetch(serde_json::json!({"a": 2})), false).await;
    assert!(
        matches!(refused, Err(EventError::UndeclaredTool { tool }) if tool == "fetch"),
        "the old root follows the head, which declares no fetch"
    );
    let read = old
        .on_tool_call(
            ProposedCall {
                tool: "read".to_string(),
                arguments: raw(serde_json::json!({"path": "a.txt"})),
                cwd: None,
            },
            false,
        )
        .await
        .expect("the old root decides at the head");
    assert!(matches!(read, ToolCallDecision::Allow { spawn: None, .. }));
    let stamps: Vec<Option<u64>> = runtime
        .inner
        .log(&root())
        .expect("the log reads")
        .stamped_batches()
        .iter()
        .map(|batch| batch.revision.map(|revision| revision.value()))
        .collect();
    assert_eq!(
        stamps.first(),
        Some(&Some(1)),
        "the opening stays at the revision that decided it"
    );
    assert_eq!(
        stamps.last(),
        Some(&Some(2)),
        "the next decision is stamped with the head"
    );

    let new = runtime
        .create_session(TrajectoryId("cc:new".to_string()), None)
        .expect("a fresh id opens");
    let refused = new.on_tool_call(fetch(serde_json::json!({"a": 1})), false).await;
    assert!(
        matches!(refused, Err(EventError::UndeclaredTool { tool }) if tool == "fetch"),
        "fetch is gone for new roots"
    );
    let allowed = new
        .on_tool_call(
            ProposedCall {
                tool: "read".to_string(),
                arguments: raw(serde_json::json!({"path": "a.txt"})),
                cwd: None,
            },
            false,
        )
        .await
        .expect("the new root decides");
    assert!(
        matches!(allowed, ToolCallDecision::Allow { spawn: None, .. }),
        "the edited policy's tool releases"
    );
}

/// The head serves from memory; an earlier revision a root's batches were decided under is
/// read from the store to replay them, and its absence refuses the root.
#[tokio::test]
async fn a_missing_file_of_an_earlier_revision_refuses_the_root() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let db = dir.path().join("appa.db");
    {
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), db.clone(), None).expect("the deployment opens");
        runtime.create_session(root(), None).expect("a fresh id opens");
    }
    let runtime = Runtime::open(config_with(READ_ONLY, None), db, None).expect("the edited deployment opens");
    let session = runtime.session(&root(), &root()).expect("the old root reopens");
    runtime.store().forget_policy_files();
    let error = session
        .on_tool_call(fetch(serde_json::json!({"a": 1})), false)
        .await
        .expect_err("the event refuses");
    assert!(matches!(error, EventError::PolicyUnavailable(_)), "got {error:?}");
}

/// A revision names its file by the hash of its exact bytes: a stored file that no longer
/// hashes to that key is refused, even where it would compile to the same policy.
#[tokio::test]
async fn a_stored_file_that_is_not_its_revisions_bytes_is_refused() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let db = dir.path().join("appa.db");
    let opened = {
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), db.clone(), None).expect("the deployment opens");
        runtime.create_session(root(), None).expect("a fresh id opens");
        runtime.config_bytes()
    };
    let runtime = Runtime::open(config_with(READ_ONLY, None), db, None).expect("the edited deployment opens");
    let session = runtime.session(&root(), &root()).expect("the old root reopens");
    let mut tampered = opened;
    tampered.extend_from_slice(b"\n# tampered\n");
    runtime.store().corrupt_policy_files(&tampered);
    let error = session
        .on_tool_call(fetch(serde_json::json!({"a": 1})), false)
        .await
        .expect_err("the event refuses");
    assert!(matches!(error, EventError::PolicyUnavailable(_)), "got {error:?}");
}

#[tokio::test]
async fn a_corrupted_opening_batch_is_refused() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let db = dir.path().join("appa.db");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), db.clone(), None).expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    let tampered = serde_json::to_string(&runtime.log_facts(&root()))
        .expect("the opening serializes")
        .replace("cc:root", "cc:evil");
    runtime.store().corrupt_batch(&root(), 0, tampered.as_bytes());
    let error = session
        .on_tool_call(fetch(serde_json::json!({"a": 1})), false)
        .await
        .expect_err("the event refuses");
    assert!(matches!(error, EventError::PolicyUnavailable(_)), "got {error:?}");
}

/// Removing a policy's authority together with its binding cannot strand an old root: the
/// root follows the head, which no longer declares the call that needed the authority. When
/// both come back, the root decides at that head and the restored binding answers.
#[tokio::test]
async fn an_old_root_follows_the_head_through_a_removed_binding_and_its_return() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let db = dir.path().join("appa.db");
    {
        let runtime = Runtime::open(
            config_with(ATTENTION, Some("https://approver.internal/")),
            db.clone(),
            None,
        )
        .expect("the deployment opens");
        let session = runtime.create_session(root(), None).expect("a fresh id opens");
        session
            .on_tool_call(wire(500), false)
            .await
            .expect("the block is delivered");
    }

    {
        let runtime =
            Runtime::open(config_with(READ_ONLY, None), db.clone(), None).expect("the edited deployment opens");
        let session = runtime.session(&root(), &root()).expect("the old root reopens");
        assert!(
            matches!(
                session.on_tool_call(wire(500), false).await,
                Err(EventError::UndeclaredTool { tool }) if tool == "wire"
            ),
            "the head declares no wire, so nothing asks the removed authority"
        );
    }

    // The binding comes back with the declaration that registers it: a binding no
    // policy declares is refused at open.
    let url = stub(serde_json::json!({"ruling": "approve"})).await;
    let runtime = Runtime::open(config_with(ATTENTION, Some(&url)), db, None)
        .expect("the deployment with the restored binding opens");
    let session = runtime.session(&root(), &root()).expect("the old root reopens");
    session
        .on_tool_call(wire(500), false)
        .await
        .expect("the block is delivered");
    let offer = latest_offer(&runtime);
    runtime.store().fail_commit_after(0);
    assert!(matches!(
        session
            .on_remedy(offer.clone(), RemedyArguments::default(), None, None)
            .await,
        Err(EventError::Storage(_)),
    ));
    assert!(matches!(
        session
            .on_remedy(offer, RemedyArguments::default(), None, None)
            .await
            .expect("the retry executes"),
        RemedyDecision::Authorized { .. },
    ));
}

#[tokio::test]
async fn a_reopened_store_continues_the_trajectory() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let db = dir.path().join("appa.db");
    {
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), db.clone(), None).expect("the deployment opens");
        let session = runtime.create_session(root(), None).expect("a fresh id opens");
        session
            .on_tool_call(fetch(serde_json::json!({"a": 1})), false)
            .await
            .expect("the call is decided");
        session
            .on_tool_result(
                fetch(serde_json::json!({"a": 1})),
                ToolOutcome::Success {
                    body: OutcomeBody::Available("data".to_string()),
                },
            )
            .await
            .expect("the result is admitted");
    }
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), db, None).expect("the deployment reopens");
    let session = runtime.session(&root(), &root()).expect("the trajectory reopens");
    let decision = session
        .on_tool_call(fetch(serde_json::json!({"a": 2})), false)
        .await
        .expect("the reopened trajectory decides");
    assert!(matches!(decision, ToolCallDecision::Allow { spawn: None, .. }));
}

#[tokio::test]
async fn an_undecodable_batch_row_is_refused() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    runtime.store().corrupt_batch(&root(), 0, b"not engine records");
    assert!(matches!(
        session.on_tool_call(fetch(serde_json::json!({"a": 1})), false).await,
        Err(EventError::UntrustedLog(_)),
    ));
}

#[tokio::test]
async fn a_corrupt_batch_row_is_refused_before_any_decision() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    session
        .on_tool_call(fetch(serde_json::json!({"a": 1})), false)
        .await
        .expect("the call is decided");
    let released: Vec<_> = runtime
        .log_facts(&root())
        .into_iter()
        .skip_while(|fact| matches!(fact, appa_engine::fact::Fact::TrajectoryOpened(_)))
        .collect();
    let tampered = serde_json::to_string(&released)
        .expect("the batch serializes")
        .replace("\"fetch\"", "\"wrench\"");
    runtime.store().corrupt_batch(&root(), 1, tampered.as_bytes());
    assert!(matches!(
        session
            .on_tool_result(
                fetch(serde_json::json!({"a": 1})),
                ToolOutcome::Success {
                    body: OutcomeBody::Available("data".to_string()),
                },
            )
            .await,
        Err(EventError::UntrustedLog(_)),
    ));
}

#[tokio::test]
async fn tampered_audience_evidence_refuses_the_log() {
    let source = {
        use axum::routing::post;
        let app = axum::Router::new().route(
            "/",
            post(|| async {
                axum::Json(serde_json::json!({
                    "version": 1,
                    "answer": {"members": ["alice@corp.example"]}
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback stub binds");
        let addr = listener.local_addr().expect("the stub has an address");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("the stub serves");
        });
        format!("http://{addr}/")
    };
    let policy = r#"
version = 2

[policy.audience.group.team]
from = ["slack:user-group/team"]

[[policy.tool]]
name = "send"
requires = { audience = { contains = ["@team"] } }
delta = {}

[policy.deployment]
starting_label = { audience = ["alice@corp.example"] }
"#;
    let text = format!(
        "[policy]\n{policy}\n[externals]\ntimeout_ms = 2000\nmax_body_bytes = 65536\n[externals.audience.slack]\nurl = \"{source}\"\nselectors = [{{ template = \"user-group/<handle>\" }}]\n"
    );
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, text).expect("the fixture writes");
    let config = Config::load(&path).expect("the fixture validates");
    let runtime = Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    let send = ProposedCall {
        tool: "send".to_string(),
        arguments: raw(serde_json::json!({})),
        cwd: None,
    };
    // The source reports Alice, whose principal the starting audience holds: released.
    assert!(matches!(
        session.on_tool_call(send.clone(), false).await,
        Ok(ToolCallDecision::Allow { .. })
    ));
    let released: Vec<_> = runtime
        .log_facts(&root())
        .into_iter()
        .skip_while(|fact| matches!(fact, appa_engine::fact::Fact::TrajectoryOpened(_)))
        .collect();
    let persisted = serde_json::to_string(&released).expect("the batch serializes");
    assert!(
        persisted.contains("alice@corp.example"),
        "the decision pins the claims it read: {persisted}"
    );
    // Rewrite the member only where the evidence pins it, so the labels the decision
    // recorded still name Alice and only the evidence check can refuse the log.
    let mut tampered: serde_json::Value = serde_json::from_str(&persisted).expect("the batch parses");
    let mut rewritten = 0;
    for fact in tampered.as_array_mut().expect("the batch is a list of facts") {
        for record in fact.as_object_mut().into_iter().flat_map(|fact| fact.values_mut()) {
            let sources = record
                .pointer_mut("/evidence/sources")
                .and_then(serde_json::Value::as_array_mut);
            for source in sources.into_iter().flatten() {
                for member in source["members"].as_array_mut().into_iter().flatten() {
                    if member == "alice@corp.example" {
                        *member = serde_json::json!("mallory@evil.example");
                        rewritten += 1;
                    }
                }
            }
        }
    }
    assert!(rewritten > 0, "the evidence pins Alice's membership: {persisted}");
    let tampered = serde_json::to_string(&tampered).expect("the tampered batch serializes");
    assert!(
        tampered.contains("alice@corp.example"),
        "the recorded labels still name Alice: {tampered}"
    );
    runtime.store().corrupt_batch(&root(), 1, tampered.as_bytes());
    assert!(matches!(
        session.on_tool_call(send, false).await,
        Err(EventError::UntrustedLog(_)),
    ));
}

#[tokio::test]
async fn a_suspicious_result_blocks_a_trusted_floor_sink() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    admit_success(&runtime, &mut session, taint(serde_json::json!({"a": 1}))).await;

    let decision = session
        .on_tool_call(
            ProposedCall {
                tool: "send".to_string(),
                arguments: raw(serde_json::json!({})),
                cwd: None,
            },
            false,
        )
        .await
        .expect("the block is delivered");
    assert!(
        matches!(decision, ToolCallDecision::Deny { .. }),
        "a narrowed trajectory must block the trusted-floor sink, got {decision:?}"
    );
    assert!(runtime.open_dispatches(&root(), &root()).pop().is_none());
}

#[tokio::test]
async fn a_subject_a_concurrent_event_moved_reads_as_lifecycle_not_a_fault() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    let refuse =
        |trajectory: &TrajectoryId, event: crate::engine::EngineEvent| runtime.refuse(&root(), trajectory, event);

    let child = TrajectoryId("cc:root:child".to_string());
    let child_session = open_child(&mut session, fetch(serde_json::json!({"a": 1})), child.clone()).await;
    child_session
        .on_child_end(None)
        .await
        .expect("the child ends with no return");

    let error = refuse(
        &child,
        crate::engine::EngineEvent::ModelResponse {
            call: fetch(serde_json::json!({"a": 2})),
            evidence: Vec::new(),
            entropy: fresh_entropy(),
            spawn: None,
            replay: None,
        },
    );
    assert!(
        matches!(error, EventError::TrajectoryEnded),
        "a call on an ended branch is a lifecycle condition; got {error:?}",
    );
    assert!(!error.is_operational());

    for value in [Some("again".to_string()), None] {
        let error = refuse(
            &root(),
            crate::engine::EngineEvent::ChildReturn {
                child: child.clone(),
                value: value.clone(),
                evidence: Vec::new(),
            },
        );
        assert!(
            matches!(error, EventError::TrajectoryEnded),
            "a duplicate return answers as a later one would; got {error:?} for {value:?}",
        );
        assert!(!error.is_operational());
    }
}

#[tokio::test]
async fn a_marked_spawn_without_context_control_releases_unmarked() {
    const UNCONTROLLED: &str = r#"
version = 2

[[policy.tool]]
name = "spawn"
delta = {}

[policy.deployment]
context_control = false
"#;
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime =
        Runtime::open(config_with(UNCONTROLLED, None), dir.path().join("appa.db"), None).expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    let decision = session
        .on_tool_call(
            ProposedCall {
                tool: "spawn".to_string(),
                arguments: raw(serde_json::json!({})),
                cwd: None,
            },
            true,
        )
        .await
        .expect("the marked call is decided");
    assert!(
        matches!(decision, ToolCallDecision::Allow { spawn: None, .. }),
        "the mark is refused and the call releases as an ordinary flow, not a fork",
    );
}

#[tokio::test]
async fn a_child_is_forked_and_a_clean_return_crosses() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    let child = open_child(
        &mut session,
        fetch(serde_json::json!({"a": 1})),
        TrajectoryId("cc:child".to_string()),
    )
    .await;
    let log = runtime.log_facts(&root());
    let fork_records: Vec<_> = log
        .iter()
        .filter(|fact| {
            matches!(
                fact,
                appa_engine::fact::Fact::ForkPrepared { .. } | appa_engine::fact::Fact::ForkOpened { .. }
            )
        })
        .collect();
    assert!(matches!(
        fork_records.as_slice(),
        [
            appa_engine::fact::Fact::ForkPrepared { .. },
            appa_engine::fact::Fact::ForkOpened { .. },
        ],
    ));

    let returned = child
        .on_child_end(Some("all done".to_string()))
        .await
        .expect("the clean return crosses");
    assert_eq!(
        returned,
        crate::api::ChildReturnDecision::Returned {
            value: "all done".to_string()
        },
    );
    assert!(
        runtime.live(&root(), &TrajectoryId("cc:child".to_string())).is_ok(),
        "a return leaves the child live"
    );
}

/// The parent declared it takes no return: the child narrows freely, its text stop is
/// blocked, and its empty stop leaves the parent's label and sinks as they were.
#[tokio::test]
async fn a_void_child_narrows_freely_and_leaves_the_parent_unchanged() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    let spawn = fetch(serde_json::json!({"a": 9}));
    let call = authorize_spawn_as(
        &session,
        spawn.clone(),
        SpawnKind::Single,
        appa_runtime_api::OfferedReturn::Void,
        None,
    )
    .await;
    let ToolCallDecision::Allow {
        spawn: Some(binding), ..
    } = session
        .on_tool_call(call.proposed(), true)
        .await
        .expect("the approved spawn releases")
    else {
        panic!("the approved spawn releases with its fork");
    };
    let mut child = session
        .on_child_start(TrajectoryId("cc:child".to_string()), SpawnRef::Binding(binding))
        .expect("the fork binds and the child opens");
    // Under the bare floor at the parent's label no plan would accept this drop.
    admit_success(&runtime, &mut child, taint(serde_json::json!({"a": 1}))).await;

    assert!(matches!(
        child
            .on_child_end(Some("summary of untrusted data".to_string()))
            .await
            .expect("the stop is judged"),
        crate::api::ChildReturnDecision::Blocked { .. }
    ));
    assert_eq!(
        child
            .on_child_turn_end()
            .await
            .expect("the empty stop ends the void child"),
        crate::api::ChildReturnDecision::NoValue
    );
    assert_eq!(child.on_child_turn_end().await.unwrap(), ChildReturnDecision::NoValue);
    assert!(matches!(
        child.on_tool_call(fetch(serde_json::json!({})), false).await,
        Err(EventError::TrajectoryEnded)
    ));
    session
        .on_tool_result(
            spawn,
            ToolOutcome::Success {
                body: OutcomeBody::Unavailable,
            },
        )
        .await
        .expect("the spawn dispatch closes");
    assert_eq!(runtime.status(&root()).expect("the root answers").trust, "trusted");
}

/// The parent declared it takes a suspicious return; the child's tainted return
/// crosses as spoken, narrows the parent, and the parent's trusted-floor sink blocks.
#[tokio::test]
async fn a_child_returns_crossing_narrows_the_parent_and_charges_its_sink() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    let mut child = open_child_floored(
        &mut session,
        fetch(serde_json::json!({"a": 9})),
        TrajectoryId("cc:child".to_string()),
        floor_trust("suspicious"),
    )
    .await;
    admit_success(&runtime, &mut child, taint(serde_json::json!({"a": 1}))).await;

    let returned = child
        .on_child_end(Some("summary of untrusted data".to_string()))
        .await
        .expect("the return crosses");
    assert_eq!(
        returned,
        crate::api::ChildReturnDecision::Returned {
            value: "summary of untrusted data".to_string()
        },
        "a return within the declared floor crosses as spoken"
    );
    assert!(
        runtime.live(&root(), &TrajectoryId("cc:child".to_string())).is_ok(),
        "a return leaves the child live"
    );

    session
        .on_tool_result(
            fetch(serde_json::json!({"a": 9})),
            ToolOutcome::Success {
                body: OutcomeBody::Unavailable,
            },
        )
        .await
        .expect("the spawn dispatch closes");

    assert_eq!(
        runtime.status(&root()).expect("the root answers").trust,
        "suspicious",
        "the crossing narrowed the parent"
    );
    let decision = session
        .on_tool_call(
            ProposedCall {
                tool: "send".to_string(),
                arguments: raw(serde_json::json!({})),
                cwd: None,
            },
            false,
        )
        .await
        .expect("the block is delivered");
    assert!(
        matches!(decision, ToolCallDecision::Deny { .. }),
        "the crossed narrowing must charge the parent's send, got {decision:?}"
    );
}
#[tokio::test]
async fn symbolic_audience_acceptance_needs_no_membership_sources() {
    let policy = r#"
version = 2
[[policy.tool]]
name = "read_internal"
delta = { audience = ["internal"] }
requires = { audience = { contains = ["internal"] } }
[[policy.tool]]
name = "send_reader"
delta = {}
requires = { audience = { contains = ["reader@example.com"] } }
"#;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("appa.db");
    let runtime = Runtime::open(config_with(policy, None), db.clone(), None).unwrap();
    let session = runtime.create_session(root(), None).unwrap();
    let read = ProposedCall {
        tool: "read_internal".into(),
        arguments: raw(serde_json::json!({})),
        cwd: None,
    };
    assert!(matches!(
        session.on_tool_call(read.clone(), false).await.unwrap(),
        ToolCallDecision::Deny { .. }
    ));
    let decision = session
        .on_remedy(surfaced_offer(&runtime), RemedyArguments::default(), None, None)
        .await
        .unwrap();
    assert!(matches!(decision, RemedyDecision::Authorized { .. }), "{decision:?}");
    assert!(matches!(
        session.on_tool_call(read.clone(), false).await.unwrap(),
        ToolCallDecision::Allow { .. }
    ));
    session
        .on_tool_result(
            read,
            ToolOutcome::Success {
                body: OutcomeBody::Available("internal result".into()),
            },
        )
        .await
        .unwrap();
    drop(session);
    drop(runtime);

    // Replay preserves the symbolic label; a real membership question still refuses.
    let runtime = Runtime::open(config_with(policy, None), db, None).unwrap();
    assert_eq!(runtime.status(&root()).unwrap().audience, "internal");
    let session = runtime.session(&root(), &root()).unwrap();
    assert!(matches!(
        session
            .on_tool_call(
                ProposedCall {
                    tool: "send_reader".into(),
                    arguments: raw(serde_json::json!({})),
                    cwd: None,
                },
                false
            )
            .await
            .unwrap(),
        ToolCallDecision::Deny { .. }
    ));
}

const ATTENTION: &str = r#"
version = 2

[[policy.tool]]
name = "wire"
parameters = { type = "object", properties = { amount = { type = "integer" } } }
requires = { attention = ["irreversible"] }
delta = {}

[[policy.authority]]
name = "approver"
[policy.authority.permits]
attention = ["irreversible"]
"#;

fn wire(amount: u64) -> ProposedCall {
    ProposedCall {
        tool: "wire".to_string(),
        arguments: raw(serde_json::json!({"amount": amount})),
        cwd: None,
    }
}

fn surfaced_offer(runtime: &Runtime) -> OfferId {
    surfaced_offer_for(runtime, &root(), &root())
}

fn surfaced_offer_for(runtime: &Runtime, root: &TrajectoryId, trajectory: &TrajectoryId) -> OfferId {
    let quoted = runtime
        .minted_offers(root, trajectory)
        .into_iter()
        .next()
        .expect("the deny surfaced an offer");
    runtime.resolve_in(root, &quoted).expect("the quoted id resolves").0
}

#[tokio::test]
async fn an_authority_approval_authorizes_the_exact_call() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let url = stub(serde_json::json!({"ruling": "approve"})).await;
    let runtime = Runtime::open(config_with(ATTENTION, Some(&url)), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");

    let denied = session
        .on_tool_call(wire(500), false)
        .await
        .expect("the block is delivered");
    assert!(matches!(denied, ToolCallDecision::Deny { .. }));
    let offer = surfaced_offer(&runtime);

    let authorized = session
        .on_remedy(offer.clone(), RemedyArguments::default(), None, None)
        .await
        .expect("the remedy executes");
    let RemedyDecision::Authorized { call } = authorized else {
        panic!("an approval must authorize the call");
    };
    assert_eq!(call.tool, "wire");
    assert_eq!(call.bytes, br#"{"amount":500}"#.to_vec());

    let resumed = session
        .on_tool_call(wire(500), false)
        .await
        .expect("the re-proposal resumes");
    assert!(matches!(resumed, ToolCallDecision::Allow { spawn: None, .. }));
    let kept = session
        .on_tool_result(
            wire(500),
            ToolOutcome::Success {
                body: OutcomeBody::Available("sent".to_string()),
            },
        )
        .await
        .expect("the result is admitted");
    assert_eq!(kept, ToolResultDecision::Keep);

    assert!(matches!(
        session.on_remedy(offer, RemedyArguments::default(), None, None).await,
        Ok(RemedyDecision::Authorized { .. }),
    ));

    let consumed = runtime
        .log_facts(&root())
        .iter()
        .filter(|fact| matches!(fact, appa_engine::fact::Fact::CallApprovalConsumed { .. }))
        .count();
    assert_eq!(consumed, 1, "the approval is consumed exactly once");
}

#[tokio::test]
async fn a_denial_retires_plans_naming_the_denier_and_sticks() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let url = stub(serde_json::json!({"ruling": "deny", "reason": "no"})).await;
    let runtime = Runtime::open(config_with(ATTENTION, Some(&url)), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");

    session
        .on_tool_call(wire(500), false)
        .await
        .expect("the block is delivered");
    let offer = surfaced_offer(&runtime);
    assert!(matches!(
        session
            .on_remedy(offer, RemedyArguments::default(), None, None)
            .await
            .expect("the denial is delivered"),
        RemedyDecision::Declined { .. },
    ));
    let before = runtime.minted_offers(&root(), &root()).len();
    assert!(matches!(
        session
            .on_tool_call(wire(500), false)
            .await
            .expect("the re-block is delivered"),
        ToolCallDecision::Deny { .. },
    ));
    let after = runtime.minted_offers(&root(), &root()).len();
    assert_eq!(after, before, "no new offer names the denying authority");
}

/// The denial leads the call's block with the authority that denied the plan.
#[tokio::test]
async fn a_denial_names_the_authority_that_denied_the_plan() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let url = stub(serde_json::json!({"ruling": "deny", "reason": "no"})).await;
    let runtime = Runtime::open(config_with(ATTENTION, Some(&url)), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    session
        .on_tool_call(wire(500), false)
        .await
        .expect("the block is delivered");
    let offer = surfaced_offer(&runtime);
    let RemedyDecision::Declined { presentation } = session
        .on_remedy(offer, RemedyArguments::default(), None, None)
        .await
        .expect("the denial is delivered")
    else {
        panic!("a denial declines the plan");
    };
    let (lead, block) = presentation
        .feedback
        .split_once('\n')
        .expect("the call's block follows the denial");
    assert_eq!(lead, "[appa] authority approver denied this plan.");
    assert!(
        block.starts_with("[appa] Blocked: wire cannot run."),
        "the call's block follows: {block}"
    );
}

#[tokio::test]
async fn a_denial_retires_only_the_owning_trajectorys_offers() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let url = stub(serde_json::json!({"ruling": "deny", "reason": "no"})).await;
    let runtime = Runtime::open(config_with(ATTENTION, Some(&url)), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let first_id = root();
    let second_id = TrajectoryId("cc:second-root".to_string());
    let first = runtime
        .create_session(first_id.clone(), None)
        .expect("the first root opens");
    let second = runtime
        .create_session(second_id.clone(), None)
        .expect("the second root opens");

    first
        .on_tool_call(wire(500), false)
        .await
        .expect("the first block is delivered");
    second
        .on_tool_call(wire(500), false)
        .await
        .expect("the second block is delivered");
    let first_offer = surfaced_offer_for(&runtime, &first_id, &first_id);
    let second_offer = surfaced_offer_for(&runtime, &second_id, &second_id);

    assert!(matches!(
        first
            .on_remedy(first_offer, RemedyArguments::default(), None, None)
            .await
            .expect("the first denial is delivered"),
        RemedyDecision::Declined { .. },
    ));
    let still_quoted = runtime.minted_offers(&second_id, &second_id);
    assert_eq!(
        still_quoted.len(),
        1,
        "the second trajectory keeps exactly its own offer"
    );
    assert_eq!(
        runtime
            .resolve_in(&second_id, &still_quoted[0])
            .expect("the quoted id resolves")
            .0,
        second_offer,
        "one trajectory's denial must not retire another trajectory's same-call offer"
    );
}

#[tokio::test]
async fn an_abstain_keeps_the_offer() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let url = stub(serde_json::json!({"note": "still thinking"})).await;
    let runtime = Runtime::open(config_with(ATTENTION, Some(&url)), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");

    session
        .on_tool_call(wire(500), false)
        .await
        .expect("the block is delivered");
    let offer = surfaced_offer(&runtime);
    assert!(matches!(
        session
            .on_remedy(offer.clone(), RemedyArguments::default(), None, None)
            .await
            .expect("the no-answer is delivered"),
        RemedyDecision::NoAnswer { .. },
    ));
    assert!(matches!(
        session
            .on_remedy(offer, RemedyArguments::default(), None, None)
            .await
            .expect("the offer is still live"),
        RemedyDecision::NoAnswer { .. },
    ));
}

#[tokio::test]
async fn a_failed_commit_leaves_the_offer_standing() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let url = stub(serde_json::json!({"ruling": "approve"})).await;
    let runtime = Runtime::open(config_with(ATTENTION, Some(&url)), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");

    session
        .on_tool_call(wire(500), false)
        .await
        .expect("the block is delivered");
    let offer = surfaced_offer(&runtime);
    runtime.store().fail_commit_after(0);
    assert!(matches!(
        session
            .on_remedy(offer.clone(), RemedyArguments::default(), None, None)
            .await,
        Err(EventError::Storage(_)),
    ));
    assert!(matches!(
        session
            .on_remedy(offer, RemedyArguments::default(), None, None)
            .await
            .expect("the retry executes"),
        RemedyDecision::Authorized { .. },
    ));
}

#[tokio::test]
async fn an_offer_survives_a_restart_and_executes() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let url = stub(serde_json::json!({"ruling": "approve"})).await;
    let db = dir.path().join("appa.db");
    let offer = {
        let runtime =
            Runtime::open(config_with(ATTENTION, Some(&url)), db.clone(), None).expect("the deployment opens");
        let session = runtime.create_session(root(), None).expect("a fresh id opens");
        session
            .on_tool_call(wire(500), false)
            .await
            .expect("the block is delivered");
        surfaced_offer(&runtime)
    };
    let runtime = Runtime::open(config_with(ATTENTION, Some(&url)), db, None).expect("the deployment reopens");
    let session = runtime.session(&root(), &root()).expect("the trajectory reopens");
    assert!(matches!(
        session
            .on_remedy(offer, RemedyArguments::default(), None, None)
            .await
            .expect("the reopened offer executes"),
        RemedyDecision::Authorized { .. },
    ));
}

const SUBSTITUTED_SEND: &str = r#"
version = 2

[[policy.tool]]
name = "read_hr"
delta = { audience = ["hr"] }

[[policy.tool]]
name = "read_legal"
delta = { audience = ["legal"] }

[[policy.tool]]
name = "send"
parameters = { type = "object", properties = { body = { type = "string" } }, required = ["body"] }
requires = { audience = { contains = ["public"] } }
delta = {}

[[policy.sanitizer]]
name = "redactor"
on = ["tool_input"]
[policy.sanitizer.permits]
audience = { from = ["hr"], to = ["public"] }
"#;

/// `send` wants a trust floor as well as the audience the redaction widens, and `read_web`
/// drops the trajectory below that floor. A redaction cannot lift trust, so this policy tells
/// a derivation that still stands apart from a call that may still run.
const SUBSTITUTED_SEND_FLOORED: &str = r#"
version = 2

[[policy.tool]]
name = "read_hr"
delta = { audience = ["hr"] }

[[policy.tool]]
name = "read_web"
delta = { trust = "suspicious" }

[[policy.tool]]
name = "send"
parameters = { type = "object", properties = { body = { type = "string" } }, required = ["body"] }
requires = { audience = { contains = ["public"] }, trust = "trusted" }
delta = {}

[[policy.sanitizer]]
name = "redactor"
on = ["tool_input"]
[policy.sanitizer.permits]
audience = { from = ["hr"], to = ["public"] }
"#;

const SUBSTITUTED_ATTENDED_SEND: &str = r#"
version = 2

[[policy.tool]]
name = "read_hr"
delta = { audience = ["hr"] }

[[policy.tool]]
name = "send"
parameters = { type = "object", properties = { body = { type = "string" } }, required = ["body"] }
requires = { audience = { contains = ["public"] }, attention = ["irreversible"] }
delta = {}

[[policy.sanitizer]]
name = "redactor"
on = ["tool_input"]
[policy.sanitizer.permits]
audience = { from = ["hr"], to = ["public"] }

[[policy.authority]]
name = "approver"
[policy.authority.permits]
attention = ["irreversible"]
"#;

const SUBSTITUTED_SEND_FORKING: &str = r#"
version = 2

[[policy.tool]]
name = "read_hr"
delta = { audience = ["hr"] }

[[policy.tool]]
name = "send"
parameters = { type = "object", properties = { body = { type = "string" } }, required = ["body"] }
requires = { audience = { contains = ["public"] } }
delta = {}

[[policy.tool]]
name = "fetch"
parameters = { type = "object", properties = { a = { type = "integer" } } }

[[policy.sanitizer]]
name = "redactor"
on = ["tool_input"]
[policy.sanitizer.permits]
audience = { from = ["hr"], to = ["public"] }

[policy.deployment]
context_control = true
"#;

fn substituting_config(policy: &str, authority_url: Option<&str>) -> Config {
    let binding = match authority_url {
        Some(url) => format!("[externals.authorities.approver]\nurl = \"{url}\"\n"),
        None => String::new(),
    };
    let text = format!(
        "[policy]\n{policy}\n[externals]\ntimeout_ms = 2000\nmax_body_bytes = 65536\n\
             [externals.sanitizers.redactor]\nbuiltin = \"redact-email\"\n{binding}"
    );
    config_from(&text)
}

fn send(body: &str) -> ProposedCall {
    ProposedCall {
        tool: "send".to_string(),
        arguments: raw(serde_json::json!({"body": body})),
        cwd: None,
    }
}

const RAW_BODY: &str = "mail alice@corp.example today";

/// The `send` call a remedy authorized, read from its answer rather than copied from the
/// stock sanitizer's placeholder text. The replaced bytes no longer carry the address.
fn substituted_send(decision: &RemedyDecision) -> ProposedCall {
    let RemedyDecision::Authorized {
        call: ExactCall { tool, bytes },
    } = decision
    else {
        panic!("the remedy authorizes the replaced call: {decision:?}");
    };
    assert_eq!(tool, "send");
    let arguments: serde_json::Value = serde_json::from_slice(bytes).expect("the replaced call is JSON");
    let body = arguments["body"].as_str().expect("the replaced call carries a body");
    assert!(
        !body.contains("@corp.example"),
        "the replaced call no longer carries the address: {body}"
    );
    ProposedCall {
        tool: tool.clone(),
        arguments: raw(arguments),
        cwd: None,
    }
}

async fn narrowed_and_blocked(runtime: &Runtime, session: &mut Session) -> OfferId {
    narrowed_and_blocked_on(runtime, session, &root()).await
}

async fn narrowed_and_blocked_on(runtime: &Runtime, session: &mut Session, trajectory: &TrajectoryId) -> OfferId {
    let read = ProposedCall {
        tool: "read_hr".to_string(),
        arguments: raw(serde_json::json!({})),
        cwd: None,
    };
    assert!(matches!(
        session.on_tool_call(read.clone(), false).await,
        Ok(ToolCallDecision::Deny { .. }),
    ));
    let accept = surfaced_offer_for(runtime, &root(), trajectory);
    assert!(matches!(
        session.on_remedy(accept, RemedyArguments::default(), None, None).await,
        Ok(RemedyDecision::Authorized { .. }),
    ));
    assert!(matches!(
        session
            .on_tool_call(read.clone(), false)
            .await
            .expect("the read releases"),
        ToolCallDecision::Allow { spawn: None, .. }
    ));
    session
        .on_tool_result(
            read,
            ToolOutcome::Success {
                body: OutcomeBody::Available("Alice Chen".to_string()),
            },
        )
        .await
        .expect("the read closes");
    assert!(matches!(
        session.on_tool_call(send(RAW_BODY), false).await,
        Ok(ToolCallDecision::Deny { .. }),
    ));
    let quoted = runtime
        .minted_offers(&root(), trajectory)
        .pop()
        .expect("the block surfaced an offer");
    runtime.resolve_in(&root(), &quoted).expect("the quoted id resolves").0
}

#[tokio::test]
async fn an_input_substitution_approves_the_replaced_call_and_the_proposal_of_it_runs() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(
        substituting_config(SUBSTITUTED_SEND, None),
        dir.path().join("appa.db"),
        None,
    )
    .expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    let hop = narrowed_and_blocked(&runtime, &mut session).await;

    let substituted = session
        .on_remedy(hop.clone(), RemedyArguments::default(), None, None)
        .await
        .expect("the hop executes");
    let replaced = substituted_send(&substituted);
    assert!(
        runtime.open_dispatches(&root(), &root()).is_empty(),
        "the hop decides nothing in advance: it stages a derivation and opens no dispatch"
    );

    assert_eq!(
        session
            .on_remedy(hop.clone(), RemedyArguments::default(), None, None)
            .await
            .expect("the replay answers"),
        substituted,
    );
    assert!(runtime.open_dispatches(&root(), &root()).is_empty());

    assert!(matches!(
        session
            .on_tool_call(replaced.clone(), false)
            .await
            .expect("the proposal takes the derivation and releases"),
        ToolCallDecision::Allow { spawn: None, .. }
    ));
    assert_eq!(runtime.open_dispatches(&root(), &root()).len(), 1);

    assert_eq!(
        session
            .on_tool_result(
                replaced.clone(),
                ToolOutcome::Success {
                    body: OutcomeBody::Available("sent".to_string()),
                },
            )
            .await
            .expect("the outcome is reported"),
        ToolResultDecision::Keep,
    );
    assert!(runtime.open_dispatches(&root(), &root()).is_empty());

    // The derivation is spent. Proposing the same bytes again is an ordinary proposal
    // judged against current state, which is what blocked the call in the first place.
    assert!(matches!(
        session.on_tool_call(replaced.clone(), false).await,
        Ok(ToolCallDecision::Deny { .. }),
    ));
    assert!(matches!(
        session.on_remedy(hop, RemedyArguments::default(), None, None).await,
        Ok(RemedyDecision::Declined { .. }),
    ));

    let entries = runtime.audit(&root()).expect("the audit reads");
    let sends: Vec<_> = entries
        .iter()
        .filter(|entry| matches!(&entry.event, crate::engine::AuditEvent::Released { tool, .. } if tool == "send"))
        .collect();
    assert_eq!(sends.len(), 1, "the replaced call is released once: {entries:?}");
    assert!(
        entries.iter().any(|entry| matches!(
            &entry.event,
            crate::engine::AuditEvent::Closed {
                outcome: crate::engine::DispatchOutcome::Ran { .. }
            }
        )),
        "the replaced call's dispatch closed as run: {entries:?}"
    );
}

#[tokio::test]
async fn an_unusable_derivation_leaves_the_offer_standing() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(
        substituting_config(SUBSTITUTED_SEND, None),
        dir.path().join("appa.db"),
        None,
    )
    .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    let read = ProposedCall {
        tool: "read_hr".to_string(),
        arguments: raw(serde_json::json!({})),
        cwd: None,
    };
    session
        .on_tool_call(read.clone(), false)
        .await
        .expect("the read is decided");
    session
        .on_remedy(surfaced_offer(&runtime), RemedyArguments::default(), None, None)
        .await
        .expect("the narrowing is accepted");
    session
        .on_tool_call(read.clone(), false)
        .await
        .expect("the read releases");
    session
        .on_tool_result(
            read,
            ToolOutcome::Success {
                body: OutcomeBody::Available("Alice Chen".to_string()),
            },
        )
        .await
        .expect("the read closes");
    session
        .on_tool_call(send("mail alice@corp.example"), false)
        .await
        .expect("the send is decided");
    let hop = latest_offer(&runtime);

    for _ in 0..2 {
        assert!(matches!(
            session
                .on_remedy(hop.clone(), RemedyArguments::default(), None, None)
                .await,
            Ok(RemedyDecision::NoAnswer { .. }),
        ));
        assert!(runtime.open_dispatches(&root(), &root()).is_empty());
    }
}

/// Criterion 1: a fan-out whose blocked calls each need a substituting remedy makes
/// progress. Both remedies execute while the other stands, and each substituted call is
/// released when the model proposes it.
#[tokio::test]
async fn two_substituting_remedies_in_one_fan_out_both_run() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(
        substituting_config(SUBSTITUTED_SEND, None),
        dir.path().join("appa.db"),
        None,
    )
    .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    let read = ProposedCall {
        tool: "read_hr".to_string(),
        arguments: raw(serde_json::json!({})),
        cwd: None,
    };

    assert!(matches!(
        session.on_tool_call(read.clone(), false).await,
        Ok(ToolCallDecision::Deny { .. }),
    ));
    let accept = surfaced_offer_for(&runtime, &root(), &root());
    assert!(matches!(
        session.on_remedy(accept, RemedyArguments::default(), None, None).await,
        Ok(RemedyDecision::Authorized { .. }),
    ));
    assert!(matches!(
        session
            .on_tool_call(read.clone(), false)
            .await
            .expect("the read releases"),
        ToolCallDecision::Allow { spawn: None, .. }
    ));
    session
        .on_tool_result(
            read,
            ToolOutcome::Success {
                body: OutcomeBody::Available("Alice Chen".to_string()),
            },
        )
        .await
        .expect("the read closes");

    const OTHER_RAW: &str = "ping bob@corp.example now";
    let mut hops = Vec::new();
    for body in [RAW_BODY, OTHER_RAW] {
        assert!(
            matches!(
                session.on_tool_call(send(body), false).await,
                Ok(ToolCallDecision::Deny { .. })
            ),
            "the send carrying {body} blocks"
        );
        let quoted = runtime
            .minted_offers(&root(), &root())
            .pop()
            .expect("the block surfaced an offer");
        hops.push(runtime.resolve_in(&root(), &quoted).expect("the quoted id resolves").0);
    }

    let mut replaced = Vec::new();
    for hop in hops {
        let executed = session.on_remedy(hop, RemedyArguments::default(), None, None).await;
        assert!(
            matches!(executed, Ok(RemedyDecision::Authorized { .. })),
            "both remedies execute, the second while the first still stands: {executed:?}"
        );
        replaced.push(substituted_send(&executed.expect("the remedy executes")));
    }
    assert_ne!(
        replaced[0].arguments.get(),
        replaced[1].arguments.get(),
        "each remedy replaced its own call"
    );

    for (call_id, call) in [("toolu-1", &replaced[0]), ("toolu-2", &replaced[1])] {
        let released = session
            .on_tool_call_identified(call.clone(), Some(call_id.to_string()), None, None)
            .await;
        assert!(
            matches!(released, Ok(ToolCallDecision::Allow { spawn: None, .. })),
            "the substituted call {call:?} releases when proposed: {released:?}"
        );
    }
    assert_eq!(runtime.open_dispatches(&root(), &root()).len(), 2);

    // Each release took its own derivation and spent it. Were one derivation covering both,
    // the second proposal above would have found nothing left to take and blocked; were a
    // derivation to survive its take, this third proposal would release a call nobody paid for.
    let again = session
        .on_tool_call_identified(replaced[0].clone(), Some("toolu-3".to_string()), None, None)
        .await;
    assert!(
        matches!(again, Ok(ToolCallDecision::Deny { .. })),
        "the spent derivation does not release the same bytes again: {again:?}"
    );
}

/// Criterion 2: a derivation is not a call in flight, so a turn that ends before the model
/// proposes it owes no outcome and discards nothing. The next turn still takes it.
#[tokio::test]
async fn a_turn_end_leaves_a_staged_derivation_alone() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(
        substituting_config(SUBSTITUTED_SEND, None),
        dir.path().join("appa.db"),
        None,
    )
    .expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    let hop = narrowed_and_blocked(&runtime, &mut session).await;
    let replaced = substituted_send(
        &session
            .on_remedy(hop, RemedyArguments::default(), None, None)
            .await
            .expect("the hop executes"),
    );

    session.on_turn_end().await.expect("the turn end acks");

    assert!(
        runtime.open_dispatches(&root(), &root()).is_empty(),
        "a staged derivation is nothing for a turn end to close"
    );
    assert!(matches!(
        session
            .on_tool_call(replaced, false)
            .await
            .expect("the next turn still takes the derivation"),
        ToolCallDecision::Allow { spawn: None, .. }
    ));
}

/// The defect this design closes: an unrelated call used to abandon the standing
/// substitution, costing the model the remedy it had already paid for. A derivation is not
/// in flight, so nothing about another call touches it.
#[tokio::test]
async fn another_call_while_a_derivation_stands_leaves_it_alone() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(
        substituting_config(SUBSTITUTED_SEND, None),
        dir.path().join("appa.db"),
        None,
    )
    .expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    let hop = narrowed_and_blocked(&runtime, &mut session).await;
    let replaced = substituted_send(
        &session
            .on_remedy(hop.clone(), RemedyArguments::default(), None, None)
            .await
            .expect("the hop executes"),
    );

    let other = ProposedCall {
        tool: "read_hr".to_string(),
        arguments: raw(serde_json::json!({})),
        cwd: None,
    };
    let identified = session
        .on_tool_call_identified(other.clone(), Some("toolu-1".to_string()), None, None)
        .await
        .expect("the unrelated call is decided on its own terms");
    assert!(matches!(identified, ToolCallDecision::Allow { spawn: None, .. }));
    session
        .on_tool_result(
            other,
            ToolOutcome::Success {
                body: OutcomeBody::Available("Alice Chen".to_string()),
            },
        )
        .await
        .expect("the unrelated call closes");

    assert!(matches!(
        session
            .on_tool_call(replaced, false)
            .await
            .expect("the derivation survived the unrelated call"),
        ToolCallDecision::Allow { spawn: None, .. }
    ));
}

#[tokio::test]
async fn a_substituted_call_survives_a_restart_and_runs() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let db = dir.path().join("appa.db");
    let replaced = {
        let runtime =
            Runtime::open(substituting_config(SUBSTITUTED_SEND, None), db.clone(), None).expect("the deployment opens");
        let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
        let hop = narrowed_and_blocked(&runtime, &mut session).await;
        substituted_send(
            &session
                .on_remedy(hop, RemedyArguments::default(), None, None)
                .await
                .expect("the hop executes"),
        )
    };
    let runtime = Runtime::open(substituting_config(SUBSTITUTED_SEND, None), db, None).expect("the deployment reopens");
    let session = runtime.session(&root(), &root()).expect("the trajectory reopens");
    assert!(matches!(
        session
            .on_tool_call(replaced, false)
            .await
            .expect("the derivation replays from the log and the proposal takes it"),
        ToolCallDecision::Allow { spawn: None, .. }
    ));
}

/// Criterion 4: a derivation carries the sanitizer's stage, never a permission. A
/// trajectory that narrows after the hop re-gates the substituted call against the label it
/// narrowed to. This is what closes the stale-authorization channel a pre-decided release
/// left open: park the emission, read a secret, then choose whether to fire it.
#[tokio::test]
async fn a_narrowing_after_the_hop_re_gates_the_substituted_call() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(
        substituting_config(SUBSTITUTED_SEND_FLOORED, None),
        dir.path().join("appa.db"),
        None,
    )
    .expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    let hop = narrowed_and_blocked(&runtime, &mut session).await;
    let substituted = session
        .on_remedy(hop.clone(), RemedyArguments::default(), None, None)
        .await
        .expect("the hop executes");
    let replaced = substituted_send(&substituted);

    // The session reads an untrusted page, which drops it below the floor `send` wants.
    let web = ProposedCall {
        tool: "read_web".to_string(),
        arguments: raw(serde_json::json!({})),
        cwd: None,
    };
    assert!(matches!(
        session.on_tool_call(web.clone(), false).await,
        Ok(ToolCallDecision::Deny { .. }),
    ));
    let accept = latest_offer(&runtime);
    assert!(matches!(
        session.on_remedy(accept, RemedyArguments::default(), None, None).await,
        Ok(RemedyDecision::Authorized { .. }),
    ));
    assert!(matches!(
        session
            .on_tool_call(web.clone(), false)
            .await
            .expect("the read releases"),
        ToolCallDecision::Allow { spawn: None, .. }
    ));
    session
        .on_tool_result(
            web,
            ToolOutcome::Success {
                body: OutcomeBody::Available("anything at all".to_string()),
            },
        )
        .await
        .expect("the read closes");

    assert!(
        matches!(
            session.on_tool_call(replaced, false).await,
            Ok(ToolCallDecision::Deny { .. })
        ),
        "the derivation still stands, but the call it stages is judged against the label now"
    );
    assert!(runtime.open_dispatches(&root(), &root()).is_empty());
    // A repeat of the hop answers with the same call only while its derivation is current,
    // so neither the narrowing nor the blocked proposal took or discarded it.
    assert_eq!(
        session
            .on_remedy(hop, RemedyArguments::default(), None, None)
            .await
            .expect("the repeat answers"),
        substituted,
        "the derivation still stands after the narrowing"
    );
}

/// The one term a derivation does freeze: what the released bytes carry. A read the session
/// makes later cannot turn redacted bytes back into a secret, so `contains` is answered by the
/// sanitizer's derived label rather than by the label the session has narrowed to since.
/// Deliberate: intersecting the two would re-taint every sanitized value and leave
/// substitution helping nothing.
#[tokio::test]
async fn a_narrowing_after_the_hop_does_not_re_taint_the_substituted_bytes() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(
        substituting_config(SUBSTITUTED_SEND, None),
        dir.path().join("appa.db"),
        None,
    )
    .expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    let hop = narrowed_and_blocked(&runtime, &mut session).await;
    let replaced = substituted_send(
        &session
            .on_remedy(hop, RemedyArguments::default(), None, None)
            .await
            .expect("the hop executes"),
    );

    // A second read narrows the session's audience further, well away from public.
    let legal = ProposedCall {
        tool: "read_legal".to_string(),
        arguments: raw(serde_json::json!({})),
        cwd: None,
    };
    assert!(matches!(
        session.on_tool_call(legal.clone(), false).await,
        Ok(ToolCallDecision::Deny { .. }),
    ));
    let accept = latest_offer(&runtime);
    assert!(matches!(
        session.on_remedy(accept, RemedyArguments::default(), None, None).await,
        Ok(RemedyDecision::Authorized { .. }),
    ));
    assert!(matches!(
        session
            .on_tool_call(legal.clone(), false)
            .await
            .expect("the read releases"),
        ToolCallDecision::Allow { spawn: None, .. }
    ));
    session
        .on_tool_result(
            legal,
            ToolOutcome::Success {
                body: OutcomeBody::Available("counsel's note".to_string()),
            },
        )
        .await
        .expect("the read closes");

    assert!(matches!(
        session
            .on_tool_call(replaced, false)
            .await
            .expect("the redacted bytes still reach the public audience"),
        ToolCallDecision::Allow { spawn: None, .. }
    ));
}

#[tokio::test]
async fn a_hop_that_leaves_a_gap_chains_into_an_approval_of_the_replaced_call() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let url = stub(serde_json::json!({"ruling": "approve"})).await;
    let runtime = Runtime::open(
        substituting_config(SUBSTITUTED_ATTENDED_SEND, Some(&url)),
        dir.path().join("appa.db"),
        None,
    )
    .expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    let hop = narrowed_and_blocked(&runtime, &mut session).await;

    assert!(matches!(
        session.on_remedy(hop, RemedyArguments::default(), None, None).await,
        Ok(RemedyDecision::Declined { .. }),
    ));
    assert!(runtime.open_dispatches(&root(), &root()).is_empty());
    let approval = latest_offer(&runtime);
    let replaced = substituted_send(
        &session
            .on_remedy(approval, RemedyArguments::default(), None, None)
            .await
            .expect("the approval executes"),
    );
    assert!(matches!(
        session.on_tool_call(send(RAW_BODY), false).await,
        Ok(ToolCallDecision::Deny { .. }),
    ));
    assert!(matches!(
        session
            .on_tool_call(replaced, false)
            .await
            .expect("the approved call releases"),
        ToolCallDecision::Allow { spawn: None, .. }
    ));
}

const SANITIZED_CHILD: &str = r#"
version = 2

[[policy.tool]]
name = "fetch"

[[policy.sanitizer]]
name = "scrub"
on = ["tool_output"]
[policy.sanitizer.permits]
audience = { from = ["insider"], to = ["public"] }

[policy.deployment]
context_control = true
"#;

fn sanitized_config(url: Option<&str>) -> Config {
    let binding = match url {
        Some(url) => format!("[externals.sanitizers.scrub]\nurl = \"{url}\"\n"),
        None => String::new(),
    };
    let text =
        format!("[policy]\n{SANITIZED_CHILD}\n[externals]\ntimeout_ms = 2000\nmax_body_bytes = 65536\n{binding}");
    config_from(&text)
}

fn sanitized_child(runtime: &Runtime) -> (Session, TrajectoryId) {
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    (session, TrajectoryId("cc:child".to_string()))
}

#[tokio::test]
async fn a_sanitized_child_return_is_staged_and_crosses_when_the_child_echoes_it() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let url = stub(serde_json::json!({"body": "scrubbed"})).await;
    let runtime =
        Runtime::open(sanitized_config(Some(&url)), dir.path().join("appa.db"), None).expect("the deployment opens");
    let (mut session, child_id) = sanitized_child(&runtime);
    let child = open_child_via(
        &mut session,
        fetch(serde_json::json!({})),
        child_id.clone(),
        "scrub",
        crate::engine::LabelSpelling::default(),
    )
    .await;
    let staged = child
        .on_child_end(Some("raw with pii".to_string()))
        .await
        .expect("the sanitized return is staged");
    assert_eq!(
        staged,
        crate::api::ChildReturnDecision::Staged {
            value: "scrubbed".to_string()
        },
        "the child is handed the derivation to echo",
    );
    let returned = child
        .on_child_end(Some("scrubbed".to_string()))
        .await
        .expect("the echo crosses");
    assert_eq!(
        returned,
        crate::api::ChildReturnDecision::Returned {
            value: "scrubbed".to_string()
        },
    );
    assert!(
        runtime.live(&root(), &child_id).is_ok(),
        "a return leaves the child live"
    );
}

#[tokio::test]
async fn a_duplicate_sanitized_return_replays_the_sanitized_crossing() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let url = stub(serde_json::json!({"body": "scrubbed"})).await;
    let runtime =
        Runtime::open(sanitized_config(Some(&url)), dir.path().join("appa.db"), None).expect("the deployment opens");
    let (mut session, child_id) = sanitized_child(&runtime);
    let child = open_child_via(
        &mut session,
        fetch(serde_json::json!({})),
        child_id,
        "scrub",
        crate::engine::LabelSpelling::default(),
    )
    .await;
    child
        .on_child_end(Some("raw with pii".to_string()))
        .await
        .expect("the sanitized return is staged");
    child
        .on_child_end(Some("scrubbed".to_string()))
        .await
        .expect("the echo crosses");

    let before = runtime.log_basis(&root());

    let replayed = child
        .on_child_end(Some("scrubbed".to_string()))
        .await
        .expect("the duplicate replays the recorded crossing");
    assert_eq!(
        replayed,
        crate::api::ChildReturnDecision::Returned {
            value: "scrubbed".to_string()
        },
        "the sanitizer is not consulted again; the recorded crossing answers",
    );
    assert_eq!(runtime.log_basis(&root()), before, "the replay appended nothing");
}

#[tokio::test]
async fn a_sanitizer_with_no_answer_withholds_the_crossing_and_keeps_the_child_live() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let url = stub(serde_json::json!(42)).await;
    let runtime =
        Runtime::open(sanitized_config(Some(&url)), dir.path().join("appa.db"), None).expect("the deployment opens");
    let (mut session, child_id) = sanitized_child(&runtime);
    let child = open_child_via(
        &mut session,
        fetch(serde_json::json!({})),
        child_id.clone(),
        "scrub",
        crate::engine::LabelSpelling::default(),
    )
    .await;
    let blocked = child
        .on_child_end(Some("raw with pii".to_string()))
        .await
        .expect("the withheld return is delivered");
    let crate::api::ChildReturnDecision::Blocked { .. } = blocked else {
        panic!("a no-answer sanitizer must withhold the crossing");
    };
    assert!(
        runtime.live(&root(), &child_id).is_ok(),
        "the child is held at its stop and may return again"
    );
}

const ATTESTED_CHILD: &str = r#"
version = 2

[[policy.sanitizer]]
name = "attest-schema"
on = ["tool_output"]
[policy.sanitizer.permits]
trust = { from = "suspicious", to = "trusted" }

[policy.deployment]
context_control = true
"#;

fn attested_text(policy: &str, binding: Option<&str>) -> String {
    let binding = match binding {
        Some(url) => format!("[externals.sanitizers.attest-schema]\nurl = \"{url}\"\n"),
        None => String::new(),
    };
    format!("[policy]\n{policy}\n[externals]\ntimeout_ms = 2000\nmax_body_bytes = 65536\n{binding}")
}

fn attested_config(policy: &str, binding: Option<&str>) -> Config {
    config_from(&attested_text(policy, binding))
}

fn hosted_config(policy: &str, binding: Option<&str>) -> Config {
    Config::hosted(
        &attested_text(policy, binding),
        crate::config::HostDefaults::new(std::time::Duration::from_millis(2000), 65536),
        |_| None,
    )
    .expect("the hosted document validates")
}

#[test]
fn the_reserved_attest_schema_needs_no_externals_binding() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    assert!(Runtime::open(attested_config(ATTESTED_CHILD, None), dir.path().join("appa.db"), None).is_ok());
}

#[test]
fn an_externals_binding_on_the_reserved_attest_schema_refuses_open() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    assert!(matches!(
        Runtime::open(
            attested_config(ATTESTED_CHILD, Some("http://127.0.0.1:1/")),
            dir.path().join("appa.db"),
            None,
        ),
        Err(OpenError::UnsupportedPolicy(_)),
    ));
}

#[test]
fn a_hosted_attest_schema_policy_needs_no_externals_binding() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let config = hosted_config(ATTESTED_CHILD, None);
    assert!(Runtime::open(config, dir.path().join("appa.db"), None).is_ok());
}

#[test]
fn a_hosted_binding_on_the_reserved_attest_schema_refuses_open() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let config = hosted_config(ATTESTED_CHILD, Some("http://127.0.0.1:1/"));
    assert!(matches!(
        Runtime::open(config, dir.path().join("appa.db"), None),
        Err(OpenError::UnsupportedPolicy(_)),
    ));
}

#[test]
fn an_unregistered_attest_schema_binding_still_refuses_open() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let config = hosted_config(
        "version = 2\n\n[policy.deployment]\ncontext_control = true\n",
        Some("http://127.0.0.1:1/"),
    );
    assert!(matches!(
        Runtime::open(config, dir.path().join("appa.db"), None),
        Err(OpenError::UnsupportedPolicy(_)),
    ));
}

const NARROWING: &str = r#"
version = 2

[[policy.tool]]
name = "leak"
parameters = { type = "object", properties = { q = { type = "string" } } }
delta = { audience = ["insider"] }

[[policy.sanitizer]]
name = "scrub"
on = ["tool_output"]
[policy.sanitizer.permits]
audience = { from = ["insider"], to = ["public"] }

[policy.deployment]
confined_results = ["leak"]
"#;

fn narrowing_config(url: &str) -> Config {
    let text = format!(
        "[policy]\n{NARROWING}\n[externals]\ntimeout_ms = 2000\nmax_body_bytes = 65536\n[externals.sanitizers.scrub]\nurl = \"{url}\"\n"
    );
    config_from(&text)
}

const EMITTING_LEAK: &str = r#"
version = 2

[[policy.tool]]
name = "leak"
parameters = { type = "object", properties = { q = { type = "string" } } }
effects = ["leak"]
delta = { audience = ["insider"] }

[[policy.sanitizer]]
name = "scrub"
on = ["tool_output"]
[policy.sanitizer.permits]
audience = { from = ["insider"], to = ["public"] }

[policy.deployment]
confined_results = ["leak"]
"#;

fn emitting_leak_config(url: &str) -> Config {
    let text = format!(
        "[policy]\n{EMITTING_LEAK}\n[externals]\ntimeout_ms = 2000\nmax_body_bytes = 65536\n[externals.sanitizers.scrub]\nurl = \"{url}\"\n"
    );
    config_from(&text)
}

fn leak() -> ProposedCall {
    ProposedCall {
        tool: "leak".to_string(),
        arguments: raw(serde_json::json!({"q": "all"})),
        cwd: None,
    }
}

async fn run_sanitize_offer(runtime: &Runtime, session: &mut crate::api::Session) -> ToolResultDecision {
    let quoted = runtime
        .minted_offers(&root(), &root())
        .into_iter()
        .find(|offer| {
            matches!(
                runtime.offer_kind(&root(), offer),
                Some(crate::api::OfferKind::Sanitizer { .. })
            )
        })
        .expect("the block surfaced a sanitize offer");
    let offer = runtime.resolve_in(&root(), &quoted).expect("the quoted id resolves").0;
    let authorized = session
        .on_remedy(offer, RemedyArguments::default(), None, None)
        .await
        .expect("the offer executes");
    assert!(matches!(authorized, RemedyDecision::Authorized { .. }));
    assert!(matches!(
        session
            .on_tool_call(leak(), false)
            .await
            .expect("the re-proposal resumes"),
        ToolCallDecision::Allow { spawn: None, .. }
    ));
    session
        .on_tool_result(
            leak(),
            ToolOutcome::Success {
                body: OutcomeBody::Available("raw with pii".to_string()),
            },
        )
        .await
        .expect("the outcome is delivered")
}

#[tokio::test]
async fn a_bound_sanitizer_derivation_replaces_the_raw() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let url = stub(serde_json::json!({"body": "scrubbed"})).await;
    let runtime =
        Runtime::open(narrowing_config(&url), dir.path().join("appa.db"), None).expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    assert!(matches!(
        session
            .on_tool_call(leak(), false)
            .await
            .expect("the block is delivered"),
        ToolCallDecision::Deny { .. },
    ));
    let decision = run_sanitize_offer(&runtime, &mut session).await;
    assert_eq!(
        decision,
        ToolResultDecision::Deliver {
            value: "scrubbed".to_string()
        },
        "the derivation is admitted and the raw is withheld",
    );
    assert!(runtime.open_dispatches(&root(), &root()).pop().is_none());
}

#[tokio::test]
async fn a_withhold_remedy_commits_effects_without_admitting_the_result() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let url = stub(serde_json::json!({"body": "unused"})).await;
    let runtime =
        Runtime::open(emitting_leak_config(&url), dir.path().join("appa.db"), None).expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");

    let ToolCallDecision::Deny { offers, .. } = session
        .on_tool_call(leak(), false)
        .await
        .expect("the narrowing block is delivered")
    else {
        panic!("the narrowing call must block before execution");
    };
    let quoted = offers
        .iter()
        .map(|offer| OfferId(offer.id.clone()))
        .find(|offer| {
            matches!(
                runtime.offer_kind(&root(), offer),
                Some(crate::api::OfferKind::Withhold)
            )
        })
        .expect("the confined result offers withholding");
    let offer = runtime.resolve_in(&root(), &quoted).expect("the quoted id resolves").0;
    assert!(matches!(
        session
            .on_remedy(offer, RemedyArguments::default(), None, None)
            .await
            .expect("the withhold offer executes"),
        RemedyDecision::Authorized { .. }
    ));
    assert!(matches!(
        session
            .on_tool_call(leak(), false)
            .await
            .expect("the approved call releases"),
        ToolCallDecision::Allow { .. }
    ));

    let decision = session
        .on_tool_result(
            leak(),
            ToolOutcome::Success {
                body: OutcomeBody::Available("raw with pii".to_string()),
            },
        )
        .await
        .expect("the successful result closes");
    let ToolResultDecision::Replace { placeholder, .. } = decision else {
        panic!("the raw result must be replaced");
    };
    assert_eq!(placeholder, "[appa] the result is withheld");

    let facts = runtime.log_facts(&root());
    assert!(
        facts
            .iter()
            .any(|fact| matches!(fact, appa_engine::fact::Fact::OutputWithheld { .. }))
    );
    assert!(facts.iter().any(|fact| matches!(
        fact,
        appa_engine::fact::Fact::DispatchClosed {
            outcome: appa_engine::fact::CloseOutcome::Success { effects },
            ..
        } if effects.contains(&appa_engine::fact::EffectKind::new("leak"))
    )));
    assert!(
        facts
            .iter()
            .all(|fact| !matches!(fact, appa_engine::fact::Fact::ValueAdmitted { .. })),
        "withholding admits no value",
    );
}

/// A tool whose result narrows on two dimensions with a sanitizer
/// that clears only one: the derivation is admitted and staged, and
/// the residual narrowing is what the model is told about.
const PARTLY_CLEARED: &str = r#"
version = 2

[[policy.tool]]
name = "leak"
parameters = { type = "object", properties = { q = { type = "string" } } }
delta = { audience = ["insider"], trust = "suspicious" }

[[policy.sanitizer]]
name = "scrub"
on = ["tool_output"]
[policy.sanitizer.permits]
audience = { from = ["insider"], to = ["public"] }

[policy.deployment]
confined_results = ["leak"]
"#;

fn partly_cleared_config(url: &str) -> Config {
    let text = format!(
        "[policy]\n{PARTLY_CLEARED}\n[externals]\ntimeout_ms = 2000\nmax_body_bytes = 65536\n[externals.sanitizers.scrub]\nurl = \"{url}\"\n"
    );
    config_from(&text)
}

#[tokio::test]
async fn a_partly_cleared_derivation_is_staged_with_its_own_remedies() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let url = stub(serde_json::json!({"body": "scrubbed"})).await;
    let runtime =
        Runtime::open(partly_cleared_config(&url), dir.path().join("appa.db"), None).expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    assert!(matches!(
        session
            .on_tool_call(leak(), false)
            .await
            .expect("the block is delivered"),
        ToolCallDecision::Deny { .. },
    ));
    let before = runtime.minted_offers(&root(), &root()).len();
    let ToolResultDecision::Replace {
        placeholder,
        presentation,
    } = run_sanitize_offer(&runtime, &mut session).await
    else {
        panic!("a staged derivation is delivered as a replacement, not kept");
    };
    assert!(
        !placeholder.contains("raw with pii"),
        "the raw body never reaches the model: {placeholder}",
    );
    assert!(
        runtime.minted_offers(&root(), &root()).len() > before,
        "the stage surfaced its own remedy for the narrowing the sanitizer left",
    );
    let presentation = presentation.expect("a result-time block preserves its remedy state");
    assert!(!presentation.offers.is_empty());
    assert_eq!(presentation.feedback, placeholder);
}

const PARTLY_CLEARED_CHILD: &str = r#"
version = 2

[[policy.tool]]
name = "fetch"

[[policy.tool]]
name = "browse"
delta = { trust = "suspicious" }

[[policy.sanitizer]]
name = "scrub"
on = ["tool_output"]
[policy.sanitizer.permits]
audience = { from = ["insider"], to = ["public"] }

[policy.deployment]
context_control = true
"#;

fn partly_cleared_child_config(url: &str) -> Config {
    config_from(&format!(
        "[policy]\n{PARTLY_CLEARED_CHILD}\n[externals]\ntimeout_ms = 2000\nmax_body_bytes = 65536\n[externals.sanitizers.scrub]\nurl = \"{url}\"\n"
    ))
}

/// A child routed through `scrub` whose own browsing narrowed it to suspicious.
async fn narrowed_child(runtime: &Runtime, session: &mut Session, floor: crate::engine::LabelSpelling) -> Session {
    let child_id = TrajectoryId("cc:child".to_string());
    let child = open_child_via(session, fetch(serde_json::json!({})), child_id.clone(), "scrub", floor).await;
    let browse = ProposedCall {
        tool: "browse".to_string(),
        arguments: raw(serde_json::json!({})),
        cwd: None,
    };
    assert!(matches!(
        child.on_tool_call(browse.clone(), false).await,
        Ok(ToolCallDecision::Deny { .. })
    ));
    let offer = surfaced_offer_for(runtime, &root(), &child_id);
    assert!(matches!(
        child.on_remedy(offer, RemedyArguments::default(), None, None).await,
        Ok(RemedyDecision::Authorized { .. })
    ));
    assert!(matches!(
        child
            .on_tool_call(browse.clone(), false)
            .await
            .expect("the accepted narrowing releases the call"),
        ToolCallDecision::Allow { spawn: None, .. }
    ));
    assert_eq!(
        child
            .on_tool_result(
                browse,
                ToolOutcome::Success {
                    body: OutcomeBody::Available("web page".to_string()),
                },
            )
            .await
            .expect("the result admits into the child"),
        ToolResultDecision::Keep,
    );
    child
}

#[tokio::test]
async fn a_parent_that_declared_a_lower_floor_takes_the_sanitized_return_and_narrows() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let url = stub(serde_json::json!({"body": "scrubbed"})).await;
    let runtime = Runtime::open(partly_cleared_child_config(&url), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    let child = narrowed_child(&runtime, &mut session, floor_trust("suspicious")).await;

    let staged = child
        .on_child_end(Some("raw with pii".to_string()))
        .await
        .expect("the sanitized return is staged");
    assert_eq!(
        staged,
        crate::api::ChildReturnDecision::Staged {
            value: "scrubbed".to_string()
        },
    );
    let returned = child
        .on_child_end(Some("scrubbed".to_string()))
        .await
        .expect("the echo crosses");
    assert_eq!(
        returned,
        crate::api::ChildReturnDecision::Returned {
            value: "scrubbed".to_string()
        },
    );
    assert_eq!(
        runtime.status(&root()).expect("the root answers").trust,
        "suspicious",
        "the crossing narrowed the parent to the floor it declared",
    );
}

#[tokio::test]
async fn a_bound_sanitizer_no_answer_withholds_and_stays_retryable() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let url = stub(serde_json::json!(42)).await;
    let runtime =
        Runtime::open(narrowing_config(&url), dir.path().join("appa.db"), None).expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    assert!(matches!(
        session
            .on_tool_call(leak(), false)
            .await
            .expect("the block is delivered"),
        ToolCallDecision::Deny { .. },
    ));
    let decision = run_sanitize_offer(&runtime, &mut session).await;
    let ToolResultDecision::Replace { placeholder, .. } = decision else {
        panic!("the raw must be withheld");
    };
    assert!(!placeholder.contains("pii"), "the raw body never reaches the model");
    assert!(
        runtime.open_dispatches(&root(), &root()).pop().is_some(),
        "a no-answer sanitizer leaves the dispatch open so the return may be retried",
    );
}

const MARKED: &str = r#"
version = 2

# A neutral second tool: its result folds at the trajectory's own label.
[[policy.tool]]
name = "fetch"
parameters = { type = "object", properties = { b = { type = "integer" }, a = { type = "integer" } } }
delta = {}

[[policy.tool]]
name = "mark"
parameters = { type = "object", properties = { a = { type = "integer" } } }
delta = { trust = "suspicious" }

[[policy.tool]]
name = "bare"
parameters = { type = "object", properties = { a = { type = "integer" } } }
delta = {}

[policy.deployment]
context_control = true
"#;

fn mark() -> ProposedCall {
    ProposedCall {
        tool: "mark".to_string(),
        arguments: raw(serde_json::json!({"a": 1})),
        cwd: None,
    }
}

async fn admit_success(runtime: &Runtime, session: &mut Session, call: ProposedCall) {
    let decision = session
        .on_tool_call(call.clone(), false)
        .await
        .expect("the call is decided");
    if matches!(decision, ToolCallDecision::Deny { .. }) {
        let offers = runtime.minted_offers(&root(), session.trajectory());
        let quoted = offers
            .first()
            .expect("the narrowing block surfaced its acceptance")
            .clone();
        let offer = runtime.resolve_in(&root(), &quoted).expect("the quoted id resolves").0;
        assert!(matches!(
            session
                .on_remedy(offer, RemedyArguments::default(), None, None)
                .await
                .expect("the acceptance executes"),
            RemedyDecision::Authorized { .. },
        ));
        assert!(matches!(
            session
                .on_tool_call(call.clone(), false)
                .await
                .expect("the re-proposal resumes"),
            ToolCallDecision::Allow { spawn: None, .. }
        ));
    }
    let kept = session
        .on_tool_result(
            call,
            ToolOutcome::Success {
                body: OutcomeBody::Available("data".to_string()),
            },
        )
        .await
        .expect("the result is admitted");
    assert_eq!(kept, ToolResultDecision::Keep, "the fixture result must actually admit");
}

#[test]
fn a_fresh_root_status_renders_the_neutral_label() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime =
        Runtime::open(config_with(MARKED, None), dir.path().join("appa.db"), None).expect("the deployment opens");
    runtime.create_session(root(), None).expect("a fresh id opens");
    let status = runtime.status(&root()).expect("a fresh root answers");
    assert_eq!(status.trajectory, "cc:root");
    assert_eq!(status.trust, "trusted");
    assert_eq!(status.audience, "public");
    assert_eq!(runtime.try_status(&root()).expect("the read succeeds"), status);
    assert!(matches!(
        runtime.try_status(&TrajectoryId("cc:ghost".to_string())),
        Err(crate::api::StatusReadError::UnknownRoot { root }) if root == "cc:ghost"
    ));
}

#[tokio::test]
async fn a_suspicious_admission_narrows_the_status_irreversibly() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime =
        Runtime::open(config_with(MARKED, None), dir.path().join("appa.db"), None).expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    admit_success(&runtime, &mut session, mark()).await;
    assert_eq!(runtime.status(&root()).expect("the root answers").trust, "suspicious");
    admit_success(
        &runtime,
        &mut session,
        ProposedCall {
            tool: "bare".to_string(),
            arguments: raw(serde_json::json!({"a": 2})),
            cwd: None,
        },
    )
    .await;
    let status = runtime.status(&root()).expect("the root answers");
    assert_eq!(status.trust, "suspicious", "the fold never widens");
    assert_eq!(status.audience, "public", "a neutral admission resolves cleanly");
}

#[tokio::test]
async fn the_status_read_appends_nothing_and_outlives_the_session() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime =
        Runtime::open(config_with(MARKED, None), dir.path().join("appa.db"), None).expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    admit_success(&runtime, &mut session, mark()).await;

    assert!(runtime.status(&TrajectoryId("cc:ghost".to_string())).is_none());

    let before = runtime.log_facts(&root()).len();
    runtime.status(&root()).expect("the root answers");
    runtime.status(&root()).expect("the root answers again");
    let after = runtime.log_facts(&root()).len();
    assert_eq!(before, after, "a status read appends nothing");

    assert_eq!(runtime.status(&root()).expect("the root answers").trust, "suspicious",);
}

#[tokio::test]
async fn an_untrusted_log_answers_no_status() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime =
        Runtime::open(config_with(MARKED, None), dir.path().join("appa.db"), None).expect("the deployment opens");
    runtime.create_session(root(), None).expect("a fresh id opens");
    runtime.store().corrupt_batch(&root(), 0, b"not engine records");
    assert!(runtime.status(&root()).is_none());
    assert!(runtime.try_status(&root()).is_err());
}

#[tokio::test]
async fn an_ended_child_is_refused_on_the_proposal_path() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime =
        Runtime::open(config_with(MARKED, None), dir.path().join("appa.db"), None).expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    let child = open_child(
        &mut session,
        fetch(serde_json::json!({"a": 1})),
        TrajectoryId("cc:child".to_string()),
    )
    .await;
    child.on_child_end(None).await.expect("the child ends with no return");

    let error = child
        .on_tool_call(fetch(serde_json::json!({"a": 2})), false)
        .await
        .expect_err("the ended child proposes nothing further");
    assert!(matches!(error, EventError::TrajectoryEnded), "got {error:?}");
    assert!(!error.is_operational());
}

#[tokio::test]
async fn a_childs_fold_stays_out_of_the_root_status() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime =
        Runtime::open(config_with(MARKED, None), dir.path().join("appa.db"), None).expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    let child_id = TrajectoryId("cc:child".to_string());
    let mut child = open_child_floored(
        &mut session,
        fetch(serde_json::json!({"a": 1})),
        child_id.clone(),
        floor_trust("suspicious"),
    )
    .await;
    admit_success(&runtime, &mut child, mark()).await;

    let child_status = runtime
        .branch_status(&root(), &child_id)
        .expect("the child's branch renders");
    assert_eq!(
        child_status.trust, "suspicious",
        "the child's admission narrowed its branch"
    );

    assert_eq!(
        runtime.status(&root()).expect("the root answers").trust,
        "trusted",
        "a dirty child never moves the root fold",
    );
    assert!(runtime.status(&child_id).is_none(), "the status read is root-only");
}

fn trusted_send() -> ProposedCall {
    ProposedCall {
        tool: "send".to_string(),
        arguments: raw(serde_json::json!({})),
        cwd: None,
    }
}

/// Admit a narrowing result on `session`'s trajectory in `family`, accepting the narrowing
/// its block offers.
async fn taint_in(runtime: &Runtime, family: &TrajectoryId, session: &mut Session) {
    let call = taint(serde_json::json!({"a": 1}));
    let decision = session
        .on_tool_call(call.clone(), false)
        .await
        .expect("the call is decided");
    if matches!(decision, ToolCallDecision::Deny { .. }) {
        let quoted = runtime
            .minted_offers(family, session.trajectory())
            .first()
            .expect("the narrowing block surfaced its acceptance")
            .clone();
        let offer = runtime.resolve_in(family, &quoted).expect("the quoted id resolves").0;
        assert!(matches!(
            session
                .on_remedy(offer, RemedyArguments::default(), None, None)
                .await
                .expect("the acceptance executes"),
            RemedyDecision::Authorized { .. },
        ));
        assert!(matches!(
            session
                .on_tool_call(call.clone(), false)
                .await
                .expect("the re-proposal resumes"),
            ToolCallDecision::Allow { spawn: None, .. }
        ));
    }
    let kept = session
        .on_tool_result(
            call,
            ToolOutcome::Success {
                body: OutcomeBody::Available("outside content".to_string()),
            },
        )
        .await
        .expect("the result is admitted");
    assert_eq!(kept, ToolResultDecision::Keep, "the taint must actually admit");
}

/// Is `call` released? A release is reported back at once as a success, so the family
/// carries no open call afterwards.
async fn runs(session: &Session, call: ProposedCall) -> bool {
    match session
        .on_tool_call(call.clone(), false)
        .await
        .expect("the call is decided")
    {
        ToolCallDecision::Allow { .. } => {
            session
                .on_tool_result(
                    call,
                    ToolOutcome::Success {
                        body: OutcomeBody::Available("done".to_string()),
                    },
                )
                .await
                .expect("the result is admitted");
            true
        }
        _ => false,
    }
}

/// Is `send`, which requires a trusted trajectory, released?
async fn sends(session: &Session) -> bool {
    runs(session, trusted_send()).await
}

fn root_fork_id(name: &str) -> TrajectoryId {
    TrajectoryId(format!("cc:{name}"))
}

/// Open `fork` as an independent root fork of `parent`, and a session on it.
fn open_root_fork_session(runtime: &Runtime, parent: &TrajectoryId, fork: &TrajectoryId) -> Session {
    runtime
        .open_root_fork(parent, parent, fork)
        .expect("the root fork opens");
    runtime.session(fork, fork).expect("the root fork reopens")
}

#[tokio::test]
async fn a_root_fork_starts_at_its_parents_label() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let mut parent = runtime.create_session(root(), None).expect("a fresh id opens");
    taint_in(&runtime, &root(), &mut parent).await;

    let fork = root_fork_id("fork");
    runtime
        .open_root_fork(&root(), &root(), &fork)
        .expect("the root fork opens");

    assert_eq!(
        runtime.status(&fork).expect("the fork is a root of its own").trust,
        "suspicious",
        "the fork starts where its parent stood"
    );
    let forked = runtime.session(&fork, &fork).expect("the fork reopens");
    assert!(
        !sends(&forked).await,
        "the parent's taint blocks the trusted-only sink in the fork"
    );
}

#[tokio::test]
async fn root_forks_and_their_parent_continue_apart() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let mut parent = runtime.create_session(root(), None).expect("a fresh id opens");
    let (a, b) = (root_fork_id("fork-a"), root_fork_id("fork-b"));
    runtime.open_root_fork(&root(), &root(), &a).expect("root fork a opens");
    runtime.open_root_fork(&root(), &root(), &b).expect("root fork b opens");

    let mut fork_a = runtime.session(&a, &a).expect("fork a reopens");
    taint_in(&runtime, &a, &mut fork_a).await;
    assert_eq!(runtime.status(&a).expect("fork a answers").trust, "suspicious");
    assert_eq!(
        runtime.status(&root()).expect("the parent answers").trust,
        "trusted",
        "a fork's admission never reaches its parent"
    );
    assert_eq!(
        runtime.status(&b).expect("fork b answers").trust,
        "trusted",
        "nor a sibling fork"
    );

    taint_in(&runtime, &root(), &mut parent).await;
    assert_eq!(
        runtime.status(&b).expect("fork b answers").trust,
        "trusted",
        "the parent's later admission never reaches a fork"
    );
    let fork_b = runtime.session(&b, &b).expect("fork b reopens");
    assert!(
        sends(&fork_b).await,
        "the clean sibling still releases the trusted-only sink"
    );
    assert!(!sends(&parent).await, "the parent's own taint blocks it there");
}

#[tokio::test]
async fn a_parents_open_call_survives_its_root_forks_turn() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let parent = runtime.create_session(root(), None).expect("a fresh id opens");
    let call = fetch(serde_json::json!({"a": 1}));
    assert!(matches!(
        parent
            .on_tool_call(call.clone(), false)
            .await
            .expect("the call is decided"),
        ToolCallDecision::Allow { .. }
    ));

    let fork = root_fork_id("fork");
    runtime
        .open_root_fork(&root(), &root(), &fork)
        .expect("the root fork opens");
    let forked = runtime.session(&fork, &fork).expect("the fork reopens");
    forked.on_turn_end().await.expect("the fork's turn ends");

    let kept = parent
        .on_tool_result(
            call,
            ToolOutcome::Success {
                body: OutcomeBody::Available("data".to_string()),
            },
        )
        .await
        .expect("the parent's result is still reportable");
    assert_eq!(
        kept,
        ToolResultDecision::Keep,
        "a fork's turn end closes nothing of its parent's"
    );
}

#[tokio::test]
async fn a_root_fork_of_a_root_fork_starts_where_its_own_parent_stands() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    runtime.create_session(root(), None).expect("a fresh id opens");
    let (first, second) = (root_fork_id("first"), root_fork_id("second"));
    runtime
        .open_root_fork(&root(), &root(), &first)
        .expect("the first root fork opens");
    let mut forked = runtime.session(&first, &first).expect("the first fork reopens");
    taint_in(&runtime, &first, &mut forked).await;

    runtime
        .open_root_fork(&first, &first, &second)
        .expect("a root fork opens from another root fork");

    assert_eq!(
        runtime.status(&second).expect("the second fork answers").trust,
        "suspicious"
    );
    assert_eq!(runtime.status(&root()).expect("the root answers").trust, "trusted");
}

#[tokio::test]
async fn opening_a_root_fork_again_is_harmless_and_an_unrelated_root_is_refused() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    runtime.create_session(root(), None).expect("a fresh id opens");
    let fork = root_fork_id("fork");
    runtime
        .open_root_fork(&root(), &root(), &fork)
        .expect("the root fork opens");
    runtime
        .open_root_fork(&root(), &root(), &fork)
        .expect("opening the same root fork again is not an error");

    let unrelated = root_fork_id("unrelated");
    runtime
        .create_session(unrelated.clone(), None)
        .expect("an unrelated root opens");
    assert_eq!(
        runtime.open_root_fork(&unrelated, &root(), &fork),
        Err(super::super::RootForkRefusal::RootIdConflict),
        "a standing fork cannot be reopened under another parent family"
    );
    assert_eq!(
        runtime.open_root_fork(&root(), &unrelated, &fork),
        Err(super::super::RootForkRefusal::RootIdConflict),
        "a standing fork cannot be reopened from another parent trajectory"
    );
    assert_eq!(
        runtime.open_root_fork(&root(), &root(), &unrelated),
        Err(super::super::RootForkRefusal::RootIdConflict),
        "a root that governs its own trajectory never becomes a fork"
    );
    assert_eq!(
        runtime.open_root_fork(
            &root_fork_id("missing"),
            &root_fork_id("missing"),
            &root_fork_id("orphan")
        ),
        Err(super::super::RootForkRefusal::ParentUnavailable)
    );
    assert_eq!(
        runtime.open_root_fork(&root(), &root(), &root()),
        Err(super::super::RootForkRefusal::RootIdConflict)
    );
}

/// A root fork that stands is recognized before its parent is read, so opening it again holds
/// after the parent has ended, while a new root fork of the ended parent is refused.
#[tokio::test]
async fn opening_a_root_fork_again_holds_after_its_parent_ended() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    let child_id = child("c1");
    let spawned = open_child(&mut session, fetch(serde_json::json!({"a": 1})), child_id.clone()).await;
    let fork = root_fork_id("fork");
    runtime
        .open_root_fork(&root(), &child_id, &fork)
        .expect("a root fork opens from a spawned child");
    spawned.on_child_end(None).await.expect("the child ends with no return");

    runtime
        .open_root_fork(&root(), &child_id, &fork)
        .expect("the standing root fork is recognized though its parent ended");
    assert_eq!(
        runtime.open_root_fork(&root(), &child_id, &root_fork_id("late")),
        Err(super::super::RootForkRefusal::ParentUnavailable),
        "an ended trajectory takes no new fork"
    );
}

const HISTORY: &str = r#"
version = 2

# `emit` records `k` when it succeeds. `guard` may not run once `k` is recorded, or while a call
# declaring it has been allowed and has not finished; `need` runs only once `k` is recorded.
[[policy.tool]]
name = "emit"
effects = ["k"]
delta = {}

[[policy.tool]]
name = "guard"
requires = { effects = { excludes = ["k"] } }
delta = {}

[[policy.tool]]
name = "need"
requires = { effects = { contains = ["k"] } }
delta = {}
"#;

fn history_call(tool: &str) -> ProposedCall {
    ProposedCall {
        tool: tool.to_string(),
        arguments: raw(serde_json::json!({})),
        cwd: None,
    }
}

async fn allowed(session: &Session, call: ProposedCall) {
    assert!(matches!(
        session.on_tool_call(call, false).await.expect("the call is decided"),
        ToolCallDecision::Allow { .. }
    ));
}

/// A root fork carries its parent family's effect history. An effect recorded before the root fork
/// satisfies `contains` there and blocks `excludes`. A call allowed before the fork that has
/// not finished, whether still running or closed at a turn end with no outcome, blocks
/// `excludes` in the fork as it does in the parent, and never counts as recorded. The fork
/// never learns how that call ends, so it stays blocked after the call fails in the parent.
#[tokio::test]
async fn a_root_fork_carries_its_parents_effects_and_unfinished_calls() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime =
        Runtime::open(config_with(HISTORY, None), dir.path().join("appa.db"), None).expect("the deployment opens");

    let recorded = root_fork_id("recorded");
    let parent = runtime
        .create_session(recorded.clone(), None)
        .expect("a fresh id opens");
    assert!(runs(&parent, history_call("emit")).await);
    let fork = open_root_fork_session(&runtime, &recorded, &root_fork_id("recorded-fork"));
    assert!(
        !runs(&fork, history_call("guard")).await,
        "a recorded effect blocks excludes in the fork"
    );
    assert!(runs(&fork, history_call("need")).await, "and satisfies contains there");

    let unknown = root_fork_id("unknown");
    let parent = runtime.create_session(unknown.clone(), None).expect("a fresh id opens");
    allowed(&parent, history_call("emit")).await;
    parent
        .on_turn_end()
        .await
        .expect("the turn end closes the unreported call");
    assert!(
        !runs(&parent, history_call("guard")).await,
        "the parent refuses guard while emit may have run"
    );
    let fork = open_root_fork_session(&runtime, &unknown, &root_fork_id("unknown-fork"));
    assert!(!runs(&fork, history_call("guard")).await, "so does the fork");
    assert!(
        !runs(&fork, history_call("need")).await,
        "an unfinished call is never a recorded effect"
    );

    let running = root_fork_id("running");
    let parent = runtime.create_session(running.clone(), None).expect("a fresh id opens");
    allowed(&parent, history_call("emit")).await;
    let fork = open_root_fork_session(&runtime, &running, &root_fork_id("running-fork"));
    parent
        .on_tool_result(
            history_call("emit"),
            ToolOutcome::Failure {
                message: "exit 1".to_string(),
            },
        )
        .await
        .expect("the failure closes");
    assert!(
        runs(&parent, history_call("guard")).await,
        "the failure frees guard in the parent"
    );
    assert!(
        !runs(&fork, history_call("guard")).await,
        "the fork never learns how the call ended"
    );
}

/// How many remedies the block of `call` on `session` offers.
async fn offered(session: &Session, call: ProposedCall) -> usize {
    match session.on_tool_call(call, false).await.expect("the block is delivered") {
        ToolCallDecision::Deny { offers, .. } => offers.len(),
        other => panic!("an attended call blocks, got {other:?}"),
    }
}

/// Block `call` on the root `session` and have the authority its first offer names deny it.
async fn denied_on_root(runtime: &Runtime, session: &Session, call: ProposedCall) {
    let ToolCallDecision::Deny { offers, .. } =
        session.on_tool_call(call, false).await.expect("the block is delivered")
    else {
        panic!("an attended call blocks");
    };
    let quoted = OfferId(offers.first().expect("the block offers its authority").id.clone());
    let offer = runtime.resolve_in(&root(), &quoted).expect("the quoted id resolves").0;
    assert!(matches!(
        session
            .on_remedy(offer, RemedyArguments::default(), None, None)
            .await
            .expect("the denial is delivered"),
        RemedyDecision::Declined { .. },
    ));
}

/// A root fork inherits the denials its parent held when the root fork was taken. A call the parent's
/// authority denied is offered no plan naming that authority in the fork, while a call the
/// parent never denied, or denied only after the fork, still is.
#[tokio::test]
async fn a_root_fork_inherits_the_denials_its_parent_held_at_the_fork() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let url = stub(serde_json::json!({"ruling": "deny", "reason": "no"})).await;
    let runtime = Runtime::open(config_with(ATTENTION, Some(&url)), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let parent = runtime.create_session(root(), None).expect("a fresh id opens");
    denied_on_root(&runtime, &parent, wire(500)).await;
    let fork = open_root_fork_session(&runtime, &root(), &root_fork_id("fork"));
    denied_on_root(&runtime, &parent, wire(700)).await;

    assert_eq!(
        offered(&fork, wire(500)).await,
        0,
        "the parent's denial holds in the fork"
    );
    assert_eq!(
        offered(&fork, wire(600)).await,
        1,
        "a call the parent never denied is still offered its authority"
    );
    assert_eq!(
        offered(&fork, wire(700)).await,
        1,
        "a denial recorded after the fork never reaches it"
    );
}

/// A root fork belongs to its parent's deployment and decides at its head, whatever revision
/// opened the parent: after a reload drops `fetch`, a root fork of an older root has the
/// reloaded policy's `read` and no `fetch`.
#[tokio::test]
async fn a_root_fork_taken_after_a_reload_decides_at_the_head() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    runtime.create_session(root(), None).expect("a fresh id opens");
    runtime
        .reload(config_with(READ_ONLY, None))
        .expect("the edited policy installs");

    let fork = open_root_fork_session(&runtime, &root(), &root_fork_id("fork"));
    assert!(
        matches!(
            fork.on_tool_call(fetch(serde_json::json!({"a": 1})), false).await,
            Err(EventError::UndeclaredTool { tool }) if tool == "fetch"
        ),
        "the fork belongs to its parent's deployment, whose head declares no fetch"
    );
    let read = ProposedCall {
        tool: "read".to_string(),
        arguments: raw(serde_json::json!({"path": "a.txt"})),
        cwd: None,
    };
    assert!(
        matches!(
            fork.on_tool_call(read, false).await,
            Ok(ToolCallDecision::Allow { spawn: None, .. })
        ),
        "the fork decides at the head"
    );
}

/// A spawned child can open a root fork. The root fork starts at the child's label, not at
/// its family's root label, and becomes an independent root.
#[tokio::test]
async fn a_root_fork_of_a_spawned_child_starts_at_the_childs_label() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let mut parent = runtime.create_session(root(), None).expect("a fresh id opens");
    let child_id = child("c1");
    let mut spawned = open_child_floored(
        &mut parent,
        fetch(serde_json::json!({"a": 1})),
        child_id.clone(),
        floor_trust("suspicious"),
    )
    .await;
    taint_in(&runtime, &root(), &mut spawned).await;

    let fork = root_fork_id("fork");
    runtime
        .open_root_fork(&root(), &child_id, &fork)
        .expect("a root fork opens from a spawned child");
    assert_eq!(
        runtime.status(&fork).expect("the fork is a root of its own").trust,
        "suspicious",
        "the fork starts at the child's label"
    );
    assert_eq!(
        runtime.status(&root()).expect("the root answers").trust,
        "trusted",
        "not at its root's"
    );
    let forked = runtime.session(&fork, &fork).expect("the fork reopens");
    assert!(
        !sends(&forked).await,
        "the child's taint blocks the trusted-only sink in the fork"
    );
}

/// The floor the parent declared bounds the child too: a narrowing below it has no
/// acceptance to offer, so nothing the child admits can fall below what may cross.
#[tokio::test]
async fn a_child_under_its_parents_floor_is_offered_no_narrowing_below_it() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime =
        Runtime::open(config_with(MARKED, None), dir.path().join("appa.db"), None).expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    let child_id = TrajectoryId("cc:child".to_string());
    let child = open_child(&mut session, fetch(serde_json::json!({"a": 1})), child_id.clone()).await;

    let decision = child.on_tool_call(mark(), false).await.expect("the block is delivered");
    let ToolCallDecision::Deny { offers, .. } = decision else {
        panic!("a narrowing below the floor blocks, got {decision:?}");
    };
    assert!(
        offers.is_empty(),
        "no acceptance is offered below the floor the parent declared: {offers:?}"
    );
    assert!(runtime.minted_offers(&root(), &child_id).is_empty());
}

fn child(name: &str) -> TrajectoryId {
    TrajectoryId(format!("cc:{name}"))
}

fn resumed_count(runtime: &Runtime) -> usize {
    runtime
        .audit(&root())
        .expect("the audit reads")
        .into_iter()
        .filter(|entry| matches!(entry.event, crate::api::AuditEvent::Resumed { .. }))
        .count()
}

fn fork_opened_count(runtime: &Runtime) -> usize {
    runtime
        .log_facts(&root())
        .iter()
        .filter(|fact| matches!(fact, appa_engine::fact::Fact::ForkOpened { .. }))
        .count()
}

fn dispatch_open(runtime: &Runtime, trajectory: &TrajectoryId) -> bool {
    !runtime.open_dispatches(&root(), trajectory).is_empty()
}

fn opened(runtime: &Runtime, trajectory: &TrajectoryId) -> bool {
    runtime.names_trajectory(&root(), trajectory)
}

async fn release_spawn(session: &mut Session, spawn: ProposedCall) -> SpawnBinding {
    declared_spawn(session, spawn).await
}

fn prompt(name: &str) -> PromptKey {
    PromptKey(name.to_string())
}

/// Declare a fan-out spawn's return as spoken, floored at the parent's current label, and
/// release it under `call_id` and `prompt`.
async fn release_fan_out(
    session: &Session,
    spawn: ProposedCall,
    call_id: &str,
    prompt: &PromptKey,
) -> Result<ToolCallDecision, EventError> {
    let call = authorize_spawn_as(
        session,
        spawn,
        SpawnKind::FanOut,
        appa_runtime_api::OfferedReturn::AsSpoken,
        Some(crate::engine::LabelSpelling::default()),
    )
    .await;
    session
        .on_tool_call_identified(
            call.proposed(),
            Some(call_id.to_string()),
            Some(SpawnKind::FanOut),
            Some(prompt.clone()),
        )
        .await
}

fn the_root() -> Actor {
    Actor {
        root: root(),
        child: None,
    }
}

#[tokio::test]
async fn a_fan_out_binds_each_child_its_prompt_names_until_its_call_settles() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    let released = release_fan_out(&session, fetch(serde_json::json!({"a": 1})), "wf-1", &prompt("p1"))
        .await
        .expect("the declared fan-out releases");
    assert!(matches!(released, ToolCallDecision::Allow { .. }), "got {released:?}");

    for name in ["c1", "c2"] {
        session
            .on_child_start(child(name), SpawnRef::FanOut(prompt("p1")))
            .expect("a child of the fan-out's prompt binds");
    }
    assert_eq!(fork_opened_count(&runtime), 2, "one declaration backs a fork per child");
    assert!(matches!(
        session.on_child_start(child("c3"), SpawnRef::FanOut(prompt("p2"))),
        Err(EventError::SpawnNotTaken)
    ));

    runtime
        .record_call_settled(&the_root(), "another-call".to_string())
        .expect("a notice for a call the log never bound records nothing");
    session
        .on_child_start(child("c3"), SpawnRef::FanOut(prompt("p1")))
        .expect("a notice naming another call leaves the fan-out open");

    for _ in 0..2 {
        runtime
            .record_call_settled(&the_root(), "wf-1".to_string())
            .expect("the notice records");
    }
    let settled = runtime
        .inner
        .log(&root())
        .expect("the log reads")
        .host_records()
        .iter()
        .filter(|record| matches!(record.observation, appa_eventlog::HostObservation::CallSettled { .. }))
        .count();
    assert_eq!(settled, 1, "a repeated notice adds nothing");
    assert!(matches!(
        session.on_child_start(child("c4"), SpawnRef::FanOut(prompt("p1"))),
        Err(EventError::SpawnNotTaken)
    ));

    let c1 = session
        .on_child_start(child("c1"), SpawnRef::FanOut(prompt("p1")))
        .expect("a child bound before the notice starts again on its own fork");
    assert_eq!(
        c1.on_child_return("{\"rows\":3}".to_string())
            .await
            .expect("the return is judged"),
        ChildReturnDecision::Returned {
            value: "{\"rows\":3}".to_string()
        },
        "a child bound before the notice still returns across its fork"
    );
}

#[tokio::test]
async fn a_second_fan_out_under_one_prompt_waits_for_the_first_to_settle() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    let workflow = |a: u64| fetch(serde_json::json!({ "a": a }));
    assert!(matches!(
        release_fan_out(&session, workflow(1), "wf-1", &prompt("p1")).await,
        Ok(ToolCallDecision::Allow { .. })
    ));
    assert!(matches!(
        release_fan_out(&session, workflow(2), "wf-2", &prompt("p1")).await,
        Err(EventError::FanOutOutstanding)
    ));
    assert!(matches!(
        release_fan_out(&session, workflow(3), "wf-3", &prompt("p2")).await,
        Ok(ToolCallDecision::Allow { .. })
    ));
    let agent = authorize_spawn(&session, workflow(4), None, crate::engine::LabelSpelling::default()).await;
    assert!(
        matches!(
            session
                .on_tool_call_identified(
                    agent.proposed(),
                    Some("agent-1".to_string()),
                    Some(SpawnKind::Single),
                    None
                )
                .await,
            Ok(ToolCallDecision::Allow { spawn: Some(_), .. })
        ),
        "a single spawn does not wait on an open fan-out"
    );

    runtime
        .record_call_settled(&the_root(), "wf-1".to_string())
        .expect("the notice records");
    let retried = session
        .on_tool_call_identified(
            workflow(2),
            Some("wf-2".to_string()),
            Some(SpawnKind::FanOut),
            Some(prompt("p1")),
        )
        .await;
    assert!(
        matches!(retried, Ok(ToolCallDecision::Allow { .. })),
        "the refused release kept its declaration and releases once the first settles: {retried:?}"
    );
}

#[tokio::test]
async fn a_fan_out_without_its_hosts_ids_is_refused() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    let call = authorize_spawn_as(
        &session,
        fetch(serde_json::json!({"a": 1})),
        SpawnKind::FanOut,
        appa_runtime_api::OfferedReturn::AsSpoken,
        Some(crate::engine::LabelSpelling::default()),
    )
    .await;
    let refused = session
        .on_tool_call_identified(call.proposed(), Some("wf-1".to_string()), Some(SpawnKind::FanOut), None)
        .await;
    assert!(matches!(refused, Err(EventError::FanOutUnkeyed)), "got {refused:?}");
    assert!(!dispatch_open(&runtime, &root()));
}

#[tokio::test]
async fn a_fan_out_childs_call_before_its_start_opens_it_under_its_prompt() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    release_fan_out(&session, fetch(serde_json::json!({"a": 1})), "wf-1", &prompt("p1"))
        .await
        .expect("the declared fan-out releases");
    assert!(matches!(
        session.open_late(child("c1"), None),
        Err(EventError::SpawnNotTaken)
    ));
    assert!(matches!(
        session.open_late(child("c1"), Some(&prompt("p1"))),
        Ok(LateOpen::Opened)
    ));
    assert!(opened(&runtime, &child("c1")));
}

/// A child that returns while one of its calls is still open is refused, and the refusal
/// closes none of its calls.
#[tokio::test]
async fn a_return_beside_an_open_child_call_is_refused_and_leaves_it_open() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    release_fan_out(&session, fetch(serde_json::json!({"a": 1})), "wf-1", &prompt("p1"))
        .await
        .expect("the declared fan-out releases");
    let c1 = session
        .on_child_start(child("c1"), SpawnRef::FanOut(prompt("p1")))
        .expect("the child binds");
    c1.on_tool_call_identified(
        fetch(serde_json::json!({"a": 2})),
        Some("c1-call".to_string()),
        None,
        None,
    )
    .await
    .expect("the child's call releases");
    assert!(matches!(
        c1.on_child_return("{}".to_string()).await,
        Err(EventError::ChildDispatchOpen)
    ));
    assert!(
        dispatch_open(&runtime, &child("c1")),
        "a return closes none of the child's calls"
    );
}

#[tokio::test]
async fn a_spawn_resume_keeps_the_original_dispatch_and_binding() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    release_spawn(&mut session, fetch(serde_json::json!({"a": 1}))).await;
    session
        .on_child_start(child("c1"), SpawnRef::InFlight)
        .expect("the child opens");
    let dispatches = runtime.open_dispatches(&root(), &root());
    session
        .on_spawn_resume(fetch(serde_json::json!({"a": 1})), child("c1"))
        .expect("same call resumes");
    assert_eq!(runtime.open_dispatches(&root(), &root())[0].id, dispatches[0].id);
    assert_eq!(fork_opened_count(&runtime), 1);
    assert_eq!(resumed_count(&runtime), 1);
}

#[tokio::test]
async fn a_spawn_resume_refuses_mismatched_or_finished_work_without_mutation() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    release_spawn(&mut session, fetch(serde_json::json!({"a": 1}))).await;
    let before = runtime.log_basis(&root());
    assert!(
        session
            .on_spawn_resume(fetch(serde_json::json!({"a": 1})), child("c1"))
            .is_err()
    );
    assert_eq!(runtime.log_basis(&root()), before, "an unbound fork cannot resume");
    let child_session = session
        .on_child_start(child("c1"), SpawnRef::InFlight)
        .expect("the child opens");
    for (call, target) in [
        (fetch(serde_json::json!({"a": 2})), child("c1")),
        (fetch(serde_json::json!({"a": 1})), child("other")),
        (leak(), child("c1")),
    ] {
        let before = runtime.log_basis(&root());
        assert!(session.on_spawn_resume(call, target).is_err());
        assert_eq!(runtime.log_basis(&root()), before);
    }
    child_session.on_child_end(None).await.expect("the child ends");
    let before = runtime.log_basis(&root());
    assert!(
        session
            .on_spawn_resume(fetch(serde_json::json!({"a": 1})), child("c1"))
            .is_err()
    );
    assert_eq!(runtime.log_basis(&root()), before, "ended child cannot resume");
    session.on_turn_end().await.expect("dispatch abandoned");
    let before = runtime.log_basis(&root());
    assert!(
        session
            .on_spawn_resume(fetch(serde_json::json!({"a": 1})), child("c1"))
            .is_err()
    );
    assert_eq!(runtime.log_basis(&root()), before, "closed dispatch cannot resume");
}

#[tokio::test]
async fn a_child_start_repeats_as_the_same_act_and_refuses_any_other_pairing() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    let binding = release_spawn(&mut session, fetch(serde_json::json!({"a": 1}))).await;
    let mut first = session
        .on_child_start(child("c1"), SpawnRef::Binding(binding.clone()))
        .expect("the fork binds and the child opens");
    let after_open = runtime.log_basis(&root());
    assert_eq!(fork_opened_count(&runtime), 1);

    session
        .on_child_start(child("c1"), SpawnRef::Binding(binding.clone()))
        .expect("the same binding again is the same act");
    session
        .on_child_start(child("c1"), SpawnRef::InFlight)
        .expect("the same child named as the spawn in flight is the same act");
    assert_ne!(
        runtime.log_basis(&root()),
        after_open,
        "a repeated start resumes the child under the parent's current label"
    );
    assert_eq!(resumed_count(&runtime), 2, "each repeated start is one resume");
    assert_eq!(fork_opened_count(&runtime), 1);

    let error = session
        .on_child_start(child("c2"), SpawnRef::Binding(binding.clone()))
        .err()
        .expect("a bound fork takes no second child");
    assert!(matches!(error, EventError::BindingMismatch), "got {error:?}");
    assert!(!error.is_operational());
    assert!(!opened(&runtime, &child("c2")), "the refused start opens no trajectory");

    let inner = release_spawn(&mut first, fetch(serde_json::json!({"a": 2}))).await;
    let error = session
        .on_child_start(child("c1"), SpawnRef::Binding(inner))
        .err()
        .expect("a bound child takes no second fork");
    assert!(matches!(error, EventError::BindingMismatch), "got {error:?}");

    for elsewhere in ["cc:root", "cc:elsewhere"] {
        let error = session
            .on_child_start(
                child("c3"),
                SpawnRef::Binding(crate::api::testing::spawn_binding(elsewhere)),
            )
            .err()
            .expect("an unprepared fork opens no child");
        assert!(matches!(error, EventError::SpawnNotTaken), "got {error:?}");
        assert!(!error.is_operational());
    }
    assert!(!opened(&runtime, &child("c3")));

    assert_eq!(fork_opened_count(&runtime), 1, "no refusal bound anything");
    assert!(
        dispatch_open(&runtime, &root()),
        "the spawn dispatch stays open through every refusal"
    );
}

#[tokio::test]
async fn a_binding_naming_the_prepared_fork_under_another_seal_opens_no_child() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    let issued = release_spawn(&mut session, fetch(serde_json::json!({"a": 1}))).await;
    let fork = runtime
        .log_facts(&root())
        .into_iter()
        .find_map(|fact| match fact {
            appa_engine::fact::Fact::ForkPrepared { fork, .. } => Some(fork),
            _ => None,
        })
        .expect("the release prepared a fork");

    for seal in [&[0u8; 32][..], &rand::random::<[u8; 32]>()[..], &[]] {
        let error = session
            .on_child_start(
                child("c1"),
                SpawnRef::Binding(crate::engine::sealed_binding(&fork, seal)),
            )
            .err()
            .expect("a binding the runtime did not issue opens no child");
        assert!(matches!(error, EventError::SpawnNotTaken), "got {error:?}");
    }
    assert!(!opened(&runtime, &child("c1")));

    session
        .on_child_start(child("c1"), SpawnRef::Binding(issued))
        .expect("the issued binding opens the child");
    assert_eq!(fork_opened_count(&runtime), 1);
}

#[tokio::test]
async fn an_issued_binding_survives_a_runtime_restart() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let db = dir.path().join("appa.db");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), db.clone(), None).expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    let binding = release_spawn(&mut session, fetch(serde_json::json!({"a": 1}))).await;
    drop(session);
    drop(runtime);

    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), db, None).expect("the deployment reopens");
    let session = runtime.session(&root(), &root()).expect("the session reattaches");
    session
        .on_child_start(child("c1"), SpawnRef::Binding(binding))
        .expect("the binding issued before the restart opens the child");
    assert!(opened(&runtime, &child("c1")));
}

#[tokio::test]
async fn two_concurrent_identical_starts_open_one_child() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    let binding = release_spawn(&mut session, fetch(serde_json::json!({"a": 1}))).await;

    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let starters: Vec<_> = (0..2)
            .map(|_| {
                let handle = runtime.session(&root(), &root()).expect("the root reopens");
                let binding = binding.clone();
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    handle.on_child_start(child("c1"), SpawnRef::Binding(binding))
                })
            })
            .collect();
        for starter in starters {
            starter
                .join()
                .expect("the starter thread joins")
                .expect("both identical starts pass");
        }
    });
    assert_eq!(fork_opened_count(&runtime), 1, "one binding landed");
    assert!(opened(&runtime, &child("c1")));
}

#[tokio::test]
async fn a_start_naming_no_spawn_binds_the_one_fork_in_flight() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");

    let error = session
        .on_child_start(child("c0"), SpawnRef::InFlight)
        .err()
        .expect("no spawn in flight opens no child");
    assert!(matches!(error, EventError::SpawnNotTaken), "got {error:?}");
    assert!(!opened(&runtime, &child("c0")), "the refused start opens no trajectory");

    let binding = release_spawn(&mut session, fetch(serde_json::json!({"a": 1}))).await;
    session
        .on_child_start(child("c1"), SpawnRef::InFlight)
        .expect("the one spawn in flight binds");
    assert!(opened(&runtime, &child("c1")));
    session
        .on_child_start(child("c1"), SpawnRef::Binding(binding))
        .expect("the echoed binding names the fork the in-flight start bound");
    assert_eq!(fork_opened_count(&runtime), 1);

    let error = session
        .on_child_start(child("c2"), SpawnRef::InFlight)
        .err()
        .expect("a bound fork is not in flight");
    assert!(matches!(error, EventError::SpawnNotTaken), "got {error:?}");
}

#[tokio::test]
async fn a_second_unbound_spawn_is_refused_across_the_family() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    release_spawn(&mut session, fetch(serde_json::json!({"a": 1}))).await;
    let mut first = session
        .on_child_start(child("c1"), SpawnRef::InFlight)
        .expect("the child opens");
    session
        .on_tool_result(
            fetch(serde_json::json!({"a": 1})),
            ToolOutcome::Success {
                body: OutcomeBody::Unavailable,
            },
        )
        .await
        .expect("the spawn dispatch closes");

    release_spawn(&mut first, fetch(serde_json::json!({"a": 2}))).await;
    let second = authorize_spawn(
        &session,
        fetch(serde_json::json!({"a": 3})),
        None,
        crate::engine::LabelSpelling::default(),
    )
    .await;
    let error = session
        .on_tool_call_identified(
            second.proposed(),
            Some("spawn-2".to_string()),
            Some(SpawnKind::Single),
            None,
        )
        .await
        .expect_err("another unbound spawn would make SubagentStart ambiguous");
    assert!(matches!(error, EventError::SpawnOutstanding), "got {error:?}");

    session
        .on_child_start(child("c2"), SpawnRef::InFlight)
        .expect("the one remaining spawn binds unambiguously");
    assert!(opened(&runtime, &child("c2")));
    assert_eq!(fork_opened_count(&runtime), 2);
}

#[tokio::test]
async fn a_fork_whose_parent_ended_is_unbindable_and_not_in_flight() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    release_spawn(&mut session, fetch(serde_json::json!({"a": 1}))).await;
    let mut first = session
        .on_child_start(child("c1"), SpawnRef::InFlight)
        .expect("the child opens");
    let orphaned = release_spawn(&mut first, fetch(serde_json::json!({"a": 2}))).await;
    first
        .on_spawn_result(
            fetch(serde_json::json!({"a": 2})),
            ToolOutcome::Indeterminate,
            None,
            None,
        )
        .await
        .expect("the nested spawn dispatch closes, its fork still prepared");
    assert!(!dispatch_open(&runtime, &child("c1")));
    first.on_child_end(None).await.expect("the child ends with no return");

    let error = session
        .on_child_start(child("orphan"), SpawnRef::Binding(orphaned))
        .err()
        .expect("an ended parent's fork binds nothing");
    assert!(matches!(error, EventError::SpawnNotTaken), "got {error:?}");
    assert!(!error.is_operational());
    assert!(!opened(&runtime, &child("orphan")));

    session
        .on_spawn_result(
            fetch(serde_json::json!({"a": 1})),
            agent_response(),
            Some(child("c1")),
            None,
        )
        .await
        .expect("the ended child's spawn closes with nothing to cross");
    release_spawn(&mut session, fetch(serde_json::json!({"a": 3}))).await;
    session
        .on_child_start(child("c2"), SpawnRef::InFlight)
        .expect("the one live fork in flight binds");
    assert!(opened(&runtime, &child("c2")));
}

fn agent_response() -> ToolOutcome {
    ToolOutcome::Success {
        body: OutcomeBody::Available(r#"{"agentId":"c1","content":[{"type":"text","text":"all done"}]}"#.to_string()),
    }
}

#[tokio::test]
async fn an_indeterminate_spawn_result_closes_the_spawn_and_leaves_the_child_live() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    release_spawn(&mut session, fetch(serde_json::json!({"a": 1}))).await;
    session
        .on_child_start(child("c1"), SpawnRef::InFlight)
        .expect("the child opens");

    let decision = session
        .on_spawn_result(
            fetch(serde_json::json!({"a": 1})),
            ToolOutcome::Indeterminate,
            None,
            None,
        )
        .await
        .expect("an indeterminate result is an ordinary close");
    assert_eq!(decision, SpawnResultDecision::Outcome(ToolResultDecision::Keep));
    assert!(!dispatch_open(&runtime, &root()), "the spawn dispatch closed");
    assert!(runtime.live(&root(), &child("c1")).is_ok(), "the child stays live");
}

/// A harness reports a spawn call as failed, and names no child, when its
/// subagent ended before it returned: on a provider error before the
/// subagent's first request, or partway through its turn. The spawn
/// closes, the model reads whether the subagent started and its next
/// step, and the parent can spawn again.
#[tokio::test]
async fn a_failed_spawn_result_naming_no_child_closes_the_spawn_with_the_next_step() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    let failure = || ToolOutcome::Failure {
        message: "Invalid LLM OAuth client access token.".to_string(),
    };

    // The subagent never started.
    release_spawn(&mut session, fetch(serde_json::json!({"a": 1}))).await;
    let error = session
        .on_spawn_result(fetch(serde_json::json!({"a": 1})), failure(), None, None)
        .await
        .expect_err("a failed spawn crosses nothing");
    assert!(matches!(error, EventError::SpawnNeverStarted), "got {error:?}");
    assert!(!error.is_operational());
    assert_eq!(error.class(), appa_runtime_api::RefusalClass::Spawn);
    assert!(!dispatch_open(&runtime, &root()), "the spawn dispatch closed");

    // The subagent started, then stopped. A spawn still in flight would refuse this one.
    release_spawn(&mut session, fetch(serde_json::json!({"a": 2}))).await;
    session
        .on_child_start(child("c1"), SpawnRef::InFlight)
        .expect("the child opens");
    let error = session
        .on_spawn_result(fetch(serde_json::json!({"a": 2})), failure(), None, None)
        .await
        .expect_err("a failed spawn crosses nothing");
    assert!(matches!(error, EventError::SpawnStopped), "got {error:?}");
    assert!(
        error
            .to_string()
            .starts_with("the subagent stopped before it returned a result"),
        "the model reads that the subagent stopped: {error}"
    );
    assert!(!dispatch_open(&runtime, &root()), "the spawn dispatch closed");
    release_spawn(&mut session, fetch(serde_json::json!({"a": 3}))).await;
}

/// A harness that launches the subagent and returns at once reports
/// the launch as the spawn call's ordinary tool result. That closes
/// the spawn like any call; the child, bound at its start, runs on
/// and returns through its own end.
#[tokio::test]
async fn a_launch_acknowledgement_closes_the_spawn_and_the_child_returns_on_its_own() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    release_spawn(&mut session, fetch(serde_json::json!({"a": 1}))).await;
    let child_session = session
        .on_child_start(child("c1"), SpawnRef::InFlight)
        .expect("the child opens");

    let decision = session
        .on_tool_result(
            fetch(serde_json::json!({"a": 1})),
            ToolOutcome::Success {
                body: OutcomeBody::Available("launched".to_string()),
            },
        )
        .await
        .expect("the launch acknowledgement is an ordinary result");
    assert_eq!(decision, ToolResultDecision::Keep);
    assert!(!dispatch_open(&runtime, &root()), "the spawn dispatch closed");
    assert!(runtime.live(&root(), &child("c1")).is_ok(), "the child stays live");

    session
        .on_tool_call(fetch(serde_json::json!({"a": 3})), false)
        .await
        .expect("the parent proposes while the child runs");
    let returned = child_session
        .on_child_end(Some("done".to_string()))
        .await
        .expect("the child's end crosses");
    assert_eq!(
        returned,
        ChildReturnDecision::Returned {
            value: "done".to_string()
        }
    );
    assert!(
        runtime.live(&root(), &child("c1")).is_ok(),
        "a return leaves the child live"
    );
}

#[tokio::test]
async fn a_spawn_result_for_a_returned_child_replays_its_return_and_withholds_another() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    release_spawn(&mut session, fetch(serde_json::json!({"a": 1}))).await;
    let first = session
        .on_child_start(child("c1"), SpawnRef::InFlight)
        .expect("the child opens");
    first
        .on_child_end(Some("all done".to_string()))
        .await
        .expect("the child's own end crosses");
    assert!(dispatch_open(&runtime, &root()), "the spawn dispatch is still open");
    let before = runtime.log_facts(&root()).len();

    let decision = session
        .on_spawn_result(
            fetch(serde_json::json!({"a": 1})),
            agent_response(),
            Some(child("c1")),
            Some("all done".to_string()),
        )
        .await
        .expect("the same message replays");
    assert_eq!(
        decision,
        SpawnResultDecision::Return(ChildReturnDecision::Returned {
            value: "all done".to_string()
        }),
    );
    assert!(!dispatch_open(&runtime, &root()), "the spawn dispatch closed");
    let facts = runtime.log_facts(&root());
    assert!(
        facts[before..]
            .iter()
            .all(|fact| !matches!(fact, appa_engine::fact::Fact::ChildReturn { .. })),
        "the replayed return crossed nothing twice",
    );

    release_spawn(&mut session, fetch(serde_json::json!({"a": 3}))).await;
    let second = session
        .on_child_start(child("c2"), SpawnRef::InFlight)
        .expect("a second child opens");
    second
        .on_child_end(Some("the first message".to_string()))
        .await
        .expect("the child's own end crosses");
    let decision = session
        .on_spawn_result(
            fetch(serde_json::json!({"a": 3})),
            agent_response(),
            Some(child("c2")),
            Some("a second message".to_string()),
        )
        .await
        .expect("a message the child never returned is withheld");
    assert!(
        matches!(
            decision,
            SpawnResultDecision::Return(ChildReturnDecision::Blocked { .. })
        ),
        "got {decision:?}"
    );
    assert!(!dispatch_open(&runtime, &root()), "the spawn dispatch closed");
    assert!(
        runtime.live(&root(), &child("c2")).is_ok(),
        "the child is still live to return again"
    );
}

#[tokio::test]
async fn a_spawn_result_naming_another_child_crosses_nothing_and_closes_the_spawn() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    release_spawn(&mut session, fetch(serde_json::json!({"a": 1}))).await;
    session
        .on_child_start(child("c1"), SpawnRef::InFlight)
        .expect("the child opens");

    let error = session
        .on_spawn_result(
            fetch(serde_json::json!({"a": 1})),
            agent_response(),
            Some(child("intruder")),
            Some("all done".to_string()),
        )
        .await
        .expect_err("another child's message does not cross");
    assert!(matches!(error, EventError::BindingMismatch), "got {error:?}");
    assert!(!error.is_operational());
    assert!(
        !dispatch_open(&runtime, &root()),
        "the spawn dispatch closed as a failure"
    );
    assert!(
        runtime.live(&root(), &child("c1")).is_ok(),
        "the bound child stays live"
    );
    assert!(
        !opened(&runtime, &child("intruder")),
        "the named child was never opened"
    );

    release_spawn(&mut session, fetch(serde_json::json!({"a": 2}))).await;
    session
        .on_child_start(child("c2"), SpawnRef::InFlight)
        .expect("a second child opens");
    let error = session
        .on_spawn_result(fetch(serde_json::json!({"a": 2})), agent_response(), None, None)
        .await
        .expect_err("a result naming no child crosses nothing");
    assert!(matches!(error, EventError::BindingMismatch), "got {error:?}");
    assert!(!dispatch_open(&runtime, &root()));
}

/// A spawn result naming a child the family never saw start binds it there,
/// as the harness's own bind by agent id at the spawn's return.
#[tokio::test]
async fn a_spawn_result_naming_an_unstarted_child_binds_it_and_closes_the_spawn() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    let binding = release_spawn(&mut session, fetch(serde_json::json!({"a": 1}))).await;

    let decision = session
        .on_spawn_result(
            fetch(serde_json::json!({"a": 1})),
            agent_response(),
            Some(child("c1")),
            None,
        )
        .await
        .expect("the spawn result binds the child and closes");
    assert_eq!(decision, SpawnResultDecision::Outcome(ToolResultDecision::Keep));
    assert!(!dispatch_open(&runtime, &root()), "the spawn dispatch closed");
    assert!(opened(&runtime, &child("c1")), "the spawn result opened the child");
    assert!(runtime.live(&root(), &child("c1")).is_ok(), "the child is live");

    session
        .on_child_start(child("c1"), SpawnRef::Binding(binding))
        .expect("the late start is the same act");
    let error = session
        .on_child_start(child("c2"), SpawnRef::InFlight)
        .err()
        .expect("nothing else is in flight");
    assert!(matches!(error, EventError::SpawnNotTaken), "got {error:?}");
    assert!(!opened(&runtime, &child("c2")), "no second child opened");
}
#[tokio::test]
async fn a_spawn_result_on_an_unforked_call_is_an_ordinary_outcome() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    session
        .on_tool_call(fetch(serde_json::json!({"a": 1})), false)
        .await
        .expect("the ordinary call releases");
    let decision = session
        .on_spawn_result(
            fetch(serde_json::json!({"a": 1})),
            ToolOutcome::Success {
                body: OutcomeBody::Available("data".to_string()),
            },
            None,
            None,
        )
        .await
        .expect("the ordinary result is admitted");
    assert_eq!(decision, SpawnResultDecision::Outcome(ToolResultDecision::Keep));
    assert!(!dispatch_open(&runtime, &root()));

    let error = session
        .on_spawn_result(fetch(serde_json::json!({"a": 1})), agent_response(), None, None)
        .await
        .expect_err("no open dispatch takes the result");
    assert!(matches!(error, EventError::UnknownDispatch), "got {error:?}");
}

/// The spawn result carries a message the child never returned at a stop: the
/// harness ended the child itself and delivered what it had. Nothing crosses, and
/// the spawn closes with no value.
#[tokio::test]
async fn a_spawn_result_carrying_an_unreturned_message_withholds_it_and_closes_the_spawn() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    release_spawn(&mut session, fetch(serde_json::json!({}))).await;
    session
        .on_child_start(child("c1"), SpawnRef::InFlight)
        .expect("the child opens");

    let decision = session
        .on_spawn_result(
            fetch(serde_json::json!({})),
            agent_response(),
            Some(child("c1")),
            Some("never returned at a stop".to_string()),
        )
        .await
        .expect("the withheld message is delivered");
    assert!(
        matches!(
            decision,
            SpawnResultDecision::Return(ChildReturnDecision::Blocked { .. })
        ),
        "got {decision:?}",
    );
    assert!(!dispatch_open(&runtime, &root()), "the spawn dispatch closed");
    assert!(
        runtime.status(&root()).expect("the root answers").trust == "trusted",
        "nothing crossed into the parent"
    );
    session
        .on_tool_call(fetch(serde_json::json!({})), false)
        .await
        .expect("the parent proposes again");
}

/// A subagent that stops with a call released and unreported gets no
/// outcome hook for it; its end closes the call as unreported, as
/// its turn end would, and the return is judged on what the child
/// admitted.
#[tokio::test]
async fn a_childs_end_closes_its_unreported_call_and_returns() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    let child_session = open_child(&mut session, fetch(serde_json::json!({"a": 1})), child("c1")).await;
    child_session
        .on_tool_call(fetch(serde_json::json!({"a": 2})), false)
        .await
        .expect("the child's call releases");
    assert!(dispatch_open(&runtime, &child("c1")), "the child's dispatch is open");

    let decision = child_session
        .on_child_end(Some("done".to_string()))
        .await
        .expect("the end closes the call and crosses");
    assert_eq!(
        decision,
        ChildReturnDecision::Returned {
            value: "done".to_string()
        }
    );
    assert!(
        runtime.log_facts(&root()).iter().any(|fact| matches!(
            fact,
            appa_engine::fact::Fact::DispatchClosed {
                trajectory,
                outcome: appa_engine::fact::CloseOutcome::Indeterminate,
                ..
            } if trajectory.as_str() == child("c1").0
        )),
        "the child's call closed as unreported",
    );
    assert!(
        !dispatch_open(&runtime, &child("c1")),
        "nothing stays open on the child"
    );
    assert!(
        runtime.live(&root(), &child("c1")).is_ok(),
        "a return ends nothing: the child may be addressed again"
    );
}

/// A harness may deliver the same stop twice, and a child may stop
/// again with something new. The latest crossed value answers again
/// without a second crossing; another value is a crossing of its own;
/// an empty stop ends the child.
#[tokio::test]
async fn a_second_end_of_a_returned_child_replays_the_same_value_and_crosses_another() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    let child_session = open_child(&mut session, fetch(serde_json::json!({"a": 1})), child("c1")).await;
    child_session
        .on_child_end(Some("done".to_string()))
        .await
        .expect("the first end crosses");
    let before = runtime.log_basis(&root());

    let decision = child_session
        .on_child_end(Some("done".to_string()))
        .await
        .expect("the same end answers again");
    assert_eq!(
        decision,
        ChildReturnDecision::Returned {
            value: "done".to_string()
        }
    );
    assert_eq!(runtime.log_basis(&root()), before, "the replay appended nothing");

    let decision = child_session
        .on_child_end(Some("something else".to_string()))
        .await
        .expect("a different return crosses on its own");
    assert_eq!(
        decision,
        ChildReturnDecision::Returned {
            value: "something else".to_string()
        }
    );
    assert_ne!(runtime.log_basis(&root()), before, "the second crossing appended");

    assert_eq!(
        child_session
            .on_child_end(None)
            .await
            .expect("an empty stop ends the child"),
        ChildReturnDecision::NoValue
    );
    assert!(matches!(
        runtime.live(&root(), &child("c1")),
        Err(EventError::TrajectoryEnded)
    ));
}

/// The harness refused the released call at its permission prompt,
/// so no outcome hook fires for it. Without the turn end that
/// dispatch would refuse every later proposal for the life of the
/// trajectory.
#[tokio::test]
async fn a_turn_end_closes_the_call_the_harness_never_ran() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    assert!(matches!(
        session
            .on_tool_call(fetch(serde_json::json!({"a": 1})), false)
            .await
            .expect("the call releases"),
        ToolCallDecision::Allow { spawn: None, .. }
    ));
    assert!(matches!(
        session.on_tool_call(fetch(serde_json::json!({"a": 2})), false).await,
        Err(EventError::CallOutstanding),
    ));

    session.on_turn_end().await.expect("the turn end closes it");

    assert!(runtime.open_dispatches(&root(), &root()).is_empty());
    assert!(
        runtime
            .audit(&root())
            .expect("the audit reads")
            .iter()
            .any(|entry| matches!(
                &entry.event,
                crate::engine::AuditEvent::Closed {
                    outcome: crate::engine::DispatchOutcome::Unknown
                }
            )),
        "the unreported dispatch closed as unknown, not as a run that failed"
    );
    assert!(matches!(
        session
            .on_tool_call(fetch(serde_json::json!({"a": 2})), false)
            .await
            .expect("the next turn proposes freely"),
        ToolCallDecision::Allow { spawn: None, .. }
    ));
}

#[tokio::test]
async fn a_turn_end_closes_all_parallel_calls_the_harness_never_ran() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    for (call_id, argument) in [("toolu-1", 1), ("toolu-2", 2)] {
        session
            .on_tool_call_identified(
                fetch(serde_json::json!({"a": argument})),
                Some(call_id.to_string()),
                None,
                None,
            )
            .await
            .expect("the identified call releases");
    }
    assert_eq!(runtime.open_dispatches(&root(), &root()).len(), 2);

    session.on_turn_end().await.expect("the turn end closes every call");

    assert!(runtime.open_dispatches(&root(), &root()).is_empty());
    let unknown_closes = runtime
        .audit(&root())
        .expect("the audit reads")
        .iter()
        .filter(|entry| {
            matches!(
                entry.event,
                crate::engine::AuditEvent::Closed {
                    outcome: crate::engine::DispatchOutcome::Unknown
                }
            )
        })
        .count();
    assert_eq!(unknown_closes, 2);
}

/// The outcome the close ruled out cannot be reported afterwards:
/// a call the log says did not run admits no value.
#[tokio::test]
async fn an_outcome_reported_after_its_turn_end_is_refused() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    session
        .on_tool_call(fetch(serde_json::json!({"a": 1})), false)
        .await
        .expect("the call releases");
    session.on_turn_end().await.expect("the turn end closes it");

    let error = session
        .on_tool_result(
            fetch(serde_json::json!({"a": 1})),
            ToolOutcome::Success {
                body: OutcomeBody::Available("the run did happen".to_string()),
            },
        )
        .await
        .expect_err("the closed dispatch takes no outcome");
    assert!(matches!(error, EventError::UnknownDispatch), "got {error:?}");
}

/// Release a declared single spawn under the harness's `call_id`.
async fn release_identified_spawn(session: &Session, spawn: ProposedCall, call_id: &str) {
    let call = authorize_spawn(session, spawn, None, crate::engine::LabelSpelling::default()).await;
    assert!(matches!(
        session
            .on_tool_call_identified(
                call.proposed(),
                Some(call_id.to_string()),
                Some(SpawnKind::Single),
                None
            )
            .await
            .expect("the approved spawn releases"),
        ToolCallDecision::Allow { spawn: Some(_), .. }
    ));
}

/// A background subagent works past its parent's turn. The turn end and the settle at
/// the next prompt's first call leave its spawn open, so the return it crosses later
/// reaches the parent through the spawn's result.
#[tokio::test]
async fn a_turn_end_leaves_a_running_subagents_spawn_open_for_its_return() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    release_identified_spawn(&session, fetch(serde_json::json!({"a": 1})), "spawn-1").await;
    let subagent = session
        .on_child_start(child("c1"), SpawnRef::InFlight)
        .expect("the child opens");

    session.on_turn_end().await.expect("the turn end acks");
    session.on_turn_end().await.expect("the next prompt's settle acks");
    assert!(
        dispatch_open(&runtime, &root()),
        "the running subagent's spawn is still open"
    );
    session
        .on_tool_call_identified(
            fetch(serde_json::json!({"a": 2})),
            Some("call-2".to_string()),
            None,
            None,
        )
        .await
        .expect("the parent proposes while the subagent runs");

    subagent
        .on_child_end(Some("all done".to_string()))
        .await
        .expect("the subagent's return crosses");
    let decision = session
        .on_spawn_result_identified(
            fetch(serde_json::json!({"a": 1})),
            Some("spawn-1".to_string()),
            agent_response(),
            Some(child("c1")),
            Some("all done".to_string()),
        )
        .await
        .expect("the spawn's result is reported after the turn");
    assert_eq!(
        decision,
        SpawnResultDecision::Return(ChildReturnDecision::Returned {
            value: "all done".to_string()
        }),
    );
    assert!(
        runtime.open_dispatches(&root(), &root()).len() == 1,
        "the spawn closed; the parent's own call is still open"
    );
}

/// A call the harness does not identify is still told apart from a spawn whose child
/// runs on past the turn: that spawn's result comes under its own call id. The call
/// opens beside it and reports, and the spawn's result still replays the return.
#[tokio::test]
async fn a_call_without_an_id_runs_beside_a_running_subagents_spawn() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    release_identified_spawn(&session, fetch(serde_json::json!({"a": 1})), "spawn-1").await;
    let subagent = session
        .on_child_start(child("c1"), SpawnRef::InFlight)
        .expect("the child opens");
    session.on_turn_end().await.expect("the turn end acks");

    assert!(matches!(
        session
            .on_tool_call(fetch(serde_json::json!({"a": 2})), false)
            .await
            .expect("the call releases beside the spawn"),
        ToolCallDecision::Allow { spawn: None, .. }
    ));
    assert_eq!(
        session
            .on_tool_result(
                fetch(serde_json::json!({"a": 2})),
                ToolOutcome::Success {
                    body: OutcomeBody::Available("body".to_string()),
                },
            )
            .await
            .expect("the report names the call, not the spawn"),
        ToolResultDecision::Keep
    );
    assert_eq!(
        runtime.open_dispatches(&root(), &root()).len(),
        1,
        "only the spawn is still open"
    );

    subagent
        .on_child_end(Some("all done".to_string()))
        .await
        .expect("the subagent's return crosses");
    assert_eq!(
        session
            .on_spawn_result_identified(
                fetch(serde_json::json!({"a": 1})),
                Some("spawn-1".to_string()),
                agent_response(),
                Some(child("c1")),
                Some("all done".to_string()),
            )
            .await
            .expect("the spawn's result replays"),
        SpawnResultDecision::Return(ChildReturnDecision::Returned {
            value: "all done".to_string()
        }),
    );
}

/// A resume names its spawn by the call alone, with no call id. It still reaches an
/// identified spawn that a turn end left open for its running child.
#[tokio::test]
async fn a_resume_reaches_an_identified_spawn_left_open_for_its_running_child() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    release_identified_spawn(&session, fetch(serde_json::json!({"a": 1})), "spawn-1").await;
    session
        .on_child_start(child("c1"), SpawnRef::InFlight)
        .expect("the child opens");
    session.on_turn_end().await.expect("the turn end acks");

    session
        .on_spawn_resume(fetch(serde_json::json!({"a": 1})), child("c1"))
        .expect("the resume binds the same pair again");
    assert!(
        dispatch_open(&runtime, &root()),
        "the spawn is still open for its result"
    );
}

/// A resume picks its spawn by the child it names, so another parent call open beside
/// the running subagent's spawn does not hide it.
#[tokio::test]
async fn a_resume_reaches_its_spawn_beside_another_open_parent_call() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    release_identified_spawn(&session, fetch(serde_json::json!({"a": 1})), "spawn-1").await;
    session
        .on_child_start(child("c1"), SpawnRef::InFlight)
        .expect("the child opens");
    session.on_turn_end().await.expect("the turn end acks");
    session
        .on_tool_call_identified(
            fetch(serde_json::json!({"a": 2})),
            Some("call-2".to_string()),
            None,
            None,
        )
        .await
        .expect("the parent proposes while the subagent runs");

    session
        .on_spawn_resume(fetch(serde_json::json!({"a": 1})), child("c1"))
        .expect("the resume finds the spawn bound to its child");
    assert!(matches!(
        session.on_spawn_resume(fetch(serde_json::json!({"a": 9})), child("c1")),
        Err(EventError::OutcomeMismatch)
    ));
    assert_eq!(
        runtime.open_dispatches(&root(), &root()).len(),
        2,
        "the spawn and the other call are still open"
    );
}

/// An identified spawn stays out of a report without a call id for its whole open life:
/// after its child ends, a call opened beside it without an id still reports, and the
/// spawn's own result still closes it.
#[tokio::test]
async fn a_report_without_an_id_skips_an_identified_spawn_whose_child_ended() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    release_identified_spawn(&session, fetch(serde_json::json!({"a": 1})), "spawn-1").await;
    let subagent = session
        .on_child_start(child("c1"), SpawnRef::InFlight)
        .expect("the child opens");
    assert!(matches!(
        session
            .on_tool_call(fetch(serde_json::json!({"a": 2})), false)
            .await
            .expect("the call releases beside the spawn"),
        ToolCallDecision::Allow { spawn: None, .. }
    ));
    assert_eq!(
        subagent.on_child_end(None).await.expect("an empty stop ends the child"),
        ChildReturnDecision::NoValue
    );

    assert_eq!(
        session
            .on_tool_result(
                fetch(serde_json::json!({"a": 2})),
                ToolOutcome::Success {
                    body: OutcomeBody::Available("body".to_string()),
                },
            )
            .await
            .expect("the report names the call, not the spawn"),
        ToolResultDecision::Keep
    );
    session
        .on_spawn_result_identified(
            fetch(serde_json::json!({"a": 1})),
            Some("spawn-1".to_string()),
            agent_response(),
            Some(child("c1")),
            None,
        )
        .await
        .expect("the ended child's spawn closes with nothing to cross");
    assert!(!dispatch_open(&runtime, &root()), "nothing is left open");
}

/// The turn end closes a spawn as unreported when its subagent is not running: one
/// that never started, one that ended, and one the harness did not identify.
#[tokio::test]
async fn a_turn_end_closes_a_spawn_whose_subagent_is_not_running() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");

    release_identified_spawn(&session, fetch(serde_json::json!({"a": 1})), "spawn-1").await;
    session
        .on_turn_end()
        .await
        .expect("the turn end closes the unstarted spawn");
    assert!(!dispatch_open(&runtime, &root()), "a spawn no child started closes");
    session
        .on_child_start(child("late"), SpawnRef::InFlight)
        .err()
        .expect("the closed spawn's fork binds nothing");

    release_identified_spawn(&session, fetch(serde_json::json!({"a": 2})), "spawn-2").await;
    session
        .on_child_start(child("c2"), SpawnRef::InFlight)
        .expect("the child opens")
        .on_child_end(None)
        .await
        .expect("the child ends with no return");
    session
        .on_turn_end()
        .await
        .expect("the turn end closes the ended child's spawn");
    assert!(!dispatch_open(&runtime, &root()), "an ended child's spawn closes");

    release_spawn(&mut session, fetch(serde_json::json!({"a": 3}))).await;
    session
        .on_child_start(child("c3"), SpawnRef::InFlight)
        .expect("the child opens");
    session
        .on_turn_end()
        .await
        .expect("the turn end closes the unidentified spawn");
    assert!(!dispatch_open(&runtime, &root()), "an unidentified spawn closes");
    assert!(
        runtime.live(&root(), &child("c3")).is_ok(),
        "closing the spawn leaves its child live"
    );
}

/// A turn end is not an engine event when nothing is outstanding,
/// which is every ordinary turn.
#[tokio::test]
async fn a_turn_end_over_a_settled_trajectory_appends_nothing() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    session
        .on_tool_call(fetch(serde_json::json!({"a": 1})), false)
        .await
        .expect("the call releases");
    session
        .on_tool_result(
            fetch(serde_json::json!({"a": 1})),
            ToolOutcome::Success {
                body: OutcomeBody::Available("body".to_string()),
            },
        )
        .await
        .expect("the outcome closes the dispatch");

    let settled = runtime.log_facts(&root()).len();
    session.on_turn_end().await.expect("the turn end acks");
    session.on_turn_end().await.expect("a repeat acks too");
    assert_eq!(runtime.log_facts(&root()).len(), settled, "no fact is appended");
}

/// A child carries its own dispatches; its turn end closes one it
/// abandoned, so a later end finds nothing to close.
#[tokio::test]
async fn a_child_turn_end_closes_its_abandoned_call_before_the_child_ends() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    let child_session = open_child(&mut session, fetch(serde_json::json!({"a": 1})), child("c1")).await;
    child_session
        .on_tool_call(fetch(serde_json::json!({"a": 2})), false)
        .await
        .expect("the child's call releases");

    child_session
        .on_turn_end()
        .await
        .expect("the child's turn end closes it");
    assert!(runtime.open_dispatches(&root(), &child("c1")).is_empty());
    let closed = runtime.log_facts(&root()).len();
    child_session
        .on_child_end(Some("done".to_string()))
        .await
        .expect("with nothing in flight the child ends");
    assert!(
        runtime.log_facts(&root())[closed..]
            .iter()
            .all(|fact| !matches!(fact, appa_engine::fact::Fact::DispatchClosed { .. })),
        "the end had nothing left to close",
    );
}

/// A child that stops with a derivation it never proposed owes nothing for it: no
/// dispatch stands, so the end closes no call and the return is judged on what the child
/// admitted.
#[tokio::test]
async fn a_childs_end_leaves_its_untaken_derivation_and_returns() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(
        substituting_config(SUBSTITUTED_SEND_FORKING, None),
        dir.path().join("appa.db"),
        None,
    )
    .expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    let mut child_session = open_child_floored(
        &mut session,
        fetch(serde_json::json!({"a": 1})),
        child("c1"),
        floor_audience(&["hr"]),
    )
    .await;
    let hop = narrowed_and_blocked_on(&runtime, &mut child_session, &child("c1")).await;
    assert!(matches!(
        child_session
            .on_remedy(hop, RemedyArguments::default(), None, None)
            .await,
        Ok(RemedyDecision::Authorized { .. }),
    ));
    assert!(
        runtime.open_dispatches(&root(), &child("c1")).is_empty(),
        "the hop staged a derivation and opened no call"
    );

    let before = runtime.log_facts(&root()).len();
    child_session
        .on_child_end(Some("done".to_string()))
        .await
        .expect("the end returns with nothing to close");
    assert!(
        runtime.log_facts(&root())[before..]
            .iter()
            .all(|fact| !matches!(fact, appa_engine::fact::Fact::DispatchClosed { .. })),
        "the end closed no call of the child's, because none stood",
    );
    assert!(
        runtime.live(&root(), &child("c1")).is_ok(),
        "a return leaves the child live"
    );
}

/// The child never stopped — a call of its own is still open — so the message the
/// spawn result carries was never checked at a stop: it is withheld and the spawn
/// closes with no value, the child's own call untouched.
#[tokio::test]
async fn a_spawn_result_for_a_child_with_a_call_open_withholds_its_message_and_closes_the_spawn() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let mut session = runtime.create_session(root(), None).expect("a fresh id opens");
    release_spawn(&mut session, fetch(serde_json::json!({"a": 1}))).await;
    let first = session
        .on_child_start(child("c1"), SpawnRef::InFlight)
        .expect("the child opens");
    first
        .on_tool_call(fetch(serde_json::json!({"a": 2})), false)
        .await
        .expect("the child's call releases");

    let decision = session
        .on_spawn_result(
            fetch(serde_json::json!({"a": 1})),
            agent_response(),
            Some(child("c1")),
            Some("all done".to_string()),
        )
        .await
        .expect("the unreturned message is withheld");
    assert!(
        matches!(
            decision,
            SpawnResultDecision::Return(ChildReturnDecision::Blocked { .. })
        ),
        "got {decision:?}"
    );
    assert!(
        runtime
            .log_facts(&root())
            .iter()
            .all(|fact| !matches!(fact, appa_engine::fact::Fact::ChildReturn { .. })),
        "nothing crossed",
    );
    assert!(!dispatch_open(&runtime, &root()), "the spawn dispatch closed");
    assert!(dispatch_open(&runtime, &child("c1")), "the child's dispatch stays open");
    assert!(runtime.live(&root(), &child("c1")).is_ok(), "the child stays live");
    session
        .on_tool_call(fetch(serde_json::json!({"a": 3})), false)
        .await
        .expect("the parent proposes again");
}
/// A loopback authority that answers the same ruling every time and
/// counts the requests it saw, so a test can pin how many
/// round-trips one event takes.
async fn counting_stub(answer: serde_json::Value) -> (String, Arc<AtomicUsize>) {
    use axum::routing::post;

    let seen = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&seen);
    let app = axum::Router::new().route(
        "/",
        post(move || {
            let answer = answer.clone();
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                axum::Json(serde_json::json!({"version": 1, "answer": answer}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback stub binds");
    let addr = listener.local_addr().expect("the stub has an address");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("the stub serves");
    });
    (format!("http://{addr}/"), seen)
}

fn open_runtime(dir: &tempfile::TempDir) -> Runtime {
    Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None).expect("the deployment opens")
}

fn only_the_opening(runtime: &Runtime) -> bool {
    matches!(
        runtime.log_facts(&root()).as_slice(),
        [appa_engine::fact::Fact::TrajectoryOpened(_)]
    )
}

#[test]
fn a_used_root_id_is_refused_and_a_persisted_one_reopens() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = open_runtime(&dir);
    runtime.create_session(root(), None).expect("a fresh id opens");
    assert!(matches!(
        runtime.create_session(root(), None),
        Err(EventError::TrajectoryExists),
    ));
    assert!(runtime.session(&root(), &root()).is_ok());
    assert!(matches!(
        runtime.session(
            &TrajectoryId("cc:ghost".to_string()),
            &TrajectoryId("cc:ghost".to_string())
        ),
        Err(EventError::UnknownTrajectory),
    ));
}

#[test]
fn a_damaged_database_is_refused_at_open() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let path = dir.path().join("appa.db");
    std::fs::write(&path, b"not a sqlite database at all").expect("the file writes");
    assert!(matches!(
        Runtime::open(config_with(FETCH_AND_SEND, None), path, None),
        Err(OpenError::Damaged(_)),
    ));
}

#[test]
fn a_session_from_an_archived_database_cannot_reopen() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let path = dir.path().join("appa.db");
    Runtime::open(config_with(FETCH_AND_SEND, None), path.clone(), None)
        .expect("the runtime opens")
        .create_session(root(), None)
        .expect("a fresh id opens");
    rusqlite::Connection::open(&path)
        .expect("the file reopens")
        .pragma_update(None, "user_version", 4)
        .expect("the version moves back past the upgrade chain");

    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), path, None)
        .expect("a database too old to upgrade is archived, not refused");
    assert!(matches!(
        runtime.create_session(root(), None),
        Err(EventError::RootArchived)
    ));
    runtime
        .create_session(TrajectoryId("cc:new".to_string()), None)
        .expect("a new session opens");
}

#[tokio::test]
async fn a_decision_whose_append_fails_never_acts() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = open_runtime(&dir);
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    runtime.store().fail_commit_after(0);
    assert!(matches!(
        session.on_tool_call(fetch(serde_json::json!({"a": 1})), false).await,
        Err(EventError::Storage(_)),
    ));
    assert!(only_the_opening(&runtime), "the killed append left nothing");
    assert!(
        runtime.open_dispatches(&root(), &root()).is_empty(),
        "a call whose release never committed is not open",
    );
}

#[tokio::test]
async fn a_lost_race_discards_the_decision_and_replays() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = open_runtime(&dir);
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    runtime.store().contend_next_appends(1);
    assert!(matches!(
        session
            .on_tool_call(fetch(serde_json::json!({"a": 1})), false)
            .await
            .expect("the replay commits"),
        ToolCallDecision::Allow { spawn: None, .. }
    ));
    assert_eq!(
        runtime.log_basis(&root()),
        3,
        "the opening, the foreign append, and one committed attempt",
    );
    assert_eq!(
        runtime.open_dispatches(&root(), &root()).len(),
        1,
        "the discarded attempt released nothing",
    );
}

#[tokio::test]
async fn a_permanently_contended_log_refuses_the_event() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = open_runtime(&dir);
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    runtime.store().contend_next_appends(REPLAY_LIMIT as u64);
    assert!(matches!(
        session.on_tool_call(fetch(serde_json::json!({"a": 1})), false).await,
        Err(EventError::Contended { attempts: REPLAY_LIMIT }),
    ));
    assert_eq!(runtime.log_basis(&root()), 1 + REPLAY_LIMIT as u64);
    assert!(
        runtime.open_dispatches(&root(), &root()).is_empty(),
        "no attempt of a refused event acted",
    );
}

fn control_call(name: &str) -> ProposedCall {
    ProposedCall {
        tool: name.to_string(),
        arguments: raw(serde_json::json!({"offer_id": "o1:cc:root:ff"})),
        cwd: None,
    }
}

#[tokio::test]
async fn a_lookalike_control_tool_is_an_undeclared_tool() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = open_runtime(&dir);
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    assert!(matches!(
        session
            .on_tool_call(control_call("mcp/evil/execute_remedy_plan"), false)
            .await,
        Err(EventError::UndeclaredTool { tool }) if tool == "mcp/evil/execute_remedy_plan",
    ));
}

#[test]
fn an_over_cap_success_body_is_carried_as_too_large() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = open_runtime(&dir);
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    let success = |len: usize| ToolOutcome::Success {
        body: OutcomeBody::Available("x".repeat(len)),
    };
    assert_eq!(
        session.cap_outcome(success(70_000)),
        ToolOutcome::Success {
            body: OutcomeBody::TooLarge {
                limit_bytes: Some(session.deployment().config.externals.max_body_bytes),
            },
        },
    );
    assert_eq!(session.cap_outcome(success(8)), success(8));
}

#[tokio::test]
async fn an_unknown_offer_is_refused() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = open_runtime(&dir);
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    assert!(matches!(
        session
            .on_remedy(
                OfferId("o1:cc:root:never".to_string()),
                RemedyArguments::default(),
                None,
                None
            )
            .await,
        Err(EventError::UnknownOffer),
    ));
    assert!(only_the_opening(&runtime), "a refused offer appends nothing");
}

#[tokio::test]
async fn an_external_answer_settles_the_event_in_one_round_trip() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, seen) = counting_stub(serde_json::json!({"ruling": "approve"})).await;
    let runtime = Runtime::open(config_with(ATTENTION, Some(&url)), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    assert!(matches!(
        session
            .on_tool_call(wire(500), false)
            .await
            .expect("the block is delivered"),
        ToolCallDecision::Deny { .. },
    ));
    assert_eq!(seen.load(Ordering::SeqCst), 0, "a proposal consults no authority");

    assert!(matches!(
        session
            .on_remedy(latest_offer(&runtime), RemedyArguments::default(), None, None)
            .await
            .expect("the approval is delivered"),
        RemedyDecision::Authorized { .. },
    ));
    assert_eq!(
        seen.load(Ordering::SeqCst),
        1,
        "the event re-drove once, carrying the answer it asked for",
    );
}

#[tokio::test]
async fn no_offer_id_repeats_within_a_trajectory() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(
        config_with(ATTENTION, Some("http://127.0.0.1:1/")),
        dir.path().join("appa.db"),
        None,
    )
    .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    for _ in 0..5 {
        assert!(matches!(
            session
                .on_tool_call(wire(500), false)
                .await
                .expect("the block is delivered"),
            ToolCallDecision::Deny { .. },
        ));
    }
    let minted = runtime.minted_offers(&root(), &root());
    let distinct: std::collections::HashSet<&str> = minted.iter().map(|offer| offer.0.as_str()).collect();
    assert!(minted.len() >= 5, "each block surfaced an offer: {minted:?}");
    assert_eq!(
        distinct.len(),
        minted.len(),
        "five identical proposals minted five distinct ids: {minted:?}",
    );
}

fn bash_call() -> ProposedCall {
    ProposedCall {
        tool: "Bash".to_string(),
        arguments: raw(serde_json::json!({"command": "ls"})),
        cwd: None,
    }
}

fn bash_dispatch(label: &str) -> appa_engine::value::DispatchId {
    let policy = appa_policy::Config::from_toml_str(
        r#"
                version = 2
                [[tool]]
                name = "Bash"
            "#,
    )
    .expect("the fixture policy compiles");
    let call = policy
        .engine()
        .resolve_call(appa_engine::value::ToolName::new("Bash"), br#"{"command":"ls"}"#)
        .expect("the fixture call resolves through the engine");
    appa_engine::value::DispatchId::new(appa_engine::value::TrajectoryId::new(label), call.digest(), 0)
}

#[test]
fn an_outcome_report_is_classified_against_the_open_dispatches() {
    let id = bash_dispatch("cc:root");
    let open = |tool: &str, bytes: &[u8]| OpenDispatch {
        id: id.clone(),
        tool: tool.to_string(),
        bytes: bytes.to_vec(),
    };
    let canonical = || Some(b"{}".to_vec());

    assert_eq!(
        classify_report(&bash_call(), canonical, &[]),
        Err(UnreportableOutcome::NoOpenDispatch),
    );
    assert_eq!(
        classify_report(&bash_call(), canonical, &[open("Bash", b"{}")]),
        Ok(id.clone()),
    );
    assert_eq!(
        classify_report(&bash_call(), canonical, &[open("Write", b"{}")]),
        Err(UnreportableOutcome::ByteMismatch),
        "another tool is another call",
    );
    assert_eq!(
        classify_report(&bash_call(), canonical, &[open("Bash", b"{\"other\":1}")]),
        Err(UnreportableOutcome::ByteMismatch),
        "other bytes are another occurrence",
    );
    assert_eq!(
        classify_report(&bash_call(), || None, &[open("Bash", b"{}")]),
        Err(UnreportableOutcome::ByteMismatch),
        "a call that cannot canonicalize matches nothing",
    );
    assert_eq!(
        classify_report(&bash_call(), canonical, &[open("Bash", b"{}"), open("Bash", b"{}")]),
        Err(UnreportableOutcome::NoOpenDispatch),
        "several open dispatches name no one occurrence",
    );
}

/// A host's transcriber: every record, in the order the consults settled.
#[derive(Default)]
struct Collected(std::sync::Mutex<Vec<crate::api::ConsultRecord>>);

impl crate::api::ConsultTranscriber for Collected {
    fn record(&self, record: crate::api::ConsultRecord) {
        self.0.lock().expect("the collector is never poisoned").push(record);
    }
}

impl Collected {
    fn taken(&self) -> Vec<crate::api::ConsultRecord> {
        std::mem::take(&mut *self.0.lock().expect("the collector is never poisoned"))
    }
}

const PERMISSIVE_ANSWER: &str =
    r#"{"version":1,"answer":{"delta":{},"requires":{"history":[],"attention":[]},"emits":[]}}"#;

/// Every call annotated by `gatekeeper`, bound as `binding`, in `dir` so a command's
/// script outlives the load.
fn annotated(dir: &std::path::Path, binding: &str) -> Config {
    let text = format!(
        "[policy]\nversion = 2\n\n[[policy.annotator]]\nname = \"gatekeeper\"\n\n\
             [[policy.tool]]\nname = \"*\"\nannotator = \"gatekeeper\"\n\n\
             [externals]\ntimeout_ms = 2000\nmax_body_bytes = 65536\n\n\
             [externals.annotators.gatekeeper]\n{binding}\n"
    );
    let path = dir.join("appa.toml");
    std::fs::write(&path, text).expect("the fixture writes");
    Config::load(&path).expect("the fixture validates")
}

#[tokio::test]
async fn a_recorded_annotation_carries_its_wire_exchange_and_join_keys() {
    use axum::routing::post;
    let app = axum::Router::new().route(
        "/",
        post(|| async { ([("x-appa-diagnostics", "model=m1")], PERMISSIVE_ANSWER) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback stub binds");
    let url = format!("http://{}/", listener.local_addr().expect("the stub has an address"));
    tokio::spawn(async move { axum::serve(listener, app).await.expect("the stub serves") });
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(
        annotated(dir.path(), &format!("url = \"{url}\"")),
        dir.path().join("appa.db"),
        None,
    )
    .expect("the deployment opens");
    let transcriber = Arc::new(Collected::default());
    let session = runtime
        .transcribing(transcriber.clone())
        .create_session(root(), None)
        .expect("a fresh id opens");

    let decision = session
        .on_tool_call_identified(
            fetch(serde_json::json!({"a": 1})),
            Some("host-call-7".to_string()),
            None,
            None,
        )
        .await
        .expect("the call is judged");
    assert!(matches!(decision, ToolCallDecision::Allow { .. }));

    let [record] = transcriber.taken().try_into().expect("one consult, one record");
    assert_eq!(record.id.get_version_num(), 7);
    assert_eq!(record.role, crate::api::ExternalRole::Annotator);
    assert_eq!(record.external_name, "gatekeeper");
    assert_eq!(record.backend, crate::api::ConsultBackend::Url);
    assert_eq!(record.request["kind"], "annotation");
    assert_eq!(
        record.request["artifact"]["args"]["arguments"],
        serde_json::json!({"a": 1})
    );
    assert_eq!(record.outcome, crate::api::ExternalOutcome::Answered);
    let wire: serde_json::Value = serde_json::from_str(PERMISSIVE_ANSWER).expect("the fixture is JSON");
    assert_eq!(record.answer.as_ref(), Some(&wire["answer"]));
    assert_eq!(record.raw_response.as_deref(), Some(PERMISSIVE_ANSWER.as_bytes()));
    assert_eq!(record.http_status, Some(200));
    assert_eq!(
        record.diagnostics,
        Some(crate::api::Diagnostics {
            bytes: b"model=m1".to_vec(),
            truncated: false
        })
    );
    assert_eq!(record.context.root, root().0);
    assert_eq!(record.context.trajectory, root().0);
    assert_eq!(record.context.call_id.as_deref(), Some("host-call-7"));
    assert_eq!(record.context.offer_id, None);
    let digest = record
        .context
        .call_digest
        .as_deref()
        .expect("an annotation names its call");
    assert!(digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()));

    let unrecorded = runtime.session(&root(), &root()).expect("the root is open");
    assert!(matches!(
        unrecorded
            .on_tool_call_identified(
                fetch(serde_json::json!({"a": 2})),
                Some("host-call-8".to_string()),
                None,
                None
            )
            .await
            .expect("the call is judged"),
        ToolCallDecision::Allow { .. }
    ));
    assert!(
        transcriber.taken().is_empty(),
        "a view without the transcriber transcribes nothing"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_recorded_command_annotation_keeps_its_stderr() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    std::fs::write(
        dir.path().join("annotate.sh"),
        format!("cat >/dev/null\necho 'thinking' >&2\nprintf '%s' '{PERMISSIVE_ANSWER}'\n"),
    )
    .expect("the script writes");
    let runtime = Runtime::open(
        annotated(dir.path(), r#"command = ["/bin/sh", "annotate.sh"]"#),
        dir.path().join("appa.db"),
        None,
    )
    .expect("the deployment opens");
    let transcriber = Arc::new(Collected::default());
    let session = runtime
        .transcribing(transcriber.clone())
        .create_session(root(), None)
        .expect("a fresh id opens");

    assert!(matches!(
        session
            .on_tool_call_identified(
                fetch(serde_json::json!({"a": 1})),
                Some("host-call-1".to_string()),
                None,
                None
            )
            .await
            .expect("the call is judged"),
        ToolCallDecision::Allow { .. }
    ));

    let [record] = transcriber.taken().try_into().expect("one consult, one record");
    assert_eq!(record.backend, crate::api::ConsultBackend::Command);
    assert_eq!(record.outcome, crate::api::ExternalOutcome::Answered);
    assert_eq!(record.raw_response.as_deref(), Some(PERMISSIVE_ANSWER.as_bytes()));
    assert_eq!(record.http_status, None);
    assert_eq!(
        record.diagnostics.map(|diagnostics| diagnostics.bytes),
        Some(b"thinking\n".to_vec())
    );
    assert_eq!(record.context.call_id.as_deref(), Some("host-call-1"));
}

/// The `jev` builtin's record names its own backend, and its diagnostics are the
/// `jev_diagnostics` object: what each attempt did and how each label settled.
#[tokio::test]
async fn a_recorded_jev_annotation_carries_its_diagnostics() {
    use axum::routing::post;
    const REPLY: &str = r#"{"answers":{"delta_audience":{"probabilities":{"self":0.0,"internal":0.1,"public":0.9}},"delta_trust":{"probabilities":{"suspicious":0.1,"trusted":0.9}},"requires_audience":{"probabilities":{"public":0.0,"internal":0.1,"none":0.9}},"requires_trusted":{"noul":0.1}}}"#;
    let app = axum::Router::new().route("/v1/systemone", post(|| async { REPLY }));
    let url = format!("http://{}/v1/systemone", crate::test_support::serve(app).await);
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let path = dir.path().join("appa.toml");
    std::fs::write(
        &path,
        "[policy]\nversion = 2\n\n[[policy.annotator]]\nname = \"gatekeeper\"\nbuiltin = \"jev\"\n\n\
             [[policy.tool]]\nname = \"*\"\nannotator = \"gatekeeper\"\n\n\
             [externals]\ntimeout_ms = 2000\nmax_body_bytes = 65536\n\n\
             [externals.jev]\ntoken_env = \"APPA_PROVIDER_JEV_API_KEY\"\n",
    )
    .expect("the fixture writes");
    let mut config = Config::load(&path).expect("the fixture validates");
    let jev = config.externals.jev.as_mut().expect("the profile is declared");
    jev.url = url;
    jev.key = crate::config::ProfileKey::Set(crate::config::Token::new("jev-test-key".to_string()));
    let runtime = Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens");
    let transcriber = Arc::new(Collected::default());
    let session = runtime
        .transcribing(transcriber.clone())
        .create_session(root(), None)
        .expect("a fresh id opens");

    assert!(matches!(
        session
            .on_tool_call_identified(
                fetch(serde_json::json!({"url": "https://example.org"})),
                Some("host-call-1".to_string()),
                None,
                None
            )
            .await
            .expect("the call is judged"),
        ToolCallDecision::Allow { .. }
    ));

    let [record] = transcriber.taken().try_into().expect("one consult, one record");
    assert_eq!(record.backend, crate::api::ConsultBackend::Jev);
    assert_eq!(record.outcome, crate::api::ExternalOutcome::Answered);
    assert_eq!(record.http_status, Some(200));
    assert_eq!(record.raw_response.as_deref(), Some(REPLY.as_bytes()));
    let diagnostics = record.diagnostics.expect("a jev consult describes itself");
    assert!(!diagnostics.truncated);
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&diagnostics.bytes).expect("the diagnostics are one JSON object");
    let diagnostics = &diagnostics["jev_diagnostics"];
    assert_eq!(diagnostics["version"], 1);
    assert!(
        diagnostics["model"]
            .as_str()
            .is_some_and(|model| model.len() > "jev-".len() && model.starts_with("jev-")),
        "the diagnostics name the jev model: {diagnostics}"
    );
    assert_eq!(diagnostics["attempts"], serde_json::json!(["ok"]));
    assert_eq!(diagnostics["labels"]["delta_trust"]["decision"], "trusted");
    assert_eq!(diagnostics.get("error"), None);
    assert!(diagnostics["elapsed_ms"].is_u64());
}

#[tokio::test]
async fn a_recorded_remedy_consult_names_its_offer() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let url = stub(serde_json::json!({"ruling": "approve"})).await;
    let runtime = Runtime::open(config_with(ATTENTION, Some(&url)), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let transcriber = Arc::new(Collected::default());
    let session = runtime
        .transcribing(transcriber.clone())
        .create_session(root(), None)
        .expect("a fresh id opens");
    assert!(matches!(
        session
            .on_tool_call(wire(500), false)
            .await
            .expect("the block is delivered"),
        ToolCallDecision::Deny { .. }
    ));
    let offer = surfaced_offer(&runtime);

    assert!(matches!(
        session
            .on_remedy(offer.clone(), RemedyArguments::default(), None, None)
            .await
            .expect("the remedy executes"),
        RemedyDecision::Authorized { .. }
    ));

    let [record] = transcriber.taken().try_into().expect("one consult, one record");
    assert_eq!(record.role, crate::api::ExternalRole::Authority);
    assert_eq!(record.external_name, "approver");
    assert_eq!(record.answer, Some(serde_json::json!({"ruling": "approve"})));
    assert_eq!(record.context.offer_id.as_deref(), Some(offer.0.as_str()));
    assert_eq!(record.context.call_id, None);
    assert_eq!(record.context.call_digest, None);
}

/// A host transcriber that fails on every record.
struct Panicking;

impl crate::api::ConsultTranscriber for Panicking {
    fn record(&self, _record: crate::api::ConsultRecord) {
        panic!("the host's transcriber fails");
    }
}

#[tokio::test]
async fn a_panicking_transcriber_leaves_the_consult_outcome_standing() {
    let url = stub(serde_json::json!({"ruling": "approve"})).await;
    let mut decisions = Vec::new();
    for transcriber in [
        None,
        Some(Arc::new(Panicking) as Arc<dyn crate::api::ConsultTranscriber>),
    ] {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(ATTENTION, Some(&url)), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let runtime = match transcriber {
            Some(transcriber) => runtime.transcribing(transcriber),
            None => runtime,
        };
        let session = runtime.create_session(root(), None).expect("a fresh id opens");
        assert!(matches!(
            session
                .on_tool_call(wire(500), false)
                .await
                .expect("the block is delivered"),
            ToolCallDecision::Deny { .. }
        ));
        let offer = surfaced_offer(&runtime);
        decisions.push(
            session
                .on_remedy(offer, RemedyArguments::default(), None, None)
                .await
                .expect("the remedy executes"),
        );
    }
    let [unrecorded, recorded] = decisions.try_into().expect("two runs");
    assert!(matches!(unrecorded, RemedyDecision::Authorized { .. }));
    assert_eq!(recorded, unrecorded, "the transcriber's panic changes nothing");
}

/// Every delivery a crash can repeat is re-answered from the record: no fact is
/// appended twice, no offer or binding is re-minted, and a repeat that disagrees with
/// the record is refused without reaching the model.
mod replay;

#[tokio::test]
async fn an_empty_crossing_child_turn_stays_open_and_a_root_cannot_end_as_a_child() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = Runtime::open(config_with(MARKED, None), dir.path().join("appa.db"), None).unwrap();
    let mut parent = runtime.create_session(root(), None).unwrap();
    assert!(matches!(parent.on_child_turn_end().await, Err(EventError::NotAChild)));
    let child = open_child(
        &mut parent,
        fetch(serde_json::json!({"a": 1})),
        TrajectoryId("cc:child".into()),
    )
    .await;
    assert_eq!(child.on_child_turn_end().await.unwrap(), ChildReturnDecision::NoValue);
    assert_eq!(child.on_child_turn_end().await.unwrap(), ChildReturnDecision::NoValue);
    assert!(
        child
            .on_tool_call(fetch(serde_json::json!({"a": 2})), false)
            .await
            .is_ok()
    );
}
