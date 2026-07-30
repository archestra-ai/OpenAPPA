
use std::path::{Path, PathBuf};

use appa_runtime::config::Config;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("appa-runtime sits one level below the repo root")
        .to_path_buf()
}

fn policies() -> Vec<PathBuf> {
    let root = repo_root();
    let mut found = Vec::new();
    for glob in ["bench-corp/policies", "harness-agentdojo/src/appa_dojo/contracts"] {
        let dir = root.join(glob);
        for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
            let path = entry.expect("dir entry").path();
            if path.extension().is_some_and(|ext| ext == "toml") {
                found.push(path);
            }
        }
    }
    let scenarios = root.join("bench-corp/scenarios");
    for entry in std::fs::read_dir(&scenarios).expect("bench-corp/scenarios") {
        let policy_dir = entry.expect("dir entry").path().join("policy");
        let Ok(read) = std::fs::read_dir(&policy_dir) else {
            continue;
        };
        for entry in read {
            let path = entry.expect("dir entry").path();
            if path.extension().is_some_and(|ext| ext == "toml") {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

#[test]
fn every_shipped_policy_loads() {
    let policies = policies();
    assert!(
        policies.len() > 10,
        "expected the repo's policy corpus, found {policies:?}"
    );
    for path in policies {
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        if let Err(error) = Config::from_toml_str(&text) {
            panic!("{} does not load: {error}", path.display());
        }
    }
}

#[test]
fn retired_policy_keys_are_load_errors() {
    for retired in [
        "version = 1\n[[preamble]]\nrole = \"system\"\ncontent = \"you are confined\"\n",
        "version = 1\n[[tool]]\nname = \"export\"\noutput_sanitizer = \"pii\"\n",
    ] {
        assert!(
            Config::from_toml_str(retired).is_err(),
            "a retired key still loads: {retired}"
        );
    }
}
