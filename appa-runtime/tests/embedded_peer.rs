//! Embedded peer messages inside one family. A send records a runtime-snapshotted
//! label and does not fold anyone. Arrival rechecks the recipient. A narrowing
//! message stays held until an explicit read binds that stored label on the
//! recipient trajectory.

use appa_runtime::api::{
    EmbeddedPeerArrival, EmbeddedPeerError, LabelSpelling, OfferId, RemedyArguments, RemedyOutcome, Runtime,
    TrajectoryId,
};
use appa_runtime::config::Config;
use appa_runtime::hooks;
use appa_runtime_api::{
    Actor, HookDecision, HookEvent, OutcomeBody, PeerDigest, ProposedCall, SpawnKind, SpawnRef, ToolOutcome,
};
use serde_json::{json, value::RawValue};

const POLICY: &str = r#"
[policy]
version = 2

[[policy.tool]]
name = "send"
delta = {}

[[policy.tool]]
name = "read"
delta = {}

[[policy.tool]]
name = "narrow"
delta = { audience = ["internal"] }

[[policy.tool]]
name = "open"
delta = {}

[[policy.tool]]
name = "spawn"
delta = {}

[policy.deployment]
context_control = true

[externals]
timeout_ms = 2000
max_body_bytes = 65536
"#;

fn runtime() -> (tempfile::TempDir, Runtime) {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let path = dir.path().join("policy.toml");
    std::fs::write(&path, POLICY).expect("the policy writes");
    let runtime = Runtime::open(
        Config::load(&path).expect("the policy loads"),
        dir.path().join("appa.db"),
        None,
    )
    .expect("the runtime opens");
    (dir, runtime)
}

fn id(text: &str) -> TrajectoryId {
    TrajectoryId(text.to_string())
}

fn family() -> TrajectoryId {
    id("family")
}

fn child() -> TrajectoryId {
    id("child")
}

fn actor(child: Option<&TrajectoryId>) -> Actor {
    Actor {
        root: family(),
        child: child.cloned(),
    }
}

fn call(tool: &str, arguments: serde_json::Value) -> ProposedCall {
    ProposedCall {
        tool: tool.to_string(),
        arguments: RawValue::from_string(arguments.to_string()).expect("arguments are json"),
        cwd: None,
    }
}

async fn start(runtime: &Runtime) {
    assert_eq!(
        hooks::handle(
            runtime,
            HookEvent::SessionStart {
                root: family(),
                principal: None,
                address: None,
                title: None,
                start: None,
                launch: None,
            },
        )
        .await,
        HookDecision::Ack,
    );
}

async fn release(runtime: &Runtime, who: Option<&TrajectoryId>, tool: &str, call_id: &str) -> HookDecision {
    hooks::handle(
        runtime,
        HookEvent::ToolCall {
            actor: actor(who),
            call: call(tool, json!({})),
            call_id: Some(call_id.to_string()),
            spawn: None,
            prompt: None,
            ruling: None,
        },
    )
    .await
}

async fn result(runtime: &Runtime, who: Option<&TrajectoryId>, tool: &str, call_id: &str, body: &str) -> HookDecision {
    hooks::handle(
        runtime,
        HookEvent::ToolResult {
            actor: actor(who),
            call: call(tool, json!({})),
            call_id: Some(call_id.to_string()),
            outcome: ToolOutcome::Success {
                body: OutcomeBody::Available(body.to_string()),
            },
        },
    )
    .await
}

async fn release_spawn(runtime: &Runtime) -> appa_runtime_api::SpawnBinding {
    let first = hooks::handle(
        runtime,
        HookEvent::ToolCall {
            actor: actor(None),
            call: call("spawn", json!({})),
            call_id: Some("spawn-1".to_string()),
            spawn: Some(SpawnKind::Single),
            prompt: None,
            ruling: None,
        },
    )
    .await;
    let HookDecision::DenyCall { offers, .. } = first else {
        panic!("a controlled spawn blocks until its return is declared, got {first:?}");
    };
    let offer = offers
        .iter()
        .find(|offer| matches!(offer.returns, Some(appa_runtime_api::OfferedReturn::AsSpoken) | None))
        .or_else(|| offers.first())
        .expect("the spawn offers a return");
    let declared = runtime
        .execute_remedy_with(
            &actor(None),
            OfferId(offer.id.clone()),
            RemedyArguments {
                label: Some(LabelSpelling::default()),
                return_schema: None,
            },
        )
        .await;
    let RemedyOutcome::Authorized { call: authorized } = declared else {
        panic!("the declaration releases the spawn, got {declared:?}");
    };
    let released = hooks::handle(
        runtime,
        HookEvent::ToolCall {
            actor: actor(None),
            call: authorized,
            call_id: Some("spawn-1".to_string()),
            spawn: Some(SpawnKind::Single),
            prompt: None,
            ruling: None,
        },
    )
    .await;
    let HookDecision::AllowCall { spawn: Some(binding) } = released else {
        panic!("the spawn releases a fork, got {released:?}");
    };
    binding
}

async fn open_child(runtime: &Runtime) {
    let binding = release_spawn(runtime).await;
    assert_eq!(
        hooks::handle(
            runtime,
            HookEvent::ChildStart {
                root: family(),
                child: child(),
                spawn: SpawnRef::Binding(binding),
            },
        )
        .await,
        HookDecision::Ack,
    );
}

#[tokio::test]
async fn a_send_does_not_fold_and_an_unstarted_child_stays_held() {
    let (_dir, runtime) = runtime();
    start(&runtime).await;
    let binding = release_spawn(&runtime).await;
    assert!(matches!(
        release(&runtime, None, "send", "send-1").await,
        HookDecision::AllowCall { .. }
    ));
    let notice = runtime
        .send_embedded_peer(
            &family(),
            &family(),
            &child(),
            Some(binding.0.as_str()),
            "send-1",
            "hello",
        )
        .expect("the send records");
    assert_eq!(notice.sender, family());
    assert_eq!(notice.recipient, child());
    let again = runtime
        .send_embedded_peer(
            &family(),
            &family(),
            &child(),
            Some(binding.0.as_str()),
            "send-1",
            "hello",
        )
        .expect("the same send retries");
    assert_eq!(again.id, notice.id);
    let changed = runtime.send_embedded_peer(
        &family(),
        &family(),
        &child(),
        Some(binding.0.as_str()),
        "send-1",
        "other",
    );
    assert!(matches!(changed, Err(EmbeddedPeerError::Refused(_))));
    let arrival = runtime
        .receive_embedded_peer(
            &family(),
            &child(),
            &notice.id,
            &family(),
            &PeerDigest::of_body("hello"),
        )
        .expect("arrival answers");
    assert!(matches!(arrival, EmbeddedPeerArrival::Held(_)));
    assert_eq!(
        hooks::handle(
            &runtime,
            HookEvent::ChildStart {
                root: family(),
                child: child(),
                spawn: SpawnRef::Binding(binding),
            },
        )
        .await,
        HookDecision::Ack,
    );
    let listed = runtime
        .list_embedded_peer(&family(), &child())
        .expect("the inbox lists");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, notice.id);
    let audit = runtime.audit(&family()).expect("the family audits");
    assert!(
        audit.iter().all(|entry| entry.trajectory != child().0
            || !matches!(entry.event, appa_runtime::api::AuditEvent::Admitted { .. })),
        "an unstarted recipient has no admitted peer body"
    );
}

#[tokio::test]
async fn a_preexisting_call_is_not_resulted_as_the_read() {
    let (_dir, runtime) = runtime();
    start(&runtime).await;
    open_child(&runtime).await;
    assert!(matches!(
        release(&runtime, Some(&child()), "open", "shared").await,
        HookDecision::AllowCall { .. }
    ));
    assert!(matches!(
        release(&runtime, None, "send", "send-foreign").await,
        HookDecision::AllowCall { .. }
    ));
    let notice = runtime
        .send_embedded_peer(&family(), &family(), &child(), None, "send-foreign", "private")
        .expect("the send records");
    let read = runtime
        .read_embedded_peer(
            &actor(Some(&child())),
            "shared",
            &notice.id,
            call("read", json!({"message_id": notice.id.as_str()})),
        )
        .await;
    assert!(matches!(read, Err(EmbeddedPeerError::Refused(_))), "{read:?}");
    let finished = result(&runtime, Some(&child()), "open", "shared", "still open").await;
    assert!(
        matches!(
            finished,
            HookDecision::DeliverValue { .. } | HookDecision::Ack | HookDecision::ReplaceOutput { .. }
        ),
        "the unrelated call is still open, got {finished:?}"
    );
    assert_eq!(runtime.list_embedded_peer(&family(), &child()).expect("list").len(), 1);
}

#[tokio::test]
async fn one_call_id_cannot_read_two_messages_and_changed_arguments_refuse() {
    let (_dir, runtime) = runtime();
    start(&runtime).await;
    open_child(&runtime).await;
    assert!(matches!(
        release(&runtime, None, "send", "send-a").await,
        HookDecision::AllowCall { .. }
    ));
    assert!(matches!(
        release(&runtime, None, "send", "send-b").await,
        HookDecision::AllowCall { .. }
    ));
    let first = runtime
        .send_embedded_peer(&family(), &family(), &child(), None, "send-a", "one")
        .expect("the first send records");
    let second = runtime
        .send_embedded_peer(&family(), &family(), &child(), None, "send-b", "two")
        .expect("the second send records");
    let read = admit_read(&runtime, &first.id, "once", json!({"message_id": first.id.as_str()})).await;
    let HookDecision::DeliverValue { value } = read else {
        panic!("the first read delivers, got {read:?}");
    };
    assert_eq!(value, "one");
    let other = runtime
        .read_embedded_peer(
            &actor(Some(&child())),
            "once",
            &second.id,
            call("read", json!({"message_id": second.id.as_str()})),
        )
        .await;
    assert!(matches!(other, Err(EmbeddedPeerError::Refused(_))), "{other:?}");
    let changed = runtime
        .read_embedded_peer(
            &actor(Some(&child())),
            "once",
            &first.id,
            call("read", json!({"message_id": first.id.as_str(), "extra": true})),
        )
        .await;
    assert!(matches!(changed, Err(EmbeddedPeerError::Refused(_))), "{changed:?}");
    let retry = admit_read(&runtime, &first.id, "once", json!({"message_id": first.id.as_str()})).await;
    assert_eq!(
        retry,
        HookDecision::DeliverValue {
            value: "one".to_string()
        }
    );
}

#[tokio::test]
async fn a_denied_read_stays_listed_and_a_new_call_can_finish_it() {
    let (_dir, runtime) = runtime();
    start(&runtime).await;
    open_child(&runtime).await;
    let blocked = release(&runtime, None, "narrow", "narrow-new").await;
    let HookDecision::DenyCall { offers, .. } = &blocked else {
        panic!("narrowing blocks until accepted, got {blocked:?}");
    };
    let accepted = runtime
        .execute_remedy(&actor(None), OfferId(offers[0].id.clone()))
        .await;
    assert!(matches!(accepted, RemedyOutcome::Authorized { .. }), "{accepted:?}");
    assert!(matches!(
        release(&runtime, None, "narrow", "narrow-new").await,
        HookDecision::AllowCall { .. }
    ));
    result(&runtime, None, "narrow", "narrow-new", "secret").await;
    assert!(matches!(
        release(&runtime, None, "send", "send-new").await,
        HookDecision::AllowCall { .. }
    ));
    let notice = runtime
        .send_embedded_peer(&family(), &family(), &child(), None, "send-new", "secret")
        .expect("the send records");
    let denied = runtime
        .read_embedded_peer(
            &actor(Some(&child())),
            "model-1",
            &notice.id,
            call("read", json!({"message_id": notice.id.as_str()})),
        )
        .await
        .expect("the first attempt is a denial, not a storage error");
    let HookDecision::DenyCall { offers, .. } = &denied.decision else {
        panic!("the narrowing read offers acceptance, got {:?}", denied.decision);
    };
    assert!(!offers.is_empty());
    let listed = runtime
        .list_embedded_peer(&family(), &child())
        .expect("the inbox lists");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, notice.id);
    assert_eq!(listed[0].sender, family());
    let accepted = runtime
        .execute_remedy(&actor(Some(&child())), OfferId(offers[0].id.clone()))
        .await;
    assert!(matches!(accepted, RemedyOutcome::Authorized { .. }), "{accepted:?}");
    let finished = runtime
        .read_embedded_peer(
            &actor(Some(&child())),
            "model-2",
            &notice.id,
            call("read", json!({"message_id": notice.id.as_str()})),
        )
        .await
        .expect("a new provider call finishes the denied read");
    assert_eq!(
        finished.decision,
        HookDecision::DeliverValue {
            value: "secret".to_string()
        }
    );
    assert!(
        runtime
            .list_embedded_peer(&family(), &child())
            .expect("list")
            .is_empty()
    );
    let again = runtime
        .read_embedded_peer(
            &actor(Some(&child())),
            "model-3",
            &notice.id,
            call("read", json!({"message_id": notice.id.as_str()})),
        )
        .await;
    assert!(matches!(again, Err(EmbeddedPeerError::Refused(_))), "{again:?}");
}

async fn admit_read(
    runtime: &Runtime,
    id: &appa_runtime::api::EmbeddedPeerId,
    call_id: &str,
    arguments: serde_json::Value,
) -> HookDecision {
    let mut read = runtime
        .read_embedded_peer(&actor(Some(&child())), call_id, id, call("read", arguments.clone()))
        .await
        .expect("the read is claimed");
    if let HookDecision::DenyCall { offers, .. } = &read.decision {
        let accepted = runtime
            .execute_remedy(&actor(Some(&child())), OfferId(offers[0].id.clone()))
            .await;
        assert!(matches!(accepted, RemedyOutcome::Authorized { .. }), "{accepted:?}");
        read = runtime
            .read_embedded_peer(&actor(Some(&child())), call_id, id, call("read", arguments))
            .await
            .expect("the accepted read admits");
    }
    read.decision
}

#[tokio::test]
async fn a_sibling_cannot_take_another_childs_message() {
    let (_dir, runtime) = runtime();
    start(&runtime).await;
    open_child(&runtime).await;
    assert!(matches!(
        release(&runtime, None, "send", "send-iso").await,
        HookDecision::AllowCall { .. }
    ));
    let notice = runtime
        .send_embedded_peer(&family(), &family(), &child(), None, "send-iso", "private")
        .expect("the send records");
    let sibling = runtime.receive_embedded_peer(
        &family(),
        &family(),
        &notice.id,
        &family(),
        &PeerDigest::of_body("private"),
    );
    assert!(matches!(sibling, Err(EmbeddedPeerError::Refused(_))));
    let read = runtime
        .read_embedded_peer(
            &actor(None),
            "read-iso",
            &notice.id,
            call("read", json!({"message_id": notice.id.as_str()})),
        )
        .await;
    assert!(matches!(read, Err(EmbeddedPeerError::Refused(_))));
    assert_eq!(runtime.list_embedded_peer(&family(), &child()).expect("list").len(), 1);
}

#[tokio::test]
async fn a_noop_arrival_returns_the_body_and_a_narrowing_read_folds_only_the_child() {
    let (_dir, runtime) = runtime();
    start(&runtime).await;
    open_child(&runtime).await;
    assert!(matches!(
        release(&runtime, None, "send", "send-top").await,
        HookDecision::AllowCall { .. }
    ));
    let top = runtime
        .send_embedded_peer(&family(), &family(), &child(), None, "send-top", "plain")
        .expect("the top send records");
    let direct = runtime
        .receive_embedded_peer(&family(), &child(), &top.id, &family(), &PeerDigest::of_body("plain"))
        .expect("arrival rechecks");
    assert!(matches!(direct, EmbeddedPeerArrival::Direct { .. }));
    assert!(
        runtime
            .list_embedded_peer(&family(), &child())
            .expect("list")
            .is_empty()
    );

    let blocked = release(&runtime, None, "narrow", "narrow-1").await;
    let HookDecision::DenyCall { offers, .. } = &blocked else {
        panic!("narrowing blocks until accepted, got {blocked:?}");
    };
    let accepted = runtime
        .execute_remedy(&actor(None), OfferId(offers[0].id.clone()))
        .await;
    assert!(matches!(accepted, RemedyOutcome::Authorized { .. }), "{accepted:?}");
    assert!(matches!(
        release(&runtime, None, "narrow", "narrow-1").await,
        HookDecision::AllowCall { .. }
    ));
    result(&runtime, None, "narrow", "narrow-1", "secret").await;
    assert!(matches!(
        release(&runtime, None, "send", "send-narrow").await,
        HookDecision::AllowCall { .. }
    ));
    let narrow = runtime
        .send_embedded_peer(&family(), &family(), &child(), None, "send-narrow", "secret")
        .expect("the narrowing send records");
    let held = runtime
        .receive_embedded_peer(
            &family(),
            &child(),
            &narrow.id,
            &family(),
            &PeerDigest::of_body("secret"),
        )
        .expect("arrival holds");
    assert!(matches!(held, EmbeddedPeerArrival::Held(_)));

    assert!(matches!(
        release(&runtime, Some(&child()), "open", "child-open").await,
        HookDecision::AllowCall { .. }
    ));
    let mut read = runtime
        .read_embedded_peer(
            &actor(Some(&child())),
            "read-1",
            &narrow.id,
            call("read", json!({"message_id": narrow.id.as_str()})),
        )
        .await
        .expect("the read admits");
    if let HookDecision::DenyCall { offers, .. } = &read.decision {
        assert!(!offers.is_empty(), "a narrowing read offers its acceptance");
        assert_eq!(
            read.presentation.as_ref().map(|shown| shown.offers.len()),
            Some(offers.len()),
            "the presentation carries the same offers"
        );
        let accepted = runtime
            .execute_remedy(&actor(Some(&child())), OfferId(offers[0].id.clone()))
            .await;
        assert!(matches!(accepted, RemedyOutcome::Authorized { .. }), "{accepted:?}");
        read = runtime
            .read_embedded_peer(
                &actor(Some(&child())),
                "read-1",
                &narrow.id,
                call("read", json!({"message_id": narrow.id.as_str()})),
            )
            .await
            .expect("the accepted read admits");
    }
    let HookDecision::DeliverValue { value } = &read.decision else {
        panic!("the read delivers the stored body, got {:?}", read.decision);
    };
    assert_eq!(value, "secret");
    let retry = runtime
        .read_embedded_peer(
            &actor(Some(&child())),
            "read-1",
            &narrow.id,
            call("read", json!({"message_id": narrow.id.as_str()})),
        )
        .await
        .expect("the same read retries");
    let HookDecision::DeliverValue { value: retried } = &retry.decision else {
        panic!("the retry delivers the stored body, got {:?}", retry.decision);
    };
    assert_eq!(retried, "secret");
    let other = runtime
        .read_embedded_peer(
            &actor(Some(&child())),
            "read-2",
            &narrow.id,
            call("read", json!({"message_id": narrow.id.as_str()})),
        )
        .await;
    assert!(matches!(other, Err(EmbeddedPeerError::Refused(_))));
    result(&runtime, Some(&child()), "open", "child-open", "still open").await;

    let ended = hooks::handle(
        &runtime,
        HookEvent::ChildEnd {
            root: family(),
            child: child(),
            value: Some("done".to_string()),
        },
    )
    .await;
    assert!(
        matches!(ended, HookDecision::Ack | HookDecision::ChildReturn { .. }),
        "the child return crosses, got {ended:?}"
    );
    let audit = runtime.audit(&family()).expect("the family audits");
    assert!(
        audit.iter().any(|entry| {
            entry.trajectory == child().0
                && matches!(
                    &entry.event,
                    appa_runtime::api::AuditEvent::ChildReturn { label, .. } if label.audience.contains("internal")
                )
        }),
        "the child return carries the peer restriction, audit={audit:?}"
    );
    assert!(
        audit.iter().any(|entry| {
            entry.trajectory == family().0 && matches!(&entry.event, appa_runtime::api::AuditEvent::Merged)
        }) && audit.iter().any(|entry| {
            entry.trajectory == family().0
                && matches!(
                    &entry.event,
                    appa_runtime::api::AuditEvent::Admitted { label } if label.audience.contains("internal")
                )
        }),
        "the parent fold includes the consumed restriction, audit={audit:?}"
    );
}

#[tokio::test]
async fn a_peer_read_refuses_a_non_top_delta_and_an_output_sanitizer() {
    let (_dir, runtime) = runtime();
    start(&runtime).await;
    assert!(matches!(
        release(&runtime, None, "send", "send-1").await,
        HookDecision::AllowCall { .. }
    ));
    let notice = runtime
        .send_embedded_peer(&family(), &family(), &family(), None, "send-1", "secret")
        .expect("the send records");
    let narrowed = runtime
        .read_embedded_peer(
            &actor(None),
            "read-narrow",
            &notice.id,
            call("narrow", json!({"message_id": notice.id.as_str()})),
        )
        .await;
    assert!(
        matches!(narrowed, Err(EmbeddedPeerError::Refused(_))),
        "a non-top delta cannot replace the captured label, got {narrowed:?}"
    );
    let listed = runtime
        .list_embedded_peer(&family(), &family())
        .expect("the inbox lists");
    assert_eq!(listed.len(), 1, "the refused tool does not consume the message");

    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let path = dir.path().join("policy.toml");
    std::fs::write(
        &path,
        r#"
[policy]
version = 2

[[policy.tool]]
name = "send"
delta = {}

[[policy.tool]]
name = "read"
delta = {}

[[policy.sanitizer]]
name = "scrub"
on = ["tool_output"]

[policy.sanitizer.permits]
audience = { from = ["internal"], to = ["public"] }

[policy.deployment]
context_control = true

[externals]
timeout_ms = 2000
max_body_bytes = 65536

[externals.sanitizers.scrub]
builtin = "redact-email"
"#,
    )
    .expect("the sanitizer policy writes");
    let scrubbed = Runtime::open(
        Config::load(&path).expect("the sanitizer policy loads"),
        dir.path().join("appa.db"),
        None,
    )
    .expect("the runtime opens");
    start(&scrubbed).await;
    assert!(matches!(
        release(&scrubbed, None, "send", "send-1").await,
        HookDecision::AllowCall { .. }
    ));
    let notice = scrubbed
        .send_embedded_peer(&family(), &family(), &family(), None, "send-1", "secret")
        .expect("the send records");
    let refused = scrubbed
        .read_embedded_peer(
            &actor(None),
            "read-1",
            &notice.id,
            call("read", json!({"message_id": notice.id.as_str()})),
        )
        .await;
    assert!(
        matches!(refused, Err(EmbeddedPeerError::Refused(_))),
        "an output sanitizer cannot replace the captured label, got {refused:?}"
    );
}
