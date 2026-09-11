//! Draft, opt-in file-content tracking for an exclusively owned workspace.
//!
//! Each path selects a current file version. Successful writes create distinct, immutable
//! version metadata, even for identical bytes. Hashes validate bytes; they do not derive Labels.
//! Historical metadata remains available, but historical bytes are not retained.
//!
//! # Ownership and propagation
//!
//! The API owns a fresh private temporary workspace and a volatile in-memory ledger. Every
//! file operation must pass through this API. Exclusive mutable access serializes reads,
//! processing, and writes across trajectories sharing the workspace. Initial content enters
//! through trusted host writes with Labels accounting for its sources, never agent-selected
//! classifications. The directory is not an OS sandbox against other code running as its owner.
//!
//! [`Label::combine`] takes minimum trust and intersects audiences:
//! - Read: stored Label combined with tool delta.
//! - Replace: checked trajectory Label combined with tool delta.
//! - Append: replacement Label combined with the existing tracked predecessor's Label.
//! - Process: replacement Label combined with every declared input version's Label.
//!
//! Replacement retains its predecessor for audit, not as a content dependency. Append and
//! processing retain input dependencies. Trusted host code must gate calls before execution
//! and admit the output Label on tool results, including constant acknowledgements. A call
//! that sends data outside the workspace must be checked with its file dependencies included.
//!
//! Processing callbacks must be trusted, read only supplied declared-input snapshots, and
//! have no external effects. Function pointers do not enforce these assumptions. Callbacks
//! are not registered Transformers and cannot declassify: all declared inputs contribute.
//!
//! Writes stage bytes in the destination directory, atomically replace the target, then
//! publish version metadata. Processing or filesystem errors publish no new version and
//! leave existing file contents unchanged; failed writes can leave empty directories.
//!
//! # Limitations
//!
//! No active harness or event-log integration, durable replay or crash recovery, imported or
//! reused workspaces, rename/delete/overlays, or sanitizer/subprocess tracking. A crash can
//! occur between file promotion and ledger update; the workspace must not be recovered or
//! reused. This prototype makes no metadata, error-message, or timing-flow guarantees.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Component, Path};

use appa_engine::contract::Delta;
use appa_engine::label::Label;
use sha2::{Digest, Sha256};

/// Workspace-relative name, with no traversal or platform-dependent aliases.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FileKey(String);

impl FileKey {
    pub fn new(path: impl Into<String>) -> Result<Self, FileError> {
        let path = path.into();
        if path.is_empty()
            || path.contains(['\\', ':', '\0'])
            || path
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            || Path::new(&path)
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(FileError::InvalidPath);
        }
        Ok(Self(path))
    }
}

/// Identity within one workspace, assigned by successful mutation order, not content alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileVersionId(usize);

/// Immutable content metadata. Labels and lineage are derived by the workspace.
#[derive(Clone, Debug)]
pub struct FileVersion {
    pub id: FileVersionId,
    pub digest: [u8; 32],
    pub label: Label,
    /// Audit history, not necessarily a content dependency (replacement drops old contents).
    pub previous: Option<FileVersionId>,
    pub dependencies: Vec<FileVersionId>,
}

#[derive(Debug)]
pub struct FileRead {
    pub bytes: Vec<u8>,
    pub version: FileVersionId,
    pub label: Label,
}

#[derive(Clone, Copy, Debug)]
pub enum WriteMode {
    Replace,
    Append,
}

#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error("expected a normalized workspace-relative file path")]
    InvalidPath,
    #[error("file has no tracked version")]
    Untracked,
    #[error("file contents disagree with the tracked version")]
    ContentMismatch,
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Owns both the files and their Labels. Exclusive mutable access serializes calls, including
/// processing and promotion. A fresh private directory prevents importing unlabelled files or
/// aliases. Do not give other tools access to this directory, including agent shell tools.
pub struct ManagedFiles {
    root: tempfile::TempDir,
    current: BTreeMap<FileKey, FileVersionId>,
    versions: Vec<FileVersion>,
}

impl ManagedFiles {
    pub fn new() -> Result<Self, FileError> {
        Ok(Self {
            root: tempfile::tempdir()?,
            current: BTreeMap::new(),
            versions: Vec::new(),
        })
    }

    pub fn current(&self, key: &FileKey) -> Option<&FileVersion> {
        self.current.get(key).map(|id| &self.versions[id.0])
    }

    pub fn versions(&self) -> &[FileVersion] {
        &self.versions
    }

    /// The caller must fold this Label into the admitted result, not just return the bytes.
    pub fn read_file(&mut self, key: &FileKey, delta: &Delta) -> Result<FileRead, FileError> {
        let version = self.current(key).ok_or(FileError::Untracked)?;
        let bytes = std::fs::read(self.root.path().join(&key.0))?;
        if <[u8; 32]>::from(Sha256::digest(&bytes)) != version.digest {
            return Err(FileError::ContentMismatch);
        }
        Ok(FileRead {
            bytes,
            version: version.id,
            label: version.label.combine(&delta.output_label()),
        })
    }

    /// A full replacement drops the predecessor's content Label; append retains it.
    /// The returned version's Label also labels the tool result, including a constant "ok".
    pub fn write_file(
        &mut self,
        key: &FileKey,
        bytes: &[u8],
        mode: WriteMode,
        trajectory: &Label,
        delta: &Delta,
    ) -> Result<FileVersionId, FileError> {
        let label = trajectory.combine(&delta.output_label());
        match mode {
            WriteMode::Replace => self.replace(key, bytes, label, Vec::new()),
            WriteMode::Append => {
                let mut old = self.read_file(key, &Delta::NONE)?;
                old.bytes.extend_from_slice(bytes);
                self.replace(key, &old.bytes, label.combine(&old.label), vec![old.version])
            }
        }
    }

    /// Run a trusted, side-effect-free implementation over snapshots of declared inputs.
    /// All inputs contribute to the output, even if the implementation ignores some bytes.
    /// Errors leave the destination and ledger unchanged. This does not sandbox the function.
    pub fn process_file(
        &mut self,
        inputs: &[FileKey],
        output: &FileKey,
        trajectory: &Label,
        delta: &Delta,
        process: fn(&[Vec<u8>]) -> io::Result<Vec<u8>>,
    ) -> Result<FileVersionId, FileError> {
        let mut label = trajectory.combine(&delta.output_label());
        let mut dependencies = Vec::new();
        let mut contents = Vec::new();
        for key in inputs {
            let read = self.read_file(key, &Delta::NONE)?;
            label = label.combine(&read.label);
            if !dependencies.contains(&read.version) {
                dependencies.push(read.version);
            }
            contents.push(read.bytes);
        }
        let bytes = process(&contents)?;
        self.replace(output, &bytes, label, dependencies)
    }

    fn replace(
        &mut self,
        key: &FileKey,
        bytes: &[u8],
        label: Label,
        dependencies: Vec<FileVersionId>,
    ) -> Result<FileVersionId, FileError> {
        let path = self.root.path().join(&key.0);
        let parent = path.parent().expect("a validated file key has a workspace parent");
        std::fs::create_dir_all(parent)?;
        let mut staged = tempfile::NamedTempFile::new_in(parent)?;
        staged.write_all(bytes)?;
        staged.flush()?;
        let id = FileVersionId(self.versions.len());
        let version = FileVersion {
            id,
            digest: Sha256::digest(bytes).into(),
            label,
            previous: self.current.get(key).copied(),
            dependencies,
        };
        staged.persist(&path).map_err(|error| error.error)?;
        self.versions.push(version);
        self.current.insert(key.clone(), id);
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use appa_engine::label::{Audience, ReaderId, Trust};

    fn key(name: &str) -> FileKey {
        FileKey::new(name).unwrap()
    }

    fn secret() -> Label {
        Label::new(
            Trust::new(2),
            Audience::restricted([ReaderId::new("alice"), ReaderId::new("bob")]),
        )
    }

    #[test]
    fn write_process_read_preserves_all_dependencies_across_trajectories() {
        let mut files = ManagedFiles::new().unwrap();
        let a = key("nested/a");
        let b = key("b");
        let c = key("c");
        let d = key("d");
        let first = files
            .write_file(&a, b"secret", WriteMode::Replace, &secret(), &Delta::NONE)
            .unwrap();
        let untrusted = Label::new(
            Trust::new(0),
            Audience::restricted([ReaderId::new("bob"), ReaderId::new("carol")]),
        );
        let second = files
            .write_file(&b, b"!", WriteMode::Replace, &untrusted, &Delta::NONE)
            .unwrap();
        let derived = files
            .process_file(&[a, b], &c, &Label::top(), &Delta::NONE, |inputs| Ok(inputs.concat()))
            .unwrap();
        files
            .process_file(&[c], &d, &Label::top(), &Delta::NONE, |inputs| Ok(inputs[0].clone()))
            .unwrap();
        let read = files.read_file(&d, &Delta::NONE).unwrap();
        assert_eq!(read.bytes, b"secret!");
        assert_eq!(
            read.label,
            Label::new(Trust::new(0), Audience::restricted([ReaderId::new("bob")]))
        );
        assert_eq!(files.versions()[derived.0].dependencies, [first, second]);
        assert_eq!(files.current(&d).unwrap().dependencies, [derived]);
    }

    #[test]
    fn processing_in_place_retains_inputs_even_when_ignored() {
        let mut files = ManagedFiles::new().unwrap();
        let path = key("a");
        let first = files
            .write_file(&path, b"secret", WriteMode::Replace, &secret(), &Delta::NONE)
            .unwrap();
        let trajectory = Label::new(
            Trust::new(1),
            Audience::restricted([ReaderId::new("bob"), ReaderId::new("carol")]),
        );
        let delta = Delta {
            trust: Some(Trust::new(0)),
            audience: None,
        };
        files
            .process_file(std::slice::from_ref(&path), &path, &trajectory, &delta, |_| {
                Ok(b"constant".to_vec())
            })
            .unwrap();
        let version = files.current(&path).unwrap();
        assert_eq!(version.previous, Some(first));
        assert_eq!(version.dependencies, [first]);
        let read = files.read_file(&path, &Delta::NONE).unwrap();
        assert_eq!(read.bytes, b"constant");
        assert_eq!(
            read.label,
            Label::new(Trust::new(0), Audience::restricted([ReaderId::new("bob")]))
        );
    }

    #[test]
    fn failed_promotion_does_not_publish_a_version() {
        let mut files = ManagedFiles::new().unwrap();
        let nested = key("dir/file");
        files
            .write_file(&nested, b"old", WriteMode::Replace, &secret(), &Delta::NONE)
            .unwrap();
        assert!(
            files
                .write_file(&key("dir"), b"new", WriteMode::Replace, &Label::top(), &Delta::NONE)
                .is_err()
        );
        assert!(files.current(&key("dir")).is_none());
        assert_eq!(files.versions().len(), 1);
        assert_eq!(files.read_file(&nested, &Delta::NONE).unwrap().bytes, b"old");
    }

    #[test]
    fn replacement_and_append_have_different_content_dependencies() {
        let mut files = ManagedFiles::new().unwrap();
        let path = key("a");
        let first = files
            .write_file(&path, b"old", WriteMode::Replace, &secret(), &Delta::NONE)
            .unwrap();
        let appended = files
            .write_file(&path, b"new", WriteMode::Append, &Label::top(), &Delta::NONE)
            .unwrap();
        assert_eq!(files.read_file(&path, &Delta::NONE).unwrap().bytes, b"oldnew");
        assert_eq!(files.current(&path).unwrap().label, secret());
        assert_eq!(files.current(&path).unwrap().dependencies, [first]);
        files
            .write_file(&path, b"new", WriteMode::Replace, &Label::top(), &Delta::NONE)
            .unwrap();
        let version = files.current(&path).unwrap();
        assert_eq!(version.label, Label::top());
        assert_eq!(version.previous, Some(appended));
        assert!(version.dependencies.is_empty());
        assert_eq!(files.read_file(&path, &Delta::NONE).unwrap().bytes, b"new");
    }

    #[test]
    fn processing_failure_and_missing_input_leave_output_unchanged() {
        let mut files = ManagedFiles::new().unwrap();
        let path = key("a");
        files
            .write_file(&path, b"old", WriteMode::Replace, &Label::top(), &Delta::NONE)
            .unwrap();
        assert!(
            files
                .process_file(std::slice::from_ref(&path), &path, &secret(), &Delta::NONE, |_| {
                    Err(io::Error::other("failed"))
                })
                .is_err()
        );
        assert!(matches!(
            files.process_file(&[key("missing")], &path, &secret(), &Delta::NONE, |_| {
                panic!("must not execute with missing inputs")
            }),
            Err(FileError::Untracked)
        ));
        assert_eq!(files.versions().len(), 1);
        assert_eq!(files.read_file(&path, &Delta::NONE).unwrap().bytes, b"old");
    }

    #[test]
    fn identical_bytes_do_not_share_labels_and_delta_is_folded() {
        let mut files = ManagedFiles::new().unwrap();
        let a = key("a");
        let b = key("b");
        files
            .write_file(&a, b"same", WriteMode::Replace, &secret(), &Delta::NONE)
            .unwrap();
        let delta = Delta {
            trust: Some(Trust::new(1)),
            audience: None,
        };
        files
            .write_file(&b, b"same", WriteMode::Replace, &Label::top(), &delta)
            .unwrap();
        assert_ne!(files.current(&a).unwrap().id, files.current(&b).unwrap().id);
        assert_eq!(files.current(&a).unwrap().digest, files.current(&b).unwrap().digest);
        assert_eq!(
            files.read_file(&b, &Delta::NONE).unwrap().label,
            Label::new(Trust::new(1), Audience::public())
        );
        assert_eq!(
            files.read_file(&a, &delta).unwrap().label,
            Label::new(
                Trust::new(1),
                Audience::restricted([ReaderId::new("alice"), ReaderId::new("bob")])
            )
        );
    }

    #[test]
    fn invalid_paths_untracked_files_and_changed_bytes_are_refused() {
        for name in ["", "/a", "../a", "a/../b", "a/./b", "a//b", "a/", "C:\\a"] {
            assert!(FileKey::new(name).is_err(), "{name}");
        }
        let mut files = ManagedFiles::new().unwrap();
        let path = key("a");
        assert!(matches!(
            files.read_file(&path, &Delta::NONE),
            Err(FileError::Untracked)
        ));
        assert!(matches!(
            files.write_file(&path, b"x", WriteMode::Append, &Label::top(), &Delta::NONE),
            Err(FileError::Untracked)
        ));
        files
            .write_file(&path, b"old", WriteMode::Replace, &secret(), &Delta::NONE)
            .unwrap();
        std::fs::write(files.root.path().join("a"), b"changed").unwrap();
        assert!(matches!(
            files.read_file(&path, &Delta::NONE),
            Err(FileError::ContentMismatch)
        ));
        assert!(matches!(
            files.write_file(&path, b"x", WriteMode::Append, &Label::top(), &Delta::NONE),
            Err(FileError::ContentMismatch)
        ));
        assert_eq!(files.versions().len(), 1);
    }
}
