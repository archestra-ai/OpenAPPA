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

use appa_package::{Host, Marketplace, Role, TreeDigest, check_ownership, validate_package};
use appa_runtime::config::Config;

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
///
/// And the packages hold together: a namespace and a credential each have one
/// owner. A deployment installs several of these side by side into one policy,
/// so a second package covering the first's namespace would append contracts to
/// tools the first never declared, and one naming a credential the first's
/// prefix covers would be handed it at spawn.
#[test]
fn every_package_the_marketplace_names_validates() {
    let root = marketplace_root();
    let mut packages = Vec::new();
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
        packages.push(package);
    }
    check_ownership(&packages).expect("the marketplace's packages hold together");
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

/// A battery that validates is a battery a deployment can include. The two
/// checks are written in crates that cannot depend on each other — the
/// validator states what a fragment may carry, the config loader enforces it —
/// so this composes each battery into the root config of every host it declares
/// and loads the result. A rule one of them learns and the other does not shows
/// up here as a battery that validates and will not load.
#[test]
fn every_battery_composes_into_each_host_it_declares() {
    let root = marketplace_root();
    let manifest = manifest();
    let default_policy = |host: Host| {
        manifest
            .packages
            .iter()
            .find_map(
                |entry| match validate_package(&root.join(entry.path.as_str())).ok()?.role {
                    Role::Adapter(adapter) if adapter.host() == host => {
                        Some(root.join(entry.path.as_str()).join(adapter.default_policy().as_str()))
                    }
                    _ => None,
                },
            )
            .unwrap_or_else(|| panic!("the marketplace ships no adapter for {host}"))
    };

    for entry in &manifest.packages {
        let directory = root.join(entry.path.as_str());
        let Role::Battery(battery) = validate_package(&directory).expect("the package validates").role else {
            continue;
        };
        for host in &battery.hosts {
            // A deployment holds the package beside its root config and
            // includes it by a relative path, which is the only shape the
            // loader accepts. The composition is that arrangement.
            let composed = tempfile::tempdir().expect("a temp dir is creatable");
            let installed = composed.path().join(entry.name.as_str());
            copy_tree(&directory, &installed);
            let seed = std::fs::read_to_string(default_policy(*host)).expect("the adapter ships its default policy");
            let path = composed.path().join("appa.toml");
            std::fs::write(
                &path,
                format!("include = [\"{}/{}\"]\n{seed}", entry.name, battery.policy),
            )
            .expect("the composed config is writable");

            Config::load(&path)
                .unwrap_or_else(|error| panic!("{} validates but does not compose into {host}: {error}", entry.path));
        }
    }
}

/// One package directory, as a deployment would hold it.
fn copy_tree(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).expect("the destination is creatable");
    for entry in std::fs::read_dir(source).expect("the package directory reads") {
        let entry = entry.expect("the directory entry reads");
        let target = destination.join(entry.file_name());
        match entry.file_type().expect("the entry has a type").is_dir() {
            true => copy_tree(&entry.path(), &target),
            false => {
                std::fs::copy(entry.path(), target).expect("the file copies");
            }
        }
    }
}

/// A fragment that validates loads.
///
/// `appa-package` states what a distributed fragment may carry and
/// `appa-runtime`'s config loader states what any include may carry, in two
/// crates that cannot depend on each other. Every rule one of them learns is a
/// chance for them to disagree, and the disagreement that matters has one
/// direction: a package that validates and then will not load breaks a
/// deployment that trusted the marketplace.
///
/// The converse is deliberately false. A battery is a fragment someone else
/// wrote and a deployment installed, so the marketplace refuses what the loader
/// merely tolerates from a file the operator wrote themselves — a `url` binding
/// reaching an endpoint the deployment did not choose is the plain case. Those
/// are the `!validates && loads` rows below, and each one is a decision rather
/// than a gap.
///
/// The rows are crafted to sit on each rule's edge. The shipped batteries sit
/// in the middle of every rule and would agree whatever the rules were.
#[test]
fn a_battery_that_validates_loads() {
    const HELPER: &str = "audience-source.py";
    let body = |declarations: &str, externals: &str| format!("[policy]\nversion = 2\n\n{declarations}\n{externals}");
    let tool = "[[policy.tool]]\nname = \"mcp/probe/read\"\ndelta = {}\n";
    let command = format!("[externals.audience.probe]\ncommand = [\"python3\", \"{HELPER}\"]\n");

    // Each row carries whether the marketplace accepts it, so a validator that
    // refused everything would fail here rather than satisfy the implication
    // vacuously.
    let accepted = true;
    let refused = false;
    let fragments = [
        ("a plain battery", accepted, body(tool, "")),
        ("a battery that runs its helper", accepted, body(tool, &command)),
        (
            "a provider credential",
            accepted,
            body(tool, &format!("{command}token_env = \"APPA_PROVIDER_PROBE\"\n")),
        ),
        (
            "a credential outside the provider namespace",
            refused,
            body(tool, &format!("{command}token_env = \"PROBE_TOKEN\"\n")),
        ),
        // Stricter than the loader on purpose: both of these load, and neither
        // belongs in a fragment a deployment installed from a marketplace.
        (
            "a url binding",
            refused,
            body(tool, "[externals.audience.probe]\nurl = \"http://127.0.0.1:9/x\"\n"),
        ),
        (
            "a builtin binding",
            refused,
            body(tool, "[externals.audience.probe]\nbuiltin = \"llm\"\n"),
        ),
        (
            "a root-only external setting",
            refused,
            body(tool, "[externals]\ntimeout_ms = 5000\n"),
        ),
        (
            "a root-only policy section",
            refused,
            body(&format!("{tool}\n[policy.audience]\nteam = []\n"), ""),
        ),
        (
            "a top-level table only a root carries",
            refused,
            format!("[deployment]\nname = \"x\"\n{}", body(tool, "")),
        ),
        ("no policy version", refused, format!("[policy]\n\n{tool}")),
        (
            "a declaration without a name",
            refused,
            body("[[policy.tool]]\ndelta = {}\n", ""),
        ),
    ];

    for (what, expected, fragment) in fragments {
        let package = tempfile::tempdir().expect("a temp dir is creatable");
        std::fs::write(
            package.path().join("appa-package.toml"),
            "schema = 1\nname = \"probe\"\ndescription = \"a fragment on a rule's edge\"\n\n[battery]\npolicy = \"appa.toml\"\nhosts = [\"claude-code\"]\nhelpers = [\"audience-source.py\"]\n",
        )
        .expect("the manifest is writable");
        std::fs::write(package.path().join("appa.toml"), &fragment).expect("the policy is writable");
        std::fs::write(package.path().join(HELPER), "print('{}')\n").expect("the helper is writable");

        let validates = validate_package(package.path()).is_ok();
        let loads = loads_beside_a_root(package.path());

        assert_eq!(
            validates, expected,
            "{what}: the marketplace changed its mind about this"
        );
        assert!(
            !validates || loads,
            "{what}: the package validates and then does not load"
        );
    }
}

/// Whether a deployment holding this package beside its root config loads.
fn loads_beside_a_root(package: &Path) -> bool {
    let deployment = tempfile::tempdir().expect("a temp dir is creatable");
    let installed = deployment.path().join("probe");
    copy_tree(package, &installed);
    let root = deployment.path().join("appa.toml");
    std::fs::write(
        &root,
        "include = [\"probe/appa.toml\"]\n[policy]\nversion = 2\n\n[externals]\ntimeout_ms = 5000\nmax_body_bytes = 65536\n",
    )
    .expect("the root config is writable");
    Config::load(&root).is_ok()
}
