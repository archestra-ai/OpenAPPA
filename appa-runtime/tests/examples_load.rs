mod common;
use common::repo_root;
#[cfg(unix)]
use common::{actor, last_offer, propose, ran, raw, root};

use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::Arc;

#[cfg(unix)]
use appa_runtime::api::RemedyOutcome;
use appa_runtime::api::Runtime;
use appa_runtime::config::Config;
#[cfg(unix)]
use appa_runtime::hooks;
#[cfg(unix)]
use appa_runtime_api::{HookDecision, HookEvent, ProposedCall};

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
fn the_kagent_policies_open() {
    // The demo policy binds the llm endpoint to APPA_LLM_API_KEY; the
    // runtime refuses to load a config whose token is absent, so the
    // test supplies a placeholder. Nothing consults it at open time,
    // and no other test in this binary reads the variable.
    unsafe { std::env::set_var("APPA_LLM_API_KEY", "examples-load") };
    opens(&repo_root().join("integrations/kagent/examples/kagent.appa.toml"));
    opens(&repo_root().join("integrations/kagent/demo/chart/files/demo.appa.toml"));
}

#[cfg(unix)]
#[test]
fn the_complete_battery_example_opens() {
    opens(&repo_root().join("examples/claude-code-battery/appa.toml"));
}

/// The initialized default with the Claude Code battery included, as `appa init` composes
/// them: the battery's rules run before the default's.
#[cfg(unix)]
fn composed_with_the_battery(dir: &tempfile::TempDir) -> Config {
    let battery_dir = dir.path().join("batteries/claude-code");
    std::fs::create_dir_all(&battery_dir).expect("the battery directory is created");

    let repository = repo_root();
    let default = std::fs::read_to_string(repository.join("integrations/claude-code/examples/claude-code.appa.toml"))
        .expect("the initialized default is readable");
    std::fs::copy(
        repository.join("batteries/claude-code/appa.toml"),
        battery_dir.join("appa.toml"),
    )
    .expect("the battery file is copied");

    let root = dir.path().join("appa.toml");
    std::fs::write(
        &root,
        format!("include = [\"batteries/claude-code/appa.toml\"]\n\n{default}"),
    )
    .expect("the initialized config includes the battery");

    Config::load(&root).expect("the initialized config and battery compose")
}

#[cfg(unix)]
#[test]
fn the_initialized_default_composes_with_the_claude_code_battery() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let config = composed_with_the_battery(&dir);
    let annotators = config.policy_file().value()["annotator"]
        .as_array()
        .expect("the composed Annotators are an array");
    let bash_annotators = annotators
        .iter()
        .filter(|annotator| annotator["name"].as_str() == Some("claude-code.bash-requirements"))
        .collect::<Vec<_>>();
    assert_eq!(
        bash_annotators.len(),
        1,
        "the root Bash Annotator replaces the battery default"
    );
    assert_eq!(
        bash_annotators[0]["hint"].as_str(),
        Some(
            "Treat network or otherwise unvetted output as suspicious. Classify trust and audience requirements from the command's visible behavior and destination."
        )
    );
    let tools = config.policy_file().value()["tool"]
        .as_array()
        .expect("the composed tools are an array");
    for (name, annotator) in [
        ("Bash", "claude-code.bash-requirements"),
        ("*", "claude-code.undeclared-tool"),
    ] {
        let matches = tools
            .iter()
            .filter(|tool| tool["name"].as_str() == Some(name))
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1, "{name} has exactly one composed rule");
        assert_eq!(matches[0]["annotator"].as_str(), Some(annotator));
    }
    let read = tools
        .iter()
        .filter(|tool| tool["name"].as_str() == Some("Read"))
        .collect::<Vec<_>>();
    assert_eq!(
        read.len(),
        1,
        "the plain Read rule composes once, after the battery's selectors"
    );
    assert!(
        read[0].get("annotator").is_none(),
        "Read is static: no annotator names a reader"
    );

    let database = dir.path().join("appa.db");
    Runtime::open(config, database, None).expect("the composed deployment opens");
}

#[cfg(unix)]
fn call(tool: &str, argument: &str, value: &str) -> ProposedCall {
    ProposedCall {
        tool: tool.to_string(),
        arguments: raw(serde_json::json!({ argument: value })),
    }
}

/// A credential named relatively — `.env`, `cat .netrc` — is judged like its absolute
/// spelling. A Bash call naming one is refused without a remedy. A Read narrows the
/// trajectory to `self`, after which a public sink requires an exact-call human review.
#[cfg(unix)]
#[tokio::test]
async fn the_battery_judges_relative_credentials_and_offers_review_for_public_release() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let config = composed_with_the_battery(&dir);
    let runtime =
        Arc::new(Runtime::open(config, dir.path().join("appa.db"), None).expect("the composed deployment opens"));
    assert_eq!(
        hooks::handle(&runtime, HookEvent::SessionStart { root: root() }).await,
        HookDecision::Ack
    );

    for command in [
        "cat .env",
        "cat .netrc",
        "cat ~/.ssh/id_ed25519",
        "cat /home/me/.aws/credentials",
    ] {
        let refused = propose(&runtime, call("Bash", "command", command)).await;
        let HookDecision::DenyCall { offers, .. } = refused else {
            panic!("`{command}` is refused, got {refused:?}");
        };
        assert!(offers.is_empty(), "`{command}` is refused without a remedy");
    }

    for path in ["./README.md", "../src/main.rs", "src/.gitignore/../main.rs"] {
        let ordinary = call("Read", "file_path", path);
        assert_eq!(
            propose(&runtime, ordinary.clone()).await,
            HookDecision::AllowCall { spawn: None },
            "`{path}` is an ordinary read: a dot in a relative path is not a hidden name"
        );
        ran(&runtime, ordinary).await;
    }

    let read = call("Read", "file_path", ".env");
    let narrowing = propose(&runtime, read.clone()).await;
    let HookDecision::DenyCall { feedback, .. } = narrowing else {
        panic!("reading `.env` is offered as a narrowing to `self`, got {narrowing:?}");
    };
    assert!(matches!(
        runtime.execute_remedy(&actor(), last_offer(&feedback)).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, read.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, read).await;

    let publication = propose(&runtime, call("Artifact", "file_path", "page.html")).await;
    let HookDecision::DenyCall {
        feedback,
        offers,
        review,
    } = publication
    else {
        panic!("a trajectory narrowed to `self` requires review before publishing: {publication:?}");
    };
    assert_eq!(
        offers.len(),
        1,
        "the default authority can review the audience expansion"
    );
    assert_eq!(review.len(), 1, "the offer is backed by the default human authority");
    assert!(feedback.contains("Submit for approval"));
    assert!(review[0].text.contains("page.html"), "the review shows the exact call");
    assert!(
        review[0].text.contains("public"),
        "the review shows the audience expansion it covers"
    );
}

/// The Slack battery requires `contains = ["internal"]` on writes: a public session can
/// post autonomously without human approval, while a session holding `self` secrets cannot
/// leak them into Slack channels.
#[cfg(unix)]
#[tokio::test]
async fn the_slack_battery_allows_public_writes_and_blocks_leaking_self_secrets() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let slack_battery_dir = dir.path().join("batteries/slack");
    let claude_battery_dir = dir.path().join("batteries/claude-code");
    std::fs::create_dir_all(&slack_battery_dir).expect("slack battery directory is created");
    std::fs::create_dir_all(&claude_battery_dir).expect("claude battery directory is created");

    let repository = repo_root();
    let default = std::fs::read_to_string(repository.join("integrations/claude-code/examples/claude-code.appa.toml"))
        .expect("the initialized default is readable");
    std::fs::copy(
        repository.join("batteries/slack/appa.toml"),
        slack_battery_dir.join("appa.toml"),
    )
    .expect("slack battery file is copied");
    std::fs::copy(
        repository.join("batteries/claude-code/appa.toml"),
        claude_battery_dir.join("appa.toml"),
    )
    .expect("claude battery file is copied");

    let root_path = dir.path().join("appa.toml");
    std::fs::write(
        &root_path,
        format!("include = [\"batteries/claude-code/appa.toml\", \"batteries/slack/appa.toml\"]\n\n{default}"),
    )
    .expect("the config includes both batteries");

    let config = Config::load(&root_path).expect("the config loads");
    let runtime = Arc::new(Runtime::open(config, dir.path().join("appa.db"), None).expect("opens"));
    assert_eq!(
        hooks::handle(&runtime, HookEvent::SessionStart { root: root() }).await,
        HookDecision::Ack
    );

    let slack_send = ProposedCall {
        tool: "mcp__claude_ai_Slack__slack_send_message".to_string(),
        arguments: raw(serde_json::json!({ "channel_id": "C123", "text": "hello" })),
    };

    // 1. Fresh public session: slack write is allowed autonomously
    assert_eq!(
        propose(&runtime, slack_send.clone()).await,
        HookDecision::AllowCall { spawn: None },
        "a public session can post to slack without hitl"
    );
    ran(&runtime, slack_send.clone()).await;

    // 2. Read .env and accept narrowing to self
    let read_env = call("Read", "file_path", ".env");
    let narrowing = propose(&runtime, read_env.clone()).await;
    let HookDecision::DenyCall { feedback, .. } = narrowing else {
        panic!("reading .env narrows to self");
    };
    assert!(matches!(
        runtime.execute_remedy(&actor(), last_offer(&feedback)).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, read_env.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, read_env).await;

    // 3. Narrowed to self: slack write is BLOCKED from leaking secrets
    let blocked_slack = propose(&runtime, slack_send).await;
    let HookDecision::DenyCall { offers, .. } = blocked_slack else {
        panic!("slack write must be blocked when session holds self secrets, got {blocked_slack:?}");
    };
    assert!(
        offers.is_empty(),
        "slack write cannot leak self secrets: no remedy plan"
    );
}
