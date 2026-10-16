//! Every family follows its deployment's current policy revision. These cases pin the runtime
//! behaviour a publish must keep: a call already running finishes under the terms its release
//! pinned met with the head's, a committed decision replays after a publish, a restarted runtime
//! replays a family across revisions without the earlier revisions' externals, and a publish by
//! another process fences this one until it serves the new head.

mod common;
use common::{last_offer, raw};

use std::path::Path;
use std::sync::Arc;

use appa_eventlog::{Backend, DeploymentName, LogStore};
use appa_runtime::api::{RemedyOutcome, Runtime};
use appa_runtime::{config::Config, hooks};
use appa_runtime_api::{Actor, HookDecision, HookEvent, OutcomeBody, ProposedCall, ToolOutcome, TrajectoryId};

const EXTERNALS: &str = "[externals]\ntimeout_ms = 1000\nmax_body_bytes = 4096\n";

/// A policy over the default trust chain with `tools` as its tool rules.
fn policy(tools: &str) -> String {
    format!("[policy]\nversion = 2\n{tools}\n{EXTERNALS}")
}

fn write(dir: &Path, text: &str) -> Config {
    let path = dir.join("appa.toml");
    std::fs::write(&path, text).expect("the policy writes");
    Config::load(&path).expect("the policy validates")
}

fn open(dir: &Path, text: &str) -> Arc<Runtime> {
    Arc::new(Runtime::open(write(dir, text), dir.join("appa.db"), None).expect("the deployment opens"))
}

fn root(name: &str) -> TrajectoryId {
    TrajectoryId(name.to_string())
}

fn actor(root: &TrajectoryId) -> Actor {
    Actor {
        root: root.clone(),
        child: None,
    }
}

fn call(tool: &str, n: u32) -> ProposedCall {
    ProposedCall {
        tool: tool.to_string(),
        arguments: raw(serde_json::json!({ "n": n })),
        cwd: None,
    }
}

async fn start(runtime: &Arc<Runtime>, root: &TrajectoryId) {
    let started = hooks::handle(
        runtime,
        HookEvent::SessionStart {
            root: root.clone(),
            principal: None,
            address: None,
            title: None,
            launch: None,
            start: None,
        },
    )
    .await;
    assert_eq!(started, HookDecision::Ack);
}

async fn propose(runtime: &Arc<Runtime>, root: &TrajectoryId, call: ProposedCall, id: &str) -> HookDecision {
    hooks::handle(
        runtime,
        HookEvent::ToolCall {
            actor: actor(root),
            call,
            call_id: Some(id.to_string()),
            spawn: None,
            prompt: None,
            ruling: None,
        },
    )
    .await
}

async fn report(runtime: &Arc<Runtime>, root: &TrajectoryId, call: ProposedCall, id: &str) -> HookDecision {
    hooks::handle(
        runtime,
        HookEvent::ToolResult {
            actor: actor(root),
            call,
            call_id: Some(id.to_string()),
            outcome: ToolOutcome::Success {
                body: OutcomeBody::Available("the result".to_string()),
            },
        },
    )
    .await
}

/// Propose `call`, accepting the one remedy its block offers when it is blocked, and return once
/// it is released.
async fn released(runtime: &Arc<Runtime>, root: &TrajectoryId, call: ProposedCall, id: &str) {
    let decision = propose(runtime, root, call.clone(), id).await;
    if let HookDecision::DenyCall { feedback, .. } = &decision {
        let accepted = runtime.execute_remedy(&actor(root), last_offer(feedback)).await;
        assert!(matches!(accepted, RemedyOutcome::Authorized { .. }), "{accepted:?}");
        let again = propose(runtime, root, call, &format!("{id}-accepted")).await;
        assert!(allowed(&again), "{again:?}");
    } else {
        assert!(allowed(&decision), "{decision:?}");
    }
}

fn allowed(decision: &HookDecision) -> bool {
    matches!(decision, HookDecision::AllowCall { .. })
}

fn trust(runtime: &Runtime, root: &TrajectoryId) -> String {
    runtime.status(root).expect("the session has a status").trust
}

fn moved(decision: &HookDecision) -> bool {
    matches!(decision, HookDecision::Refuse { detail } if detail.contains("moved to policy revision"))
}

/// A result reported after a publish is labelled by the meet of the terms its release pinned and
/// the head's: the head can tighten what a running call returns.
#[tokio::test]
async fn a_running_calls_result_takes_a_tighter_head() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let runtime = open(dir.path(), &policy("[[policy.tool]]\nname = \"fetch\"\n"));
    let id = root("tightened");
    start(&runtime, &id).await;
    released(&runtime, &id, call("fetch", 1), "c1").await;

    runtime
        .reload(write(
            dir.path(),
            &policy("[[policy.tool]]\nname = \"fetch\"\ndelta = { trust = \"suspicious\" }\n"),
        ))
        .expect("the revision publishes");
    assert_eq!(report(&runtime, &id, call("fetch", 1), "c1").await, HookDecision::Ack);
    assert_eq!(trust(&runtime, &id), "suspicious", "the head's terms label the result");
}

/// The head never loosens what a release promised: a result the release declared suspicious stays
/// suspicious under a head that declares the tool clean.
#[tokio::test]
async fn a_running_calls_result_keeps_its_release_terms_under_a_looser_head() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let runtime = open(
        dir.path(),
        &policy("[[policy.tool]]\nname = \"fetch\"\ndelta = { trust = \"suspicious\" }\n"),
    );
    let id = root("loosened");
    start(&runtime, &id).await;
    released(&runtime, &id, call("fetch", 1), "c1").await;

    runtime
        .reload(write(dir.path(), &policy("[[policy.tool]]\nname = \"fetch\"\n")))
        .expect("the revision publishes");
    report(&runtime, &id, call("fetch", 1), "c1-accepted").await;
    assert_eq!(
        trust(&runtime, &id),
        "suspicious",
        "the release's terms still bound the result"
    );
}

/// A result reported after a publish dropped its tool is admitted under the terms its release
/// pinned; a new call of the dropped tool is refused.
#[tokio::test]
async fn a_running_call_finishes_after_its_tool_is_dropped() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let runtime = open(
        dir.path(),
        &policy("[[policy.tool]]\nname = \"fetch\"\n[[policy.tool]]\nname = \"notes\"\n"),
    );
    let id = root("dropped");
    start(&runtime, &id).await;
    released(&runtime, &id, call("fetch", 1), "c1").await;

    runtime
        .reload(write(dir.path(), &policy("[[policy.tool]]\nname = \"notes\"\n")))
        .expect("the revision publishes");
    assert_eq!(report(&runtime, &id, call("fetch", 1), "c1").await, HookDecision::Ack);
    assert_eq!(trust(&runtime, &id), "trusted");
    assert!(matches!(
        propose(&runtime, &id, call("fetch", 2), "c2").await,
        HookDecision::Refuse { detail } if detail.contains("fetch")
    ));
    assert!(allowed(&propose(&runtime, &id, call("notes", 3), "c3").await));
}

/// A hook the harness retries after its decision committed replays that decision, even when a
/// publish since dropped the tool: the retry asks again about a decision already made.
#[tokio::test]
async fn a_retried_hook_replays_its_committed_decision_after_a_publish() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let runtime = open(dir.path(), &policy("[[policy.tool]]\nname = \"notes\"\n"));
    let id = root("retried");
    start(&runtime, &id).await;
    assert!(allowed(&propose(&runtime, &id, call("notes", 1), "c1").await));

    runtime
        .reload(write(dir.path(), &policy("")))
        .expect("the revision publishes");
    let retried = propose(&runtime, &id, call("notes", 1), "c1").await;
    assert!(
        allowed(&retried),
        "the retried hook replays the committed release: {retried:?}"
    );
    assert_eq!(report(&runtime, &id, call("notes", 1), "c1").await, HookDecision::Ack);
}

/// Reloading the bytes the head already names publishes nothing.
#[tokio::test]
async fn reloading_the_heads_own_bytes_publishes_no_revision() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let text = policy("[[policy.tool]]\nname = \"notes\"\n");
    let runtime = open(dir.path(), &text);
    let first = runtime.reload(write(dir.path(), &text)).expect("the reload runs");
    assert!(!first.changed);
    let edited = runtime
        .reload(write(dir.path(), &policy("")))
        .expect("the edit publishes");
    assert!(edited.changed);
    assert_eq!(edited.policy_revision, first.policy_revision + 1);
    let same = runtime.reload(write(dir.path(), &policy(""))).expect("the reload runs");
    assert!(!same.changed);
    assert_eq!(same.policy_revision, edited.policy_revision);
}

/// Processes serving one deployment from identical bytes — pods booting one release — publish one
/// revision between them, and each decides at it.
#[tokio::test]
async fn runtimes_booting_identical_bytes_share_one_revision() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let text = policy("[[policy.tool]]\nname = \"notes\"\n");
    let one = open(dir.path(), &text);
    let other = open(dir.path(), &text);
    let probe = |runtime: &Arc<Runtime>| runtime.reload(write(dir.path(), &text)).expect("the reload runs");
    assert_eq!(probe(&one).policy_revision, 1);
    assert_eq!(probe(&other).policy_revision, 1);
    let id = root("shared");
    start(&one, &id).await;
    assert!(allowed(&propose(&other, &id, call("notes", 1), "c1").await));
}

/// A publish by another process fences this one: its decisions refuse, retryably, and commit
/// nothing until it serves the new head — then the family decides at it.
#[tokio::test]
async fn a_publish_by_another_process_fences_until_this_one_serves_the_head() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let first = policy("[[policy.tool]]\nname = \"notes\"\n");
    let stale = open(dir.path(), &first);
    let id = root("fenced");
    start(&stale, &id).await;

    let next = policy("[[policy.tool]]\nname = \"notes\"\n[[policy.tool]]\nname = \"fetch\"\n");
    let publisher = open(dir.path(), &next);
    assert_eq!(
        publisher
            .reload(write(dir.path(), &next))
            .expect("the reload runs")
            .policy_revision,
        2
    );

    let refused = propose(&stale, &id, call("notes", 1), "c1").await;
    assert!(moved(&refused), "{refused:?}");
    assert!(allowed(&propose(&publisher, &id, call("fetch", 2), "c2").await));

    stale.reload(write(dir.path(), &next)).expect("the head's bytes load");
    assert!(allowed(&propose(&stale, &id, call("notes", 3), "c3").await));
}

/// A host serving each tenant through a view pinned to its line's head, from two runtimes on one
/// store: once the other runtime publishes past the pin, the pinned view refuses retryably and
/// commits nothing. Publishing the head's bytes again publishes nothing, and the call retried
/// through a view pinned to it decides at the head.
#[tokio::test]
async fn a_view_pinned_behind_its_lines_head_refuses_until_its_host_pins_the_head() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let store = Arc::new(LogStore::open(Backend::Memory).expect("an in-memory log opens"));
    let base = policy("[[policy.tool]]\nname = \"notes\"\n");
    let pod = || {
        Arc::new(
            Runtime::open_with_store(write(dir.path(), &base), Arc::clone(&store), None)
                .expect("the base deployment opens"),
        )
    };
    let (one, other) = (pod(), pod());
    let line = DeploymentName::new("tenant");
    let pin = |runtime: &Arc<Runtime>, text: &str| {
        let prepared = runtime
            .prepare_deployment(write(dir.path(), text))
            .expect("the tenant's policy loads");
        let published = runtime.publish(&line, &prepared).expect("the policy publishes");
        (published.revision(), Arc::new(runtime.pinned(&published)))
    };

    let first = policy("[[policy.tool]]\nname = \"notes\"\n");
    let (revision, stale) = pin(&one, &first);
    assert_eq!(revision, 1);
    let id = root("tenant-conversation");
    start(&stale, &id).await;
    assert!(allowed(&propose(&stale, &id, call("notes", 1), "c1").await));
    assert_eq!(report(&stale, &id, call("notes", 1), "c1").await, HookDecision::Ack);

    let next = policy("[[policy.tool]]\nname = \"notes\"\n[[policy.tool]]\nname = \"fetch\"\n");
    assert_eq!(pin(&other, &next).0, 2, "the other runtime publishes the next revision");

    let before = store.log(&id).expect("the family reads");
    let refused = propose(&stale, &id, call("notes", 2), "c2").await;
    assert!(moved(&refused), "{refused:?}");
    let after = store.log(&id).expect("the family reads");
    assert_eq!(after.facts().len(), before.facts().len(), "nothing commits");
    assert_eq!(after.host_records().len(), before.host_records().len());

    let (revision, head) = pin(&one, &next);
    assert_eq!(revision, 2, "the head's own bytes publish nothing");
    assert!(allowed(&propose(&head, &id, call("notes", 2), "c2").await));
    assert!(
        allowed(&propose(&head, &id, call("fetch", 3), "c3").await),
        "the family decides at the head"
    );
}

/// A runtime restarted under a later policy replays a family across the revisions that decided
/// it, without those revisions' externals: an earlier revision only replays and consults nothing.
#[tokio::test]
async fn a_restarted_runtime_replays_across_revisions_without_their_externals() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let signed = r#"
[policy]
version = 2

[[policy.tool]]
name = "publish"
requires = { attention = ["signoff"] }

[[policy.authority]]
name = "operator"
permits = { attention = ["signoff"] }

[externals]
timeout_ms = 1000
max_body_bytes = 4096

[externals.authorities.operator]
builtin = "approve"
"#;
    let id = root("restarted");
    {
        let runtime = open(dir.path(), signed);
        start(&runtime, &id).await;
        let blocked = propose(&runtime, &id, call("publish", 1), "c1").await;
        let HookDecision::DenyCall { feedback, .. } = &blocked else {
            panic!("the call waits for its signoff: {blocked:?}")
        };
        assert!(matches!(
            runtime.execute_remedy(&actor(&id), last_offer(feedback)).await,
            RemedyOutcome::Authorized { .. }
        ));
        assert!(allowed(&propose(&runtime, &id, call("publish", 1), "c1-signed").await));
        assert_eq!(
            report(&runtime, &id, call("publish", 1), "c1-signed").await,
            HookDecision::Ack
        );
    }

    let runtime = open(dir.path(), &policy("[[policy.tool]]\nname = \"publish\"\n"));
    assert_eq!(trust(&runtime, &id), "trusted", "the family replays");
    assert!(
        allowed(&propose(&runtime, &id, call("publish", 2), "c2").await),
        "the head needs no signoff and no authority"
    );
}

/// An independent root fork of a following family follows the same deployment: a publish reaches
/// parent and fork alike.
#[tokio::test]
async fn a_root_fork_follows_its_parents_deployment() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let runtime = open(
        dir.path(),
        &policy("[[policy.tool]]\nname = \"notes\"\n[[policy.tool]]\nname = \"fetch\"\n"),
    );
    let parent = root("parent");
    start(&runtime, &parent).await;
    assert!(allowed(&propose(&runtime, &parent, call("notes", 1), "c1").await));
    let fork = root("fork");
    runtime
        .open_root_fork(&parent, &parent, &fork)
        .expect("the parent can be forked");

    runtime
        .reload(write(dir.path(), &policy("[[policy.tool]]\nname = \"fetch\"\n")))
        .expect("the revision publishes");
    for id in [&parent, &fork] {
        assert!(matches!(
            propose(&runtime, id, call("notes", 2), "c2").await,
            HookDecision::Refuse { detail } if detail.contains("notes")
        ));
        assert!(allowed(&propose(&runtime, id, call("fetch", 3), "c3").await));
    }
}
