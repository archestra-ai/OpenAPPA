//! How a batteries tree is copied into the archive a generation carries,
//! and into the tree the build digests for its identity.
//!
//! This module is also compiled by `build.rs`. One source drives build-time
//! identity and the staging of a development build's own archive
//! ([`crate::batteries_staging`]).

use std::fs;
use std::io;
use std::path::Path;

pub(crate) fn copy_entry(source: &Path, destination: &Path) -> io::Result<()> {
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
/// manifest. A battery's `test_*.py` suites exercise its scripts in the
/// repository; a deployment runs the scripts, never the suites.
fn excluded_from_staging(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy();
    name == "__pycache__"
        || name.ends_with(".pyc")
        || name.ends_with(".pyo")
        || name == PACKAGE_MANIFEST
        || (name.starts_with("test_") && name.ends_with(".py"))
}
