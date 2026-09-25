//! In-memory file-version ledger for native filesystem tool calls.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use appa_engine::label::Label;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub mod beneath;

const ABSENT: &str = "-";
/// How many input snapshots one Process call may declare. Every one of them is hashed at
/// reservation and copied into the job, so the count is a ceiling on both, not a convenience.
const MAX_PROCESS_INPUTS: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum FileStoreError {
    #[error("invalid file tracking configuration: {0}")]
    Configuration(String),
    #[error("invalid workspace path: {0}")]
    InvalidPath(String),
    #[error("a file operation is already pending")]
    Pending,
    #[error("the reservation does not exist or belongs to another call")]
    UnknownReservation,
    #[error("the reservation is already bound")]
    AlreadyBound,
    #[error("the file is not tracked")]
    Untracked,
    #[error("the file differs from its recorded version")]
    DigestMismatch,
    #[error("the host changed the file after a failed operation; the reservation is quarantined")]
    Quarantined,
    #[error("stored data is invalid: {0}")]
    Corrupt(String),
    #[error("filesystem failure: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileOperation {
    Read,
    Replace,
    Edit,
    Copy,
    Move,
    Process,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSourcePin {
    pub path: String,
    pub version: i64,
    pub label: Label,
    pub digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilePin {
    pub path: String,
    pub operation: FileOperation,
    pub predecessor_version: Option<i64>,
    pub predecessor_label: Option<Label>,
    pub predecessor_digest: Option<String>,
    /// The source bytes consumed by a Copy or Move.
    pub source: Option<FileSourcePin>,
    /// The source bytes consumed by a Process. Empty for all other operations.
    pub inputs: Vec<FileSourcePin>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileVersion {
    pub id: i64,
    pub path: String,
    pub digest: String,
    pub label: Label,
    pub previous: Option<i64>,
    /// The versions whose bytes contributed to this content.
    pub content_dependencies: Vec<i64>,
    pub dispatch: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileReceipt {
    pub path: String,
    pub operation: FileOperation,
    pub success: bool,
    pub source_label: Option<Label>,
    pub version: Option<FileVersion>,
    pub dispatch: Option<String>,
}

/// A live reservation: the pinned operation a released call holds the workspace for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reservation {
    pub actor: String,
    pub call_key: String,
    pub pin: FilePin,
    pub bound_dispatch: Option<String>,
}

/// What releasing a call that never ran did to its reservation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AbandonOutcome {
    /// The call held no reservation.
    Absent,
    /// The workspace still showed the pinned state; the reservation is released.
    Released,
    /// The workspace moved away from the pin. The reservation stands, and every later file
    /// call in this session is refused for the rest of the runtime process.
    Quarantined,
}

pub struct FileStore {
    state: Mutex<State>,
    workspace: PathBuf,
}

struct State {
    initial: Label,
    next_id: i64,
    versions: BTreeMap<i64, FileVersion>,
    current: BTreeMap<String, i64>,
    reservation: Option<StoredReservation>,
    receipts: HashMap<(String, String), FileReceipt>,
}

struct StoredReservation {
    actor: String,
    call_key: String,
    pin: FilePin,
    bound_dispatch: Option<String>,
    output_label: Option<Label>,
}

impl FileStore {
    pub fn new(workspace: &Path, initial: &Label) -> Result<Self, FileStoreError> {
        let workspace = canonical_workspace(workspace)?;
        check_links(&workspace)?;
        Ok(Self {
            state: Mutex::new(State {
                initial: initial.clone(),
                next_id: 1,
                versions: BTreeMap::new(),
                current: BTreeMap::new(),
                reservation: None,
                receipts: HashMap::new(),
            }),
            workspace,
        })
    }

    pub fn prepare(
        &self,
        actor: &str,
        call_key: &str,
        operation: FileOperation,
        path: &str,
    ) -> Result<FilePin, FileStoreError> {
        if matches!(
            operation,
            FileOperation::Copy | FileOperation::Move | FileOperation::Process
        ) {
            return Err(FileStoreError::Configuration(
                "use prepare_transfer for Copy or Move".into(),
            ));
        }
        let relative = validated_relative(&self.workspace, path)?;
        let mut state = self.lock()?;
        if state.reservation.is_some() {
            return Err(FileStoreError::Pending);
        }
        let predecessor = current_or_adopt(&mut state, &self.workspace, &relative)?;
        match operation {
            FileOperation::Read | FileOperation::Edit if predecessor.is_none() => {
                return Err(FileStoreError::Untracked);
            }
            _ => {}
        }
        let actual = state_digest(&self.workspace, &relative)?;
        if actual != ABSENT {
            let Some(version) = &predecessor else {
                return Err(FileStoreError::Untracked);
            };
            if actual != version.digest {
                return Err(FileStoreError::DigestMismatch);
            }
        } else if predecessor.is_some() || operation != FileOperation::Replace {
            return Err(FileStoreError::DigestMismatch);
        }
        let pin = FilePin {
            path: relative.clone(),
            operation,
            predecessor_version: predecessor.as_ref().map(|v| v.id),
            predecessor_label: predecessor.as_ref().map(|v| v.label.clone()),
            predecessor_digest: predecessor.as_ref().map(|v| v.digest.clone()),
            source: None,
            inputs: vec![],
        };
        state.reservation = Some(stored_reservation(actor, call_key, &pin));
        Ok(pin)
    }

    pub fn prepare_transfer(
        &self,
        actor: &str,
        call_key: &str,
        operation: FileOperation,
        source_path: &str,
        destination_path: &str,
    ) -> Result<FilePin, FileStoreError> {
        if !matches!(operation, FileOperation::Copy | FileOperation::Move) {
            return Err(FileStoreError::Configuration(
                "transfer operation must be Copy or Move".into(),
            ));
        }
        let source = validated_relative(&self.workspace, source_path)?;
        let destination = validated_relative(&self.workspace, destination_path)?;
        if source == destination {
            return Err(FileStoreError::InvalidPath(
                "source and destination are the same path".into(),
            ));
        }
        let source_absolute = self.workspace.join(&source);
        let destination_absolute = self.workspace.join(&destination);

        let mut state = self.lock()?;
        if state.reservation.is_some() {
            return Err(FileStoreError::Pending);
        }
        let source_version =
            current_or_adopt(&mut state, &self.workspace, &source)?.ok_or(FileStoreError::Untracked)?;
        let source_actual = state_digest(&self.workspace, &source)?;
        if source_actual == ABSENT {
            return Err(FileStoreError::Untracked);
        }
        check_move_filesystem(operation, &source_absolute, &destination_absolute)?;
        if source_actual != source_version.digest {
            return Err(FileStoreError::DigestMismatch);
        }
        let predecessor = current_or_adopt(&mut state, &self.workspace, &destination)?;
        let destination_actual = state_digest(&self.workspace, &destination)?;
        if destination_actual != predecessor.as_ref().map(|v| v.digest.as_str()).unwrap_or(ABSENT) {
            return Err(FileStoreError::DigestMismatch);
        }
        let pin = FilePin {
            path: destination,
            operation,
            predecessor_version: predecessor.as_ref().map(|v| v.id),
            predecessor_label: predecessor.as_ref().map(|v| v.label.clone()),
            predecessor_digest: predecessor.as_ref().map(|v| v.digest.clone()),
            source: Some(FileSourcePin {
                path: source,
                version: source_version.id,
                label: source_version.label,
                digest: source_version.digest,
            }),
            inputs: vec![],
        };
        state.reservation = Some(stored_reservation(actor, call_key, &pin));
        Ok(pin)
    }

    pub fn prepare_process(
        &self,
        actor: &str,
        call_key: &str,
        input_paths: &[String],
        destination_path: &str,
    ) -> Result<FilePin, FileStoreError> {
        let destination = validated_relative(&self.workspace, destination_path)?;
        let mut seen = HashSet::new();
        let inputs = input_paths
            .iter()
            .map(|path| validated_relative(&self.workspace, path))
            .collect::<Result<Vec<_>, _>>()?;
        if inputs.iter().any(|path| path == &destination) || inputs.iter().any(|path| !seen.insert(path.clone())) {
            return Err(FileStoreError::InvalidPath(
                "process inputs must be unique and exclude the destination".into(),
            ));
        }
        if inputs.len() > MAX_PROCESS_INPUTS {
            return Err(FileStoreError::InvalidPath("too many process inputs".into()));
        }
        let mut state = self.lock()?;
        if state.reservation.is_some() {
            return Err(FileStoreError::Pending);
        }
        let mut pinned = Vec::with_capacity(inputs.len());
        for path in inputs {
            let version = current_or_adopt(&mut state, &self.workspace, &path)?.ok_or(FileStoreError::Untracked)?;
            let actual = state_digest(&self.workspace, &path)?;
            if actual == ABSENT {
                return Err(FileStoreError::Untracked);
            }
            if actual != version.digest {
                return Err(FileStoreError::DigestMismatch);
            }
            pinned.push(FileSourcePin {
                path,
                version: version.id,
                label: version.label,
                digest: version.digest,
            });
        }
        let predecessor = current_or_adopt(&mut state, &self.workspace, &destination)?;
        if state_digest(&self.workspace, &destination)?
            != predecessor.as_ref().map(|v| v.digest.as_str()).unwrap_or(ABSENT)
        {
            return Err(FileStoreError::DigestMismatch);
        }
        let pin = FilePin {
            path: destination,
            operation: FileOperation::Process,
            predecessor_version: predecessor.as_ref().map(|v| v.id),
            predecessor_label: predecessor.as_ref().map(|v| v.label.clone()),
            predecessor_digest: predecessor.as_ref().map(|v| v.digest.clone()),
            source: None,
            inputs: pinned,
        };
        state.reservation = Some(stored_reservation(actor, call_key, &pin));
        Ok(pin)
    }

    pub fn bind(
        &self,
        actor: &str,
        call_key: &str,
        dispatch: &str,
        output_label: &Label,
    ) -> Result<(), FileStoreError> {
        let mut state = self.lock()?;
        let reservation = matching_reservation_mut(&mut state, actor, call_key)?;
        if reservation.bound_dispatch.is_some() {
            return Err(FileStoreError::AlreadyBound);
        }
        reservation.bound_dispatch = Some(dispatch.into());
        reservation.output_label = Some(output_label.clone());
        Ok(())
    }

    pub fn cancel(&self, actor: &str, call_key: &str) -> Result<(), FileStoreError> {
        let mut state = self.lock()?;
        if matching_reservation(&state, actor, call_key)?.bound_dispatch.is_some() {
            return Err(FileStoreError::AlreadyBound);
        }
        state.reservation = None;
        Ok(())
    }

    pub fn finish(&self, actor: &str, call_key: &str, success: bool) -> Result<FileReceipt, FileStoreError> {
        let mut state = self.lock()?;
        let key = (actor.to_owned(), call_key.to_owned());
        if let Some(receipt) = state.receipts.get(&key) {
            return Ok(receipt.clone());
        }
        let reservation = matching_reservation(&state, actor, call_key)?;
        let pin = reservation.pin.clone();
        let dispatch = reservation
            .bound_dispatch
            .clone()
            .ok_or(FileStoreError::UnknownReservation)?;
        let output = reservation
            .output_label
            .clone()
            .ok_or(FileStoreError::UnknownReservation)?;
        let actual = state_digest(&self.workspace, &pin.path)?;
        let expected = pin.predecessor_digest.as_deref().unwrap_or(ABSENT);
        let source_actual = pin
            .source
            .as_ref()
            .map(|source| state_digest(&self.workspace, &source.path))
            .transpose()?;
        let input_states = pin
            .inputs
            .iter()
            .map(|input| Ok(state_digest(&self.workspace, &input.path)? == input.digest))
            .collect::<Result<Vec<_>, FileStoreError>>()?;
        let inputs_unchanged = input_states.iter().all(|unchanged| *unchanged);
        let source_label = if pin.operation == FileOperation::Process {
            pin.inputs
                .iter()
                .map(|input| input.label.clone())
                .reduce(|label, next| label.combine(&next))
        } else {
            pin.source
                .as_ref()
                .map(|source| source.label.clone())
                .or_else(|| pin.predecessor_label.clone())
        };
        let receipt = if !success {
            if !undisturbed(&pin, &actual, source_actual.as_deref(), &input_states) {
                return Err(FileStoreError::Quarantined);
            }
            FileReceipt {
                path: pin.path.clone(),
                operation: pin.operation,
                success: false,
                source_label: source_label.clone(),
                version: None,
                dispatch: Some(dispatch),
            }
        } else if pin.operation == FileOperation::Read {
            if actual != expected {
                return Err(FileStoreError::DigestMismatch);
            }
            FileReceipt {
                path: pin.path.clone(),
                operation: pin.operation,
                success: true,
                source_label: source_label.clone(),
                version: None,
                dispatch: Some(dispatch),
            }
        } else {
            let dependencies = match pin.operation {
                FileOperation::Edit => pin.predecessor_version.into_iter().collect(),
                FileOperation::Copy | FileOperation::Move => {
                    let source = pin
                        .source
                        .as_ref()
                        .ok_or_else(|| FileStoreError::Corrupt("transfer has no source pin".into()))?;
                    let source_ok = if pin.operation == FileOperation::Move {
                        source_actual.as_deref() == Some(ABSENT)
                    } else {
                        source_actual.as_deref() == Some(source.digest.as_str())
                    };
                    if !source_ok || actual != source.digest {
                        return Err(FileStoreError::DigestMismatch);
                    }
                    vec![source.version]
                }
                FileOperation::Process => {
                    if !inputs_unchanged {
                        return Err(FileStoreError::DigestMismatch);
                    }
                    pin.inputs.iter().map(|input| input.version).collect()
                }
                FileOperation::Replace => vec![],
                FileOperation::Read => unreachable!(),
            };
            if actual == ABSENT {
                return Err(FileStoreError::DigestMismatch);
            }
            let id = state.next_id;
            state.next_id += 1;
            if pin.operation == FileOperation::Move {
                state.current.remove(
                    &pin.source
                        .as_ref()
                        .expect("successful Move validated its source pin")
                        .path,
                );
            }
            let version = FileVersion {
                id,
                path: pin.path.clone(),
                digest: actual,
                label: output,
                previous: pin.predecessor_version,
                content_dependencies: dependencies,
                dispatch: Some(dispatch.clone()),
            };
            state.versions.insert(id, version.clone());
            state.current.insert(pin.path.clone(), id);
            FileReceipt {
                path: pin.path.clone(),
                operation: pin.operation,
                success: true,
                source_label,
                version: Some(version),
                dispatch: Some(dispatch),
            }
        };
        state.receipts.insert(key, receipt.clone());
        state.reservation = None;
        Ok(receipt)
    }

    /// Whether the workspace still shows exactly the state this pin recorded: the bytes the
    /// operation would have replaced, the bytes a transfer would have consumed, and every
    /// declared input. `finish` and `abandon` answer the same question from values they have
    /// already hashed; diagnostics can ask it here.
    pub fn pin_matches_workspace(&self, pin: &FilePin) -> Result<bool, FileStoreError> {
        let destination = state_digest(&self.workspace, &pin.path)?;
        let source = pin
            .source
            .as_ref()
            .map(|source| state_digest(&self.workspace, &source.path))
            .transpose()?;
        let inputs = pin
            .inputs
            .iter()
            .map(|input| Ok(state_digest(&self.workspace, &input.path)? == input.digest))
            .collect::<Result<Vec<_>, FileStoreError>>()?;
        Ok(undisturbed(pin, &destination, source.as_deref(), &inputs))
    }

    /// Release the reservation of a call that was released and never ran, provided the
    /// workspace still shows its pinned state. The runtime cannot tell an unrun call from one
    /// whose report was lost, so anything else keeps the reservation: a workspace that moved
    /// is never released by guessing here.
    pub fn abandon(&self, actor: &str, call_key: &str) -> Result<AbandonOutcome, FileStoreError> {
        let mut state = self.lock()?;
        let Some(reservation) = state.reservation.as_ref() else {
            return Ok(AbandonOutcome::Absent);
        };
        if reservation.actor != actor || reservation.call_key != call_key {
            return Ok(AbandonOutcome::Absent);
        }
        let pin = reservation.pin.clone();
        let destination = state_digest(&self.workspace, &pin.path)?;
        let source = pin
            .source
            .as_ref()
            .map(|source| state_digest(&self.workspace, &source.path))
            .transpose()?;
        let inputs = pin
            .inputs
            .iter()
            .map(|input| Ok(state_digest(&self.workspace, &input.path)? == input.digest))
            .collect::<Result<Vec<_>, FileStoreError>>()?;
        if !undisturbed(&pin, &destination, source.as_deref(), &inputs) {
            return Ok(AbandonOutcome::Quarantined);
        }
        state.reservation = None;
        Ok(AbandonOutcome::Released)
    }

    /// Every tracked path that no longer holds the bytes its recorded version describes. A
    /// path that is gone counts: the ledger cannot tell a deletion from a loss.
    pub fn drifted(&self) -> Result<Vec<FileVersion>, FileStoreError> {
        let state = self.lock()?;
        let current = snapshot_state(&state);
        let mut drifted = Vec::new();
        for version in current {
            if state_digest(&self.workspace, &version.path)? != version.digest {
                drifted.push(version);
            }
        }
        Ok(drifted)
    }

    /// The pin a live reservation holds for this exact call, if it holds one. What an
    /// operation executes is the path this pin recorded, never the path the call spelled: the
    /// ledger validated and hashed that one.
    pub fn pin_for(&self, actor: &str, call_key: &str) -> Result<Option<FilePin>, FileStoreError> {
        let state = self.lock()?;
        Ok(state
            .reservation
            .as_ref()
            .filter(|r| r.actor == actor && r.call_key == call_key)
            .map(|r| r.pin.clone()))
    }

    /// The live reservation, if any. One workspace holds at most one.
    pub fn reservation(&self) -> Result<Option<Reservation>, FileStoreError> {
        let state = self.lock()?;
        Ok(state.reservation.as_ref().map(|r| Reservation {
            actor: r.actor.clone(),
            call_key: r.call_key.clone(),
            pin: r.pin.clone(),
            bound_dispatch: r.bound_dispatch.clone(),
        }))
    }

    /// The workspace this ledger is bound to.
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub fn current(&self, path: &str) -> Result<Option<FileVersion>, FileStoreError> {
        let relative = validated_relative(&self.workspace, path)?;
        let mut state = self.lock()?;
        current_or_adopt(&mut state, &self.workspace, &relative)
    }

    pub fn history(&self, path: &str) -> Result<Vec<FileVersion>, FileStoreError> {
        let relative = validated_relative(&self.workspace, path)?;
        let state = self.lock()?;
        Ok(state
            .versions
            .values()
            .filter(|v| v.path == relative)
            .cloned()
            .collect())
    }

    pub fn snapshot(&self) -> Result<Vec<FileVersion>, FileStoreError> {
        let state = self.lock()?;
        Ok(snapshot_state(&state))
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, State>, FileStoreError> {
        self.state
            .lock()
            .map_err(|_| FileStoreError::Corrupt("file store lock poisoned".into()))
    }
}
fn canonical_workspace(path: &Path) -> Result<PathBuf, FileStoreError> {
    if !cfg!(unix) {
        return Err(FileStoreError::Configuration(
            "native file tracking currently requires Unix".into(),
        ));
    }
    let p = fs::canonicalize(path)?;
    if !p.is_dir() {
        return Err(FileStoreError::Configuration("workspace is not a directory".into()));
    }
    Ok(p)
}
fn validated_relative(workspace: &Path, input: &str) -> Result<String, FileStoreError> {
    let supplied = Path::new(input);
    let joined = if supplied.is_absolute() {
        supplied.to_path_buf()
    } else {
        workspace.join(supplied)
    };
    let stripped = joined
        .strip_prefix(workspace)
        .map_err(|_| FileStoreError::InvalidPath(input.into()))?;
    if stripped.as_os_str().is_empty() || stripped.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(FileStoreError::InvalidPath(input.into()));
    }
    let mut cursor = workspace.to_path_buf();
    for component in stripped.components() {
        cursor.push(component);
        if let Ok(meta) = fs::symlink_metadata(&cursor)
            && meta.file_type().is_symlink()
        {
            return Err(FileStoreError::InvalidPath("symlink component".into()));
        }
    }
    Ok(stripped.to_string_lossy().into_owned())
}
fn check_regular_metadata(m: &fs::Metadata) -> Result<(), FileStoreError> {
    #[cfg(unix)]
    let singly_linked = m.nlink() == 1;
    #[cfg(not(unix))]
    let singly_linked = false;
    if !m.is_file() || !singly_linked {
        return Err(FileStoreError::InvalidPath(
            "file must be regular and singly linked".into(),
        ));
    }
    Ok(())
}
fn digest(mut f: File) -> Result<String, FileStoreError> {
    let mut h = Sha256::new();
    let mut b = [0; 8192];
    loop {
        let n = f.read(&mut b)?;
        if n == 0 {
            break;
        }
        h.update(&b[..n]);
    }
    Ok(h.finalize().iter().map(|byte| format!("{byte:02x}")).collect())
}
fn state_digest(workspace: &Path, relative: &str) -> Result<String, FileStoreError> {
    let Some(file) = beneath::open(workspace, relative)? else {
        return Ok(ABSENT.into());
    };
    check_regular_metadata(&file.metadata()?)?;
    digest(file)
}
#[cfg(not(unix))]
fn check_move_filesystem(_: FileOperation, _: &Path, _: &Path) -> Result<(), FileStoreError> {
    Err(FileStoreError::Configuration("file tracking requires Unix".into()))
}
#[cfg(unix)]
fn check_move_filesystem(operation: FileOperation, source: &Path, destination: &Path) -> Result<(), FileStoreError> {
    if operation != FileOperation::Move {
        return Ok(());
    }
    let mut parent = destination
        .parent()
        .ok_or_else(|| FileStoreError::InvalidPath("destination has no parent".into()))?;
    while !parent.exists() {
        parent = parent
            .parent()
            .ok_or_else(|| FileStoreError::InvalidPath("destination has no existing parent".into()))?;
    }
    let destination_device = if destination.exists() {
        fs::metadata(destination)?.dev()
    } else {
        fs::metadata(parent)?.dev()
    };
    if fs::metadata(source)?.dev() != destination_device {
        return Err(FileStoreError::InvalidPath("cross-filesystem move".into()));
    }
    Ok(())
}
/// Refuse a workspace holding a symlink or a multiply linked file anywhere. Only metadata
/// is read: each file is hashed when a call first touches it.
fn check_links(dir: &Path) -> Result<(), FileStoreError> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            return Err(FileStoreError::InvalidPath("workspace contains symlink".into()));
        }
        if meta.is_dir() {
            check_links(&path)?;
        } else {
            check_regular_metadata(&meta)?;
        }
    }
    Ok(())
}
/// Whether a pin still describes the workspace: the destination holds the bytes the operation
/// would have replaced, a transfer's source is where the operation would have left it, and
/// every declared input is unchanged. `finish` and `abandon` compute these values themselves;
/// both ask this one question of them.
fn undisturbed(pin: &FilePin, destination: &str, source_state: Option<&str>, inputs: &[bool]) -> bool {
    destination == pin.predecessor_digest.as_deref().unwrap_or(ABSENT)
        && pin
            .source
            .as_ref()
            .is_none_or(|source| source_state == Some(source.digest.as_str()))
        && inputs.iter().all(|unchanged| *unchanged)
}

fn stored_reservation(actor: &str, call_key: &str, pin: &FilePin) -> StoredReservation {
    StoredReservation {
        actor: actor.into(),
        call_key: call_key.into(),
        pin: pin.clone(),
        bound_dispatch: None,
        output_label: None,
    }
}

fn matching_reservation<'a>(
    state: &'a State,
    actor: &str,
    call_key: &str,
) -> Result<&'a StoredReservation, FileStoreError> {
    state
        .reservation
        .as_ref()
        .filter(|r| r.actor == actor && r.call_key == call_key)
        .ok_or(FileStoreError::UnknownReservation)
}

fn matching_reservation_mut<'a>(
    state: &'a mut State,
    actor: &str,
    call_key: &str,
) -> Result<&'a mut StoredReservation, FileStoreError> {
    state
        .reservation
        .as_mut()
        .filter(|r| r.actor == actor && r.call_key == call_key)
        .ok_or(FileStoreError::UnknownReservation)
}

fn current_state(state: &State, path: &str) -> Option<FileVersion> {
    state.current.get(path).and_then(|id| state.versions.get(id)).cloned()
}

/// The path's current version. A path the ledger has never versioned that holds a file on
/// disk is adopted first, with the operator's initial label.
fn current_or_adopt(state: &mut State, workspace: &Path, path: &str) -> Result<Option<FileVersion>, FileStoreError> {
    if let Some(version) = current_state(state, path) {
        return Ok(Some(version));
    }
    if state.versions.values().any(|version| version.path == path) {
        return Ok(None);
    }
    let digest = state_digest(workspace, path)?;
    if digest == ABSENT {
        return Ok(None);
    }
    let version = FileVersion {
        id: state.next_id,
        path: path.into(),
        digest,
        label: state.initial.clone(),
        previous: None,
        content_dependencies: vec![],
        dispatch: None,
    };
    state.next_id += 1;
    state.versions.insert(version.id, version.clone());
    state.current.insert(version.path.clone(), version.id);
    Ok(Some(version))
}

fn snapshot_state(state: &State) -> Vec<FileVersion> {
    state
        .current
        .values()
        .filter_map(|id| state.versions.get(id))
        .cloned()
        .collect()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn file_digest_preserves_sha256_lowercase_hex() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("value.txt"), b"abc").unwrap();
        assert_eq!(
            state_digest(dir.path(), "value.txt").unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    struct Fixture {
        _root: TempDir,
        workspace: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let workspace = root.path().join("workspace");
            fs::create_dir(&workspace).unwrap();
            fs::write(workspace.join("tracked.txt"), "old").unwrap();
            Self { _root: root, workspace }
        }

        fn store(&self, label: &Label) -> FileStore {
            FileStore::new(&self.workspace, label).unwrap()
        }
    }

    #[test]
    fn abandoning_releases_only_an_undisturbed_workspace() {
        let fixture = Fixture::new();
        fs::write(fixture.workspace.join("destination.txt"), "before").unwrap();
        let store = fixture.store(&Label::top());
        // A released call the harness never ran: the workspace still shows the pin.
        store.prepare("a", "unrun", FileOperation::Edit, "tracked.txt").unwrap();
        store.bind("a", "unrun", "dispatch", &Label::top()).unwrap();
        assert_eq!(store.reservation().unwrap().unwrap().actor, "a");
        assert!(
            store
                .pin_matches_workspace(&store.reservation().unwrap().unwrap().pin)
                .unwrap()
        );
        assert_eq!(store.abandon("a", "unrun").unwrap(), AbandonOutcome::Released);
        assert!(store.reservation().unwrap().is_none());
        assert_eq!(store.abandon("a", "unrun").unwrap(), AbandonOutcome::Absent);
        // The next call proceeds: the release did not leave the workspace wedged.
        store.prepare("b", "next", FileOperation::Read, "tracked.txt").unwrap();
        store.cancel("b", "next").unwrap();

        // A transfer whose destination moved is not released: the runtime cannot tell an
        // unrun call from one whose report was lost.
        store
            .prepare_transfer("a", "copy", FileOperation::Copy, "tracked.txt", "destination.txt")
            .unwrap();
        store.bind("a", "copy", "dispatch-2", &Label::top()).unwrap();
        fs::write(fixture.workspace.join("destination.txt"), "partial").unwrap();
        assert_eq!(store.abandon("a", "copy").unwrap(), AbandonOutcome::Quarantined);
        assert!(store.reservation().unwrap().is_some());
        assert!(matches!(
            store.prepare("b", "after-quarantine", FileOperation::Read, "tracked.txt"),
            Err(FileStoreError::Pending)
        ));
    }

    #[test]
    fn stores_over_the_same_workspace_are_isolated_in_memory() {
        let fixture = Fixture::new();
        let first = fixture.store(&Label::top());
        let second = fixture.store(&Label::top());
        first.prepare("a", "one", FileOperation::Read, "tracked.txt").unwrap();
        second.prepare("b", "two", FileOperation::Read, "tracked.txt").unwrap();
        assert_eq!(first.reservation().unwrap().unwrap().call_key, "one");
        assert_eq!(second.reservation().unwrap().unwrap().call_key, "two");
    }

    #[test]
    fn replace_uses_bound_label_and_edit_records_dependency() {
        let fixture = Fixture::new();
        let store = fixture.store(&Label::top());
        let output = Label::new(
            appa_engine::label::Trust::new(4),
            appa_engine::label::Audience::public(),
        );
        let first = store
            .prepare("a", "replace", FileOperation::Replace, "tracked.txt")
            .unwrap();
        store.bind("a", "replace", "dispatch-1", &output).unwrap();
        fs::write(fixture.workspace.join("tracked.txt"), "replacement").unwrap();
        let replaced = store.finish("a", "replace", true).unwrap();
        assert_eq!(replaced.version.as_ref().unwrap().label, output);
        assert_eq!(replaced.version.as_ref().unwrap().previous, first.predecessor_version);
        assert!(replaced.version.as_ref().unwrap().content_dependencies.is_empty());
        assert_eq!(store.finish("a", "replace", true).unwrap(), replaced);

        let pin = store.prepare("a", "edit", FileOperation::Edit, "tracked.txt").unwrap();
        store.bind("a", "edit", "dispatch-2", &Label::top()).unwrap();
        fs::write(fixture.workspace.join("tracked.txt"), "edited").unwrap();
        let edited = store.finish("a", "edit", true).unwrap();
        assert_eq!(
            edited.version.unwrap().content_dependencies,
            vec![pin.predecessor_version.unwrap()]
        );
        assert_eq!(store.history("tracked.txt").unwrap().len(), 3);
    }

    #[test]
    fn mismatch_and_failed_partial_write_stay_pending() {
        let fixture = Fixture::new();
        let store = fixture.store(&Label::top());
        store.prepare("a", "touch", FileOperation::Read, "tracked.txt").unwrap();
        store.cancel("a", "touch").unwrap();
        fs::write(fixture.workspace.join("tracked.txt"), "foreign").unwrap();
        assert!(matches!(
            store.prepare("a", "bad", FileOperation::Read, "tracked.txt"),
            Err(FileStoreError::DigestMismatch)
        ));
        fs::write(fixture.workspace.join("tracked.txt"), "old").unwrap();
        store.prepare("a", "fail", FileOperation::Edit, "tracked.txt").unwrap();
        store.bind("a", "fail", "dispatch", &Label::top()).unwrap();
        assert!(!store.finish("a", "fail", false).unwrap().success);

        store
            .prepare("a", "partial", FileOperation::Edit, "tracked.txt")
            .unwrap();
        store.bind("a", "partial", "dispatch-2", &Label::top()).unwrap();
        fs::write(fixture.workspace.join("tracked.txt"), "partial").unwrap();
        assert!(matches!(
            store.finish("a", "partial", false),
            Err(FileStoreError::Quarantined)
        ));
        assert!(matches!(
            store.prepare("b", "next", FileOperation::Read, "tracked.txt"),
            Err(FileStoreError::Pending)
        ));
    }

    #[test]
    fn rejects_missing_ledger_traversal_symlinks_and_hardlinks() {
        let fixture = Fixture::new();
        let store = fixture.store(&Label::top());
        assert!(matches!(
            store.prepare("a", "escape", FileOperation::Replace, "../escape"),
            Err(FileStoreError::InvalidPath(_))
        ));

        std::os::unix::fs::symlink("tracked.txt", fixture.workspace.join("link")).unwrap();
        assert!(matches!(
            store.prepare("a", "link", FileOperation::Read, "link"),
            Err(FileStoreError::InvalidPath(_))
        ));
        fs::hard_link(fixture.workspace.join("tracked.txt"), fixture.workspace.join("hard")).unwrap();
        assert!(matches!(
            store.prepare("a", "hard", FileOperation::Read, "tracked.txt"),
            Err(FileStoreError::InvalidPath(_))
        ));
    }

    #[test]
    fn a_parent_swapped_for_a_symlink_after_prepare_is_never_followed() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.workspace.join("sub")).unwrap();
        fs::write(fixture.workspace.join("sub/file.txt"), "old").unwrap();
        let outside = fixture._root.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("file.txt"), "old").unwrap();
        let store = fixture.store(&Label::top());
        let pinned = store.current("sub/file.txt").unwrap().unwrap();

        store.prepare("a", "edit", FileOperation::Edit, "sub/file.txt").unwrap();
        store.bind("a", "edit", "dispatch", &Label::top()).unwrap();
        fs::rename(fixture.workspace.join("sub"), fixture.workspace.join("real")).unwrap();
        std::os::unix::fs::symlink(&outside, fixture.workspace.join("sub")).unwrap();

        assert!(store.abandon("a", "edit").is_err());
        fs::write(outside.join("file.txt"), "escaped").unwrap();
        assert!(store.finish("a", "edit", true).is_err());
        assert!(store.snapshot().unwrap().contains(&pinned));
        assert!(store.drifted().is_err());
    }

    #[test]
    fn copy_tracks_raw_content_source_not_same_byte_destination() {
        let fixture = Fixture::new();
        fs::write(fixture.workspace.join("tracked.txt"), [0, 255, 1, 128]).unwrap();
        fs::write(fixture.workspace.join("destination.bin"), [0, 255, 1, 128]).unwrap();
        let source_label = Label::top();
        let store = fixture.store(&source_label);
        let destination_label = Label::new(
            appa_engine::label::Trust::new(4),
            appa_engine::label::Audience::public(),
        );
        store
            .prepare("a", "label-destination", FileOperation::Replace, "destination.bin")
            .unwrap();
        store
            .bind("a", "label-destination", "dispatch-0", &destination_label)
            .unwrap();
        store.finish("a", "label-destination", true).unwrap();

        let pin = store
            .prepare_transfer("a", "copy", FileOperation::Copy, "tracked.txt", "destination.bin")
            .unwrap();
        let source = pin.source.as_ref().unwrap();
        assert_ne!(Some(source.version), pin.predecessor_version);
        assert_eq!(source.label, source_label);
        assert_eq!(pin.predecessor_label, Some(destination_label));
        store.bind("a", "copy", "dispatch", &source_label).unwrap();
        fs::copy(
            fixture.workspace.join("tracked.txt"),
            fixture.workspace.join("destination.bin"),
        )
        .unwrap();
        let receipt = store.finish("a", "copy", true).unwrap();
        assert_eq!(receipt.source_label, Some(source_label));
        assert_eq!(receipt.version.unwrap().content_dependencies, vec![source.version]);
    }

    #[test]
    fn move_marks_source_absent_preserves_history_and_allows_reuse() {
        let fixture = Fixture::new();
        let store = fixture.store(&Label::top());
        let source_id = store.current("tracked.txt").unwrap().unwrap().id;
        store
            .prepare_transfer("a", "move", FileOperation::Move, "tracked.txt", "moved.txt")
            .unwrap();
        store.bind("a", "move", "dispatch", &Label::top()).unwrap();
        fs::rename(
            fixture.workspace.join("tracked.txt"),
            fixture.workspace.join("moved.txt"),
        )
        .unwrap();
        let moved = store.finish("a", "move", true).unwrap();
        assert_eq!(moved.version.unwrap().content_dependencies, vec![source_id]);
        assert!(store.current("tracked.txt").unwrap().is_none());
        assert_eq!(store.history("tracked.txt").unwrap().len(), 1);
        assert!(!store.snapshot().unwrap().iter().any(|v| v.path == "tracked.txt"));

        store
            .prepare("a", "reuse", FileOperation::Replace, "tracked.txt")
            .unwrap();
        store.bind("a", "reuse", "dispatch-2", &Label::top()).unwrap();
        fs::write(fixture.workspace.join("tracked.txt"), "new").unwrap();
        store.finish("a", "reuse", true).unwrap();
        assert!(store.current("tracked.txt").unwrap().is_some());
    }

    #[test]
    fn transfer_reserves_both_paths_and_partial_failure_quarantines() {
        let fixture = Fixture::new();
        let store = fixture.store(&Label::top());
        store
            .prepare_transfer("a", "copy", FileOperation::Copy, "tracked.txt", "copy.txt")
            .unwrap();
        assert!(matches!(
            store.prepare("b", "other", FileOperation::Read, "tracked.txt"),
            Err(FileStoreError::Pending)
        ));
        store.bind("a", "copy", "dispatch", &Label::top()).unwrap();
        fs::write(fixture.workspace.join("copy.txt"), "partial").unwrap();
        assert!(matches!(
            store.finish("a", "copy", false),
            Err(FileStoreError::Quarantined)
        ));
        assert!(matches!(
            store.prepare("b", "next", FileOperation::Read, "tracked.txt"),
            Err(FileStoreError::Pending)
        ));
        assert!(matches!(
            store.finish("a", "copy", false),
            Err(FileStoreError::Quarantined)
        ));
    }

    #[test]
    fn failed_transfer_releases_only_when_both_paths_are_unchanged() {
        let fixture = Fixture::new();
        let store = fixture.store(&Label::top());
        store
            .prepare_transfer("a", "copy", FileOperation::Copy, "tracked.txt", "copy.txt")
            .unwrap();
        store.bind("a", "copy", "dispatch", &Label::top()).unwrap();
        assert!(!store.finish("a", "copy", false).unwrap().success);
        store.prepare("b", "next", FileOperation::Read, "tracked.txt").unwrap();
    }

    #[test]
    fn process_pins_raw_multi_inputs_and_publishes_all_dependencies() {
        let fixture = Fixture::new();
        fs::write(fixture.workspace.join("second.bin"), [0, 255, 1, 128]).unwrap();
        fs::write(fixture.workspace.join("output.bin"), "previous").unwrap();
        let store = fixture.store(&Label::top());
        let inputs = vec!["tracked.txt".to_string(), "second.bin".to_string()];
        let pin = store.prepare_process("a", "process", &inputs, "output.bin").unwrap();
        let dependencies: Vec<_> = pin.inputs.iter().map(|input| input.version).collect();
        let previous = pin.predecessor_version;
        let output = Label::new(
            appa_engine::label::Trust::new(3),
            appa_engine::label::Audience::restricted([appa_engine::label::ReaderId::new("result")]),
        );
        store.bind("a", "process", "dispatch", &output).unwrap();
        fs::write(fixture.workspace.join("output.bin"), [128, 2, 0, 255]).unwrap();
        let receipt = store.finish("a", "process", true).unwrap();
        let version = receipt.version.unwrap();
        assert_eq!(version.label, output);
        assert_eq!(version.previous, previous);
        assert_eq!(version.content_dependencies, dependencies);
        assert_eq!(store.current("output.bin").unwrap().unwrap(), version);
        assert_eq!(store.history("output.bin").unwrap().len(), 2);
    }

    #[test]
    fn process_refuses_missing_or_aliased_inputs_and_quarantines_partial_failure() {
        let fixture = Fixture::new();
        let store = fixture.store(&Label::top());
        assert!(matches!(
            store.prepare_process("a", "missing", &["missing".into()], "output"),
            Err(FileStoreError::Untracked)
        ));
        assert!(matches!(
            store.prepare_process(
                "a",
                "duplicate",
                &["tracked.txt".into(), "tracked.txt".into()],
                "output"
            ),
            Err(FileStoreError::InvalidPath(_))
        ));
        assert!(matches!(
            store.prepare_process("a", "alias", &["tracked.txt".into()], "tracked.txt"),
            Err(FileStoreError::InvalidPath(_))
        ));
        let many = (0..MAX_PROCESS_INPUTS + 1)
            .map(|index| format!("input-{index}"))
            .collect::<Vec<_>>();
        assert!(matches!(
            store.prepare_process("a", "many", &many, "output"),
            Err(FileStoreError::InvalidPath(_))
        ));
        store
            .prepare_process("a", "partial", &["tracked.txt".into()], "output")
            .unwrap();
        store.bind("a", "partial", "dispatch", &Label::top()).unwrap();
        fs::write(fixture.workspace.join("output"), "partial").unwrap();
        assert!(matches!(
            store.finish("a", "partial", false),
            Err(FileStoreError::Quarantined)
        ));
    }

    fn secret() -> Label {
        Label::new(
            appa_engine::label::Trust::new(2),
            appa_engine::label::Audience::restricted([appa_engine::label::ReaderId::new("operator")]),
        )
    }

    #[test]
    fn binding_reads_no_file_contents() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        let sealed = fixture.workspace.join("sealed.bin");
        fs::write(&sealed, "unreadable").unwrap();
        fs::set_permissions(&sealed, fs::Permissions::from_mode(0o000)).unwrap();
        if File::open(&sealed).is_ok() {
            return;
        }
        let store = fixture.store(&Label::top());
        assert!(store.snapshot().unwrap().is_empty());
        assert!(matches!(
            store.prepare("a", "sealed", FileOperation::Read, "sealed.bin"),
            Err(FileStoreError::Io(_))
        ));
        store.prepare("a", "other", FileOperation::Read, "tracked.txt").unwrap();
    }

    #[test]
    fn first_touch_adopts_current_bytes_and_later_drift_is_caught() {
        let fixture = Fixture::new();
        let store = fixture.store(&secret());
        fs::write(fixture.workspace.join("tracked.txt"), "edited before first touch").unwrap();
        let pin = store.prepare("a", "read", FileOperation::Read, "tracked.txt").unwrap();
        let adopted = store.current("tracked.txt").unwrap().unwrap();
        assert_eq!(pin.predecessor_label, Some(secret()));
        assert_eq!(adopted.label, secret());
        assert_eq!(pin.predecessor_digest, Some(adopted.digest));
        assert_eq!(adopted.previous, None);
        store.cancel("a", "read").unwrap();

        fs::write(fixture.workspace.join("tracked.txt"), "out of band").unwrap();
        assert!(matches!(
            store.prepare("a", "again", FileOperation::Read, "tracked.txt"),
            Err(FileStoreError::DigestMismatch)
        ));
        assert_eq!(store.drifted().unwrap().len(), 1);
    }

    #[test]
    fn links_anywhere_in_the_workspace_refuse_binding() {
        let symlinked = Fixture::new();
        fs::create_dir_all(symlinked.workspace.join("deep/er")).unwrap();
        std::os::unix::fs::symlink("../../tracked.txt", symlinked.workspace.join("deep/er/link")).unwrap();
        assert!(matches!(
            FileStore::new(&symlinked.workspace, &Label::top()),
            Err(FileStoreError::InvalidPath(_))
        ));

        let hard_linked = Fixture::new();
        fs::create_dir(hard_linked.workspace.join("deep")).unwrap();
        fs::hard_link(
            hard_linked.workspace.join("tracked.txt"),
            hard_linked.workspace.join("deep/hard"),
        )
        .unwrap();
        assert!(matches!(
            FileStore::new(&hard_linked.workspace, &Label::top()),
            Err(FileStoreError::InvalidPath(_))
        ));
    }

    #[test]
    fn transfers_and_process_from_untouched_sources_carry_the_initial_label() {
        let fixture = Fixture::new();
        fs::write(fixture.workspace.join("second.txt"), "second").unwrap();
        fs::write(fixture.workspace.join("third.txt"), "third").unwrap();
        let store = fixture.store(&secret());

        let copy = store
            .prepare_transfer("a", "copy", FileOperation::Copy, "tracked.txt", "copy.txt")
            .unwrap();
        assert_eq!(copy.source.as_ref().unwrap().label, secret());
        store.bind("a", "copy", "dispatch-1", &secret()).unwrap();
        fs::copy(
            fixture.workspace.join("tracked.txt"),
            fixture.workspace.join("copy.txt"),
        )
        .unwrap();
        assert_eq!(store.finish("a", "copy", true).unwrap().source_label, Some(secret()));

        let moved = store
            .prepare_transfer("a", "move", FileOperation::Move, "second.txt", "moved.txt")
            .unwrap();
        assert_eq!(moved.source.as_ref().unwrap().label, secret());
        store.bind("a", "move", "dispatch-2", &secret()).unwrap();
        fs::rename(
            fixture.workspace.join("second.txt"),
            fixture.workspace.join("moved.txt"),
        )
        .unwrap();
        assert_eq!(store.finish("a", "move", true).unwrap().source_label, Some(secret()));
        assert!(store.current("second.txt").unwrap().is_none());

        let process = store
            .prepare_process("a", "process", &["third.txt".into()], "output.txt")
            .unwrap();
        assert_eq!(process.inputs[0].label, secret());
        store.bind("a", "process", "dispatch-3", &secret()).unwrap();
        fs::write(fixture.workspace.join("output.txt"), "derived").unwrap();
        assert_eq!(store.finish("a", "process", true).unwrap().source_label, Some(secret()));
    }
}
