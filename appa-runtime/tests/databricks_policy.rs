//! Databricks battery: one namespace over two managed servers; Genie reads are internal,
//! each SQL statement runs as the classifier reads it, inside the battery's mandate.
#![cfg(unix)]
mod common;

use appa_package::BatteryTarget;
use appa_runtime::api::{AuditEvent, OpenError, RemedyOutcome, Runtime};
use appa_runtime::config::{Config, HostDefaults, HostedBattery};
use appa_runtime::hooks;
use appa_runtime_api::{HookDecision, HookEvent, ProposedCall};
use common::{
    Classifier, actor, members_source, offer_of, propose, ran, raw, repo_root, root, serve_runtime, session_runtime,
};
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

fn call(tool: &str, args: serde_json::Value) -> ProposedCall {
    ProposedCall {
        tool: format!("mcp/databricks/{tool}"),
        arguments: raw(args),
        cwd: None,
    }
}

fn statement(query: &str) -> ProposedCall {
    call("execute_sql", serde_json::json!({ "query": query }))
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

fn battery_dir() -> std::path::PathBuf {
    repo_root().join("marketplace/batteries/databricks")
}

/// The shipped package copied under `dir`, so the base composes its target policies.
fn copy_battery(dir: &std::path::Path) -> std::path::PathBuf {
    let target = dir.join("marketplace/batteries/databricks");
    std::fs::create_dir_all(&target).unwrap();
    for entry in std::fs::read_dir(battery_dir()).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            std::fs::copy(entry.path(), target.join(entry.file_name())).unwrap();
        }
    }
    target
}

/// The shipped battery with its single-user audience source swapped for the loopback.
async fn install_battery(dir: &tempfile::TempDir) {
    let target = copy_battery(dir.path());
    let source = members_source().await;
    let binding = "command = [\"python3\", \"audience-source.py\"]\ntoken_env = \"APPA_PROVIDER_DATABRICKS_TOKEN\"\n";
    let policy = std::fs::read_to_string(target.join("single-user.toml")).unwrap();
    assert!(policy.contains(binding), "the battery binds its source as documented");
    std::fs::write(
        target.join("single-user.toml"),
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
    let runtime = session_runtime(dir.path(), &path).await;
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

    reads(&classifier);
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

    changes(&classifier);
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

    changes(&classifier);
    let insert = statement("INSERT INTO sales VALUES (1)");
    assert_eq!(
        propose(&runtime, insert.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, insert).await;

    needs_review(&classifier);
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
    reads(&classifier);
    let config = root_config(&dir, &command, "\n[aliases.mcp]\ndatabricks = [\"genie\", \"sql\"]\n");
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

const ROOT: &str = r#"
[policy]
version = 2
trust_chain = ["suspicious", "trusted"]

[externals]
timeout_ms = 30000
max_body_bytes = 1048576
"#;

fn policy(file: &str) -> toml::Value {
    toml::from_str(&std::fs::read_to_string(battery_dir().join(file)).unwrap()).unwrap()
}

/// The shipped battery copied under `dir`, beside a root config that includes its base
/// and then reads `root`.
fn installed(dir: &std::path::Path, root: &str) -> std::path::PathBuf {
    copy_battery(dir);
    let path = dir.join("appa.toml");
    std::fs::write(
        &path,
        format!("include = [\"marketplace/batteries/databricks/appa.toml\"]\n{root}"),
    )
    .unwrap();
    path
}

/// The base alone, as a host composes it with no target: it loads without the audience
/// source, and a Genie question narrows the trajectory to suspicious internal as before.
#[tokio::test]
async fn the_base_alone_keeps_every_rule_without_the_audience_source() {
    let base = std::fs::read_to_string(battery_dir().join("appa.toml")).unwrap();
    let config = Config::hosted_composed(
        ROOT,
        &[HostedBattery {
            name: "databricks",
            base: &base,
            target: None,
            token_env: &[],
        }],
        HostDefaults::new(Duration::from_secs(5), 65_536),
        |_| None,
    )
    .expect("the base composes alone");
    let store =
        Arc::new(appa_eventlog::LogStore::open(appa_eventlog::Backend::Memory).expect("an in-memory log opens"));
    let runtime = Arc::new(Runtime::open_with_store(config, store, None).expect("the deployment opens"));
    assert_eq!(
        hooks::handle(
            &runtime,
            HookEvent::SessionStart {
                root: root(),
                principal: None,
                address: None,
                title: None,
                launch: None,
                start: None,
            }
        )
        .await,
        HookDecision::Ack
    );
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
    let labels: Vec<_> = runtime
        .audit(&root())
        .unwrap()
        .into_iter()
        .filter_map(|entry| match entry.event {
            AuditEvent::Admitted { label } => Some((label.trust, label.audience)),
            _ => None,
        })
        .collect();
    assert_eq!(labels, vec![("suspicious".to_string(), "internal".to_string())]);
}

/// A `databricks` CLI stand-in first on this test binary's PATH, answering one active user
/// to `current-user me` and `users list`, and logging each call's arguments and the CLI
/// login variables it saw to the file it answers; every helper the runtime spawns inherits
/// it, a service principal's login, and a person's token and profile beside them.
fn databricks_cli() -> std::path::PathBuf {
    static LOG: OnceLock<std::path::PathBuf> = OnceLock::new();
    LOG.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap().keep();
        let log = dir.join("calls.jsonl");
        let cli = dir.join("databricks");
        std::fs::write(
            &cli,
            format!(
                r#"#!/bin/sh
python3 -c 'import json, os, sys; print(json.dumps({{"args": sys.argv[1:], **{{name: os.environ.get(name) for name in ["DATABRICKS_AUTH_TYPE", "DATABRICKS_CONFIG_FILE", "DATABRICKS_TOKEN", "DATABRICKS_CONFIG_PROFILE"]}}}}))' "$@" >> {log}
case "$1" in
  current-user) echo '{{"id": "1", "userName": "ana@corp.example", "active": true}}' ;;
  users) echo '[{{"id": "1", "userName": "ana@corp.example", "active": true}}]' ;;
  *) exit 1 ;;
esac
"#,
                log = log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&cli, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let path = std::env::join_paths(
            std::iter::once(dir.clone()).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())),
        )
        .unwrap();
        // SAFETY: set once, before any deployment that spawns the CLI loads; nothing else in
        // this binary writes the environment.
        unsafe {
            std::env::set_var("PATH", path);
            std::env::set_var("APPA_PROVIDER_DATABRICKS_HOST", "https://workspace.example");
            std::env::set_var("APPA_PROVIDER_DATABRICKS_CLIENT_ID", "principal-fixture");
            std::env::set_var("APPA_PROVIDER_DATABRICKS_CLIENT_SECRET", "secret-fixture");
            std::env::set_var("DATABRICKS_TOKEN", "dapi-personal-fixture");
            std::env::set_var("DATABRICKS_CONFIG_PROFILE", "personal");
        }
        log
    })
    .clone()
}

/// The CLI calls since the last one taken, each its arguments and the login variables seen.
fn cli_calls(log: &std::path::Path) -> Vec<serde_json::Value> {
    let calls = std::fs::read_to_string(log).unwrap_or_default();
    std::fs::write(log, "").unwrap();
    calls.lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

/// Each composition's startup probe reads the root's selectors through its own audience
/// source. Single-user runs the CLI as the person's own login and serves `viewer`;
/// multi-user runs it as the service principal alone, the person's token, profile and config
/// file out of reach, and serves no `viewer`, so a deployment mapping onto it does not open.
#[tokio::test]
async fn each_target_reads_the_workspace_as_its_own_login() {
    let log = databricks_cli();
    let members = "[policy.audience]\ninternal = [\"databricks:members\"]\n";
    let viewer = "[policy.audience]\nself = [\"databricks:viewer\"]\ninternal = [\"databricks:members\"]\n";
    let opened = |dir: &tempfile::TempDir, target, mapping: &str| {
        let config = Config::load_for_target(&installed(dir.path(), &format!("{ROOT}{mapping}")), &[], target)
            .expect("the composition loads");
        Runtime::open(config, dir.path().join("runtime.db"), None)
    };
    let probed = async |target, mapping: &str| {
        let dir = tempfile::tempdir().unwrap();
        let runtime = opened(&dir, target, mapping).expect("the deployment opens");
        runtime.probe_sources().await.expect("the probe reads every selector");
        cli_calls(&log)
    };
    let call = |args: &[&str], login: [Option<&str>; 4]| {
        serde_json::json!({
            "args": ([args, &["-o", "json"]].concat()),
            "DATABRICKS_AUTH_TYPE": login[0],
            "DATABRICKS_CONFIG_FILE": login[1],
            "DATABRICKS_TOKEN": login[2],
            "DATABRICKS_CONFIG_PROFILE": login[3],
        })
    };
    let personal = [None, None, Some("dapi-personal-fixture"), Some("personal")];
    let principal = [Some("oauth-m2m"), Some("/dev/null"), None, None];
    let members_listing = [
        "users",
        "list",
        "--attributes",
        "id,userName,active",
        "--filter",
        "active eq true",
    ];

    let mut calls = probed(BatteryTarget::SingleUser, viewer).await;
    calls.sort_by_key(|call| call["args"][0].as_str().unwrap().to_owned());
    assert_eq!(
        calls,
        vec![
            call(&["current-user", "me"], personal),
            call(&members_listing, personal)
        ]
    );

    assert_eq!(
        probed(BatteryTarget::MultiUser, members).await,
        vec![call(&members_listing, principal)]
    );

    let dir = tempfile::tempdir().unwrap();
    let refused = opened(&dir, BatteryTarget::MultiUser, viewer).err();
    assert!(
        matches!(&refused, Some(OpenError::Policy(error))
            if matches!(&**error, appa_policy::ConfigError::BadAudienceSource { selector, .. } if selector == "databricks:viewer")),
        "{refused:?}"
    );
}

/// The two target policies differ in their helpers alone: the same rules, the same
/// declarations, and the same externals bound.
#[test]
fn the_target_policies_declare_the_same_rules() {
    let single = policy("single-user.toml");
    let multi = policy("multi-user.toml");
    assert_eq!(single["policy"], multi["policy"]);
    let bound = |document: &toml::Value| -> Vec<(String, Vec<String>)> {
        document["externals"]
            .as_table()
            .unwrap()
            .iter()
            .map(|(section, entries)| (section.clone(), entries.as_table().unwrap().keys().cloned().collect()))
            .collect()
    };
    assert_eq!(bound(&single), bound(&multi));
}
