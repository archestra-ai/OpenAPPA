//! The policy reference's TOML examples are held to the loader.
//!
//! `website/content/docs/contracts.md` is a golden file: what it shows a reader typing has to
//! be what this crate accepts. Nothing else in the test suite reads it, so a dialect change
//! that landed in the loader and not in the guide would otherwise ship unnoticed.

use std::path::PathBuf;

use appa_policy::Config;

fn policy_reference() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crate sits one level under the repository root")
        .join("website/content/docs/contracts.md");
    std::fs::read_to_string(path).expect("the policy reference is readable")
}

/// Every fenced TOML block, in document order.
fn toml_fences(reference: &str) -> Vec<&str> {
    let mut fences = Vec::new();
    let mut rest = reference;
    while let Some((_, after)) = rest.split_once("```toml\n") {
        let (fence, tail) = after.split_once("```").expect("a fenced block closes");
        fences.push(fence);
        rest = tail;
    }
    fences
}

/// The guide shows policy fragments; a reader's file opens with the version.
fn as_policy(fence: &str) -> String {
    match fence.starts_with("version") {
        true => fence.to_string(),
        false => format!("version = 2\n\n{fence}"),
    }
}

/// A fence holding only `[externals.…]` entries is a deployment-binding example. The policy
/// dialect this crate loads has no `[externals]` table — the deployment owns it — so such a
/// fence answers only to TOML syntax here.
fn is_externals_example(table: &toml::Table) -> bool {
    !table.is_empty() && table.keys().all(|key| key == "externals")
}

#[test]
fn every_toml_fence_in_the_policy_reference_loads() {
    let reference = policy_reference();
    let fences = toml_fences(&reference);
    assert!(!fences.is_empty(), "the policy reference shows TOML examples");
    for fence in fences {
        let table: toml::Table = fence
            .parse()
            .unwrap_or_else(|error| panic!("a fence is not valid TOML: {error}\n{fence}"));
        if is_externals_example(&table) {
            continue;
        }
        let policy = as_policy(fence);
        if let Err(error) = Config::from_toml_str(&policy) {
            panic!("a policy example does not load: {error}\n{policy}");
        }
    }
}
