//! The repository paths that make up the distributable Claude Code plugin,
//! and the canonical identity of a staged plugin tree.
//!
//! This module is also compiled by `build.rs`, which reads only the mappings
//! and the digest, so keep its dependencies to `std` and `appa-package`. One
//! mapping drives build-time identity and runtime staging from a GitHub source
//! archive, and one digest names both.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

pub const REPOSITORY_MAPPINGS: [(&str, &str); 9] = [
    ("marketplace/adapters/claude-code/.claude-plugin", ".claude-plugin"),
    ("marketplace/adapters/claude-code/plugin", "plugin"),
    ("integrations/appa-guide", "plugin/skills/appa-guide"),
    (
        "marketplace/adapters/claude-code/default.appa.toml",
        "examples/claude-code.appa.toml",
    ),
    (
        "marketplace/adapters/claude-code/hitl.appa.toml",
        "examples/claude-code-hitl.appa.toml",
    ),
    ("marketplace/batteries", "batteries"),
    ("marketplace/adapters/claude-code/README.md", "README.md"),
    (
        "marketplace/adapters/claude-code/live-gate-check.py",
        "live-gate-check.py",
    ),
    ("website/content/docs/contracts.md", "website/content/docs/contracts.md"),
];

/// Stage the plugin marketplace tree from an OpenAPPA repository checkout.
pub fn stage_repository(repository: &Path, destination: &Path) -> io::Result<()> {
    fs::create_dir_all(destination)?;
    for (source, target) in REPOSITORY_MAPPINGS {
        let source = repository.join(source);
        let target = destination.join(target);
        copy_entry(&source, &target)?;
    }
    materialize_claude_guide(destination)?;
    Ok(())
}

/// Claude Code loads only SKILL.md when a slash command starts. Reading a
/// reference would itself be a gated `Read` call, so materialize this host's
/// reference into that file. The canonical package stays decomposed for hosts
/// such as kagent that load their own reference through their native file tool.
fn materialize_claude_guide(destination: &Path) -> io::Result<()> {
    let guide = destination.join("plugin/skills/appa-guide");
    let reference = fs::read(guide.join("references/claude-code.md"))?;
    let mut skill = fs::OpenOptions::new().append(true).open(guide.join("SKILL.md"))?;
    skill.write_all(b"\n\n")?;
    skill.write_all(&reference)
}

fn copy_entry(source: &Path, destination: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is a symlink", source.display()),
        ));
    }
    if metadata.is_dir() {
        fs::create_dir_all(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            let name = entry.file_name();
            if excluded_from_staging(&name) {
                continue;
            }
            copy_entry(&entry.path(), &destination.join(name))?;
        }
        return Ok(());
    }
    if metadata.is_file() {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, destination)?;
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{} is neither a regular file nor a directory", source.display()),
    ))
}

/// The per-package marketplace manifest, by the one name every package uses.
const PACKAGE_MANIFEST: &str = "appa-package.toml";

/// What a mapped directory carries that the deployment does not.
///
/// Generated Python caches are not plugin source: a developer's checkout has
/// them while a GitHub source archive and a clean release runner never do, so
/// excluding them keeps all three staging paths byte-identical.
/// `appa-package.toml` is marketplace metadata — it describes the package to
/// the marketplace, and the deployment reads the policy beside it, never the
/// manifest.
fn excluded_from_staging(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy();
    name == "__pycache__" || name.ends_with(".pyc") || name.ends_with(".pyo") || name == PACKAGE_MANIFEST
}

// The canonical tree digest lives in `appa-package`: it names a package
// directory in the marketplace as well as a staged bundle, and the marketplace
// crate may not depend on this one. Re-exported here so this module stays the
// one name the staging path and the bundle installer read.
// `build.rs` includes this file and reads only `canonical_tree_digest`; the
// rest of the digest vocabulary is re-exported for the bundle installer.
#[allow(unused_imports)]
pub use appa_package::tree::{
    EntryKind, MAX_ENTRIES, MAX_UNCOMPRESSED_BYTES, StagedEntry, TreeDigestError, absorb_field, canonical_tree_digest,
    walk,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_staging_inlines_its_reference_after_the_router() {
        let directory = tempfile::tempdir().unwrap();
        let guide = directory.path().join("plugin/skills/appa-guide");
        fs::create_dir_all(guide.join("references")).unwrap();
        fs::write(guide.join("SKILL.md"), "router\n").unwrap();
        fs::write(guide.join("references/claude-code.md"), "# Claude Code\nflow\n").unwrap();

        materialize_claude_guide(directory.path()).unwrap();

        assert_eq!(
            fs::read_to_string(guide.join("SKILL.md")).unwrap(),
            "router\n\n\n# Claude Code\nflow\n"
        );
    }
}
