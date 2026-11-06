use super::super::{OutcomeBody, Runtime};
use super::*;
use crate::api::{ToolCallDecision, ToolOutcome, ToolResultDecision};
use crate::config::Config;
use appa_eventlog::{Backend, LogStore};

const POLICY: &str = r#"
[policy]
version = 2

[[policy.tool]]
name = "fetch"
parameters = { type = "object", properties = { a = { type = "integer" } } }
delta = {}

[externals]
timeout_ms = 2000
max_body_bytes = 65536
"#;

fn fetch(a: i64) -> ProposedCall {
    ProposedCall {
        tool: "fetch".to_string(),
        arguments: serde_json::value::to_raw_value(&serde_json::json!({ "a": a })).expect("the call serializes"),
        cwd: None,
    }
}

/// A runtime over a pool of `connections`, and a fresh root opened in it.
fn opened(connections: usize) -> (Arc<Runtime>, Arc<LogStore>, TrajectoryId) {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, POLICY).expect("the fixture writes");
    let config = Config::load(&path).expect("the fixture validates");
    let url = std::env::var("OPENAPPA_TEST_DATABASE_URL").expect("test database URL");
    let store = Arc::new(
        LogStore::open(Backend::Postgres {
            url,
            max_connections: std::num::NonZeroUsize::new(connections).expect("a pool holds a connection"),
        })
        .expect("the PostgreSQL store opens"),
    );
    let root = TrajectoryId(format!("pg-writers:{}", dir.path().display()));
    // Each test serves a line of its own: the database is shared with every other test.
    let runtime = Arc::new(
        Runtime::open_with_store_on(
            config,
            Arc::clone(&store),
            appa_eventlog::DeploymentName::new(root.0.clone()),
        )
        .expect("the runtime opens"),
    );
    runtime.create_session(root.clone(), None).expect("a fresh id opens");
    (runtime, store, root)
}

fn forget(store: &LogStore, root: &TrajectoryId) {
    let key = root.0.clone();
    store
        .lease()
        .expect("a connection leases")
        .postgres()
        .expect("the PostgreSQL API is present")
        .with_client(move |client| {
            client.execute("DELETE FROM openappa_events WHERE root = $1", &[&key])?;
            client.execute("DELETE FROM openappa_roots WHERE root = $1", &[&key])?;
            Ok(())
        })
        .expect("the test root cleans up");
}

/// Propose `calls` at once, one task each.
async fn propose_together(
    runtime: &Arc<Runtime>,
    root: &TrajectoryId,
    calls: Vec<(String, i64)>,
) -> Vec<Result<ToolCallDecision, EventError>> {
    let barrier = Arc::new(tokio::sync::Barrier::new(calls.len()));
    let tasks: Vec<_> = calls
        .into_iter()
        .map(|(call_id, a)| {
            let (runtime, root, barrier) = (Arc::clone(runtime), root.clone(), Arc::clone(&barrier));
            tokio::spawn(async move {
                let session = runtime.session(&root, &root).expect("the session reattaches");
                barrier.wait().await;
                session
                    .on_tool_call_identified(fetch(a), Some(call_id), None, None)
                    .await
            })
        })
        .collect();
    let mut decisions = Vec::new();
    for task in tasks {
        decisions.push(task.await.expect("the task joins"));
    }
    decisions
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "requires OPENAPPA_TEST_DATABASE_URL and host migrations"]
async fn postgres_concurrent_calls_of_one_family_all_land() {
    let writers = 16;
    let (runtime, store, root) = opened(4);
    let calls = (0..writers).map(|i| (format!("toolu-{i}"), i as i64)).collect();
    for decision in propose_together(&runtime, &root, calls).await {
        assert!(
            matches!(decision, Ok(ToolCallDecision::Allow { .. })),
            "every call releases: {decision:?}"
        );
    }
    assert_eq!(runtime.open_dispatches(&root, &root).len(), writers);
    forget(&store, &root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "requires OPENAPPA_TEST_DATABASE_URL and host migrations"]
async fn postgres_concurrent_results_of_one_family_all_land() {
    let writers = 16;
    let (runtime, store, root) = opened(4);
    let session = runtime.session(&root, &root).expect("the session reattaches");
    for i in 0..writers {
        session
            .on_tool_call_identified(fetch(i as i64), Some(format!("toolu-{i}")), None, None)
            .await
            .expect("the call releases");
    }
    let barrier = Arc::new(tokio::sync::Barrier::new(writers));
    let tasks: Vec<_> = (0..writers)
        .map(|i| {
            let (runtime, root, barrier) = (Arc::clone(&runtime), root.clone(), Arc::clone(&barrier));
            tokio::spawn(async move {
                let session = runtime.session(&root, &root).expect("the session reattaches");
                barrier.wait().await;
                session
                    .on_tool_result_identified(
                        fetch(i as i64),
                        Some(format!("toolu-{i}")),
                        ToolOutcome::Success {
                            body: OutcomeBody::Available("ok".to_string()),
                        },
                    )
                    .await
            })
        })
        .collect();
    for task in tasks {
        assert_eq!(
            task.await.expect("the task joins").expect("every result lands"),
            ToolResultDecision::Keep
        );
    }
    assert!(runtime.open_dispatches(&root, &root).is_empty());
    forget(&store, &root);
}

/// One host call delivered by several dispatches at once, as a retry racing the first
/// delivery would: every driver gets the one decision, and one dispatch opens.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "requires OPENAPPA_TEST_DATABASE_URL and host migrations"]
async fn postgres_one_call_from_many_drivers_opens_one_dispatch() {
    let (runtime, store, root) = opened(4);
    let calls = (0..8).map(|_| ("toolu-same".to_string(), 7)).collect();
    for decision in propose_together(&runtime, &root, calls).await {
        assert!(
            matches!(decision, Ok(ToolCallDecision::Allow { .. })),
            "every driver gets the release: {decision:?}"
        );
    }
    assert_eq!(runtime.open_dispatches(&root, &root).len(), 1);
    forget(&store, &root);
}
