use super::*;
use crate::api::{RemedyOutcome, RemedyRefusal};

fn host_records(session: &Session) -> usize {
    session
        .inner
        .log(&session.root)
        .expect("the log reads")
        .host_records()
        .len()
}

#[tokio::test]
async fn a_closed_result_replays_its_admitted_decision() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let db = dir.path().join("appa.db");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), db.clone(), None).expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    let call = fetch(serde_json::json!({"a": 1}));
    assert!(matches!(
        session
            .on_tool_call_identified(call.clone(), Some("toolu-1".to_string()), None, None)
            .await
            .expect("the call releases"),
        ToolCallDecision::Allow { spawn: None, .. }
    ));
    assert_eq!(
        session
            .on_tool_result_identified(
                call.clone(),
                Some("toolu-1".to_string()),
                ToolOutcome::Success {
                    body: OutcomeBody::Available("first".to_string()),
                },
            )
            .await
            .expect("the result is admitted"),
        ToolResultDecision::Keep,
    );
    let facts = runtime.log_facts(&root());
    let hosts = host_records(&session);
    drop(session);
    drop(runtime);

    // The crash landed after the admission committed, before the answer reached the
    // host: the retry is answered from the record and appends nothing.
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), db, None).expect("the deployment reopens");
    let session = runtime.session(&root(), &root()).expect("the session reattaches");
    assert_eq!(
        session
            .on_tool_result_identified(
                call,
                Some("toolu-1".to_string()),
                ToolOutcome::Success {
                    body: OutcomeBody::Available("first".to_string()),
                },
            )
            .await
            .expect("the repeated result is answered"),
        ToolResultDecision::Keep,
    );
    assert_eq!(runtime.log_facts(&root()), facts, "a replay appends no facts");
    assert_eq!(host_records(&session), hosts, "a replay writes no host records");
}

#[tokio::test]
async fn a_replayed_result_must_carry_the_recorded_outcome() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    let call = fetch(serde_json::json!({"a": 1}));
    session
        .on_tool_call_identified(call.clone(), Some("toolu-1".to_string()), None, None)
        .await
        .expect("the call releases");
    session
        .on_tool_result_identified(
            call.clone(),
            Some("toolu-1".to_string()),
            ToolOutcome::Success {
                body: OutcomeBody::Available("first".to_string()),
            },
        )
        .await
        .expect("the result is admitted");

    // Other bytes than the admitted result.
    let error = session
        .on_tool_result_identified(
            call.clone(),
            Some("toolu-1".to_string()),
            ToolOutcome::Success {
                body: OutcomeBody::Available("second".to_string()),
            },
        )
        .await
        .expect_err("a changed body is not the recorded outcome");
    assert!(matches!(error, EventError::OutcomeMismatch), "got {error:?}");
    assert!(!error.is_operational(), "a host's disagreement is not an engine fault");

    // A failure where a success committed.
    let error = session
        .on_tool_result_identified(
            call.clone(),
            Some("toolu-1".to_string()),
            ToolOutcome::Failure {
                message: "it broke".to_string(),
            },
        )
        .await
        .expect_err("a committed success cannot be withdrawn");
    assert!(matches!(error, EventError::OutcomeMismatch), "got {error:?}");

    // The id bound to a different call.
    let error = session
        .on_tool_result_identified(
            fetch(serde_json::json!({"a": 2})),
            Some("toolu-1".to_string()),
            ToolOutcome::Success {
                body: OutcomeBody::Available("first".to_string()),
            },
        )
        .await
        .expect_err("the id names one occurrence, bytes included");
    assert!(matches!(error, EventError::UnknownDispatch), "got {error:?}");

    // An id no call was bound to.
    let error = session
        .on_tool_result_identified(
            call.clone(),
            Some("toolu-9".to_string()),
            ToolOutcome::Success {
                body: OutcomeBody::Available("first".to_string()),
            },
        )
        .await
        .expect_err("an unbound id reports nothing");
    assert!(matches!(error, EventError::UnknownDispatch), "got {error:?}");

    // Another family cannot answer for this one's call.
    let other = runtime
        .create_session(TrajectoryId("cc:other".to_string()), None)
        .expect("a second root opens");
    let error = other
        .on_tool_result_identified(
            call.clone(),
            Some("toolu-1".to_string()),
            ToolOutcome::Success {
                body: OutcomeBody::Available("first".to_string()),
            },
        )
        .await
        .expect_err("a binding is the family's own");
    assert!(matches!(error, EventError::UnknownDispatch), "got {error:?}");

    // The forgeries changed nothing: the honest repeat is still answered.
    assert_eq!(
        session
            .on_tool_result_identified(
                call,
                Some("toolu-1".to_string()),
                ToolOutcome::Success {
                    body: OutcomeBody::Available("first".to_string()),
                },
            )
            .await
            .expect("the recorded answer stands"),
        ToolResultDecision::Keep,
    );
}

#[tokio::test]
async fn failure_withheld_and_uncarried_results_replay_their_placeholders() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");

    // A failure closes and replays as kept: the harness's own failure text stands.
    let call = fetch(serde_json::json!({"a": 1}));
    session
        .on_tool_call_identified(call.clone(), Some("toolu-1".to_string()), None, None)
        .await
        .expect("the call releases");
    for _ in 0..2 {
        assert_eq!(
            session
                .on_tool_result_identified(
                    call.clone(),
                    Some("toolu-1".to_string()),
                    ToolOutcome::Failure {
                        message: "boom".to_string(),
                    },
                )
                .await
                .expect("the failure is answered"),
            ToolResultDecision::Keep,
        );
    }

    // A result that never reached the runtime replays its not-carried placeholder.
    let call = fetch(serde_json::json!({"a": 2}));
    session
        .on_tool_call_identified(call.clone(), Some("toolu-2".to_string()), None, None)
        .await
        .expect("the call releases");
    for _ in 0..2 {
        let decision = session
            .on_tool_result_identified(
                call.clone(),
                Some("toolu-2".to_string()),
                ToolOutcome::Success {
                    body: OutcomeBody::Unavailable,
                },
            )
            .await
            .expect("the uncarried result is answered");
        assert_eq!(
            decision,
            ToolResultDecision::Replace {
                placeholder: "[appa] the tool ran, but the harness returned no output".to_string(),
                presentation: None,
            },
        );
    }
    assert!(runtime.open_dispatches(&root(), &root()).is_empty());
}

#[tokio::test]
async fn a_withheld_result_replays_its_withholding() {
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
        panic!("the confined call blocks first");
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
            .on_tool_call_identified(leak(), Some("toolu-1".to_string()), None, None)
            .await
            .expect("the approved call releases"),
        ToolCallDecision::Allow { .. }
    ));
    let withheld = ToolResultDecision::Replace {
        placeholder: "[appa] the result is withheld".to_string(),
        presentation: None,
    };
    let outcome = ToolOutcome::Success {
        body: OutcomeBody::Available("raw with pii".to_string()),
    };
    assert_eq!(
        session
            .on_tool_result_identified(leak(), Some("toolu-1".to_string()), outcome.clone())
            .await
            .expect("the raw is withheld"),
        withheld,
    );
    let facts = runtime.log_facts(&root());
    assert_eq!(
        session
            .on_tool_result_identified(leak(), Some("toolu-1".to_string()), outcome)
            .await
            .expect("the repeat is answered"),
        withheld,
        "the withheld result replays its withholding, raw included",
    );
    assert_eq!(runtime.log_facts(&root()), facts, "a replay appends no facts");
}

#[tokio::test]
async fn a_sanitized_result_replays_its_admitted_value() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let url = stub(serde_json::json!({"body": "scrubbed"})).await;
    let runtime =
        Runtime::open(narrowing_config(&url), dir.path().join("appa.db"), None).expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    assert!(matches!(
        session
            .on_tool_call(leak(), false)
            .await
            .expect("the block is delivered"),
        ToolCallDecision::Deny { .. }
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
        .expect("the block surfaced a sanitize offer");
    let offer = runtime.resolve_in(&root(), &quoted).expect("the quoted id resolves").0;
    assert!(matches!(
        session
            .on_remedy(offer, RemedyArguments::default(), None, None)
            .await
            .expect("the offer executes"),
        RemedyDecision::Authorized { .. }
    ));
    assert!(matches!(
        session
            .on_tool_call_identified(leak(), Some("toolu-1".to_string()), None, None)
            .await
            .expect("the re-proposal releases"),
        ToolCallDecision::Allow { .. }
    ));
    let outcome = ToolOutcome::Success {
        body: OutcomeBody::Available("raw with pii".to_string()),
    };
    let admitted = ToolResultDecision::Deliver {
        value: "scrubbed".to_string(),
    };
    assert_eq!(
        session
            .on_tool_result_identified(leak(), Some("toolu-1".to_string()), outcome.clone())
            .await
            .expect("the derivation is admitted"),
        admitted,
    );
    let facts = runtime.log_facts(&root());
    assert_eq!(
        session
            .on_tool_result_identified(leak(), Some("toolu-1".to_string()), outcome)
            .await
            .expect("the repeat is answered"),
        admitted,
        "the replay delivers the admitted derivation, never the raw",
    );
    assert_eq!(runtime.log_facts(&root()), facts, "a replay appends no facts");
}

#[tokio::test]
async fn a_staged_result_replays_its_pending_remedies() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let url = stub(serde_json::json!({"body": "scrubbed"})).await;
    let runtime =
        Runtime::open(partly_cleared_config(&url), dir.path().join("appa.db"), None).expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    assert!(matches!(
        session
            .on_tool_call(leak(), false)
            .await
            .expect("the block is delivered"),
        ToolCallDecision::Deny { .. }
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
        .expect("the block surfaced a sanitize offer");
    let offer = runtime.resolve_in(&root(), &quoted).expect("the quoted id resolves").0;
    assert!(matches!(
        session
            .on_remedy(offer, RemedyArguments::default(), None, None)
            .await
            .expect("the offer executes"),
        RemedyDecision::Authorized { .. }
    ));
    assert!(matches!(
        session
            .on_tool_call_identified(leak(), Some("toolu-1".to_string()), None, None)
            .await
            .expect("the re-proposal releases"),
        ToolCallDecision::Allow { .. }
    ));
    let outcome = ToolOutcome::Success {
        body: OutcomeBody::Available("raw with pii".to_string()),
    };
    let staged = session
        .on_tool_result_identified(leak(), Some("toolu-1".to_string()), outcome.clone())
        .await
        .expect("the staged derivation is delivered");
    let ToolResultDecision::Replace {
        presentation: Some(ref presentation),
        ..
    } = staged
    else {
        panic!("a staged derivation is delivered as a replacement with its remedies: {staged:?}");
    };
    assert!(!presentation.offers.is_empty(), "the stage surfaces its remedies");
    let facts = runtime.log_facts(&root());
    let minted = runtime.minted_offers(&root(), &root());
    let replayed = session
        .on_tool_result_identified(leak(), Some("toolu-1".to_string()), outcome)
        .await
        .expect("the repeat is answered");
    assert_eq!(replayed, staged, "the stage replays with its recorded offers");
    assert_eq!(runtime.log_facts(&root()), facts, "a replay appends no facts");
    assert_eq!(
        runtime.minted_offers(&root(), &root()),
        minted,
        "a replay mints no offer identities"
    );
}

#[tokio::test]
async fn a_repeated_call_is_answered_with_its_own_release() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    let call = fetch(serde_json::json!({"a": 1}));
    let ToolCallDecision::Allow { dispatch, .. } = session
        .on_tool_call_identified(call.clone(), Some("toolu-1".to_string()), None, None)
        .await
        .expect("the call releases")
    else {
        panic!("the call releases");
    };

    // While the dispatch stands open the repeat answers with the same release.
    let facts = runtime.log_facts(&root());
    let hosts = host_records(&session);
    let replayed = session
        .on_tool_call_identified(call.clone(), Some("toolu-1".to_string()), None, None)
        .await
        .expect("the repeat is answered");
    assert!(
        matches!(replayed, ToolCallDecision::Allow { dispatch: ref again, spawn: None } if *again == dispatch),
        "the repeat releases the same dispatch: {replayed:?}"
    );
    assert_eq!(runtime.log_facts(&root()), facts, "a replay appends no facts");
    assert_eq!(host_records(&session), hosts, "a replay writes no host records");
    assert_eq!(runtime.open_dispatches(&root(), &root()).len(), 1);

    // After the outcome landed, the release still answers: the host re-driving its
    // transcript hears what it was first told.
    session
        .on_tool_result_identified(
            call.clone(),
            Some("toolu-1".to_string()),
            ToolOutcome::Success {
                body: OutcomeBody::Available("first".to_string()),
            },
        )
        .await
        .expect("the result is admitted");
    let facts = runtime.log_facts(&root());
    let replayed = session
        .on_tool_call_identified(call.clone(), Some("toolu-1".to_string()), None, None)
        .await
        .expect("the repeat is answered");
    assert!(
        matches!(replayed, ToolCallDecision::Allow { dispatch: ref again, spawn: None } if *again == dispatch),
        "the settled release replays its dispatch: {replayed:?}"
    );
    assert_eq!(runtime.log_facts(&root()), facts, "a replay appends no facts");

    // The id names one occurrence: other bytes under it are a reused id.
    assert!(matches!(
        session
            .on_tool_call_identified(
                fetch(serde_json::json!({"a": 2})),
                Some("toolu-1".to_string()),
                None,
                None
            )
            .await,
        Err(EventError::CallIdReused),
    ));

    // A genuinely new identical call takes a new id and a new occurrence.
    let replayed = session
        .on_tool_call_identified(call.clone(), Some("toolu-2".to_string()), None, None)
        .await
        .expect("a new id decides anew");
    assert!(
        matches!(replayed, ToolCallDecision::Allow { dispatch: ref again, .. } if *again != dispatch),
        "a new call id is a new occurrence: {replayed:?}"
    );
}

#[tokio::test]
async fn a_spawn_call_replays_its_fork_binding() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    let call = authorize_spawn(
        &session,
        fetch(serde_json::json!({"a": 1})),
        None,
        crate::engine::LabelSpelling::default(),
    )
    .await;
    let ToolCallDecision::Allow {
        spawn: Some(binding),
        dispatch,
    } = session
        .on_tool_call_identified(
            call.clone().proposed(),
            Some("spawn-1".to_string()),
            Some(SpawnKind::Single),
            None,
        )
        .await
        .expect("the approved spawn releases")
    else {
        panic!("a context-controlled spawn releases a fork binding");
    };
    let facts = runtime.log_facts(&root());
    let replayed = session
        .on_tool_call_identified(
            call.proposed(),
            Some("spawn-1".to_string()),
            Some(SpawnKind::Single),
            None,
        )
        .await
        .expect("the repeat is answered");
    let ToolCallDecision::Allow {
        spawn: Some(again),
        dispatch: again_dispatch,
    } = replayed
    else {
        panic!("the replay releases the same fork binding: {replayed:?}");
    };
    assert_eq!(again, binding, "the fork binding is derived, never re-minted");
    assert_eq!(again_dispatch, dispatch);
    assert_eq!(runtime.log_facts(&root()), facts, "a replay appends no facts");
}

#[tokio::test]
async fn a_denied_call_replays_its_recorded_denial() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let url = stub(serde_json::json!({"ruling": "approve"})).await;
    let runtime = Runtime::open(config_with(ATTENTION, Some(&url)), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");

    let denied = session
        .on_tool_call_identified(wire(500), Some("toolu-1".to_string()), None, None)
        .await
        .expect("the call is judged");
    let ToolCallDecision::Deny { ref offers, .. } = denied else {
        panic!("the call blocks: {denied:?}");
    };
    assert!(!offers.is_empty(), "the block surfaces its remedies");
    let facts = runtime.log_facts(&root());
    let hosts = host_records(&session);

    // The repeat is the recorded denial: same feedback, same offer identities, and
    // no consult, no append, no new offers.
    let replayed = session
        .on_tool_call_identified(wire(500), Some("toolu-1".to_string()), None, None)
        .await
        .expect("the repeat is answered");
    assert_eq!(replayed, denied, "the denial replays from the record");
    assert_eq!(runtime.log_facts(&root()), facts, "a replay appends no facts");
    assert_eq!(host_records(&session), hosts, "a replay writes no host records");

    // The id names one occurrence: other bytes under it are a reused id.
    assert!(matches!(
        session
            .on_tool_call_identified(wire(600), Some("toolu-1".to_string()), None, None)
            .await,
        Err(EventError::CallIdReused),
    ));
}

#[tokio::test]
async fn a_denial_replayed_does_not_pin_the_call_after_a_remedy() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let url = stub(serde_json::json!({"ruling": "approve"})).await;
    let runtime = Runtime::open(config_with(ATTENTION, Some(&url)), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");

    assert!(matches!(
        session
            .on_tool_call_identified(wire(500), Some("toolu-1".to_string()), None, None)
            .await
            .expect("the call is judged"),
        ToolCallDecision::Deny { .. }
    ));
    let offer = surfaced_offer(&runtime);
    assert!(matches!(
        session
            .on_remedy(offer, RemedyArguments::default(), None, None)
            .await
            .expect("the remedy executes"),
        RemedyDecision::Authorized { .. }
    ));

    // The state moved: the block the denial recorded no longer stands, so the same
    // call id decides anew — the replay never traps a remedied call in its denial.
    let released = session
        .on_tool_call_identified(wire(500), Some("toolu-1".to_string()), None, None)
        .await
        .expect("the re-proposal resumes");
    let ToolCallDecision::Allow { dispatch, .. } = released else {
        panic!("the approved call releases: {released:?}");
    };

    // And from here the release is what repeats answer with.
    let replayed = session
        .on_tool_call_identified(wire(500), Some("toolu-1".to_string()), None, None)
        .await
        .expect("the repeat is answered");
    assert!(
        matches!(replayed, ToolCallDecision::Allow { dispatch: ref again, .. } if *again == dispatch),
        "the bound release replays its dispatch: {replayed:?}"
    );
}

#[tokio::test]
async fn an_executed_offer_replays_its_recorded_outcome() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let url = stub(serde_json::json!({"ruling": "approve"})).await;
    let runtime = Runtime::open(config_with(ATTENTION, Some(&url)), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    assert!(matches!(
        session
            .on_tool_call(wire(500), false)
            .await
            .expect("the block is delivered"),
        ToolCallDecision::Deny { .. }
    ));
    let offer = surfaced_offer(&runtime);
    let authorized = session
        .on_remedy(offer.clone(), RemedyArguments::default(), None, None)
        .await
        .expect("the remedy executes");
    let facts = runtime.log_facts(&root());

    let replayed = session
        .on_remedy(offer, RemedyArguments::default(), None, None)
        .await
        .expect("the repeat is answered");
    assert_eq!(replayed, authorized, "the executed offer re-answers from its record");
    assert_eq!(runtime.log_facts(&root()), facts, "a replay appends no facts");
    let consumed = runtime
        .log_facts(&root())
        .iter()
        .filter(|fact| matches!(fact, appa_engine::fact::Fact::OfferAccepted { .. }))
        .count();
    assert_eq!(consumed, 1, "the offer is executed exactly once");
}

#[tokio::test]
async fn another_attempt_waits_out_a_running_consult() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    // The first consult is held until the test lets it answer; any later one answers
    // at once, so a second consult would finish and authorize.
    let asked = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(tokio::sync::Notify::new());
    let answer = Arc::new(tokio::sync::Notify::new());
    let app = axum::Router::new().route(
        "/",
        axum::routing::post({
            let (asked, entered, answer) = (asked.clone(), entered.clone(), answer.clone());
            move || async move {
                if asked.fetch_add(1, Ordering::SeqCst) == 0 {
                    entered.notify_one();
                    answer.notified().await;
                }
                axum::Json(serde_json::json!({"version": 1, "answer": {"ruling": "approve"}}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback stub binds");
    let url = format!("http://{}/", listener.local_addr().expect("the stub has an address"));
    tokio::spawn(async move { axum::serve(listener, app).await.expect("the stub serves") });

    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(ATTENTION, Some(&url)), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    let actor = crate::api::Actor {
        root: root(),
        child: None,
    };
    let denied = session
        .on_tool_call(wire(500), false)
        .await
        .expect("the block is delivered");
    let ToolCallDecision::Deny { ref offers, .. } = denied else {
        panic!("the call blocks: {denied:?}");
    };
    let quoted = OfferId(offers[0].id.clone());
    let key = crate::api::PermitKey::offer(&quoted);
    let args = || crate::api::ExecuteRemedyPlanArgs {
        offer_id: Some(quoted.0.clone()),
        label: None,
        return_schema: None,
    };

    runtime.vouch(&key, &actor, None);
    let mut first = Box::pin(runtime.execute_embedded_remedy(&actor, args()));
    tokio::select! {
        outcome = &mut first => panic!("the consult is held, yet the remedy answered: {outcome:?}"),
        () = entered.notified() => {}
    }
    runtime.vouch(&key, &actor, None);
    assert_eq!(
        runtime.execute_embedded_remedy(&actor, args()).await,
        RemedyOutcome::Refused {
            reason: RemedyRefusal::AlreadyExecuting
        },
    );
    assert_eq!(
        asked.load(Ordering::SeqCst),
        1,
        "the offer's authority is consulted once"
    );
    answer.notify_one();
    let first = first.await;
    assert!(
        matches!(first, RemedyOutcome::Authorized { .. }),
        "the running remedy completes: {first:?}"
    );
}

#[tokio::test]
async fn a_spent_embedded_offer_answers_no_other_attempt() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let url = stub(serde_json::json!({"ruling": "approve"})).await;
    let runtime = Runtime::open(config_with(ATTENTION, Some(&url)), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    let actor = crate::api::Actor {
        root: root(),
        child: None,
    };
    let denied = session
        .on_tool_call(wire(500), false)
        .await
        .expect("the block is delivered");
    let ToolCallDecision::Deny { ref offers, .. } = denied else {
        panic!("the call blocks: {denied:?}");
    };
    let quoted = OfferId(offers[0].id.clone());
    let key = crate::api::PermitKey::offer(&quoted);
    // Two attempts pass the control gate before either executes; one session's
    // standings behind one offer are one standing.
    runtime.vouch(&key, &actor, None);
    runtime.vouch(&key, &actor, None);
    let args = || crate::api::ExecuteRemedyPlanArgs {
        offer_id: Some(quoted.0.clone()),
        label: None,
        return_schema: None,
    };
    let first = runtime.execute_embedded_remedy(&actor, args()).await;
    assert!(
        matches!(first, RemedyOutcome::Authorized { .. }),
        "the vouched remedy executes: {first:?}"
    );
    let facts = runtime.log_facts(&root());

    assert_eq!(
        runtime.execute_embedded_remedy(&actor, args()).await,
        RemedyOutcome::Refused {
            reason: RemedyRefusal::Unvouched
        },
        "the other attempt's standing was spent with the first"
    );
    runtime.vouch(&key, &actor, None);
    assert_eq!(
        runtime.execute_embedded_remedy(&actor, args()).await,
        RemedyOutcome::Refused {
            reason: RemedyRefusal::UnknownOffer
        },
        "a later attempt vouches for an offer that no longer stands"
    );
    assert_eq!(runtime.log_facts(&root()), facts, "a refused attempt appends no facts");
}

#[tokio::test]
async fn a_sibling_offer_outrun_by_an_admitted_narrowing_is_superseded() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    let actor = crate::api::Actor {
        root: root(),
        child: None,
    };
    let mut blocked = Vec::new();
    for a in [1, 2] {
        let denied = session
            .on_tool_call(taint(serde_json::json!({ "a": a })), false)
            .await
            .expect("the block is delivered");
        let ToolCallDecision::Deny { ref offers, .. } = denied else {
            panic!("the narrowing call blocks: {denied:?}");
        };
        blocked.push(OfferId(offers[0].id.clone()));
    }
    let execute = |quoted: &OfferId| {
        runtime.vouch(&crate::api::PermitKey::offer(quoted), &actor, None);
        runtime.execute_embedded_remedy(
            &actor,
            crate::api::ExecuteRemedyPlanArgs {
                offer_id: Some(quoted.0.clone()),
                label: None,
                return_schema: None,
            },
        )
    };

    let RemedyOutcome::Authorized { call } = execute(&blocked[0]).await else {
        panic!("the first acceptance executes");
    };
    assert!(matches!(
        session
            .on_tool_call(call.clone(), false)
            .await
            .expect("the re-proposal is decided"),
        ToolCallDecision::Allow { spawn: None, .. }
    ));
    let kept = session
        .on_tool_result(
            call,
            ToolOutcome::Success {
                body: OutcomeBody::Available("data".to_string()),
            },
        )
        .await
        .expect("the result is admitted");
    assert_eq!(kept, ToolResultDecision::Keep);
    assert_eq!(
        execute(&blocked[1]).await,
        RemedyOutcome::Refused {
            reason: RemedyRefusal::Superseded
        },
        "the sibling's offer was made before the session narrowed"
    );
}

#[tokio::test]
async fn a_taken_claim_is_neither_renewed_nor_released_by_its_former_holder() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime =
        Runtime::open(config_with(ATTENTION, None), dir.path().join("appa.db"), None).expect("the deployment opens");
    runtime.create_session(root(), None).expect("a fresh id opens");
    let actor = crate::api::host::host_actor(&crate::api::Actor {
        root: root(),
        child: None,
    });
    let key = crate::api::PermitKey::offer(&OfferId("0123456789abcdef".to_string()));
    let claim = |until| appa_eventlog::HostObservation::Claimed {
        actor: actor.clone(),
        key: key.wire(),
        until,
    };
    let lapsed = std::time::SystemTime::now();
    let successor = lapsed + std::time::Duration::from_secs(600);
    for until in [lapsed, successor] {
        runtime
            .inner
            .append_host(&root(), &claim(until))
            .expect("the claim lands");
    }
    let log = runtime.inner.log(&root()).expect("the log reads");
    assert_eq!(crate::api::reduced(&log).claimed(&key), Some(successor));
    let mut former = crate::api::Claimed {
        inner: Arc::clone(&runtime.inner),
        root: root(),
        actor: actor.clone(),
        key: key.clone(),
        until: lapsed,
        released: false,
    };

    assert!(matches!(
        former.renew(std::time::Duration::from_secs(600)),
        Ok(crate::api::Holding::Lost)
    ));
    former.release();
    let log = runtime.inner.log(&root()).expect("the log reads");
    assert_eq!(crate::api::reduced(&log).claimed(&key), Some(successor));
}
