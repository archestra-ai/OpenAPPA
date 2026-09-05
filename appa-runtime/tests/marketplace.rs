//! The marketplace manifest against the marketplace it describes.
//!
//! `marketplace.toml` is what a deployment authenticates a package against, so
//! a digest that has drifted from the directory it names is the one failure
//! that must never reach a release. This runs in the repository, where the
//! packages are files; a deployment checks the same digests against what it
//! fetched.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

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

/// The recorded digest is the digest of the package's committed content, which
/// is what a deployment fetches and digests in turn. It is read out of Git
/// rather than off the working tree, because a working tree also holds what the
/// tests left behind — a `__pycache__` beside a battery's helper is not part of
/// the package, and never reaches the archive a deployment resolves. When this
/// fails, the package changed and its digest did not: the message carries the
/// lines to write back.
#[test]
fn every_recorded_digest_is_the_digest_of_its_committed_package() {
    let exported = tempfile::tempdir().expect("a temp dir is creatable");
    export_committed(exported.path());
    let root = exported.path().join("marketplace");

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

/// The marketplace subtree as `HEAD` holds it. `git archive` is how the build
/// script already reads committed content, and how a deployment will receive a
/// generation.
fn export_committed(destination: &Path) {
    let archive = Command::new("git")
        .arg("-C")
        .arg(repository())
        .args(["archive", "--format=tar", "HEAD", "--", "marketplace"])
        .output()
        .expect("git runs");
    assert!(
        archive.status.success(),
        "git archive failed: {}",
        String::from_utf8_lossy(&archive.stderr)
    );
    let mut extract = Command::new("tar")
        .arg("-x")
        .arg("-C")
        .arg(destination)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("tar runs");
    extract
        .stdin
        .take()
        .expect("tar takes its input")
        .write_all(&archive.stdout)
        .expect("the archive is written");
    assert!(extract.wait().expect("tar finishes").success(), "tar failed");
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
