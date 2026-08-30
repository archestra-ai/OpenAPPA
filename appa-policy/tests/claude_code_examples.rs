//! The shipped Claude Code example deployments are held to the loader.
//!
//! Each example is a deployment file: the policy under `[policy]`, the bindings under
//! `[externals]`. Only the policy is this crate's dialect, so the test lifts that table out and
//! compiles it exactly as the runtime hands it over.

use std::path::PathBuf;

use appa_policy::Config;

fn examples_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crate sits one level under the repository root")
        .join("integrations/claude-code/examples")
}

/// The `[policy]` table of one deployment file, rendered as a policy of its own.
fn policy_of(example: &str) -> String {
    let path = examples_dir().join(example);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{} is readable: {error}", path.display()));
    let file: toml::Value = toml::from_str(&text).unwrap_or_else(|error| panic!("{example} parses: {error}"));
    let policy = file
        .get("policy")
        .unwrap_or_else(|| panic!("{example} carries a [policy] table"));
    toml::to_string(policy).expect("a parsed table renders")
}

fn load(example: &str) {
    Config::from_toml_str(&policy_of(example)).unwrap_or_else(|error| panic!("{example} does not load: {error}"));
}

#[test]
fn every_shipped_example_loads() {
    let mut examples: Vec<String> = std::fs::read_dir(examples_dir())
        .expect("the examples directory is readable")
        .map(|entry| {
            entry
                .expect("a directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.ends_with(".appa.toml"))
        .collect();
    examples.sort();
    assert!(!examples.is_empty(), "the shipped examples are present");
    for example in &examples {
        load(example);
    }
}
