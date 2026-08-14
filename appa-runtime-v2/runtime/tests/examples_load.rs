
use std::path::{Path, PathBuf};

use appa_runtime_v2::api::Runtime;
use appa_runtime_v2::config::Config;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the runtime crate sits two levels below the repo root")
        .to_path_buf()
}

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
fn every_bench_deployment_opens() {
    let root = repo_root();
    let mut deployments = toml_files(&root.join("bench-corp/policies"));
    for entry in std::fs::read_dir(root.join("bench-corp/scenarios")).expect("bench-corp/scenarios") {
        let policy_dir = entry.expect("the directory entry is readable").path().join("policy");
        if policy_dir.is_dir() {
            deployments.extend(toml_files(&policy_dir));
        }
    }
    assert!(
        deployments.len() > 10,
        "expected the benchmark's deployments, found {deployments:?}"
    );
    for path in &deployments {
        opens(path);
    }
}
