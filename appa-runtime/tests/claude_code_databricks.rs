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
        cwd: None,
    }
}

/// The shipped default and battery under a failing `claude`: a static credential
/// rule narrows without consulting the root's Bash Annotator.
async fn runtime(dir: &tempfile::TempDir) -> Arc<Runtime> {
    let target = dir.path().join("batteries/claude-code");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::copy(
        repo_root().join("marketplace/batteries/claude-code/appa.toml"),
        target.join("appa.toml"),
    )
    .unwrap();
    std::fs::copy(
        repo_root().join("marketplace/batteries/claude-code/repository.py"),
        target.join("repository.py"),
    )
    .unwrap();
    let command = fake_claude(dir.path(), "exit 1");
    let path = dir.path().join("appa.toml");
    let root_policy =
        std::fs::read_to_string(repo_root().join("marketplace/plugins/claude-code/default.appa.toml")).unwrap();
    std::fs::write(
        &path,
        format!(
            "include = [\"batteries/claude-code/appa.toml\"]\n{root_policy}\n[externals.claude_code]\ncommand = \"{command}\"\n",
            command = command.display(),
        ),
    )
    .unwrap();
    let runtime = Arc::new(Runtime::open(Config::load(&path).unwrap(), dir.path().join("runtime.db"), None).unwrap());
    assert_eq!(
        hooks::handle(
            &runtime,
            HookEvent::SessionStart {
                root: root(),
                principal: None
            }
        )
        .await,
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
