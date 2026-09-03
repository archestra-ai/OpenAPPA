mod common;
use common::repo_root;

use std::path::{Path, PathBuf};

use appa_runtime::api::Runtime;
use appa_runtime::config::Config;

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

#[cfg(unix)]
#[test]
fn the_initialized_default_composes_with_the_claude_code_battery() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
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

    let config = Config::load(&root).expect("the initialized config and battery compose");
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
    assert_eq!(read.len(), 1, "the plain Read rule composes once, after the battery's selectors");
    assert!(read[0].get("annotator").is_none(), "Read is static: no annotator names a reader");

    let database = dir.path().join("appa.db");
    Runtime::open(config, database, None).expect("the composed deployment opens");
}
