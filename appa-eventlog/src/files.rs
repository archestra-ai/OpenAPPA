//! In-memory file-version ledger for native filesystem tool calls.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use appa_engine::label::Label;
use appa_engine::value::{DispatchId, FileBasis, FileSource};
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

/// One tracked version as a reservation pinned it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinnedVersion {
    pub id: i64,
    pub digest: String,
    pub label: Label,
}

/// A pinned version at another path whose bytes the operation consumes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinnedSource {
    pub path: String,
    pub version: PinnedVersion,
}

/// What a reserved operation reads and replaces. `replaced` is the destination's version
/// before the operation, absent when the destination does not exist yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PinnedBasis {
    Read(PinnedVersion),
    Replace(Option<PinnedVersion>),
    Edit(PinnedVersion),
    Copy {
        source: PinnedSource,
        replaced: Option<PinnedVersion>,
    },
    Move {
        source: PinnedSource,
        replaced: Option<PinnedVersion>,
    },
    Process {
        inputs: Vec<PinnedSource>,
        replaced: Option<PinnedVersion>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilePin {
    pub path: String,
    pub basis: PinnedBasis,
}

impl PinnedVersion {
    fn of(version: FileVersion) -> Self {
        Self {
            id: version.id,
            digest: version.digest,
            label: version.label,
        }
    }

    fn file_source(&self) -> FileSource {
        FileSource {
            version: self.id.to_string(),
            digest: self.digest.clone(),
            label: self.label.clone(),
        }
    }
}

impl PinnedBasis {
    fn operation(&self) -> FileOperation {
        match self {
            PinnedBasis::Read(_) => FileOperation::Read,
            PinnedBasis::Replace(_) => FileOperation::Replace,
            PinnedBasis::Edit(_) => FileOperation::Edit,
            PinnedBasis::Copy { .. } => FileOperation::Copy,
            PinnedBasis::Move { .. } => FileOperation::Move,
            PinnedBasis::Process { .. } => FileOperation::Process,
        }
    }

    /// The version the destination held when the reservation was taken.
    fn predecessor(&self) -> Option<&PinnedVersion> {
        match self {
            PinnedBasis::Read(version) | PinnedBasis::Edit(version) => Some(version),
            PinnedBasis::Replace(replaced)
            | PinnedBasis::Copy { replaced, .. }
            | PinnedBasis::Move { replaced, .. }
            | PinnedBasis::Process { replaced, .. } => replaced.as_ref(),
        }
    }

    fn transferred(&self) -> Option<&PinnedSource> {
        match self {
            PinnedBasis::Copy { source, .. } | PinnedBasis::Move { source, .. } => Some(source),
            _ => None,
        }
    }

    fn inputs(&self) -> &[PinnedSource] {
        match self {
            PinnedBasis::Process { inputs, .. } => inputs,
            _ => &[],
        }
    }
}

impl FilePin {
    /// The engine's view of this pin: what the call's flow check rules on.
    pub fn file_basis(&self) -> FileBasis {
        let pinned = |replaced: &Option<PinnedVersion>| replaced.as_ref().map(PinnedVersion::file_source);
        match &self.basis {
            PinnedBasis::Read(version) => FileBasis::Read(version.file_source()),
            PinnedBasis::Replace(replaced) => FileBasis::Replace(pinned(replaced)),
            PinnedBasis::Edit(version) => FileBasis::Edit(version.file_source()),
            PinnedBasis::Copy { source, replaced } => FileBasis::Copy {
                source: source.version.file_source(),
                replaced: pinned(replaced),
            },
            PinnedBasis::Move { source, replaced } => FileBasis::Move {
                source: source.version.file_source(),
                replaced: pinned(replaced),
            },
            PinnedBasis::Process { inputs, replaced } => FileBasis::Process {
                inputs: inputs.iter().map(|input| input.version.file_source()).collect(),
                replaced: pinned(replaced),
            },
        }
    }
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
    pub dispatch: Option<DispatchId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileReceipt {
    pub path: String,
    pub operation: FileOperation,
    pub success: bool,
    pub source_label: Option<Label>,
    pub version: Option<FileVersion>,
    pub dispatch: Option<DispatchId>,
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
    reservation: Option<Reservation>,
    receipts: HashMap<(String, String), FileReceipt>,
}

/// A live reservation: the pinned operation a released call holds the workspace for.
struct Reservation {
    actor: String,
    call_key: String,
    pin: FilePin,
    bound: Option<Bound>,
}

/// What the released dispatch fixed for a reservation: the Label its output file receives.
#[derive(Clone)]
struct Bound {
    dispatch: DispatchId,
    output_label: Label,
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
        let mut state = self.lock();
        if state.reservation.is_some() {
            return Err(FileStoreError::Pending);
        }
        let Touched {
            version: predecessor,
            actual,
        } = touch(&mut state, &self.workspace, &relative)?;
        let basis = match (operation, predecessor.map(PinnedVersion::of)) {
            (FileOperation::Replace, predecessor) => PinnedBasis::Replace(predecessor),
            (FileOperation::Read, Some(version)) => PinnedBasis::Read(version),
            (FileOperation::Edit, Some(version)) => PinnedBasis::Edit(version),
            _ => return Err(FileStoreError::Untracked),
        };
        match (actual.as_str(), basis.predecessor()) {
            (ABSENT, None) => {}
            (_, None) => return Err(FileStoreError::Untracked),
            (actual, Some(version)) if actual == version.digest => {}
            (_, Some(_)) => return Err(FileStoreError::DigestMismatch),
        }
        let pin = FilePin { path: relative, basis };
        state.reservation = Some(pending_reservation(actor, call_key, &pin));
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

        let mut state = self.lock();
        if state.reservation.is_some() {
            return Err(FileStoreError::Pending);
        }
        let Touched {
            version: source_version,
            actual: source_actual,
        } = touch(&mut state, &self.workspace, &source)?;
        let source_version = source_version.ok_or(FileStoreError::Untracked)?;
        if source_actual == ABSENT {
            return Err(FileStoreError::Untracked);
        }
        check_move_filesystem(operation, &source_absolute, &destination_absolute)?;
        if source_actual != source_version.digest {
            return Err(FileStoreError::DigestMismatch);
        }
        let Touched {
            version: predecessor,
            actual: destination_actual,
        } = touch(&mut state, &self.workspace, &destination)?;
        if destination_actual != predecessor.as_ref().map(|v| v.digest.as_str()).unwrap_or(ABSENT) {
            return Err(FileStoreError::DigestMismatch);
        }
        let source = PinnedSource {
            path: source,
            version: PinnedVersion::of(source_version),
        };
        let replaced = predecessor.map(PinnedVersion::of);
        let basis = match operation {
            FileOperation::Copy => PinnedBasis::Copy { source, replaced },
            _ => PinnedBasis::Move { source, replaced },
        };
        let pin = FilePin {
            path: destination,
            basis,
        };
        state.reservation = Some(pending_reservation(actor, call_key, &pin));
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
        let mut state = self.lock();
        if state.reservation.is_some() {
            return Err(FileStoreError::Pending);
        }
        let mut pinned = Vec::with_capacity(inputs.len());
        for path in inputs {
            let Touched { version, actual } = touch(&mut state, &self.workspace, &path)?;
            let version = version.ok_or(FileStoreError::Untracked)?;
            if actual == ABSENT {
                return Err(FileStoreError::Untracked);
            }
            if actual != version.digest {
                return Err(FileStoreError::DigestMismatch);
            }
            pinned.push(PinnedSource {
                path,
                version: PinnedVersion::of(version),
            });
        }
        let Touched {
            version: predecessor,
            actual,
        } = touch(&mut state, &self.workspace, &destination)?;
        if actual != predecessor.as_ref().map(|v| v.digest.as_str()).unwrap_or(ABSENT) {
            return Err(FileStoreError::DigestMismatch);
        }
        let pin = FilePin {
            path: destination,
            basis: PinnedBasis::Process {
                inputs: pinned,
                replaced: predecessor.map(PinnedVersion::of),
            },
        };
        state.reservation = Some(pending_reservation(actor, call_key, &pin));
        Ok(pin)
    }

    pub fn bind(
        &self,
        actor: &str,
        call_key: &str,
        dispatch: &DispatchId,
        output_label: &Label,
    ) -> Result<(), FileStoreError> {
        let mut state = self.lock();
        let reservation = matching_reservation_mut(&mut state, actor, call_key)?;
        if reservation.bound.is_some() {
            return Err(FileStoreError::AlreadyBound);
        }
        reservation.bound = Some(Bound {
            dispatch: dispatch.clone(),
            output_label: output_label.clone(),
        });
        Ok(())
    }

    pub fn cancel(&self, actor: &str, call_key: &str) -> Result<(), FileStoreError> {
        let mut state = self.lock();
        if matching_reservation(&state, actor, call_key)?.bound.is_some() {
            return Err(FileStoreError::AlreadyBound);
        }
        state.reservation = None;
        Ok(())
    }

    pub fn finish(&self, actor: &str, call_key: &str, success: bool) -> Result<FileReceipt, FileStoreError> {
        let mut state = self.lock();
        let key = (actor.to_owned(), call_key.to_owned());
        if let Some(receipt) = state.receipts.get(&key) {
            return Ok(receipt.clone());
        }
        let reservation = matching_reservation(&state, actor, call_key)?;
        let pin = reservation.pin.clone();
        let Bound {
            dispatch,
            output_label: output,
        } = reservation.bound.clone().ok_or(FileStoreError::UnknownReservation)?;
        let observed = Observed::of(&self.workspace, &pin)?;
        let operation = pin.basis.operation();
        let source_label = match &pin.basis {
            PinnedBasis::Process { inputs, .. } => inputs
                .iter()
                .map(|input| input.version.label.clone())
                .reduce(|label, next| label.combine(&next)),
            PinnedBasis::Copy { source, .. } | PinnedBasis::Move { source, .. } => Some(source.version.label.clone()),
            basis => basis.predecessor().map(|version| version.label.clone()),
        };
        let receipt = if !success {
            if !observed.undisturbed(&pin) {
                return Err(FileStoreError::Quarantined);
            }
            FileReceipt {
                path: pin.path.clone(),
                operation,
                success: false,
                source_label: source_label.clone(),
                version: None,
                dispatch: Some(dispatch),
            }
        } else {
            let dependencies = match &pin.basis {
                PinnedBasis::Read(version) => {
                    if observed.destination != version.digest {
                        return Err(FileStoreError::DigestMismatch);
                    }
                    None
                }
                PinnedBasis::Replace(_) => Some(vec![]),
                PinnedBasis::Edit(version) => Some(vec![version.id]),
                PinnedBasis::Copy { source, .. } | PinnedBasis::Move { source, .. } => {
                    let source_after = match operation {
                        FileOperation::Move => ABSENT,
                        _ => source.version.digest.as_str(),
                    };
                    if observed.source.as_deref() != Some(source_after) || observed.destination != source.version.digest
                    {
                        return Err(FileStoreError::DigestMismatch);
                    }
                    Some(vec![source.version.id])
                }
                PinnedBasis::Process { inputs, .. } => {
                    if !observed.inputs_unchanged {
                        return Err(FileStoreError::DigestMismatch);
                    }
                    Some(inputs.iter().map(|input| input.version.id).collect())
                }
            };
            let version = match dependencies {
                None => None,
                Some(dependencies) => {
                    if observed.destination == ABSENT {
                        return Err(FileStoreError::DigestMismatch);
                    }
                    let id = state.next_id;
                    state.next_id += 1;
                    if let PinnedBasis::Move { source, .. } = &pin.basis {
                        state.current.remove(&source.path);
                    }
                    let version = FileVersion {
                        id,
                        path: pin.path.clone(),
                        digest: observed.destination,
                        label: output,
                        previous: pin.basis.predecessor().map(|version| version.id),
                        content_dependencies: dependencies,
                        dispatch: Some(dispatch.clone()),
                    };
                    state.versions.insert(id, version.clone());
                    state.current.insert(pin.path.clone(), id);
                    Some(version)
                }
            };
            FileReceipt {
                path: pin.path.clone(),
                operation,
                success: true,
                source_label,
                version,
                dispatch: Some(dispatch),
            }
        };
        state.receipts.insert(key, receipt.clone());
        state.reservation = None;
        Ok(receipt)
    }

    /// Release the reservation of a call that was released and never ran, provided the
    /// workspace still shows its pinned state. The runtime cannot tell an unrun call from one
    /// whose report was lost, so anything else keeps the reservation: a workspace that moved
    /// is never released by guessing here.
    pub fn abandon(&self, actor: &str, call_key: &str) -> Result<AbandonOutcome, FileStoreError> {
        let mut state = self.lock();
        let Some(reservation) = state.reservation.as_ref() else {
            return Ok(AbandonOutcome::Absent);
        };
        if reservation.actor != actor || reservation.call_key != call_key {
            return Ok(AbandonOutcome::Absent);
        }
        if !Observed::of(&self.workspace, &reservation.pin)?.undisturbed(&reservation.pin) {
            return Ok(AbandonOutcome::Quarantined);
        }
        state.reservation = None;
        Ok(AbandonOutcome::Released)
    }

    /// The pin a live reservation holds for this exact call, if it holds one. What an
    /// operation executes is the path this pin recorded, never the path the call spelled: the
    /// ledger validated and hashed that one.
    pub fn pin_for(&self, actor: &str, call_key: &str) -> Result<Option<FilePin>, FileStoreError> {
        let state = self.lock();
        Ok(state
            .reservation
            .as_ref()
            .filter(|r| r.actor == actor && r.call_key == call_key)
            .map(|r| r.pin.clone()))
    }

    /// The workspace this ledger is bound to.
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub fn current(&self, path: &str) -> Result<Option<FileVersion>, FileStoreError> {
        let relative = validated_relative(&self.workspace, path)?;
        let state = self.lock();
        Ok(current_state(&state, &relative))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .expect("the file store mutex is never poisoned: no panics under the lock")
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
    let stripped = match supplied.is_absolute() {
        false => supplied,
        true if supplied.components().any(|c| c == Component::ParentDir) => {
            return Err(FileStoreError::InvalidPath(input.into()));
        }
        // The harness may spell the workspace through a symlinked ancestor (macOS `/var`,
        // `/tmp`); the shortest ancestor that is the canonical workspace anchors the path.
        true => {
            let mut ancestors: Vec<&Path> = supplied.ancestors().collect();
            ancestors.reverse();
            ancestors
                .into_iter()
                .find(|ancestor| fs::canonicalize(ancestor).is_ok_and(|canonical| canonical == workspace))
                .and_then(|anchor| supplied.strip_prefix(anchor).ok())
                .ok_or_else(|| FileStoreError::InvalidPath(input.into()))?
        }
    };
    if stripped.as_os_str().is_empty() || stripped.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(FileStoreError::InvalidPath(input.into()));
    }
    let mut cursor = workspace.to_path_buf();
    for component in stripped.components() {
        cursor.push(component);
        match fs::symlink_metadata(&cursor) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(FileStoreError::InvalidPath("symlink component".into()));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
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
/// What the workspace holds now at every path a pin recorded, each hashed once.
struct Observed {
    destination: String,
    source: Option<String>,
    inputs_unchanged: bool,
}

impl Observed {
    fn of(workspace: &Path, pin: &FilePin) -> Result<Self, FileStoreError> {
        let destination = state_digest(workspace, &pin.path)?;
        let source = pin
            .basis
            .transferred()
            .map(|source| state_digest(workspace, &source.path))
            .transpose()?;
        let mut inputs_unchanged = true;
        for input in pin.basis.inputs() {
            inputs_unchanged &= state_digest(workspace, &input.path)? == input.version.digest;
        }
        Ok(Self {
            destination,
            source,
            inputs_unchanged,
        })
    }

    /// Whether the pin still describes the workspace: the destination holds the bytes the
    /// operation would have replaced, a transfer's source is where the operation would have
    /// left it, and every declared input is unchanged.
    fn undisturbed(&self, pin: &FilePin) -> bool {
        self.destination
            == pin
                .basis
                .predecessor()
                .map_or(ABSENT, |version| version.digest.as_str())
            && pin
                .basis
                .transferred()
                .is_none_or(|source| self.source.as_deref() == Some(source.version.digest.as_str()))
            && self.inputs_unchanged
    }
}

fn pending_reservation(actor: &str, call_key: &str, pin: &FilePin) -> Reservation {
    Reservation {
        actor: actor.into(),
        call_key: call_key.into(),
        pin: pin.clone(),
        bound: None,
    }
}

fn matching_reservation<'a>(state: &'a State, actor: &str, call_key: &str) -> Result<&'a Reservation, FileStoreError> {
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
) -> Result<&'a mut Reservation, FileStoreError> {
    state
        .reservation
        .as_mut()
        .filter(|r| r.actor == actor && r.call_key == call_key)
        .ok_or(FileStoreError::UnknownReservation)
}

fn current_state(state: &State, path: &str) -> Option<FileVersion> {
    state.current.get(path).and_then(|id| state.versions.get(id)).cloned()
}

/// A path's current version beside the digest of what is on disk now.
struct Touched {
    version: Option<FileVersion>,
    actual: String,
}

/// Hash the path once and return its current version. A path the ledger has never versioned
/// that holds a file on disk is adopted first, with the operator's initial label.
fn touch(state: &mut State, workspace: &Path, path: &str) -> Result<Touched, FileStoreError> {
    let actual = state_digest(workspace, path)?;
    let version = match current_state(state, path) {
        Some(version) => Some(version),
        None if actual == ABSENT || state.versions.values().any(|version| version.path == path) => None,
        None => {
            let version = FileVersion {
                id: state.next_id,
                path: path.into(),
                digest: actual.clone(),
                label: state.initial.clone(),
                previous: None,
                content_dependencies: vec![],
                dispatch: None,
            };
            state.next_id += 1;
            state.versions.insert(version.id, version.clone());
            state.current.insert(version.path.clone(), version.id);
            Some(version)
        }
    };
    Ok(Touched { version, actual })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn dispatch(occurrence: u32) -> DispatchId {
        DispatchId::new(
            appa_engine::value::TrajectoryId::new("a"),
            serde_json::from_value(serde_json::json!("ab".repeat(32))).unwrap(),
            occurrence,
        )
    }

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
        store.bind("a", "unrun", &dispatch(0), &Label::top()).unwrap();
        assert!(store.pin_for("a", "unrun").unwrap().is_some());
        assert_eq!(store.abandon("a", "unrun").unwrap(), AbandonOutcome::Released);
        assert!(store.pin_for("a", "unrun").unwrap().is_none());
        assert_eq!(store.abandon("a", "unrun").unwrap(), AbandonOutcome::Absent);
        // The next call proceeds: the release did not leave the workspace wedged.
        store.prepare("b", "next", FileOperation::Read, "tracked.txt").unwrap();
        store.cancel("b", "next").unwrap();

        // A transfer whose destination moved is not released: the runtime cannot tell an
        // unrun call from one whose report was lost.
        store
            .prepare_transfer("a", "copy", FileOperation::Copy, "tracked.txt", "destination.txt")
            .unwrap();
        store.bind("a", "copy", &dispatch(2), &Label::top()).unwrap();
        fs::write(fixture.workspace.join("destination.txt"), "partial").unwrap();
        assert_eq!(store.abandon("a", "copy").unwrap(), AbandonOutcome::Quarantined);
        assert!(store.pin_for("a", "copy").unwrap().is_some());
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
        assert!(first.pin_for("a", "one").unwrap().is_some());
        assert!(first.pin_for("b", "two").unwrap().is_none());
        assert!(second.pin_for("b", "two").unwrap().is_some());
        assert!(second.pin_for("a", "one").unwrap().is_none());
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
        store.bind("a", "replace", &dispatch(1), &output).unwrap();
        fs::write(fixture.workspace.join("tracked.txt"), "replacement").unwrap();
        let replaced = store.finish("a", "replace", true).unwrap();
        assert_eq!(replaced.version.as_ref().unwrap().label, output);
        assert_eq!(
            replaced.version.as_ref().unwrap().previous,
            first.basis.predecessor().map(|version| version.id)
        );
        assert!(replaced.version.as_ref().unwrap().content_dependencies.is_empty());
        assert_eq!(store.finish("a", "replace", true).unwrap(), replaced);

        let pin = store.prepare("a", "edit", FileOperation::Edit, "tracked.txt").unwrap();
        store.bind("a", "edit", &dispatch(2), &Label::top()).unwrap();
        fs::write(fixture.workspace.join("tracked.txt"), "edited").unwrap();
        let edited = store.finish("a", "edit", true).unwrap().version.unwrap();
        assert_eq!(
            pin.basis.predecessor().map(|version| version.id),
            Some(replaced.version.unwrap().id)
        );
        assert_eq!(edited.previous, pin.basis.predecessor().map(|version| version.id));
        assert_eq!(
            edited.content_dependencies,
            vec![pin.basis.predecessor().map(|version| version.id).unwrap()]
        );
        assert_eq!(store.current("tracked.txt").unwrap(), Some(edited));
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
        store.bind("a", "fail", &dispatch(0), &Label::top()).unwrap();
        assert!(!store.finish("a", "fail", false).unwrap().success);

        store
            .prepare("a", "partial", FileOperation::Edit, "tracked.txt")
            .unwrap();
        store.bind("a", "partial", &dispatch(2), &Label::top()).unwrap();
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
    fn absolute_paths_through_a_symlinked_workspace_alias_resolve_to_the_workspace() {
        let fixture = Fixture::new();
        let store = fixture.store(&Label::top());
        let alias = fixture._root.path().join("alias");
        std::os::unix::fs::symlink(&fixture.workspace, &alias).unwrap();
        std::os::unix::fs::symlink(".", fixture.workspace.join("inner")).unwrap();
        let absolute = |path: PathBuf| path.to_str().unwrap().to_string();

        let pin = store
            .prepare("a", "alias", FileOperation::Read, &absolute(alias.join("tracked.txt")))
            .unwrap();
        assert_eq!(pin.path, "tracked.txt");
        store.cancel("a", "alias").unwrap();
        for rejected in [
            alias.join("inner/tracked.txt"),
            alias.join("../workspace/tracked.txt"),
            fixture._root.path().join("tracked.txt"),
        ] {
            assert!(matches!(
                store.prepare("a", "rejected", FileOperation::Read, &absolute(rejected)),
                Err(FileStoreError::InvalidPath(_))
            ));
        }
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
        store.prepare("a", "edit", FileOperation::Edit, "sub/file.txt").unwrap();
        let pinned = store.current("sub/file.txt").unwrap().unwrap();
        store.bind("a", "edit", &dispatch(0), &Label::top()).unwrap();
        fs::rename(fixture.workspace.join("sub"), fixture.workspace.join("real")).unwrap();
        std::os::unix::fs::symlink(&outside, fixture.workspace.join("sub")).unwrap();

        assert!(store.abandon("a", "edit").is_err());
        fs::write(outside.join("file.txt"), "escaped").unwrap();
        assert!(store.finish("a", "edit", true).is_err());
        fs::remove_file(fixture.workspace.join("sub")).unwrap();
        fs::rename(fixture.workspace.join("real"), fixture.workspace.join("sub")).unwrap();
        assert_eq!(store.current("sub/file.txt").unwrap(), Some(pinned));
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
            .bind("a", "label-destination", &dispatch(0), &destination_label)
            .unwrap();
        store.finish("a", "label-destination", true).unwrap();

        let pin = store
            .prepare_transfer("a", "copy", FileOperation::Copy, "tracked.txt", "destination.bin")
            .unwrap();
        let source = pin.basis.transferred().unwrap();
        assert_ne!(
            Some(source.version.id),
            pin.basis.predecessor().map(|version| version.id)
        );
        assert_eq!(source.version.label, source_label);
        assert_eq!(
            pin.basis.predecessor().map(|version| version.label.clone()),
            Some(destination_label)
        );
        store.bind("a", "copy", &dispatch(0), &source_label).unwrap();
        fs::copy(
            fixture.workspace.join("tracked.txt"),
            fixture.workspace.join("destination.bin"),
        )
        .unwrap();
        let receipt = store.finish("a", "copy", true).unwrap();
        assert_eq!(receipt.source_label, Some(source_label));
        assert_eq!(receipt.version.unwrap().content_dependencies, vec![source.version.id]);
    }

    #[test]
    fn move_marks_source_absent_preserves_history_and_allows_reuse() {
        let fixture = Fixture::new();
        let store = fixture.store(&Label::top());
        store
            .prepare_transfer("a", "move", FileOperation::Move, "tracked.txt", "moved.txt")
            .unwrap();
        let source_id = store.current("tracked.txt").unwrap().unwrap().id;
        store.bind("a", "move", &dispatch(0), &Label::top()).unwrap();
        fs::rename(
            fixture.workspace.join("tracked.txt"),
            fixture.workspace.join("moved.txt"),
        )
        .unwrap();
        let moved = store.finish("a", "move", true).unwrap();
        assert_eq!(moved.version.unwrap().content_dependencies, vec![source_id]);
        assert!(store.current("tracked.txt").unwrap().is_none());
        // The moved-away version stays in the ledger, so bytes reappearing out of band are
        // not adopted as a fresh first touch.
        fs::write(fixture.workspace.join("tracked.txt"), "out of band").unwrap();
        assert!(matches!(
            store.prepare("a", "reappeared", FileOperation::Read, "tracked.txt"),
            Err(FileStoreError::Untracked)
        ));
        fs::remove_file(fixture.workspace.join("tracked.txt")).unwrap();

        store
            .prepare("a", "reuse", FileOperation::Replace, "tracked.txt")
            .unwrap();
        store.bind("a", "reuse", &dispatch(2), &Label::top()).unwrap();
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
        store.bind("a", "copy", &dispatch(0), &Label::top()).unwrap();
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
        store.bind("a", "copy", &dispatch(0), &Label::top()).unwrap();
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
        let dependencies: Vec<_> = pin.basis.inputs().iter().map(|input| input.version.id).collect();
        let previous = pin.basis.predecessor().map(|version| version.id);
        let output = Label::new(
            appa_engine::label::Trust::new(3),
            appa_engine::label::Audience::restricted([appa_engine::label::ReaderId::new("result")]),
        );
        store.bind("a", "process", &dispatch(0), &output).unwrap();
        fs::write(fixture.workspace.join("output.bin"), [128, 2, 0, 255]).unwrap();
        let receipt = store.finish("a", "process", true).unwrap();
        let version = receipt.version.unwrap();
        assert_eq!(version.label, output);
        assert_eq!(version.previous, previous);
        assert_eq!(version.content_dependencies, dependencies);
        assert_eq!(store.current("output.bin").unwrap().unwrap(), version);
        assert!(previous.is_some());
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
        store.bind("a", "partial", &dispatch(0), &Label::top()).unwrap();
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
        assert!(store.current("tracked.txt").unwrap().is_none());
        assert!(matches!(
            store.prepare("a", "sealed", FileOperation::Read, "sealed.bin"),
            Err(FileStoreError::Io(_))
        ));
        store.prepare("a", "other", FileOperation::Read, "tracked.txt").unwrap();
    }

    #[test]
    fn a_path_whose_parent_cannot_be_inspected_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        let locked = fixture.workspace.join("locked");
        fs::create_dir(&locked).unwrap();
        fs::write(locked.join("file.txt"), "hidden").unwrap();
        let store = fixture.store(&Label::top());
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let inspectable = fs::symlink_metadata(locked.join("file.txt")).is_ok();
        let lookup = store.current("locked/file.txt");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        if inspectable {
            return;
        }
        assert!(matches!(lookup, Err(FileStoreError::Io(_))));
    }

    #[test]
    fn first_touch_adopts_current_bytes_and_later_drift_is_caught() {
        let fixture = Fixture::new();
        let store = fixture.store(&secret());
        fs::write(fixture.workspace.join("tracked.txt"), "edited before first touch").unwrap();
        let pin = store.prepare("a", "read", FileOperation::Read, "tracked.txt").unwrap();
        let adopted = store.current("tracked.txt").unwrap().unwrap();
        assert_eq!(
            pin.basis.predecessor().map(|version| version.label.clone()),
            Some(secret())
        );
        assert_eq!(adopted.label, secret());
        assert_eq!(
            pin.basis.predecessor().map(|version| version.digest.clone()),
            Some(adopted.digest)
        );
        assert_eq!(adopted.previous, None);
        store.cancel("a", "read").unwrap();

        fs::write(fixture.workspace.join("tracked.txt"), "out of band").unwrap();
        assert!(matches!(
            store.prepare("a", "again", FileOperation::Read, "tracked.txt"),
            Err(FileStoreError::DigestMismatch)
        ));
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

    fn trust(level: u8) -> Label {
        Label::new(
            appa_engine::label::Trust::new(level),
            appa_engine::label::Audience::public(),
        )
    }

    #[test]
    fn receipts_carry_the_label_of_what_each_operation_consumed() {
        let fixture = Fixture::new();
        fs::write(fixture.workspace.join("second.txt"), "second").unwrap();
        let store = fixture.store(&secret());

        store.prepare("a", "read", FileOperation::Read, "tracked.txt").unwrap();
        store.bind("a", "read", &dispatch(1), &trust(1)).unwrap();
        let read = store.finish("a", "read", true).unwrap();
        assert!(read.success);
        assert_eq!(read.source_label, Some(secret()));
        assert_eq!(read.version, None);

        store
            .prepare("a", "create", FileOperation::Replace, "fresh.txt")
            .unwrap();
        store.bind("a", "create", &dispatch(2), &trust(2)).unwrap();
        fs::write(fixture.workspace.join("fresh.txt"), "fresh").unwrap();
        let created = store.finish("a", "create", true).unwrap();
        assert_eq!(created.source_label, None);
        let version = created.version.unwrap();
        assert_eq!(version.previous, None);
        assert_eq!(version.label, trust(2));
        assert_eq!(version.dispatch, Some(dispatch(2)));

        store.prepare("a", "edit", FileOperation::Edit, "fresh.txt").unwrap();
        store.bind("a", "edit", &dispatch(3), &trust(3)).unwrap();
        let failed = store.finish("a", "edit", false).unwrap();
        assert!(!failed.success);
        assert_eq!(failed.source_label, Some(trust(2)));
        assert_eq!(failed.version, None);
        assert_eq!(failed.dispatch, Some(dispatch(3)));
        assert_eq!(store.current("fresh.txt").unwrap(), Some(version));

        store
            .prepare_process("a", "process", &["tracked.txt".into(), "fresh.txt".into()], "out.txt")
            .unwrap();
        store.bind("a", "process", &dispatch(4), &trust(0)).unwrap();
        fs::write(fixture.workspace.join("out.txt"), "derived").unwrap();
        let processed = store.finish("a", "process", true).unwrap();
        assert_eq!(processed.source_label, Some(secret().combine(&trust(2))));
    }

    #[test]
    fn a_reservation_binds_once_and_finishes_only_after_binding() {
        let fixture = Fixture::new();
        let store = fixture.store(&Label::top());
        store.prepare("a", "edit", FileOperation::Edit, "tracked.txt").unwrap();
        assert!(matches!(
            store.finish("a", "edit", true),
            Err(FileStoreError::UnknownReservation)
        ));
        assert!(matches!(
            store.bind("a", "other", &dispatch(0), &Label::top()),
            Err(FileStoreError::UnknownReservation)
        ));
        store.bind("a", "edit", &dispatch(0), &Label::top()).unwrap();
        assert!(matches!(
            store.bind("a", "edit", &dispatch(2), &Label::top()),
            Err(FileStoreError::AlreadyBound)
        ));
        assert!(matches!(store.cancel("a", "edit"), Err(FileStoreError::AlreadyBound)));
        fs::write(fixture.workspace.join("tracked.txt"), "edited").unwrap();
        let version = store.finish("a", "edit", true).unwrap().version.unwrap();
        assert_eq!(version.dispatch, Some(dispatch(0)));
    }

    #[test]
    fn a_failed_call_whose_source_or_input_changed_is_quarantined() {
        let fixture = Fixture::new();
        let store = fixture.store(&Label::top());
        store
            .prepare_transfer("a", "copy", FileOperation::Copy, "tracked.txt", "copy.txt")
            .unwrap();
        store.bind("a", "copy", &dispatch(0), &Label::top()).unwrap();
        fs::write(fixture.workspace.join("tracked.txt"), "changed").unwrap();
        assert!(matches!(
            store.finish("a", "copy", false),
            Err(FileStoreError::Quarantined)
        ));

        let fixture = Fixture::new();
        let store = fixture.store(&Label::top());
        store
            .prepare_process("a", "process", &["tracked.txt".into()], "out.txt")
            .unwrap();
        store.bind("a", "process", &dispatch(0), &Label::top()).unwrap();
        fs::write(fixture.workspace.join("tracked.txt"), "changed").unwrap();
        assert_eq!(store.abandon("a", "process").unwrap(), AbandonOutcome::Quarantined);
        assert!(matches!(
            store.finish("a", "process", false),
            Err(FileStoreError::Quarantined)
        ));
        fs::write(fixture.workspace.join("tracked.txt"), "old").unwrap();
        assert!(!store.finish("a", "process", false).unwrap().success);
    }

    #[test]
    fn a_read_whose_file_changed_during_the_call_is_refused() {
        let fixture = Fixture::new();
        let store = fixture.store(&Label::top());
        store.prepare("a", "read", FileOperation::Read, "tracked.txt").unwrap();
        store.bind("a", "read", &dispatch(0), &Label::top()).unwrap();
        fs::write(fixture.workspace.join("tracked.txt"), "changed").unwrap();
        assert!(matches!(
            store.finish("a", "read", true),
            Err(FileStoreError::DigestMismatch)
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
        assert_eq!(copy.basis.transferred().unwrap().version.label, secret());
        store.bind("a", "copy", &dispatch(1), &secret()).unwrap();
        fs::copy(
            fixture.workspace.join("tracked.txt"),
            fixture.workspace.join("copy.txt"),
        )
        .unwrap();
        assert_eq!(store.finish("a", "copy", true).unwrap().source_label, Some(secret()));

        let moved = store
            .prepare_transfer("a", "move", FileOperation::Move, "second.txt", "moved.txt")
            .unwrap();
        assert_eq!(moved.basis.transferred().unwrap().version.label, secret());
        store.bind("a", "move", &dispatch(2), &secret()).unwrap();
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
        assert_eq!(process.basis.inputs()[0].version.label, secret());
        store.bind("a", "process", &dispatch(3), &secret()).unwrap();
        fs::write(fixture.workspace.join("output.txt"), "derived").unwrap();
        assert_eq!(store.finish("a", "process", true).unwrap().source_label, Some(secret()));
    }
}
