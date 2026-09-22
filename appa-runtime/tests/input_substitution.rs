mod common;
use common::{actor, last_offer, propose, raw, root};

use std::sync::Arc;

use appa_runtime::api::{AuditEvent, DispatchOutcome, OfferId, RemedyOutcome, Runtime};
use appa_runtime::{config::Config, hooks};
use appa_runtime_api::{HookDecision, HookEvent, OutcomeBody, ProposedCall, ToolOutcome};

const POLICY: &str = r#"
[policy]
version = 2

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
hint = "Remove email addresses before sending."
permits = { audience = { from = ["hr"], to = ["public"] } }

[externals]
timeout_ms = 1000
max_body_bytes = 4096

[externals.sanitizers.redactor]
builtin = "redact-email"
"#;

const RAW_BODY: &str = "mail alice@corp.example today";
const REDACTED_BODY: &str = "mail [redacted-email] today";

fn read_hr() -> ProposedCall {
    ProposedCall {
        tool: "read_hr".to_string(),
        arguments: raw(serde_json::json!({})),
        cwd: None,
    }
}

fn send(body: &str) -> ProposedCall {
    ProposedCall {
        tool: "send".to_string(),
        arguments: raw(serde_json::json!({"body": body})),
        cwd: None,
    }
}

async fn report(runtime: &Arc<Runtime>, call: ProposedCall, body: &str) -> HookDecision {
    hooks::handle(
        runtime,
        HookEvent::ToolResult {
            actor: actor(),
            call,
            call_id: None,
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

    let blocked = hooks::handle_embedded(
        &runtime,
        HookEvent::ToolCall {
            actor: actor(),
            call: send(RAW_BODY),
            call_id: None,
            spawn: false,
            ruling: None,
        },
    )
    .await;
    let presentation = blocked
        .presentation
        .expect("the input block carries typed remedy metadata");
    let sanitizer = presentation
        .offers
        .iter()
        .find_map(|offer| offer.input_sanitizer.as_ref())
        .expect("the input-sanitizer offer identifies its sanitizer");
    assert_eq!(sanitizer.name, "redactor");
    assert_eq!(sanitizer.target, "send");
    assert_eq!(
        sanitizer.description.as_deref(),
        Some("Remove email addresses before sending.")
    );
    assert!(
        presentation
            .feedback
            .contains("Use sanitizer redactor to rewrite the arguments for send")
    );
    assert!(!presentation.feedback.contains("Apply the offered remedy"));
    let hop = OfferId(
        presentation
            .offers
            .first()
            .expect("the block offers the input rewrite")
            .id
            .clone(),
    );
    (runtime, hop)
}

/// No `send` was released and no effect of one committed: the remedy staged a derivation and
/// decided nothing in advance. Asserted before every re-proposal, so a regression that went back
/// to releasing the call at remedy time cannot hide behind the later assertions.
fn nothing_ran(runtime: &Runtime) -> bool {
    runtime.audit(&root()).expect("the audit reads").iter().all(|entry| {
        !matches!(
            &entry.event,
            AuditEvent::Released { tool, .. } if tool == "send"
        ) && !matches!(&entry.event, AuditEvent::EffectsCommitted { .. })
    })
}

#[tokio::test]
async fn the_replaced_call_runs_through_the_hooks_and_closes() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (runtime, hop) = narrowed_and_blocked(&dir).await;

    let RemedyOutcome::Authorized { call } = runtime.execute_remedy(&actor(), hop.clone()).await else {
        panic!("the input sanitizer's hop approves the substituted call");
    };
    assert_eq!(call.tool, "send");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(call.arguments.get()).expect("canonical JSON"),
        serde_json::json!({"body": REDACTED_BODY}),
    );
    assert!(
        nothing_ran(&runtime),
        "the remedy stages a derivation and runs nothing: {:?}",
        runtime.audit(&root()).expect("the audit reads")
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

/// Another call does not cost the model the remedy it already paid for: a derivation is a
/// record, not a call in flight, and nothing about an unrelated call touches it.
#[tokio::test]
async fn another_call_leaves_the_staged_derivation_alone() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (runtime, hop) = narrowed_and_blocked(&dir).await;
    assert!(matches!(
        runtime.execute_remedy(&actor(), hop).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert!(nothing_ran(&runtime), "the remedy runs nothing of its own");

    assert_eq!(
        propose(&runtime, read_hr()).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert_eq!(report(&runtime, read_hr(), "Alice Chen").await, HookDecision::Ack);

    let entries = runtime.audit(&root()).expect("the audit reads");
    assert!(
        !entries.iter().any(|entry| matches!(
            &entry.event,
            AuditEvent::Closed {
                outcome: DispatchOutcome::Failed
            }
        )),
        "nothing was abandoned: {entries:?}"
    );
    assert_eq!(
        propose(&runtime, send(REDACTED_BODY)).await,
        HookDecision::AllowCall { spawn: None },
        "the derivation still stands and the proposal takes it"
    );
}

#[tokio::test]
async fn the_staged_derivation_survives_a_reopen() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (runtime, hop) = narrowed_and_blocked(&dir).await;
    assert!(matches!(
        runtime.execute_remedy(&actor(), hop).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert!(nothing_ran(&runtime), "the remedy runs nothing of its own");
    drop(runtime);

    let config = Config::load(&dir.path().join("appa.toml")).expect("the fixture validates");
    let runtime = Arc::new(Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment reopens"));
    assert_eq!(
        propose(&runtime, send(REDACTED_BODY)).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert_eq!(report(&runtime, send(REDACTED_BODY), "sent").await, HookDecision::Ack);
}
