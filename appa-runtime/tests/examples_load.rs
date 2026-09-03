mod common;
use common::{actor, last_offer, propose, ran, raw, repo_root, root};

use std::path::{Path, PathBuf};
use std::sync::Arc;

use appa_runtime::api::{RemedyOutcome, Runtime};
use appa_runtime::config::Config;
use appa_runtime::hooks;
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

fn call(tool: &str, argument: &str, value: &str) -> ProposedCall {
    ProposedCall {
        tool: tool.to_string(),
        arguments: raw(serde_json::json!({ argument: value })),
    }
}

/// A credential named relatively — `.env`, `cat .netrc` — is judged like its absolute
/// spelling: the read narrows the session to `self`, after which a public sink is out of
/// reach, and the command is refused with no remedy.
#[cfg(unix)]
#[tokio::test]
async fn the_battery_judges_relative_credential_paths_like_absolute_ones() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let config = composed_with_the_battery(&dir);
    let runtime = Arc::new(
        Runtime::open(config, dir.path().join("appa.db"), None).expect("the composed deployment opens"),
    );
    assert_eq!(
        hooks::handle(&runtime, HookEvent::SessionStart { root: root() }).await,
        HookDecision::Ack
    );

    for command in ["cat .netrc", "cat ~/.ssh/id_ed25519", "cat /home/me/.aws/credentials"] {
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
    assert_eq!(propose(&runtime, read.clone()).await, HookDecision::AllowCall { spawn: None });
    ran(&runtime, read).await;

    assert!(
        matches!(
            propose(&runtime, call("Artifact", "file_path", "page.html")).await,
            HookDecision::DenyCall { .. }
        ),
        "a session narrowed to `self` cannot publish"
    );
}
