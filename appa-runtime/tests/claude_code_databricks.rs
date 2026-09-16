//! Claude Code battery, Databricks CLI section: a `databricks` command in Bash is classified
//! in the databricks battery's vocabulary, its SQL by the statement, and the CLI's credential
//! commands narrow to `self` before any classifier runs.
#![cfg(unix)]
mod common;

use appa_runtime::{
    api::{AuditEvent, RemedyOutcome, Runtime},
    config::Config,
    hooks,
};
use appa_runtime_api::{HookDecision, HookEvent, ProposedCall};
use axum::{Router, routing::post};
use common::{Classifier, actor, offer_of, propose, ran, raw, repo_root, root, serve};
use std::sync::Arc;

fn bash(command: &str) -> ProposedCall {
    ProposedCall {
        tool: "host/claude-code/Bash".to_string(),
        arguments: raw(serde_json::json!({ "command": command })),
    }
}

/// A loopback audience source answering every collection with one reader: the root maps
/// `self` and `internal` onto it, as a deployment maps them onto its directory.
async fn directory_source() -> String {
    let router = Router::new().route(
        "/audience",
        post(|_body: String| async move {
            serde_json::json!({ "version": 1, "answer": { "members": ["alice@corp.example"] } }).to_string()
        }),
    );
    format!("{}/audience", serve(router).await)
}

/// The Databricks mandate's three answers: a read of suspicious internal data from input
/// sharable with internal; a change on trusted internal input recording `databricks.changed`;
/// a statement for a person, under the `databricks-review` mark, recording
/// `databricks.sensitive`.
fn reads(classifier: &Classifier) {
    classifier.answers(
        serde_json::json!({ "trust": "suspicious", "audience": ["internal"] }),
        serde_json::json!({ "audience": { "contains": ["internal"] }, "history": [], "attention": [] }),
        &[],
    );
}

fn changes(classifier: &Classifier) {
    classifier.answers(
        serde_json::json!({}),
        serde_json::json!({ "trust": "trusted", "audience": { "contains": ["internal"] }, "history": [], "attention": [] }),
        &["databricks.changed"],
    );
}

fn needs_review(classifier: &Classifier) {
    classifier.answers(
        serde_json::json!({}),
        serde_json::json!({ "attention": ["databricks-review"], "history": [] }),
        &["databricks.sensitive"],
    );
}

/// The shipped battery under a root that maps the audiences, permits the review mark, and
/// runs the fake `claude`.
async fn runtime(dir: &tempfile::TempDir) -> (Arc<Runtime>, Classifier) {
    let target = dir.path().join("marketplace/batteries/claude-code");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::copy(
        repo_root().join("marketplace/batteries/claude-code/appa.toml"),
        target.join("appa.toml"),
    )
    .unwrap();
    let (command, classifier) = Classifier::install(dir.path());
    let source = directory_source().await;
    let path = dir.path().join("appa.toml");
    std::fs::write(
        &path,
        format!(
            r#"include = ["marketplace/batteries/claude-code/appa.toml"]

[policy]
version = 2

[policy.audience]
self = ["directory:viewer"]
internal = ["directory:members"]

[externals.audience.directory]
url = "{source}"
selectors = [{{ template = "viewer", feeds = "self" }}, {{ template = "members", feeds = "internal" }}]

[[policy.authority]]
name = "databricks-operator"
permits = {{ trust_below = "trusted", attention = ["databricks-review"] }}

[externals]
timeout_ms = 30000
review_timeout_ms = 600000
max_body_bytes = 1048576

[externals.authorities.databricks-operator]
builtin = "approve"

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
    (runtime, classifier)
}

fn effects_of(runtime: &Runtime) -> Vec<Vec<String>> {
    runtime
        .audit(&root())
        .unwrap()
        .into_iter()
        .filter_map(|entry| match entry.event {
            AuditEvent::Released { tool, effects, .. } if tool == "host/claude-code/Bash" => Some(effects),
            _ => None,
        })
        .collect()
}

/// Both classifiers see the command line they were routed: a CLI read narrows the session
/// once, a read-only statement then runs outright, and suspicious content cannot feed a write.
#[tokio::test]
async fn a_cli_read_narrows_once_and_its_sql_reaches_the_statement_classifier() {
    let dir = tempfile::tempdir().unwrap();
    let (runtime, classifier) = runtime(&dir).await;

    reads(&classifier);
    let list = bash("databricks catalogs list --profile dev");
    let offer = offer_of(&propose(&runtime, list.clone()).await);
    assert!(
        classifier.prompt().contains("catalogs list"),
        "the CLI command reached its classifier"
    );
    assert!(matches!(
        runtime.execute_remedy(&actor(), offer).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, list.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, list).await;

    let select = bash("databricks experimental aitools tools query \"SELECT region FROM sales\" --profile dev");
    assert_eq!(
        propose(&runtime, select.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert!(
        classifier.prompt().contains("SELECT region FROM sales"),
        "the statement reached its classifier"
    );
    ran(&runtime, select).await;

    changes(&classifier);
    assert!(!matches!(
        propose(
            &runtime,
            bash("databricks experimental aitools tools query \"INSERT INTO sales VALUES (1)\"")
        )
        .await,
        HookDecision::AllowCall { .. }
    ));
}

/// A CLI change runs on trusted input and records its effect; SQL the model cannot see, read
/// from a file, needs the reviewer and records the sensitive effect.
#[tokio::test]
async fn a_cli_change_records_its_effect_and_sql_from_a_file_needs_the_reviewer() {
    let dir = tempfile::tempdir().unwrap();
    let (runtime, classifier) = runtime(&dir).await;

    changes(&classifier);
    let run_now = bash("databricks jobs run-now --job-id 42 --profile dev");
    assert_eq!(
        propose(&runtime, run_now.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, run_now).await;

    needs_review(&classifier);
    let from_file = bash("databricks experimental aitools tools query --file report.sql --profile dev");
    let decision = propose(&runtime, from_file.clone()).await;
    assert!(matches!(decision, HookDecision::DenyCall { .. }), "{decision:?}");
    assert!(matches!(
        runtime.execute_remedy(&actor(), offer_of(&decision)).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, from_file.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, from_file).await;

    assert_eq!(
        effects_of(&runtime),
        vec![
            vec!["databricks.changed".to_string()],
            vec!["databricks.sensitive".to_string()]
        ]
    );
}

/// The CLI's credential commands are the battery's credential rules, with the CLI's global
/// flags anywhere before the verb: the session narrows to `self` by a static rule, and no
/// classifier is asked.
#[tokio::test]
async fn a_credential_command_narrows_to_self_without_a_classifier() {
    let dir = tempfile::tempdir().unwrap();
    let (runtime, classifier) = runtime(&dir).await;

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
        assert_eq!(classifier.prompt(), "", "{command} asked a classifier");
    }
}

/// An answer outside the mandate is no answer: the command is refused, never run under a
/// label the battery did not write.
#[tokio::test]
async fn an_answer_outside_the_mandate_refuses_the_command() {
    let dir = tempfile::tempdir().unwrap();
    let (runtime, classifier) = runtime(&dir).await;

    classifier.answers(
        serde_json::json!({ "trust": "trusted", "audience": ["public"] }),
        serde_json::json!({ "history": [], "attention": [] }),
        &[],
    );
    let decision = propose(
        &runtime,
        bash("databricks experimental aitools tools query \"SELECT 1\""),
    )
    .await;
    assert!(matches!(decision, HookDecision::Refuse { .. }), "{decision:?}");

    classifier.answers(
        serde_json::json!({}),
        serde_json::json!({ "history": [], "attention": ["signoff"] }),
        &["databricks.deleted"],
    );
    let decision = propose(&runtime, bash("databricks api post /api/2.0/jobs/delete --json '{}'")).await;
    assert!(matches!(decision, HookDecision::Refuse { .. }), "{decision:?}");
    assert!(effects_of(&runtime).is_empty());
}
