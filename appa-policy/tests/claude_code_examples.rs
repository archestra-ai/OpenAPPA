//! The shipped Claude Code example deployments are held to the loader.
//!
//! Each example is a deployment file: the policy under `[policy]`, the bindings under
//! `[externals]`. Only the policy is this crate's dialect, so the test lifts that table out and
//! compiles it exactly as the runtime hands it over.

use std::path::PathBuf;

use appa_engine::authority::CastResolution;
use appa_engine::label::Dimension;
use appa_engine::names::CastName;
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

fn load(example: &str) -> Config {
    Config::from_toml_str(&policy_of(example)).unwrap_or_else(|error| panic!("{example} does not load: {error}"))
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
    assert!(examples.len() >= 3, "expected the shipped examples, found {examples:?}");
    for example in &examples {
        load(example);
    }
}

#[test]
fn the_casts_example_registers_what_it_describes() {
    let config = load("claude-code-casts.appa.toml");
    let registry = config.registry();
    // Each example tool has one contract, so the name alone finds it.
    let tool = |name: &str| registry.tools().find(|tool| tool.name.as_str() == name);

    let unannotated = tool("mcp__github__issue_read").expect("the unannotated tool registers");
    assert_eq!(unannotated.delta, None);

    let pending = tool("WebFetch").expect("the pending-cast tool registers");
    assert_eq!(pending.pending_cast_dim(), Some(Dimension::Trust));

    let sink = tool("mcp__github__issue_write").expect("the sink registers");
    assert!(sink.requires.label.trust_floor.is_some());

    let classifier = registry
        .cast(&CastName::new("page-classifier"))
        .expect("the resolver cast registers");
    assert!(matches!(classifier.resolution, CastResolution::Resolver { .. }));
    let fallback = registry
        .cast(&CastName::new("github-content"))
        .expect("the constant cast registers");
    assert!(matches!(fallback.resolution, CastResolution::Constant(_)));
}
