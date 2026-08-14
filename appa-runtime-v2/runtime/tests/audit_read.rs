
use std::sync::Arc;

use appa_runtime_api::{Actor, HookDecision, HookEvent, OutcomeBody, ProposedCall, ToolOutcome, TrajectoryId};
use appa_runtime_v2::api::{AuditEntry, AuditEvent, AuditLabel, DispatchOutcome, OfferId, RemedyOutcome, Runtime};
use appa_runtime_v2::{config::Config, hooks};

const POLICY: &str = r#"
[policy]
version = 1
trust_chain = ["suspicious", "internal"]

[[policy.tool]]
name = "publish"
requires = { audience = { includes = ["public"] } }
effects = ["egress"]
delta = {}

[[policy.tool]]
name = "read_hr"
delta = { audience = { exactly = ["hr"] } }

[[policy.tool]]
name = "delegate"
parameters = { type = "object", properties = { task = { type = "string" } } }

[[policy.sanitizer]]
name = "redactor"
on = ["tool_output"]
mandate = { audience = { from = { includes = ["hr"] }, to = { exactly = ["public"] } } }

[policy.deployment]
context_control = true
confined_child_return = true

[externals]
timeout_ms = 1000
max_body_bytes = 4096

[externals.sanitizers.redactor]
builtin = "redact-email"
"#;

fn raw(value: serde_json::Value) -> Box<serde_json::value::RawValue> {
    serde_json::value::to_raw_value(&value).expect("the fixture serializes")
}

fn call(tool: &str) -> ProposedCall {
    ProposedCall {
        tool: tool.to_string(),
        arguments: raw(serde_json::json!({"file": "alice.md"})),
    }
}

fn actor(child: Option<&TrajectoryId>) -> Actor {
    Actor {
        root: root(),
        child: child.cloned(),
    }
}

fn root() -> TrajectoryId {
    TrajectoryId("audit-test".to_string())
}

fn child() -> TrajectoryId {
    TrajectoryId("audit-test:c1".to_string())
}

async fn deployment(dir: &tempfile::TempDir) -> Arc<Runtime> {
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, POLICY).expect("the fixture writes");
    let config = Config::load(&path).expect("the fixture validates");
    let runtime = Arc::new(Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens"));
    let started = hooks::handle(&runtime, HookEvent::SessionStart { root: root() }).await;
    assert_eq!(started, HookDecision::Ack);
    runtime
}

async fn propose(runtime: &Arc<Runtime>, within: Option<&TrajectoryId>, call: ProposedCall) -> HookDecision {
    hooks::handle(
        runtime,
        HookEvent::ToolCall {
            actor: actor(within),
            call,
        },
    )
    .await
}

async fn released(runtime: &Arc<Runtime>, within: Option<&TrajectoryId>, tool: &str, body: &str) {
    let call = call(tool);
    let decision = propose(runtime, within, call.clone()).await;
    assert_eq!(decision, HookDecision::AllowCall, "{tool} must be released here");
    hooks::handle(
        runtime,
        HookEvent::ToolResult {
            actor: actor(within),
            call,
            outcome: ToolOutcome::Success {
                body: OutcomeBody::Available(body.to_string()),
            },
        },
    )
    .await;
}

fn offer_for(feedback: &str, wanted: &str) -> OfferId {
    let lines: Vec<&str> = feedback.lines().collect();
    let start = lines
        .iter()
        .position(|line| line.contains(wanted))
        .unwrap_or_else(|| panic!("no option mentioning {wanted:?} in: {feedback}"));
    let id = lines[start..]
        .iter()
        .find_map(|line| opaque_offer_id(line))
        .unwrap_or_else(|| panic!("the {wanted:?} option has no offer id in: {feedback}"));
    OfferId(id)
}

fn first_offer(feedback: &str) -> OfferId {
    OfferId(opaque_offer_id(feedback).unwrap_or_else(|| panic!("no offer id in feedback: {feedback}")))
}

fn opaque_offer_id(text: &str) -> Option<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric() && c != '-')
        .find(|word| word.starts_with("offer-") && word.len() > "offer-".len())
        .map(str::to_string)
}

fn feedback_of(decision: &HookDecision) -> String {
    match decision {
        HookDecision::DenyCall { feedback } | HookDecision::Block { reason: feedback } => feedback.clone(),
        other => panic!("expected a refusal carrying feedback, got {other:?}"),
    }
}

fn events(entries: &[AuditEntry]) -> Vec<&AuditEvent> {
    entries.iter().map(|entry| &entry.event).collect()
}

fn label(trust: &str, audience: &str) -> AuditLabel {
    AuditLabel {
        trust: trust.to_string(),
        audience: audience.to_string(),
    }
}

#[tokio::test]
async fn a_released_call_records_its_label_its_effects_and_what_it_admitted() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = deployment(&dir).await;

    released(&runtime, None, "publish", "posted").await;

    let entries = runtime.audit(&root()).expect("the audit reads");
    assert!(
        entries.iter().all(|entry| entry.trajectory == "audit-test"),
        "a root-only run records under the root: {entries:?}",
    );
    assert_eq!(
        events(&entries),
        vec![
            &AuditEvent::Released {
                tool: "publish".to_string(),
                label: label("internal", "public"),
                effects: vec!["egress".to_string()],
            },
            &AuditEvent::EffectsCommitted {
                effects: vec!["egress".to_string()],
            },
            &AuditEvent::Closed {
                outcome: DispatchOutcome::Ran { effects: Vec::new() },
            },
            &AuditEvent::Admitted {
                label: label("internal", "public"),
            },
        ],
    );
}

#[tokio::test]
async fn a_blocked_call_leaves_no_entry() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = deployment(&dir).await;

    released(&runtime, None, "publish", "posted").await;
    let before = runtime.audit(&root()).expect("the audit reads").len();

    let blocked = propose(&runtime, None, call("read_hr")).await;
    assert!(
        matches!(blocked, HookDecision::DenyCall { .. }),
        "the fixture depends on this call blocking, got {blocked:?}",
    );

    let after = runtime.audit(&root()).expect("the audit reads");
    assert_eq!(after.len(), before, "a refused call adds no entry: {after:?}");
}

#[tokio::test]
async fn an_accepted_narrowing_records_where_the_label_moved() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = deployment(&dir).await;

    let blocked = propose(&runtime, None, call("read_hr")).await;
    let offer = first_offer(&feedback_of(&blocked));
    assert!(matches!(
        runtime.execute_remedy(offer).await,
        RemedyOutcome::Authorized { .. }
    ));
    released(&runtime, None, "read_hr", "Alice Chen, alice@corp.example").await;

    let entries = runtime.audit(&root()).expect("the audit reads");
    assert_eq!(
        entries.first().map(|entry| &entry.event),
        Some(&AuditEvent::Narrowed {
            from: label("internal", "public"),
            to: label("internal", "hr"),
        }),
        "the acceptance is the first thing recorded: {entries:?}",
    );
    assert!(
        entries.iter().any(|entry| entry.event
            == AuditEvent::Admitted {
                label: label("internal", "hr"),
            }),
        "the value enters at the narrowed label: {entries:?}",
    );
}

#[tokio::test]
async fn a_branch_records_its_seed_its_own_flows_and_how_its_return_crossed() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = deployment(&dir).await;

    let spawn = ProposedCall {
        tool: "delegate".to_string(),
        arguments: raw(serde_json::json!({"task": "look Alice up"})),
    };
    assert_eq!(propose(&runtime, None, spawn).await, HookDecision::AllowCall);
    let opened = hooks::handle(
        &runtime,
        HookEvent::ChildStart {
            parent: root(),
            child: child(),
        },
    )
    .await;
    assert_eq!(opened, HookDecision::Ack);

    let blocked = propose(&runtime, Some(&child()), call("read_hr")).await;
    let offer = first_offer(&feedback_of(&blocked));
    assert!(matches!(
        runtime.execute_remedy(offer).await,
        RemedyOutcome::Authorized { .. }
    ));
    released(&runtime, Some(&child()), "read_hr", "Alice Chen, alice@corp.example").await;

    let end = HookEvent::ChildEnd {
        parent: root(),
        child: child(),
        value: Some("Alice Chen, alice@corp.example".to_string()),
    };
    let stopped = hooks::handle(&runtime, end).await;
    let derivation = offer_for(&feedback_of(&stopped), "redactor");
    assert!(matches!(
        runtime.execute_remedy(derivation).await,
        RemedyOutcome::Returned { .. }
    ));

    let entries = runtime.audit(&root()).expect("the audit reads");
    assert_eq!(
        entries
            .iter()
            .find(|entry| matches!(entry.event, AuditEvent::Forked { .. })),
        Some(&AuditEntry {
            trajectory: "audit-test:c1".to_string(),
            event: AuditEvent::Forked {
                parent: "audit-test".to_string(),
                seed: label("internal", "public"),
            },
        }),
        "the child inherits the parent's label at the fork",
    );
    assert!(
        entries.iter().any(|entry| {
            entry.trajectory == "audit-test:c1"
                && matches!(&entry.event, AuditEvent::Released { tool, .. } if tool == "read_hr")
        }),
        "the child's own flows record under the child: {entries:?}",
    );
    assert!(
        entries.iter().any(|entry| entry.event
            == AuditEvent::ChildReturn {
                sanitizer: Some("redactor".to_string()),
                label: label("internal", "public"),
            }),
        "the crossing names the derivation that carried it: {entries:?}",
    );
}

#[tokio::test]
async fn only_a_root_names_the_audit() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = deployment(&dir).await;
    let spawn = ProposedCall {
        tool: "delegate".to_string(),
        arguments: raw(serde_json::json!({"task": "look Alice up"})),
    };
    assert_eq!(propose(&runtime, None, spawn).await, HookDecision::AllowCall);
    let opened = hooks::handle(
        &runtime,
        HookEvent::ChildStart {
            parent: root(),
            child: child(),
        },
    )
    .await;
    assert_eq!(opened, HookDecision::Ack);

    assert!(runtime.audit(&root()).is_some());
    assert!(runtime.audit(&child()).is_none(), "a child shares its family's log");
    assert!(runtime.audit(&TrajectoryId("nobody".to_string())).is_none());
}
