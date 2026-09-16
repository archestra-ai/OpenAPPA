//! Databricks battery: one namespace over two managed servers; Genie reads are internal,
//! each SQL statement runs as the classifier reads it, inside the battery's mandate.
#![cfg(unix)]
mod common;

use appa_runtime::{
    api::{AuditEvent, RemedyOutcome, Runtime},
    config::Config,
    hooks,
};
use appa_runtime_api::{HookDecision, HookEvent, ProposedCall};
use axum::{Router, routing::post};
use common::{actor, offer_of, propose, ran, raw, repo_root, root, serve, serve_runtime};
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Arc;

fn call(tool: &str, args: serde_json::Value) -> ProposedCall {
    ProposedCall {
        tool: format!("mcp/databricks/{tool}"),
        arguments: raw(args),
    }
}

fn statement(query: &str) -> ProposedCall {
    call("execute_sql", serde_json::json!({ "query": query }))
}

/// A loopback `databricks` audience source answering every collection with one reader.
async fn members_source() -> String {
    let router = Router::new().route(
        "/audience",
        post(|_body: String| async move {
            serde_json::json!({ "version": 1, "answer": { "members": ["alice@corp.example"] } }).to_string()
        }),
    );
    format!("{}/audience", serve(router).await)
}

/// The classifier's next answer, and what it was asked; the fake `claude` reads its
/// answer from `answer.json` and keeps its prompt in `prompt.txt`.
struct Classifier {
    answer: std::path::PathBuf,
    prompt: std::path::PathBuf,
}

impl Classifier {
    fn install(dir: &std::path::Path) -> (std::path::PathBuf, Classifier) {
        use std::os::unix::fs::PermissionsExt;
        let classifier = Classifier {
            answer: dir.join("answer.json"),
            prompt: dir.join("prompt.txt"),
        };
        let command = dir.join("fake-claude");
        std::fs::write(
            &command,
            format!(
                "#!/bin/sh\ncat > {prompt}\ncat {answer}\n",
                prompt = classifier.prompt.display(),
                answer = classifier.answer.display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o755)).unwrap();
        (command, classifier)
    }

    fn answers(&self, delta: serde_json::Value, requires: serde_json::Value, emits: &[&str]) {
        let structured = serde_json::json!({ "delta": delta, "requires": requires, "emits": emits });
        std::fs::write(
            &self.answer,
            serde_json::json!({ "structured_output": structured }).to_string(),
        )
        .unwrap();
    }

    fn reads_only(&self) {
        self.answers(
            serde_json::json!({ "trust": "suspicious", "audience": ["internal"] }),
            serde_json::json!({ "audience": { "contains": ["internal"] }, "history": [], "attention": [] }),
            &[],
        );
    }

    fn writes(&self) {
        self.answers(
            serde_json::json!({}),
            serde_json::json!({ "trust": "trusted", "audience": { "contains": ["internal"] }, "history": [], "attention": [] }),
            &["databricks.changed"],
        );
    }

    fn needs_review(&self) {
        self.answers(
            serde_json::json!({}),
            serde_json::json!({ "attention": ["databricks-review"], "history": [] }),
            &["databricks.sensitive"],
        );
    }

    fn prompt(&self) -> String {
        std::fs::read_to_string(&self.prompt).unwrap_or_default()
    }
}

/// The shipped battery with its audience source swapped for the loopback.
async fn install_battery(dir: &tempfile::TempDir) {
    let target = dir.path().join("marketplace/batteries/databricks");
    std::fs::create_dir_all(&target).unwrap();
    let source = members_source().await;
    let binding = "command = [\"python3\", \"audience-source.py\"]\ntoken_env = \"APPA_PROVIDER_DATABRICKS_TOKEN\"\n";
    let policy = std::fs::read_to_string(repo_root().join("marketplace/batteries/databricks/appa.toml")).unwrap();
    assert!(policy.contains(binding), "the battery binds its source as documented");
    std::fs::write(
        target.join("appa.toml"),
        policy.replace(binding, &format!("url = \"{source}\"\n")),
    )
    .unwrap();
}

/// The root config over the installed battery, its classifier the fake `claude`.
fn root_config(dir: &tempfile::TempDir, command: &std::path::Path, extra: &str) -> std::path::PathBuf {
    let path = dir.path().join("appa.toml");
    std::fs::write(
        &path,
        format!(
            r#"include = ["marketplace/batteries/databricks/appa.toml"]
{extra}
[policy]
version = 2

[policy.audience]
self = ["databricks:viewer"]
internal = ["databricks:members"]

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
    path
}

async fn runtime(dir: &tempfile::TempDir) -> (Arc<Runtime>, Classifier) {
    install_battery(dir).await;
    let (command, classifier) = Classifier::install(dir.path());
    let path = root_config(dir, &command, "");
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
            AuditEvent::Released { tool, effects, .. } if tool.starts_with("mcp/databricks/") => Some(effects),
            _ => None,
        })
        .collect()
}

/// A Genie question narrows the trajectory to suspicious internal; the later Genie reads
/// and a read-only statement then run outright, and the statement reached the classifier.
/// Suspicious content cannot feed a write.
#[tokio::test]
async fn genie_reads_narrow_once_and_a_read_only_statement_follows() {
    let dir = tempfile::tempdir().unwrap();
    let (runtime, classifier) = runtime(&dir).await;

    let ask = call("genie_ask", serde_json::json!({ "question": "revenue by region" }));
    let offer = offer_of(&propose(&runtime, ask.clone()).await);
    assert!(matches!(
        runtime.execute_remedy(&actor(), offer).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, ask.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, ask).await;

    for read in [
        call(
            "genie_poll_response",
            serde_json::json!({ "conversation_id": "c1", "message_id": "m1" }),
        ),
        call(
            "genie_get_query_result",
            serde_json::json!({ "conversation_id": "c1", "message_id": "m1" }),
        ),
        call(
            "genie_cancel_response",
            serde_json::json!({ "conversation_id": "c1", "message_id": "m1" }),
        ),
    ] {
        assert_eq!(
            propose(&runtime, read.clone()).await,
            HookDecision::AllowCall { spawn: None },
            "{}",
            read.tool
        );
        ran(&runtime, read).await;
    }

    classifier.reads_only();
    let select = statement("SELECT region, sum(amount) FROM sales GROUP BY region");
    assert_eq!(
        propose(&runtime, select.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert!(
        classifier.prompt().contains("FROM sales GROUP BY region"),
        "the statement reached the classifier"
    );
    ran(&runtime, select).await;

    classifier.writes();
    assert!(!matches!(
        propose(&runtime, statement("INSERT INTO sales VALUES (1)")).await,
        HookDecision::AllowCall { .. }
    ));
}

/// A write runs on trusted input and records its effect; a statement the classifier sends
/// to review needs the reviewer and records the sensitive effect.
#[tokio::test]
async fn a_write_records_its_effect_and_a_reviewed_statement_needs_the_reviewer() {
    let dir = tempfile::tempdir().unwrap();
    let (runtime, classifier) = runtime(&dir).await;

    classifier.writes();
    let insert = statement("INSERT INTO sales VALUES (1)");
    assert_eq!(
        propose(&runtime, insert.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, insert).await;

    classifier.needs_review();
    let grant = statement("GRANT SELECT ON TABLE sales TO `analysts`");
    let decision = propose(&runtime, grant.clone()).await;
    assert!(matches!(decision, HookDecision::DenyCall { .. }), "{decision:?}");
    assert!(matches!(
        runtime.execute_remedy(&actor(), offer_of(&decision)).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, grant.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, grant).await;

    assert_eq!(
        effects_of(&runtime),
        vec![
            vec!["databricks.changed".to_string()],
            vec!["databricks.sensitive".to_string()]
        ]
    );
}

/// An answer outside the battery's mandate is no answer: the statement is refused, never
/// run under a label the battery did not write.
#[tokio::test]
async fn an_answer_outside_the_mandate_refuses_the_statement() {
    let dir = tempfile::tempdir().unwrap();
    let (runtime, classifier) = runtime(&dir).await;

    classifier.answers(
        serde_json::json!({ "trust": "trusted", "audience": ["public"] }),
        serde_json::json!({ "history": [], "attention": [] }),
        &[],
    );
    let decision = propose(&runtime, statement("SELECT 1")).await;
    assert!(matches!(decision, HookDecision::Refuse { .. }), "{decision:?}");

    classifier.answers(
        serde_json::json!({}),
        serde_json::json!({ "history": [], "attention": ["signoff"] }),
        &["databricks.deleted"],
    );
    let decision = propose(&runtime, statement("DROP TABLE sales")).await;
    assert!(matches!(decision, HookDecision::Refuse { .. }), "{decision:?}");
    assert!(effects_of(&runtime).is_empty());
}

/// One claude-code hook event against the served binary, as the hook client renders it.
fn hook(url: &str, event: &str) -> (i32, serde_json::Value) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_appa"))
        .arg("hook")
        .arg("--deployment-url")
        .arg(url)
        .env("APPA_GATE", "1")
        .env_remove("APPA_RUNTIME_URL")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(event.as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    (
        output.status.code().unwrap(),
        serde_json::from_str(&stdout).unwrap_or_else(|_| panic!("the hook answer is JSON: {stdout}")),
    )
}

fn pre_tool_use(server: &str, tool: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "databricks-test",
        "tool_name": format!("mcp__{server}__{tool}"),
        "tool_input": { "question": "revenue by region" },
    })
    .to_string()
}

/// Served to Claude Code, the one namespace bound to two host servers covers every rule
/// under it on each server: a fresh session is offered the narrowing a Genie question or
/// a read-only statement needs, whichever server names it. A server the deployment did not
/// bind, even one named `databricks`, is not covered.
#[tokio::test(flavor = "multi_thread")]
async fn the_namespace_covers_each_bound_server_under_the_host() {
    let dir = tempfile::tempdir().unwrap();
    install_battery(&dir).await;
    let (command, classifier) = Classifier::install(dir.path());
    classifier.reads_only();
    let config = root_config(
        &dir,
        &command,
        "\n[server_aliases]\ndatabricks = [\"genie\", \"sql\"]\n",
    );
    let served = serve_runtime(&config, &dir.path().join("served.db"));

    for (server, tool) in [("genie", "genie_ask"), ("sql", "execute_sql"), ("genie", "execute_sql")] {
        let (code, answer) = hook(&served.url, &pre_tool_use(server, tool));
        assert_eq!(code, 0, "{server}/{tool} is covered: {answer}");
        assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "deny", "{answer}");
        assert!(
            answer["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .is_some_and(|reason| reason.contains("execute_remedy_plan")),
            "{server}/{tool} is offered its narrowing: {answer}"
        );
    }
    let (code, answer) = hook(&served.url, &pre_tool_use("databricks", "genie_ask"));
    assert_eq!(code, 2, "an unbound server is not covered: {answer}");
}
