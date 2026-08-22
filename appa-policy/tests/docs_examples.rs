//! The policy reference's resolver examples are held to the loader.
//!
//! `website/content/docs/contracts.md` is a golden file: what it shows a reader typing has to
//! be what this crate accepts. Nothing else in the test suite reads it, so a syntax change that
//! landed in the loader and not in the guide would otherwise ship unnoticed.

use std::path::PathBuf;

use appa_policy::Config;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crate sits one level under the repository root")
        .to_path_buf()
}

/// Every fenced TOML block of the "Dynamic resolvers" section, in document order.
fn resolver_examples(reference: &str) -> Vec<String> {
    let section = reference
        .split_once("\n### Dynamic resolvers\n")
        .expect("the reference has a dynamic resolvers section")
        .1;
    let section = section.split_once("\n### ").map_or(section, |(before, _)| before);

    let mut examples = Vec::new();
    let mut rest = section;
    while let Some((_, after)) = rest.split_once("```toml\n") {
        let (block, tail) = after.split_once("```").expect("a fenced block closes");
        examples.push(block.to_string());
        rest = tail;
    }
    examples
}

#[test]
fn every_resolver_example_in_the_policy_reference_loads() {
    let reference = std::fs::read_to_string(repo_root().join("website/content/docs/contracts.md"))
        .expect("the policy reference is readable");
    let examples = resolver_examples(&reference);
    assert!(
        examples.len() >= 4,
        "expected the three worked examples and the builtin declaration, found {}",
        examples.len()
    );

    for (index, example) in examples.iter().enumerate() {
        // The guide shows fragments; a reader's file opens with the version.
        let policy = match example.starts_with("version") {
            true => example.clone(),
            false => format!("version = 1\n\n{example}"),
        };
        assert!(
            !policy.contains("resolvers = ["),
            "example {index} still shows the retired binding syntax:\n{example}"
        );
        if let Err(error) = Config::from_toml_str(&policy) {
            panic!("example {index} does not load: {error}\n{policy}");
        }
    }
}
