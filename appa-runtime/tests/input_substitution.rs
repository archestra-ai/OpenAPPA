mod common;
use common::{offers, raw};

use std::sync::Arc;

use appa_runtime::api::{AuditEvent, DispatchOutcome, OfferId, RemedyOutcome, Runtime};
use appa_runtime::{config::Config, hooks};
use appa_runtime_api::{Actor, HookDecision, HookEvent, OutcomeBody, ProposedCall, ToolOutcome, TrajectoryId};

const POLICY: &str = r#"
[policy]
version = 1

[[policy.tool]]
name = "read_hr"
delta = { audience = ["hr"] }

[[policy.tool]]
name = "send"
parameters = { type = "object", properties = { body = { type = "string" } }, required = ["body"] }
requires = { audience = { contains = ["public"] } }
effects = ["egress"]
delta = {}

[[policy.sanitizer]]
name = "redactor"
on = ["tool_input"]
permits = { audience = { from = ["hr"], to = ["public"] } }

[externals]
timeout_ms = 1000
max_body_bytes = 4096

[externals.sanitizers.redactor]
builtin = "redact-email"
"#;

const RAW_BODY: &str = "mail alice@corp.example today";
const REDACTED_BODY: &str = "mail [redacted-email] today";

fn root() -> TrajectoryId {
    TrajectoryId("substitution-test".to_string())
}

fn actor() -> Actor {
    Actor {
        root: root(),
        child: None,
    }
}

fn read_hr() -> ProposedCall {
    ProposedCall {
        tool: "read_hr".to_string(),
        arguments: raw(serde_json::json!({})),
    }
}

fn send(body: &str) -> ProposedCall {
    ProposedCall {
        tool: "send".to_string(),
        arguments: raw(serde_json::json!({"body": body})),
    }
}

async fn propose(runtime: &Arc<Runtime>, call: ProposedCall) -> HookDecision {
    hooks::handle(
        runtime,
        HookEvent::ToolCall {
            actor: actor(),
            call,
            spawn: false,
        },
    )
    .await
}

async fn report(runtime: &Arc<Runtime>, call: ProposedCall, body: &str) -> HookDecision {
    hooks::handle(
        runtime,
        HookEvent::ToolResult {
            actor: actor(),
            call,
            outcome: ToolOutcome::Success {
                body: OutcomeBody::Available(body.to_string()),
            },
        },
    )
    .await
}

fn feedback_of(decision: &HookDecision) -> String {
    match decision {
        HookDecision::DenyCall { feedback, .. } => feedback.clone(),
        other => panic!("expected a deny carrying feedback, got {other:?}"),
    }
}

fn last_offer(feedback: &str) -> OfferId {
    offers(feedback)
        .last()
        .cloned()
        .unwrap_or_else(|| panic!("no offer id in feedback: {feedback}"))
}

async fn narrowed_and_blocked(dir: &tempfile::TempDir) -> (Arc<Runtime>, OfferId) {
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, POLICY).expect("the fixture writes");
    let config = Config::load(&path).expect("the fixture validates");
    let runtime = Arc::new(Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens"));
    assert_eq!(
        hooks::handle(&runtime, HookEvent::SessionStart { root: root() }).await,
        HookDecision::Ack
    );

    let blocked = propose(&runtime, read_hr()).await;
    let accept = last_offer(&feedback_of(&blocked));
    assert!(matches!(
        runtime.execute_remedy(&actor(), accept).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, read_hr()).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert_eq!(report(&runtime, read_hr(), "Alice Chen").await, HookDecision::Ack);

    let blocked = propose(&runtime, send(RAW_BODY)).await;
    let hop = last_offer(&feedback_of(&blocked));
    (runtime, hop)
}

#[tokio::test]
async fn the_replaced_call_runs_through_the_hooks_and_closes() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (runtime, hop) = narrowed_and_blocked(&dir).await;

    let RemedyOutcome::Substituted { call } = runtime.execute_remedy(&actor(), hop.clone()).await else {
        panic!("the input sanitizer's hop substitutes the call");
    };
    assert_eq!(call.tool, "send");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(call.arguments.get()).expect("canonical JSON"),
        serde_json::json!({"body": REDACTED_BODY}),
    );

    assert_eq!(
        propose(&runtime, send(REDACTED_BODY)).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert_eq!(report(&runtime, send(REDACTED_BODY), "sent").await, HookDecision::Ack);

    assert!(matches!(
        runtime.execute_remedy(&actor(), hop).await,
        RemedyOutcome::Declined { .. }
    ));
    assert!(matches!(
        propose(&runtime, send(RAW_BODY)).await,
        HookDecision::DenyCall { .. }
    ));

    let entries = runtime.audit(&root()).expect("the audit reads");
    let released: Vec<_> = entries
        .iter()
        .filter_map(|entry| match &entry.event {
            AuditEvent::Released { tool, effects, .. } if tool == "send" => Some(effects.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(released, vec![vec!["egress".to_string()]], "{entries:?}");
    assert!(
        entries.iter().any(|entry| matches!(
            &entry.event,
            AuditEvent::Closed {
                outcome: DispatchOutcome::Ran { .. }
            }
        )),
        "the replaced call closed as run: {entries:?}"
    );
}

#[tokio::test]
async fn another_call_abandons_the_standing_replaced_call() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (runtime, hop) = narrowed_and_blocked(&dir).await;
    assert!(matches!(
        runtime.execute_remedy(&actor(), hop).await,
        RemedyOutcome::Substituted { .. }
    ));

    assert!(matches!(
        propose(&runtime, read_hr()).await,
        HookDecision::DenyCall { .. }
    ));
    assert_eq!(
        propose(&runtime, read_hr()).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert_eq!(report(&runtime, read_hr(), "Alice Chen").await, HookDecision::Ack);

    let entries = runtime.audit(&root()).expect("the audit reads");
    assert!(
        entries.iter().any(|entry| matches!(
            &entry.event,
            AuditEvent::Closed {
                outcome: DispatchOutcome::Failed
            }
        )),
        "the abandoned replaced call closed as not run: {entries:?}"
    );
    assert!(
        !entries
            .iter()
            .any(|entry| matches!(&entry.event, AuditEvent::EffectsCommitted { .. })),
        "no effect of the replaced call committed: {entries:?}"
    );
}

#[tokio::test]
async fn the_standing_replaced_call_survives_a_reopen() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (runtime, hop) = narrowed_and_blocked(&dir).await;
    assert!(matches!(
        runtime.execute_remedy(&actor(), hop).await,
        RemedyOutcome::Substituted { .. }
    ));
    drop(runtime);

    let config = Config::load(&dir.path().join("appa.toml")).expect("the fixture validates");
    let runtime = Arc::new(Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment reopens"));
    assert_eq!(
        propose(&runtime, send(REDACTED_BODY)).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert_eq!(report(&runtime, send(REDACTED_BODY), "sent").await, HookDecision::Ack);
}
