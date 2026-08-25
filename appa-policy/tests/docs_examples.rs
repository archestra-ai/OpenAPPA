//! The policy reference's resolver and cast examples are held to the loader.
//!
//! `website/content/docs/contracts.md` is a golden file: what it shows a reader typing has to
//! be what this crate accepts. Nothing else in the test suite reads it, so a syntax change that
//! landed in the loader and not in the guide would otherwise ship unnoticed.

use std::path::PathBuf;

use appa_engine::authority::CastResolution;
use appa_engine::names::CastName;
use appa_policy::Config;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crate sits one level under the repository root")
        .to_path_buf()
}

fn policy_reference() -> String {
    std::fs::read_to_string(repo_root().join("website/content/docs/contracts.md"))
        .expect("the policy reference is readable")
}

/// Every fenced TOML block of one section, in document order. The section runs from its
/// heading line to the next heading of the same level.
fn section_examples<'a>(reference: &'a str, heading: &str) -> Vec<&'a str> {
    let level = heading.split(' ').next().expect("a heading opens with its hashes");
    let section = reference
        .split_once(&format!("\n{heading}\n"))
        .unwrap_or_else(|| panic!("the reference has a {heading:?} section"))
        .1;
    let section = section
        .split_once(&format!("\n{level} "))
        .map_or(section, |(before, _)| before);

    let mut examples = Vec::new();
    let mut rest = section;
    while let Some((_, after)) = rest.split_once("```toml\n") {
        let (block, tail) = after.split_once("```").expect("a fenced block closes");
        if !block.starts_with("[externals.") {
            examples.push(block);
        }
        rest = tail;
    }
    examples
}

/// The guide shows fragments; a reader's file opens with the version.
fn as_policy(example: &str) -> String {
    match example.starts_with("version") {
        true => example.to_string(),
        false => format!("version = 1\n\n{example}"),
    }
}

#[test]
fn every_resolver_example_in_the_policy_reference_loads() {
    let reference = policy_reference();
    let examples = section_examples(&reference, "### Dynamic resolvers");
    assert!(
        examples.len() >= 3,
        "expected the three worked policy examples, found {}",
        examples.len()
    );

    for (index, example) in examples.iter().enumerate() {
        let policy = as_policy(example);
        assert!(
            !policy.contains("resolvers = ["),
            "example {index} still shows the retired binding syntax:\n{example}"
        );
        if let Err(error) = Config::from_toml_str(&policy) {
            panic!("example {index} does not load: {error}\n{policy}");
        }
    }
}

#[test]
fn the_cast_example_in_the_policy_reference_loads() {
    let reference = policy_reference();
    let examples = section_examples(&reference, "## Casts");
    // The section shows the cast declarations, then the deployment's `[externals]` binding for
    // the resolver. Only the declarations are this crate's dialect.
    let declarations = examples
        .iter()
        .find(|example| example.contains("[[cast]]"))
        .expect("the casts section declares a cast");

    // A scoped cast is unreachable — a load error — until a tool in its scope can use it, and
    // the guide shows the declarations without their tools. One unannotated tool tagged for
    // the classifier's scope is the origin the declarations assume.
    let policy = as_policy(&format!(
        "[[tool]]\nname = \"read_ticket\"\ntags = [\"support\"]\n\n{declarations}"
    ));
    let config = match Config::from_toml_str(&policy) {
        Ok(config) => config,
        Err(error) => panic!("the cast example does not load: {error}\n{policy}"),
    };
    let classifier = config
        .registry()
        .cast(&CastName::new("content-classifier"))
        .expect("the resolver cast registers");
    assert!(matches!(classifier.resolution, CastResolution::Resolver { .. }));
    let fallback = config
        .registry()
        .cast(&CastName::new("paranoid-default"))
        .expect("the constant fallback registers");
    assert!(matches!(fallback.resolution, CastResolution::Constant(_)));
}
