mod common;
use common::{actor, last_offer, propose, ran, raw, root};

use std::sync::Arc;

use appa_runtime::api::{RemedyOutcome, Runtime};
use appa_runtime::{config::Config, hooks};
use appa_runtime_api::{Actor, HookDecision, HookEvent, ProposedCall, TrajectoryId};

/// No audience source at all: `self` has members only when a session names its principal.
const POLICY: &str = r#"
[policy]
version = 2

[[policy.tool]]
name = "read_secret"
delta = { audience = ["self"] }

[[policy.tool]]
name = "send"
parameters = { type = "object", properties = { to = { type = "string" } }, required = ["to"] }
requires = { audience = { contains = ["$to"] } }
effects = ["egress"]
delta = {}

[externals]
timeout_ms = 1000
max_body_bytes = 4096
"#;

const ALICE: &str = "alice@corp.example";

fn open(dir: &tempfile::TempDir) -> Arc<Runtime> {
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, POLICY).expect("the fixture writes");
    let config = Config::load(&path).expect("the fixture validates");
    Arc::new(Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens"))
}

async fn start(runtime: &Arc<Runtime>, principal: Option<&str>) -> HookDecision {
    hooks::handle(
        runtime,
        HookEvent::SessionStart {
            root: root(),
            principal: principal.map(str::to_string),
        },
    )
    .await
}

fn read_secret() -> ProposedCall {
    ProposedCall {
        tool: "read_secret".to_string(),
        arguments: raw(serde_json::json!({})),
        cwd: None,
    }
}

fn send(to: &str) -> ProposedCall {
    ProposedCall {
        tool: "send".to_string(),
        arguments: raw(serde_json::json!({ "to": to })),
        cwd: None,
    }
}

/// Narrow the root to `self` through the accepted read.
async fn narrow_to_self(runtime: &Arc<Runtime>) {
    let HookDecision::DenyCall { feedback, .. } = propose(runtime, read_secret()).await else {
        panic!("the narrowing read is offered for acceptance");
    };
    assert!(matches!(
        runtime.execute_remedy(&actor(), last_offer(&feedback)).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(runtime, read_secret()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(runtime, read_secret()).await;
}

#[tokio::test]
async fn the_session_principal_is_the_self_audience() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = open(&dir);
    assert_eq!(start(&runtime, Some(ALICE)).await, HookDecision::Ack);
    narrow_to_self(&runtime).await;

    assert_eq!(
        propose(&runtime, send(ALICE)).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert!(matches!(
        propose(&runtime, send("bob@corp.example")).await,
        HookDecision::DenyCall { .. }
    ));
}

/// Without a principal the policy's own `self` sources answer, and this policy has none: the
/// same fail-closed gap as before principals existed.
#[tokio::test]
async fn without_a_principal_an_unsourced_self_denies_as_a_policy_gap() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = open(&dir);
    assert_eq!(start(&runtime, None).await, HookDecision::Ack);
    narrow_to_self(&runtime).await;

    let HookDecision::DenyCall { feedback, offers, .. } = propose(&runtime, send(ALICE)).await else {
        panic!("the send is denied");
    };
    assert!(feedback.contains("[policy.audience] self"), "{feedback}");
    assert!(offers.is_empty());
}

#[tokio::test]
async fn a_principal_that_is_not_an_address_refuses_the_start() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = open(&dir);
    for malformed in ["alice", "self", "@team", "slack:U1"] {
        assert!(
            matches!(start(&runtime, Some(malformed)).await, HookDecision::Refuse { .. }),
            "{malformed:?}"
        );
    }
    assert_eq!(start(&runtime, Some(ALICE)).await, HookDecision::Ack);
}

/// A reopen keeps the principal the opening pinned: naming it again or naming none continues,
/// naming another refuses.
#[tokio::test]
async fn a_reopened_session_cannot_change_its_principal() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = open(&dir);
    assert_eq!(start(&runtime, Some(ALICE)).await, HookDecision::Ack);
    assert!(matches!(
        start(&runtime, Some("bob@corp.example")).await,
        HookDecision::Refuse { .. }
    ));
    assert_eq!(start(&runtime, None).await, HookDecision::Ack);
    assert_eq!(start(&runtime, Some("alice@CORP.example")).await, HookDecision::Ack);

    narrow_to_self(&runtime).await;
    assert_eq!(
        propose(&runtime, send(ALICE)).await,
        HookDecision::AllowCall { spawn: None }
    );
}

#[tokio::test]
async fn a_root_fork_acts_for_its_parents_principal() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = open(&dir);
    assert_eq!(start(&runtime, Some(ALICE)).await, HookDecision::Ack);
    narrow_to_self(&runtime).await;

    let fork = TrajectoryId("test-fork".to_string());
    runtime.open_root_fork(&root(), &root(), &fork).expect("the root forks");
    let on_fork = |call: ProposedCall| {
        hooks::handle(
            &runtime,
            HookEvent::ToolCall {
                actor: Actor {
                    root: fork.clone(),
                    child: None,
                },
                call,
                call_id: None,
                spawn: false,
                ruling: None,
            },
        )
    };
    assert_eq!(on_fork(send(ALICE)).await, HookDecision::AllowCall { spawn: None });
    assert!(matches!(
        on_fork(send("bob@corp.example")).await,
        HookDecision::DenyCall { .. }
    ));
}
