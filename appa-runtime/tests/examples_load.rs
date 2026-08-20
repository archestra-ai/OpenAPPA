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
