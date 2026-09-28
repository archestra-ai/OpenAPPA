//! Embeds the repository's batteries tree, so a host that links this crate
//! from a cargo checkout serves every battery of the version it pinned
//! without a marketplace download: one `include_str!` per file, listed per
//! battery, generated into `OUT_DIR` and read by `src/bundled.rs`.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let crate_root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR"));
    let batteries = crate_root
        .parent()
        .expect("appa-package is inside the repository")
        .join("marketplace")
        .join("batteries");
    println!("cargo:rerun-if-changed={}", batteries.display());

    let mut directories = fs::read_dir(&batteries)
        .unwrap_or_else(|error| panic!("{}: {error}", batteries.display()))
        .map(|entry| entry.expect("a batteries entry is readable").path())
        .filter(|path| path.join("appa-package.toml").is_file())
        .collect::<Vec<_>>();
    directories.sort();

    let mut table = String::from("&[\n");
    for directory in directories {
        let name = directory
            .file_name()
            .and_then(|name| name.to_str())
            .expect("a battery directory has a UTF-8 name");
        writeln!(table, "    BundledBattery {{ name: {name:?}, files: &[").expect("a String grows");
        let mut files = Vec::new();
        collect_files(&directory, &directory, &mut files);
        if let Some(listed) = git_listed(&directory) {
            files.retain(|(relative, _)| listed.contains(relative));
        }
        files.sort();
        for (relative, absolute) in files {
            let absolute = absolute.to_str().expect("a battery file has a UTF-8 path");
            writeln!(
                table,
                "        BundledFile {{ path: {relative:?}, text: include_str!({absolute:?}) }},"
            )
            .expect("a String grows");
        }
        table.push_str("    ] },\n");
    }
    table.push(']');

    let out = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR")).join("bundled_batteries.rs");
    fs::write(&out, table).unwrap_or_else(|error| panic!("{}: {error}", out.display()));
}

/// The files git tracks or would track under `directory`, relative to it: the set
/// `scripts/appa-marketplace.sh` digests, so a test run's `__pycache__` is never
/// bundled. `None` outside a git checkout, where every file counts.
fn git_listed(directory: &Path) -> Option<Vec<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(["ls-files", "-z", "--cached", "--others", "--exclude-standard"])
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    let listed = String::from_utf8(output.stdout).ok()?;
    Some(
        listed
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

/// Every regular file under `directory`, as the path relative to the battery root the
/// package tree names it by, with `/` separators on every platform.
fn collect_files(root: &Path, directory: &Path, files: &mut Vec<(String, PathBuf)>) {
    for entry in fs::read_dir(directory).unwrap_or_else(|error| panic!("{}: {error}", directory.display())) {
        let path = entry.expect("a battery entry is readable").path();
        if path.is_dir() {
            collect_files(root, &path, files);
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .expect("the file is under its battery")
            .components()
            .map(|component| component.as_os_str().to_str().expect("a battery file has a UTF-8 name"))
            .collect::<Vec<_>>()
            .join("/");
        files.push((relative, path));
    }
}
