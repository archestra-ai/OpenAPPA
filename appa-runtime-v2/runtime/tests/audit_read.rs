
use std::sync::Arc;

use appa_runtime_api::{
    Actor, HookDecision, HookEvent, OutcomeBody, ProposedCall, SpawnRef, ToolOutcome, TrajectoryId,
};
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
            spawn: false,
        },
    )
    .await
}

async fn released(runtime: &Arc<Runtime>, within: Option<&TrajectoryId>, tool: &str, body: &str) {
    let call = call(tool);
    let decision = propose(runtime, within, call.clone()).await;
    assert_eq!(
        decision,
        HookDecision::AllowCall { spawn: None },
        "{tool} must be released here"
    );
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

async fn open_child(runtime: &Arc<Runtime>, spawn: ProposedCall) -> HookDecision {
    let released = hooks::handle(
        runtime,
        HookEvent::ToolCall {
            actor: actor(None),
            call: spawn,
            spawn: true,
        },
    )
    .await;
    let HookDecision::AllowCall { spawn: Some(binding) } = released else {
        panic!("a context-controlled spawn releases a fork binding, got {released:?}");
    };
    hooks::handle(
        runtime,
        HookEvent::ChildStart {
            root: root(),
            child: child(),
            spawn: SpawnRef::Binding(binding),
        },
    )
    .await
}

fn first_offer(feedback: &str) -> OfferId {
    OfferId(opaque_offer_id(feedback).unwrap_or_else(|| panic!("no offer id in feedback: {feedback}")))
}

fn all_offers(feedback: &str) -> Vec<OfferId> {
    feedback.lines().filter_map(opaque_offer_id).map(OfferId).collect()
}

fn opaque_offer_id(text: &str) -> Option<String> {
    let after = text.split("offer_id:").nth(1)?;
    let rest = after.trim_start().strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
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
            &AuditEvent::Closed {
                outcome: DispatchOutcome::Ran {
                    effects: vec!["egress".to_string()],
                },
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
    assert_eq!(open_child(&runtime, spawn).await, HookDecision::Ack);

    let blocked = propose(&runtime, Some(&child()), call("read_hr")).await;
    let offer = first_offer(&feedback_of(&blocked));
    assert!(matches!(
        runtime.execute_remedy(offer).await,
        RemedyOutcome::Authorized { .. }
    ));
    released(&runtime, Some(&child()), "read_hr", "Alice Chen, alice@corp.example").await;

    let end = HookEvent::ChildEnd {
        root: root(),
        child: child(),
        value: Some("Alice Chen, alice@corp.example".to_string()),
    };
    let stopped = hooks::handle(&runtime, end).await;
    let derivation = all_offers(&feedback_of(&stopped))
        .into_iter()
        .next()
        .expect("the stop surfaces the derivation plan");
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

const ATTEST_POLICY: &str = r#"
[policy]
version = 1

[[policy.tool]]
name = "spawn"
delta = {}

[[policy.tool]]
name = "read_untrusted"
delta = { trust = "suspicious" }

[[policy.sanitizer]]
name = "attest-schema"
on = ["tool_output"]
[policy.sanitizer.mandate]
trust = { from = "suspicious", to = "trusted" }

[policy.child]
return_sanitizer = "attest-schema"

[policy.deployment]
context_control = true
confined_child_return = true

[externals]
timeout_ms = 1000
max_body_bytes = 4096
"#;

async fn attest_deployment(dir: &tempfile::TempDir) -> Arc<Runtime> {
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, ATTEST_POLICY).expect("the fixture writes");
    let config = Config::load(&path).expect("the fixture validates");
    let runtime = Arc::new(Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens"));
    let started = hooks::handle(&runtime, HookEvent::SessionStart { root: root() }).await;
    assert_eq!(started, HookDecision::Ack);
    runtime
}

#[tokio::test]
async fn a_child_bound_attest_schema_return_crosses_in_engine() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = attest_deployment(&dir).await;

    let spawn = ProposedCall {
        tool: "spawn".to_string(),
        arguments: raw(serde_json::json!({
            "return_schema": {
                "type": "object",
                "properties": { "verdict": { "type": "string", "enum": ["allow", "deny"] } },
                "required": ["verdict"],
            }
        })),
    };
    assert_eq!(open_child(&runtime, spawn).await, HookDecision::Ack);

    let blocked = propose(&runtime, Some(&child()), call("read_untrusted")).await;
    let accept = first_offer(&feedback_of(&blocked));
    assert!(matches!(
        runtime.execute_remedy(accept).await,
        RemedyOutcome::Authorized { .. }
    ));
    released(&runtime, Some(&child()), "read_untrusted", "raw notes").await;

    let end = HookEvent::ChildEnd {
        root: root(),
        child: child(),
        value: Some(r#"{"verdict":"allow"}"#.to_string()),
    };
    assert_eq!(hooks::handle(&runtime, end).await, HookDecision::Ack);

    let entries = runtime.audit(&root()).expect("the audit reads");
    assert!(
        entries.iter().any(|entry| entry.event
            == AuditEvent::ChildReturn {
                sanitizer: Some("attest-schema".to_string()),
                label: label("trusted", "public"),
            }),
        "the reserved builtin carried the crossing in-engine: {entries:?}",
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
    assert_eq!(open_child(&runtime, spawn).await, HookDecision::Ack);

    assert!(runtime.audit(&root()).is_some());
    assert!(runtime.audit(&child()).is_none(), "a child shares its family's log");
    assert!(runtime.audit(&TrajectoryId("nobody".to_string())).is_none());
}
