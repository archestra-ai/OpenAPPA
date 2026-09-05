//! The canonical identity of a staged tree.
//!
//! This module is also compiled by `appa-runtime`'s `build.rs`, so keep it
//! limited to `std` and `sha2`. The digest names deployment directories and is
//! stamped into released binaries, so its encoding is observable identity: any
//! change here changes what a release claims to be.

use std::error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Caps on what a package source may be, whichever way it arrives. The bundle is
/// a few hundred small text files; these bound a hostile archive or a
/// development checkout that has accumulated a large generated directory,
/// without being tight enough to constrain the real one.
pub const MAX_ENTRIES: usize = 4096;
pub const MAX_UNCOMPRESSED_BYTES: u64 = 64 * 1024 * 1024;

/// Why a staged tree has no canonical identity.
#[derive(Debug)]
pub enum TreeDigestError {
    Read { path: PathBuf, source: io::Error },
    UnportablePath { path: PathBuf },
    UnsupportedEntry { path: PathBuf },
    Oversized { path: PathBuf, reason: String },
}

impl fmt::Display for TreeDigestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => write!(formatter, "cannot read {}: {source}", path.display()),
            Self::UnportablePath { path } => write!(formatter, "{} is not a portable path", path.display()),
            Self::UnsupportedEntry { path } => {
                write!(
                    formatter,
                    "{} is neither a regular file nor a directory",
                    path.display()
                )
            }
            Self::Oversized { path, reason } => write!(formatter, "{} is too large: {reason}", path.display()),
        }
    }
}

impl error::Error for TreeDigestError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            Self::UnportablePath { .. } | Self::UnsupportedEntry { .. } | Self::Oversized { .. } => None,
        }
    }
}

/// Every length prefix in the canonical digests is an unsigned 64-bit
/// big-endian integer followed immediately by exactly that many bytes. Naive
/// concatenation would be ambiguous, and the digests name deployment
/// directories, so the encoding is observable identity and is pinned rather
/// than left to the implementation.
pub fn absorb_field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
}

impl EntryKind {
    fn tag(self) -> u8 {
        match self {
            Self::File => b'f',
            Self::Directory => b'd',
        }
    }
}

/// One entry of a staged tree, in the spelling the digest encodes it under.
pub struct StagedEntry {
    pub portable: String,
    pub kind: EntryKind,
    pub absolute: PathBuf,
}

/// The identity of a staged tree: its portable paths, kinds and file bytes in
/// canonical order. Computed after staging and before rendering, so it never
/// depends on the paths rendered into the tree.
pub fn canonical_tree_digest(root: &Path) -> Result<[u8; 32], TreeDigestError> {
    let mut hasher = Sha256::new();
    for entry in walk(root)? {
        absorb_field(&mut hasher, entry.portable.as_bytes());
        hasher.update([entry.kind.tag()]);
        match entry.kind {
            EntryKind::Directory => absorb_field(&mut hasher, &[]),
            EntryKind::File => absorb_file(&mut hasher, &entry.absolute)?,
        }
    }
    Ok(hasher.finalize().into())
}

/// The tree in canonical order, refused if it exceeds the source bounds.
pub fn walk(root: &Path) -> Result<Vec<StagedEntry>, TreeDigestError> {
    let mut entries = Vec::new();
    collect(root, root, &mut entries)?;
    if entries.len() > MAX_ENTRIES {
        return Err(TreeDigestError::Oversized {
            path: root.to_path_buf(),
            reason: format!("it holds more than {MAX_ENTRIES} entries"),
        });
    }
    let mut total = 0u64;
    for entry in entries.iter().filter(|entry| entry.kind == EntryKind::File) {
        let length = fs::metadata(&entry.absolute)
            .map_err(|source| TreeDigestError::Read {
                path: entry.absolute.clone(),
                source,
            })?
            .len();
        total = total.saturating_add(length);
        if total > MAX_UNCOMPRESSED_BYTES {
            return Err(TreeDigestError::Oversized {
                path: root.to_path_buf(),
                reason: format!("it holds more than {MAX_UNCOMPRESSED_BYTES} bytes"),
            });
        }
    }
    // Bytewise on the UTF-8 path bytes, never locale collation.
    entries.sort_by(|left, right| left.portable.as_bytes().cmp(right.portable.as_bytes()));
    Ok(entries)
}

/// A staged relative path as the canonical digest encodes it: UTF-8 with `/`
/// separators on every platform. The bundle is ASCII filenames, so a path that
/// cannot be spelled this way is refused rather than normalized.
fn portable_relative_path(relative: &Path) -> Result<String, TreeDigestError> {
    let unportable = || TreeDigestError::UnportablePath {
        path: relative.to_path_buf(),
    };
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            std::path::Component::Normal(part) => parts.push(part.to_str().ok_or_else(unportable)?),
            _ => return Err(unportable()),
        }
    }
    Ok(parts.join("/"))
}

/// A file's length prefix and then its bytes, streamed.
///
/// The length comes from the file's own metadata and exactly that many bytes are
/// absorbed, so the encoding stays the length-prefixed one the digest is defined
/// as, without holding a whole file in memory.
fn absorb_file(hasher: &mut Sha256, path: &Path) -> Result<(), TreeDigestError> {
    use std::io::Read;

    let read = |source: io::Error| TreeDigestError::Read {
        path: path.to_path_buf(),
        source,
    };
    let mut file = fs::File::open(path).map_err(read)?;
    let length = file.metadata().map_err(read)?.len();
    hasher.update(length.to_be_bytes());

    let mut remaining = length;
    let mut buffer = [0u8; 64 * 1024];
    while remaining > 0 {
        let wanted = remaining.min(buffer.len() as u64) as usize;
        let filled = file.read(&mut buffer[..wanted]).map_err(read)?;
        if filled == 0 {
            return Err(TreeDigestError::Oversized {
                path: path.to_path_buf(),
                reason: "it changed size while being read".to_owned(),
            });
        }
        hasher.update(&buffer[..filled]);
        remaining -= filled as u64;
    }
    Ok(())
}

/// What a Python interpreter leaves behind next to a package's helpers. It is
/// generated, it is never distributed — staging drops it on the way into a
/// bundle — and running the tests must not change what a package digests to.
fn generated(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy();
    name == "__pycache__" || name.ends_with(".pyc") || name.ends_with(".pyo")
}

fn collect(root: &Path, directory: &Path, entries: &mut Vec<StagedEntry>) -> Result<(), TreeDigestError> {
    let read = |source: io::Error| TreeDigestError::Read {
        path: directory.to_path_buf(),
        source,
    };
    for entry in fs::read_dir(directory).map_err(read)? {
        let entry = entry.map_err(read)?;
        if generated(&entry.file_name()) {
            continue;
        }
        let absolute = entry.path();
        let relative = absolute
            .strip_prefix(root)
            .map_err(|_| TreeDigestError::UnportablePath { path: absolute.clone() })?;
        let portable = portable_relative_path(relative)?;
        // Symlinks and special files are refused here exactly as in extraction:
        // the bundle is regular files and directories.
        let kind = entry.file_type().map_err(|source| TreeDigestError::Read {
            path: absolute.clone(),
            source,
        })?;
        if kind.is_dir() {
            entries.push(StagedEntry {
                portable,
                kind: EntryKind::Directory,
                absolute: absolute.clone(),
            });
            collect(root, &absolute, entries)?;
        } else if kind.is_file() {
            entries.push(StagedEntry {
                portable,
                kind: EntryKind::File,
                absolute,
            });
        } else {
            return Err(TreeDigestError::UnsupportedEntry { path: absolute });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(digest: [u8; 32]) -> String {
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// A released binary stamps this digest, so the algorithm is pinned against
    /// a fixture whose expected value was derived from the encoding rules
    /// independently of this implementation.
    #[test]
    fn digest_of_a_fixture_tree_is_pinned() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fs::write(root.join("a.txt"), "alpha\n").unwrap();
        fs::create_dir(root.join("dir")).unwrap();
        fs::write(root.join("dir/b.bin"), [0u8, 1, 2, 255]).unwrap();
        fs::write(root.join("z"), "").unwrap();

        assert_eq!(
            hex(canonical_tree_digest(root).unwrap()),
            "2619322b5feb343f951e2ed55ab86948a93d9460eea0fc70d46a20fdb9f87dec"
        );
    }

    #[test]
    fn digest_ignores_directory_iteration_order() {
        let first = tempfile::tempdir().unwrap();
        fs::write(first.path().join("a"), "one").unwrap();
        fs::write(first.path().join("b"), "two").unwrap();

        let second = tempfile::tempdir().unwrap();
        fs::write(second.path().join("b"), "two").unwrap();
        fs::write(second.path().join("a"), "one").unwrap();

        assert_eq!(
            canonical_tree_digest(first.path()).unwrap(),
            canonical_tree_digest(second.path()).unwrap()
        );
    }

    #[test]
    fn digest_separates_a_path_from_the_bytes_beside_it() {
        let first = tempfile::tempdir().unwrap();
        fs::write(first.path().join("ab"), "c").unwrap();

        let second = tempfile::tempdir().unwrap();
        fs::write(second.path().join("a"), "bc").unwrap();

        assert_ne!(
            canonical_tree_digest(first.path()).unwrap(),
            canonical_tree_digest(second.path()).unwrap()
        );
    }

    #[test]
    fn a_python_cache_does_not_change_what_a_package_digests_to() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("audience-source.py"), "print()").unwrap();
        let clean = canonical_tree_digest(directory.path()).unwrap();

        fs::create_dir(directory.path().join("__pycache__")).unwrap();
        fs::write(
            directory
                .path()
                .join("__pycache__")
                .join("audience-source.cpython-313.pyc"),
            "compiled",
        )
        .unwrap();
        fs::write(directory.path().join("stray.pyc"), "compiled").unwrap();

        assert_eq!(
            canonical_tree_digest(directory.path()).unwrap(),
            clean,
            "running the tests must not move a package's digest"
        );
    }

    #[test]
    fn walk_refuses_a_symlink() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("real"), "x").unwrap();
        std::os::unix::fs::symlink(directory.path().join("real"), directory.path().join("link")).unwrap();

        assert!(matches!(
            canonical_tree_digest(directory.path()),
            Err(TreeDigestError::UnsupportedEntry { .. })
        ));
    }

    #[test]
    fn walk_refuses_more_entries_than_the_cap() {
        let directory = tempfile::tempdir().unwrap();
        for index in 0..=MAX_ENTRIES {
            fs::write(directory.path().join(format!("f{index}")), "").unwrap();
        }

        assert!(matches!(walk(directory.path()), Err(TreeDigestError::Oversized { .. })));
    }

    #[test]
    fn portable_path_uses_forward_slashes() {
        let relative: PathBuf = ["plugin", "hooks", "hooks.json"].iter().collect();
        assert_eq!(portable_relative_path(&relative).unwrap(), "plugin/hooks/hooks.json");
    }

    #[test]
    fn portable_path_refuses_traversal_components() {
        assert!(portable_relative_path(Path::new("../escape")).is_err());
    }
}
