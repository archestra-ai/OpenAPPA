//! The repository paths that make up the distributable Claude Code plugin.
//!
//! This module is also compiled by `build.rs`, so keep it limited to `std`.
//! One mapping drives build-time identity and runtime staging from a GitHub
//! source archive.

use std::fs;
use std::io;
use std::path::Path;

pub const REPOSITORY_MAPPINGS: [(&str, &str); 7] = [
    ("integrations/claude-code/.claude-plugin", ".claude-plugin"),
    ("integrations/claude-code/plugin", "plugin"),
    ("integrations/claude-code/examples", "examples"),
    ("batteries", "batteries"),
    ("integrations/claude-code/README.md", "README.md"),
    ("integrations/claude-code/live-gate-check.py", "live-gate-check.py"),
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
            if ignored_generated_entry(&name) {
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

fn ignored_generated_entry(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy();
    name == "__pycache__" || name.ends_with(".pyc") || name.ends_with(".pyo")
}
