//! Claude Code battery: the Databricks CLI's credential commands are credential paths, so
//! they narrow the session to `self` by a static rule, with the CLI's global flags anywhere
//! before the verb, and no classifier is asked.
#![cfg(unix)]
mod common;

use appa_runtime::{api::Runtime, config::Config, hooks};
use appa_runtime_api::{HookDecision, HookEvent, ProposedCall};
use common::{fake_claude, propose, raw, repo_root, root};
use std::sync::Arc;

fn bash(command: &str) -> ProposedCall {
    ProposedCall {
        tool: "host/claude-code/Bash".to_string(),
        arguments: raw(serde_json::json!({ "command": command })),
    }
}

/// The shipped battery under a root whose `claude` fails: a command that reaches the Bash
/// annotator is refused, so a static rule's narrowing is told apart from a classification.
async fn runtime(dir: &tempfile::TempDir) -> Arc<Runtime> {
    let target = dir.path().join("marketplace/batteries/claude-code");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::copy(
        repo_root().join("marketplace/batteries/claude-code/appa.toml"),
        target.join("appa.toml"),
    )
    .unwrap();
    let command = fake_claude(dir.path(), "exit 1");
    let path = dir.path().join("appa.toml");
    std::fs::write(
        &path,
        format!(
            r#"include = ["marketplace/batteries/claude-code/appa.toml"]

[policy]
version = 2

[externals]
timeout_ms = 30000
max_body_bytes = 1048576

[externals.claude_code]
command = "{command}"
"#,
            command = command.display(),
        ),
    )
    .unwrap();
    let runtime = Arc::new(Runtime::open(Config::load(&path).unwrap(), dir.path().join("runtime.db"), None).unwrap());
    assert_eq!(
        hooks::handle(&runtime, HookEvent::SessionStart { root: root() }).await,
        HookDecision::Ack
    );
    runtime
}

#[tokio::test]
async fn a_credential_command_narrows_to_self_without_a_classifier() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;

    for command in [
        "databricks auth token --host https://dbc-1.cloud.databricks.com",
        "databricks --profile dev auth env",
        "databricks -p dev secrets get-secret scope key",
        "databricks configure --token",
        "cat ~/.databrickscfg",
    ] {
        let decision = propose(&runtime, bash(command)).await;
        assert!(
            matches!(decision, HookDecision::DenyCall { .. }),
            "{command}: {decision:?}"
        );
    }

    let decision = propose(&runtime, bash("databricks catalogs list --profile dev")).await;
    assert!(
        matches!(decision, HookDecision::Refuse { .. }),
        "any other command is the annotator's: {decision:?}"
    );
}
