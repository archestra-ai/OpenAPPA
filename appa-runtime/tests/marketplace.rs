//! The marketplace manifest against the marketplace it describes.
//!
//! `marketplace.toml` is what a deployment authenticates a package against, so
//! a digest that has drifted from the directory it names is the one failure
//! that must never reach a release. This runs in the repository, where the
//! packages are files; a deployment checks the same digests against what it
//! fetched.

use std::path::{Path, PathBuf};

use appa_package::{Marketplace, TreeDigest, validate_package};

fn repository() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("appa-runtime is inside the repository")
        .to_path_buf()
}

fn marketplace_root() -> PathBuf {
    repository().join("marketplace")
}

fn manifest() -> Marketplace {
    Marketplace::read(&marketplace_root().join("marketplace.toml")).expect("the marketplace manifest reads")
}

/// Every package the manifest names is a package: it parses, its declared paths
/// exist, its policy names only namespaces it covers, and it runs only helpers
/// it ships.
#[test]
fn every_package_the_marketplace_names_validates() {
    let root = marketplace_root();
    for entry in manifest().packages {
        let directory = root.join(entry.path.as_str());
        let package = validate_package(&directory).unwrap_or_else(|error| {
            panic!("{} does not validate: {error}", entry.path);
        });
        assert_eq!(
            package.name, entry.name,
            "{} is listed as {} and calls itself {}",
            entry.path, entry.name, package.name
        );
    }
}

/// The recorded digest is the digest of the directory. When this fails, the
/// package changed and its digest did not: the message carries the lines to
/// write back.
#[test]
fn every_recorded_digest_is_the_digest_of_its_package() {
    let root = marketplace_root();
    let drifted: Vec<String> = manifest()
        .packages
        .iter()
        .filter_map(|entry| {
            let directory = root.join(entry.path.as_str());
            let found = TreeDigest::of_tree(&directory).unwrap_or_else(|error| {
                panic!("{} cannot be digested: {error}", entry.path);
            });
            (found != entry.digest).then(|| format!("[{}] {} -> {}", entry.path, entry.digest, found))
        })
        .collect();

    assert!(
        drifted.is_empty(),
        "marketplace.toml records a digest for a package that has since changed:\n  {}",
        drifted.join("\n  ")
    );
}

/// A package directory that no entry names would ship unauthenticated, so the
/// manifest must list every one of them.
#[test]
fn every_package_directory_is_named_by_the_marketplace() {
    let root = marketplace_root();
    let listed: Vec<PathBuf> = manifest()
        .packages
        .iter()
        .map(|entry| root.join(entry.path.as_str()))
        .collect();

    let mut found = Vec::new();
    for kind in ["adapters", "batteries"] {
        let directory = root.join(kind);
        for entry in std::fs::read_dir(&directory).expect("the marketplace holds both package kinds") {
            let path = entry.expect("the directory entry reads").path();
            if path.join(appa_package::MANIFEST_FILE).is_file() {
                found.push(path);
            }
        }
    }

    let unlisted: Vec<&Path> = found
        .iter()
        .filter(|path| !listed.contains(path))
        .map(PathBuf::as_path)
        .collect();
    assert!(
        unlisted.is_empty(),
        "a package directory the marketplace does not name: {unlisted:?}"
    );
}
