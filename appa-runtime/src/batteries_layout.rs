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

/// Keep manifests in deployed batteries: local setup reads their readiness metadata.
fn excluded_from_staging(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy();
    name == "__pycache__"
        || name.ends_with(".pyc")
        || name.ends_with(".pyo")
        || (name.starts_with("test_") && name.ends_with(".py"))
}
