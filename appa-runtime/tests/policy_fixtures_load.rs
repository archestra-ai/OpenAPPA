
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
fn every_marked_documentation_example_loads() {
    let root = repo_root();
    let mut checked = 0;
    for doc in [
        "website/content/docs/how-it-works.md",
        "website/content/docs/contracts.md",
    ] {
        let path = root.join(doc);
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        for (marker, wrap) in [
            ("<!-- appa:example -->", false),
            ("<!-- appa:example-fragment -->", true),
        ] {
            let mut rest = text.as_str();
            while let Some(at) = rest.find(marker) {
                rest = &rest[at + marker.len()..];
                let fence = rest
                    .find("```toml")
                    .unwrap_or_else(|| panic!("{doc}: a marker has no following ```toml fence"));
                let body_start = fence
                    + rest[fence..]
                        .find('\n')
                        .unwrap_or_else(|| panic!("{doc}: unterminated fence line"))
                    + 1;
                let body_end = body_start
                    + rest[body_start..]
                        .find("```")
                        .unwrap_or_else(|| panic!("{doc}: unterminated ```toml fence"));
                let body = &rest[body_start..body_end];
                let policy = if wrap {
                    format!("version = 1\n{body}")
                } else {
                    body.to_string()
                };
                if let Err(error) = Config::from_toml_str(&policy) {
                    panic!("{doc}: a marked example does not load ({error}):\n{body}");
                }
                checked += 1;
                rest = &rest[body_end..];
            }
        }
    }
    assert!(checked >= 5, "expected the guides' marked examples, found {checked}");
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
