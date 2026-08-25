//! The bench policy's three branch-critical mechanics, end to end: the real
//! [`appa_example_agent::Agent`] loop, the real runtime over the shipped policy, the
//! real shim executing real `corp-systems` calls over a scratch corpus. Only
//! inference is scripted, and the script cannot cheat — an offer id is
//! minted per block (`RUL-6`), so the scripted model reads each one out of
//! the feedback exactly as a model would.
//!
//! 1. Forum taint is terminal in a trajectory: once a root accepts the
//!    suspicious narrowing, `create_task_tracker` never runs there.
//! 2. A fork confines the taint: the child reads and replies, and its egress
//!    is family history, so the untainted parent can file the ticket.
//! 3. An hr-audience child return crosses only as the `pii-redactor`
//!    derivation; the parent keeps its public audience and may still email.

mod model;

use std::sync::Arc;

use appa_example_agent::wire::WireMessage;
use appa_example_agent::{
    Agent, ArgumentKey, Limits, OpenAiCompatible, OpenAiConfig, Outcome, SpawnTool, ToolCatalogue, ToolName, ToolShim,
    TranscriptHead,
};
use appa_runtime::api::{AuditEntry, AuditEvent, Runtime, TrajectoryId};
use appa_runtime::config::{Config, Implementation};
use corp_systems::systems::System;
use corporate_agent_demo::catalogue;
use corporate_agent_demo::shim::{self, CorpWorld};
use model::Provider;

const FORK_POLICY: &str = include_str!("../../../bench/corp/policies/appa.toml");
const HR_RECORD: &str = "Onboarding buddy: Priya Sharma (ext 4471)\n";
const FORUM_THREAD: &str = "nightly deploy times out since v2.31; 45 minutes fixes it\n";

/// The shared policy's trust and destination-store gate, and the
/// `reply-and-file-ticket` deployment posture that adds prior egress: a change
/// ticket follows the public acknowledgement it responds to. The bench
/// declares that history clause only in the scenario that tests it, while the
/// anti-laundering destination gate remains shared.
const SHARED_TICKET_REQUIRES: &str = r#"requires = { trust = "internal", audience = { contains = ["public"] } }"#;
const TICKET_REQUIRES_PRIOR_EGRESS: &str =
    r#"requires = { trust = "internal", audience = { contains = ["public"] }, effects = { contains = ["egress"] } }"#;

fn ticket_gated_on_prior_egress() -> String {
    assert_eq!(
        FORK_POLICY.matches(SHARED_TICKET_REQUIRES).count(),
        1,
        "the ticket's shared sink gate has one place for the scenario history requirement to land",
    );
    FORK_POLICY.replace(SHARED_TICKET_REQUIRES, TICKET_REQUIRES_PRIOR_EGRESS)
}

fn root() -> TrajectoryId {
    TrajectoryId("fork-scenarios".to_string())
}

/// Everything one episode needs, alive for its whole run.
struct Episode {
    agent: Agent,
    runtime: Arc<Runtime>,
    _dir: tempfile::TempDir,
}

async fn episode(policy: &str, provider: &Provider) -> Episode {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let data = dir.path().join("data");
    std::fs::create_dir_all(data.join("hr")).expect("the corpus writes");
    std::fs::create_dir_all(data.join("public_forum")).expect("the corpus writes");
    std::fs::write(data.join("hr/alice-chen.md"), HR_RECORD).expect("the hr record writes");
    std::fs::write(data.join("public_forum/thread.md"), FORUM_THREAD).expect("the thread writes");

    let path = dir.path().join("appa.toml");
    std::fs::write(&path, policy).expect("the policy writes");
    let mut config = Config::load(&path).expect("the policy is a loadable deployment");
    let compiled = appa_policy::Config::from_toml_str(
        &toml::to_string(config.policy_file().value()).expect("the policy re-renders"),
    )
    .expect("the policy compiles");

    let address = shim::serve(CorpWorld {
        data_root: data,
        sink_root: dir.path().join("sink"),
        enabled: System::ALL.into_iter().collect(),
    })
    .await
    .expect("the shim binds");
    let origin = format!("http://{address}");
    for implementation in config.externals.sanitizers.values_mut() {
        if let Implementation::Resolver(endpoint) = implementation
            && let Some(path) = endpoint.url.strip_prefix("http://127.0.0.1:0")
            && shim::serves(path)
        {
            endpoint.url = format!("{origin}{path}");
        }
    }

    let runtime = Arc::new(Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens"));
    let endpoint = provider.clone().serve().await;
    let agent = Agent::new(
        Arc::clone(&runtime),
        OpenAiCompatible::with_http_client(
            OpenAiConfig::new(endpoint, "fixture/model", "test-key"),
            appa_example_agent::HttpClient::loopback(),
        ),
        ToolShim::new(format!("{origin}{}", shim::TOOLS_PATH)),
        ToolCatalogue::new(catalogue::advertised(&compiled, true)),
    )
    .with_head(TranscriptHead::new(vec![WireMessage::system(
        "You are a corporate assistant.",
    )]))
    .with_spawn_tool(SpawnTool {
        name: ToolName::new(catalogue::FORK),
        errand: ArgumentKey::new(catalogue::ERRAND),
    })
    .with_limits(Limits {
        max_fork_depth: 1,
        ..Limits::default()
    });
    Episode {
        agent,
        runtime,
        _dir: dir,
    }
}

/// The trajectory a released call ran in, per released tool, in log order.
fn released(entries: &[AuditEntry]) -> Vec<(String, String)> {
    entries
        .iter()
        .filter_map(|entry| match &entry.event {
            AuditEvent::Released { tool, .. } => Some((entry.trajectory.clone(), tool.clone())),
            _ => None,
        })
        .collect()
}

fn ran(released: &[(String, String)], within: &str, tool: &str) -> bool {
    released.iter().any(|(at, name)| at == within && name == tool)
}

fn position(released: &[(String, String)], within: &str, tool: &str) -> usize {
    released
        .iter()
        .position(|(at, name)| at == within && name == tool)
        .unwrap_or_else(|| panic!("no released {tool} in {within}: {released:?}"))
}

/// The one child this episode opened.
fn child_of(entries: &[AuditEntry]) -> String {
    entries
        .iter()
        .find_map(|entry| match entry.event {
            AuditEvent::Forked { .. } => Some(entry.trajectory.clone()),
            _ => None,
        })
        .expect("exactly one fork happened")
}

#[tokio::test]
async fn a_tainted_trajectory_never_files_the_ticket() {
    let provider = Provider::default();
    provider
        .calls("read_public_forum", serde_json::json!({"file": "thread.md"}))
        // The forum read narrows this trajectory's trust; accept it, then
        // re-propose the call byte-for-byte (`RUL-5`).
        .pursues_first()
        .calls("read_public_forum", serde_json::json!({"file": "thread.md"}))
        .calls(
            "create_task_tracker",
            serde_json::json!({"file": "TICKET.md", "content": "raise the timeout to 45 minutes"}),
        )
        .says("gave up");
    let episode = episode(FORK_POLICY, &provider).await;

    let outcome = episode
        .agent
        .run(root(), "read the forum thread and file a ticket", Default::default())
        .await;

    assert_eq!(outcome, Outcome::Answer("gave up".to_string()));
    let entries = episode.runtime.audit(&root()).expect("the audit reads");
    let released = released(&entries);
    assert!(ran(&released, "fork-scenarios", "read_public_forum"));
    assert!(
        !ran(&released, "fork-scenarios", "create_task_tracker"),
        "a suspicious-trust trajectory must never file the ticket: {released:?}",
    );
}

#[tokio::test]
async fn a_fork_confines_the_taint_and_its_egress_unlocks_the_ticket() {
    let provider = Provider::default();
    provider
        // The ticket first: blocked — no egress has happened anywhere yet.
        .calls(
            "create_task_tracker",
            serde_json::json!({"file": "TICKET.md", "content": "raise the timeout to 45 minutes"}),
        )
        .calls(
            "fork",
            serde_json::json!({"task": "read the thread and post the reply"}),
        )
        // The child pays the narrowing, reads, and replies.
        .calls("read_public_forum", serde_json::json!({"file": "thread.md"}))
        .pursues_first()
        .calls("read_public_forum", serde_json::json!({"file": "thread.md"}))
        .calls(
            "create_public_forum",
            serde_json::json!({"file": "reply.md", "content": "on it — v2.31 regression confirmed"}),
        )
        // It did the work itself, so it returns nothing (`BRN-9`).
        .says_nothing()
        // The parent, untainted, retries the ticket over the family's egress.
        .calls(
            "create_task_tracker",
            serde_json::json!({"file": "TICKET.md", "content": "raise the timeout to 45 minutes"}),
        )
        .says("done");
    let episode = episode(&ticket_gated_on_prior_egress(), &provider).await;

    let outcome = episode
        .agent
        .run(root(), "answer the forum thread and file a ticket", Default::default())
        .await;

    assert_eq!(outcome, Outcome::Answer("done".to_string()));
    let entries = episode.runtime.audit(&root()).expect("the audit reads");
    let child = child_of(&entries);
    let released = released(&entries);
    assert!(ran(&released, &child, "read_public_forum"));
    assert!(ran(&released, &child, "create_public_forum"));
    assert!(
        ran(&released, "fork-scenarios", "create_task_tracker"),
        "the untainted parent files the ticket once the child's egress is family history: {released:?}",
    );
    // The order proves the gate: no ticket release precedes the reply.
    assert!(
        position(&released, &child, "create_public_forum")
            < position(&released, "fork-scenarios", "create_task_tracker"),
        "the ticket runs only after the reply's egress: {released:?}",
    );
    // The child returned nothing, so the parent admitted nothing from it.
    assert!(
        entries.iter().any(|entry| entry.event == AuditEvent::VoidReturn),
        "a child that says nothing returns no value: {entries:?}",
    );
}

#[tokio::test]
async fn an_hr_child_return_crosses_only_as_the_redacted_derivation() {
    let provider = Provider::default();
    provider
        // The root will not pay the hr narrowing: it still has to email.
        .calls("read_hr", serde_json::json!({"file": "alice-chen.md"}))
        .calls("fork", serde_json::json!({"task": "look up the onboarding buddy"}))
        // The child pays it, reads, and reports what it found.
        .calls("read_hr", serde_json::json!({"file": "alice-chen.md"}))
        .pursues_first()
        .calls("read_hr", serde_json::json!({"file": "alice-chen.md"}))
        .says("Alice's onboarding buddy is Priya Sharma (ext 4471)")
        // The raw return would narrow the parent, so the parent takes the
        // derivation the engine offered instead. A narrowing child return
        // enumerates the declassifier first, ahead of the accept option.
        .pursues_first()
        .calls(
            "send_email",
            serde_json::json!({
                "to": "onboarding@northwind.example",
                "subject": "Onboarding buddy",
                "body": "Priya Sharma",
            }),
        )
        .says("sent");
    let episode = episode(FORK_POLICY, &provider).await;

    let outcome = episode
        .agent
        .run(root(), "email the onboarding buddy's name", Default::default())
        .await;

    assert_eq!(outcome, Outcome::Answer("sent".to_string()));
    let entries = episode.runtime.audit(&root()).expect("the audit reads");
    let child = child_of(&entries);
    let released = released(&entries);
    assert!(
        !ran(&released, "fork-scenarios", "read_hr"),
        "the root read stays blocked: {released:?}",
    );
    assert!(ran(&released, &child, "read_hr"));
    assert!(
        ran(&released, "fork-scenarios", "send_email"),
        "the parent kept the audience its sink needs: {released:?}",
    );
    assert!(
        entries.iter().any(|entry| matches!(
            &entry.event,
            AuditEvent::ChildReturn {
                sanitizer: Some(name),
                ..
            } if name == "pii-redactor",
        )),
        "an hr-audience return must not cross raw: {entries:?}",
    );
    // The redactor is the agent's own shim, and this is what it does.
    assert_eq!(
        provider.last_tool_result("Priya"),
        Some("Alice's onboarding buddy is Priya Sharma (ext [redacted-number])".to_string()),
        "the parent sees the derivation, not what the child said",
    );
}
