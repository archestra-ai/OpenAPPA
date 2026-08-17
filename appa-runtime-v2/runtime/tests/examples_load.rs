
use std::path::{Path, PathBuf};

use appa_runtime_v2::api::Runtime;
use appa_runtime_v2::config::Config;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the runtime crate sits two levels below the repo root")
        .to_path_buf()
}

fn toml_files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
        let path = entry.expect("the directory entry is readable").path();
        if path.extension().is_some_and(|extension| extension == "toml") {
            found.push(path);
        }
    }
    found.sort();
    found
}

fn opens(path: &Path) {
    let config = Config::load(path).unwrap_or_else(|error| panic!("{} does not load: {error}", path.display()));
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    Runtime::open(config, dir.path().join("appa.db"), None)
        .unwrap_or_else(|error| panic!("{} does not open: {error}", path.display()));
}

#[test]
fn every_shipped_example_opens() {
    let examples = toml_files(&repo_root().join("integrations/claude-code/examples"));
    assert!(
        examples.len() >= 2,
        "both shipped examples were checked, not {examples:?}"
    );
    for path in &examples {
        opens(path);
    }
}

#[test]
fn every_bench_deployment_opens() {
    let root = repo_root();
    let mut deployments = toml_files(&root.join("bench/corp/policies"));
    for entry in std::fs::read_dir(root.join("bench/corp/scenarios")).expect("bench/corp/scenarios") {
        let policy_dir = entry.expect("the directory entry is readable").path().join("policy");
        if policy_dir.is_dir() {
            deployments.extend(toml_files(&policy_dir));
        }
    }
    assert!(
        deployments.len() > 10,
        "expected the benchmark's deployments, found {deployments:?}"
    );
    for path in &deployments {
        opens(path);
    }
}

#[tokio::test]
async fn the_shipped_examples_control_context_and_refuse_background_subagents() {
    use appa_runtime_api::{Actor, HookDecision, HookEvent, ProposedCall, TrajectoryId};

    for path in toml_files(&repo_root().join("integrations/claude-code/examples")) {
        let config = Config::load(&path).unwrap_or_else(|error| panic!("{} does not load: {error}", path.display()));
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config, dir.path().join("appa.db"), None)
            .unwrap_or_else(|error| panic!("{} does not open: {error}", path.display()));
        let cases = [
            (
                "Agent",
                serde_json::json!({"prompt": "list files", "subagent_type": "Explore"}),
                true,
            ),
            (
                "Agent",
                serde_json::json!({"prompt": "list files", "run_in_background": false}),
                true,
            ),
            (
                "Agent",
                serde_json::json!({"prompt": "list files", "run_in_background": true}),
                false,
            ),
            ("Task", serde_json::json!({"prompt": "list files"}), true),
            (
                "Task",
                serde_json::json!({"prompt": "list files", "run_in_background": true}),
                false,
            ),
        ];
        for (index, (tool, arguments, releases)) in cases.into_iter().enumerate() {
            let root = TrajectoryId(format!("cc:example-{index}"));
            assert_eq!(
                appa_runtime_v2::hooks::handle(&runtime, HookEvent::SessionStart { root: root.clone() }).await,
                HookDecision::Ack,
            );
            let call = ProposedCall {
                tool: tool.to_string(),
                arguments: serde_json::value::to_raw_value(&arguments).expect("the arguments serialize"),
            };
            let decision = appa_runtime_v2::hooks::handle(
                &runtime,
                HookEvent::ToolCall {
                    actor: Actor { root, child: None },
                    call,
                    spawn: true,
                },
            )
            .await;
            match (releases, decision) {
                (true, HookDecision::AllowCall { spawn: Some(_) }) => {}
                (false, HookDecision::DenyCall { .. }) => {}
                (releases, decision) => panic!(
                    "{}: {tool} {arguments} should {} but decided {decision:?}",
                    path.display(),
                    if releases {
                        "release with a fork binding"
                    } else {
                        "be denied"
                    },
                ),
            }
        }
    }
}
