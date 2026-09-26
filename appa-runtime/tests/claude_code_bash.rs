//! Claude Code battery: a command naming a credential path, or one of the Databricks CLI's
//! credential commands with its global flags anywhere before the verb, narrows the session
//! to `self` by a static rule, and no classifier is asked. Every other shell command, and
//! every Monitor call, is the Annotator's.
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

#[tokio::test]
async fn a_command_naming_any_credential_directory_the_read_rules_name_narrows_to_self() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;

    for command in [
        "cat ~/.azure/accessTokens.json",
        "ls ~/.gnupg/private-keys-v1.d",
        "cat ~/.config/gcloud/credentials.db",
        "cat ~/.password-store/work.gpg",
        "cat ~/.vault-token",
        "cat ~/.aws/config",
        "cat ~/.config/gh/config.yml",
    ] {
        let decision = propose(&runtime, bash(command)).await;
        assert!(
            matches!(decision, HookDecision::DenyCall { .. }),
            "{command}: {decision:?}"
        );
    }
}

#[tokio::test]
async fn a_monitor_call_is_the_annotators_whether_it_runs_a_command_or_opens_a_socket() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;

    for arguments in [
        serde_json::json!({ "command": "tail -f build.log", "description": "build", "timeout_ms": 60000 }),
        serde_json::json!({ "ws": { "url": "wss://events.example/stream" }, "description": "events", "timeout_ms": 60000 }),
    ] {
        let monitor = ProposedCall {
            tool: "host/claude-code/Monitor".to_string(),
            arguments: raw(arguments.clone()),
            cwd: None,
        };
        let decision = propose(&runtime, monitor).await;
        assert!(
            matches!(decision, HookDecision::Refuse { .. }),
            "an unanswered Annotator fails closed for {arguments}: {decision:?}"
        );
    }
}
