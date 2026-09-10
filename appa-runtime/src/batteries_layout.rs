//! The repository paths that make up the batteries archive a generation
//! carries, and the tree the build digests for its identity.
//!
//! This module is also compiled by `build.rs`, which reads only the mappings,
//! so keep its dependencies to `std`. One mapping drives build-time identity
//! and the staging of a development build's own archive.

use std::fs;
use std::io;
use std::path::Path;

/// The host-side code of a protected session is the deployed binary, so the
/// archive carries only the batteries a policy may include.
pub const REPOSITORY_MAPPINGS: [(&str, &str); 1] = [("marketplace/batteries", "batteries")];

/// Stage the batteries archive's tree from an OpenAPPA repository checkout.
pub fn stage_repository(repository: &Path, destination: &Path) -> io::Result<()> {
    fs::create_dir_all(destination)?;
    for (source, target) in REPOSITORY_MAPPINGS {
        let source = repository.join(source);
        let target = destination.join(target);
        copy_entry(&source, &target)?;
    }
    Ok(())
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

/// What a mapped directory carries that the archive does not.
///
/// Generated Python caches are not source: a developer's checkout has them
/// while a GitHub source archive and a clean release runner never do, so
/// excluding them keeps every staging path byte-identical.
/// `appa-package.toml` is marketplace metadata — it describes the package to
/// the marketplace, and a deployment reads the policy beside it, never the
/// manifest.
fn excluded_from_staging(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy();
    name == "__pycache__" || name.ends_with(".pyc") || name.ends_with(".pyo") || name == PACKAGE_MANIFEST
}
