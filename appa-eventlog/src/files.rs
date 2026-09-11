//! Durable file-version ledger for native filesystem tool calls.

use std::fs::{self, File};
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use appa_engine::label::Label;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const SCHEMA: i64 = 1;
const ABSENT: &str = "-";

#[derive(Debug, thiserror::Error)]
pub enum FileStoreError {
    #[error("invalid file-ledger configuration: {0}")]
    Configuration(String),
    #[error("file ledger is not initialized")]
    Uninitialized,
    #[error("file ledger belongs to a different workspace or policy")]
    BindingMismatch,
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
    #[error("storage failure: {0}")]
    Storage(#[from] rusqlite::Error),
    #[error("label encoding failure: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileOperation {
    Read,
    Replace,
    Edit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilePin {
    pub path: String,
    pub operation: FileOperation,
    pub predecessor_version: Option<i64>,
    pub predecessor_label: Option<Label>,
    pub predecessor_digest: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileVersion {
    pub id: i64,
    pub path: String,
    pub digest: String,
    pub label: Label,
    pub previous: Option<i64>,
    /// Present only when an Edit consumed the predecessor bytes.
    pub edit_dependency: Option<i64>,
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

pub struct FileStore {
    connection: Mutex<Connection>,
    workspace: PathBuf,
}

impl FileStore {
    pub fn initialize(db: &Path, workspace: &Path, policy: &str, initial: &Label) -> Result<Self, FileStoreError> {
        let workspace = canonical_workspace(workspace)?;
        validate_database_path(db, &workspace, true)?;
        let mut connection = Connection::open(db)?;
        configure(&connection)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        create_schema(&transaction)?;
        if transaction.query_row("SELECT count(*) FROM ledger_meta", [], |r| r.get::<_, i64>(0))? != 0 {
            return Err(FileStoreError::Configuration("ledger already initialized".into()));
        }
        transaction.execute(
            "INSERT INTO ledger_meta(workspace,policy) VALUES (?1,?2)",
            params![workspace.to_string_lossy(), policy],
        )?;
        let label = serde_json::to_string(initial)?;
        for (relative, digest) in scan(&workspace)? {
            transaction.execute("INSERT INTO versions(path,digest,label,previous,edit_dependency,dispatch) VALUES (?1,?2,?3,NULL,NULL,NULL)", params![relative, digest, label])?;
        }
        transaction.pragma_update(None, "user_version", SCHEMA)?;
        transaction.commit()?;
        Ok(Self {
            connection: Mutex::new(connection),
            workspace,
        })
    }

    pub fn open(db: &Path, workspace: &Path, policy: &str, _initial: &Label) -> Result<Self, FileStoreError> {
        let workspace = canonical_workspace(workspace)?;
        validate_database_path(db, &workspace, false)?;
        if !db.is_file() {
            return Err(FileStoreError::Uninitialized);
        }
        let connection = Connection::open(db)?;
        configure(&connection)?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version != SCHEMA {
            return Err(FileStoreError::Uninitialized);
        }
        let binding = connection
            .query_row("SELECT workspace,policy FROM ledger_meta", [], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .optional()?;
        if binding.as_ref() != Some(&(workspace.to_string_lossy().into_owned(), policy.to_owned())) {
            return Err(FileStoreError::BindingMismatch);
        }
        Ok(Self {
            connection: Mutex::new(connection),
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
        let relative = validated_relative(&self.workspace, path)?;
        let absolute = self.workspace.join(&relative);
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| FileStoreError::Corrupt("connection lock poisoned".into()))?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if tx.query_row("SELECT count(*) FROM reservation", [], |r| r.get::<_, i64>(0))? != 0 {
            return Err(FileStoreError::Pending);
        }
        let predecessor = current_tx(&tx, &relative)?;
        match operation {
            FileOperation::Read | FileOperation::Edit if predecessor.is_none() => {
                return Err(FileStoreError::Untracked);
            }
            _ => {}
        }
        if absolute.exists() {
            check_regular(&absolute)?;
            let actual = hash(&absolute)?;
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
        };
        tx.execute(
            "INSERT INTO reservation(actor,call_key,pin,bound_dispatch,output_label) VALUES (?1,?2,?3,NULL,NULL)",
            params![actor, call_key, serde_json::to_string(&pin)?],
        )?;
        tx.commit()?;
        Ok(pin)
    }

    pub fn bind(
        &self,
        actor: &str,
        call_key: &str,
        dispatch: &str,
        output_label: &Label,
    ) -> Result<(), FileStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| FileStoreError::Corrupt("connection lock poisoned".into()))?;
        let changed = connection.execute("UPDATE reservation SET bound_dispatch=?3,output_label=?4 WHERE actor=?1 AND call_key=?2 AND bound_dispatch IS NULL", params![actor,call_key,dispatch,serde_json::to_string(output_label)?])?;
        if changed == 0 {
            if reservation_exists(&connection, actor, call_key)? {
                Err(FileStoreError::AlreadyBound)
            } else {
                Err(FileStoreError::UnknownReservation)
            }
        } else {
            Ok(())
        }
    }

    pub fn cancel(&self, actor: &str, call_key: &str) -> Result<(), FileStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| FileStoreError::Corrupt("connection lock poisoned".into()))?;
        if connection.execute(
            "DELETE FROM reservation WHERE actor=?1 AND call_key=?2 AND bound_dispatch IS NULL",
            params![actor, call_key],
        )? == 0
        {
            if reservation_exists(&connection, actor, call_key)? {
                Err(FileStoreError::AlreadyBound)
            } else {
                Err(FileStoreError::UnknownReservation)
            }
        } else {
            Ok(())
        }
    }

    pub fn finish(&self, actor: &str, call_key: &str, success: bool) -> Result<FileReceipt, FileStoreError> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| FileStoreError::Corrupt("connection lock poisoned".into()))?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(encoded) = tx
            .query_row(
                "SELECT receipt FROM receipts WHERE actor=?1 AND call_key=?2",
                params![actor, call_key],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            return Ok(serde_json::from_str(&encoded)?);
        }
        let row = tx
            .query_row(
                "SELECT pin,bound_dispatch,output_label FROM reservation WHERE actor=?1 AND call_key=?2",
                params![actor, call_key],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or(FileStoreError::UnknownReservation)?;
        let (pin, dispatch, output): (FilePin, String, Label) = (
            serde_json::from_str(&row.0)?,
            row.1.ok_or(FileStoreError::UnknownReservation)?,
            serde_json::from_str(&row.2.ok_or(FileStoreError::UnknownReservation)?)?,
        );
        let absolute = self.workspace.join(&pin.path);
        let actual = state_digest(&absolute)?;
        let expected = pin.predecessor_digest.as_deref().unwrap_or(ABSENT);
        let receipt = if !success {
            if actual != expected {
                return Err(FileStoreError::Quarantined);
            }
            FileReceipt {
                path: pin.path.clone(),
                operation: pin.operation,
                success: false,
                source_label: pin.predecessor_label.clone(),
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
                source_label: pin.predecessor_label.clone(),
                version: None,
                dispatch: Some(dispatch),
            }
        } else {
            if actual == ABSENT {
                return Err(FileStoreError::DigestMismatch);
            }
            check_regular(&absolute)?;
            tx.execute(
                "INSERT INTO versions(path,digest,label,previous,edit_dependency,dispatch) VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    pin.path,
                    actual,
                    serde_json::to_string(&output)?,
                    pin.predecessor_version,
                    if pin.operation == FileOperation::Edit {
                        pin.predecessor_version
                    } else {
                        None
                    },
                    dispatch
                ],
            )?;
            let id = tx.last_insert_rowid();
            let version = FileVersion {
                id,
                path: pin.path.clone(),
                digest: actual,
                label: output,
                previous: pin.predecessor_version,
                edit_dependency: if pin.operation == FileOperation::Edit {
                    pin.predecessor_version
                } else {
                    None
                },
                dispatch: Some(dispatch.clone()),
            };
            FileReceipt {
                path: pin.path.clone(),
                operation: pin.operation,
                success: true,
                source_label: pin.predecessor_label.clone(),
                version: Some(version),
                dispatch: Some(dispatch),
            }
        };
        tx.execute(
            "INSERT INTO receipts(actor,call_key,receipt) VALUES (?1,?2,?3)",
            params![actor, call_key, serde_json::to_string(&receipt)?],
        )?;
        tx.execute(
            "DELETE FROM reservation WHERE actor=?1 AND call_key=?2",
            params![actor, call_key],
        )?;
        tx.commit()?;
        Ok(receipt)
    }

    pub fn current(&self, path: &str) -> Result<Option<FileVersion>, FileStoreError> {
        let relative = validated_relative(&self.workspace, path)?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| FileStoreError::Corrupt("connection lock poisoned".into()))?;
        current_connection(&connection, &relative)
    }

    pub fn history(&self, path: &str) -> Result<Vec<FileVersion>, FileStoreError> {
        let relative = validated_relative(&self.workspace, path)?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| FileStoreError::Corrupt("connection lock poisoned".into()))?;
        let mut statement = connection.prepare(
            "SELECT id,path,digest,label,previous,edit_dependency,dispatch FROM versions WHERE path=?1 ORDER BY id",
        )?;
        decode_versions(statement.query_map([relative], decode_version)?)
    }

    pub fn snapshot(&self) -> Result<Vec<FileVersion>, FileStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| FileStoreError::Corrupt("connection lock poisoned".into()))?;
        let mut statement = connection.prepare("SELECT v.id,v.path,v.digest,v.label,v.previous,v.edit_dependency,v.dispatch FROM versions v JOIN (SELECT path,max(id) id FROM versions GROUP BY path) c ON c.id=v.id ORDER BY v.path")?;
        decode_versions(statement.query_map([], decode_version)?)
    }
}

fn configure(c: &Connection) -> Result<(), rusqlite::Error> {
    c.busy_timeout(std::time::Duration::from_secs(5))?;
    c.pragma_update(None, "foreign_keys", "ON")
}
fn create_schema(tx: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    tx.execute_batch("CREATE TABLE ledger_meta(workspace TEXT NOT NULL,policy TEXT NOT NULL); CREATE TABLE versions(id INTEGER PRIMARY KEY,path TEXT NOT NULL,digest TEXT NOT NULL,label TEXT NOT NULL,previous INTEGER,edit_dependency INTEGER,dispatch TEXT); CREATE TABLE reservation(singleton INTEGER PRIMARY KEY DEFAULT 1 CHECK(singleton=1),actor TEXT NOT NULL,call_key TEXT NOT NULL,pin TEXT NOT NULL,bound_dispatch TEXT,output_label TEXT,UNIQUE(actor,call_key)); CREATE TABLE receipts(actor TEXT NOT NULL,call_key TEXT NOT NULL,receipt TEXT NOT NULL,PRIMARY KEY(actor,call_key));")
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
fn validate_database_path(db: &Path, workspace: &Path, new: bool) -> Result<(), FileStoreError> {
    if !db.is_absolute() {
        return Err(FileStoreError::Configuration("database path must be absolute".into()));
    }
    let parent = db
        .parent()
        .ok_or_else(|| FileStoreError::Configuration("database has no parent".into()))?;
    let parent = fs::canonicalize(parent)?;
    let resolved_db = if db.exists() { Some(fs::canonicalize(db)?) } else { None };
    if parent.starts_with(workspace)
        || db.starts_with(workspace)
        || resolved_db.as_ref().is_some_and(|path| path.starts_with(workspace))
    {
        return Err(FileStoreError::Configuration(
            "database must be outside workspace".into(),
        ));
    }
    if new && db.exists() {
        return Err(FileStoreError::Configuration("database already exists".into()));
    }
    Ok(())
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
fn check_regular(path: &Path) -> Result<(), FileStoreError> {
    let m = fs::symlink_metadata(path)?;
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
fn hash(path: &Path) -> Result<String, FileStoreError> {
    check_regular(path)?;
    let mut f = File::open(path)?;
    let mut h = Sha256::new();
    let mut b = [0; 8192];
    loop {
        let n = f.read(&mut b)?;
        if n == 0 {
            break;
        }
        h.update(&b[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}
fn state_digest(path: &Path) -> Result<String, FileStoreError> {
    if path.exists() { hash(path) } else { Ok(ABSENT.into()) }
}
fn scan(root: &Path) -> Result<Vec<(String, String)>, FileStoreError> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) -> Result<(), FileStoreError> {
        for e in fs::read_dir(dir)? {
            let e = e?;
            let p = e.path();
            let m = fs::symlink_metadata(&p)?;
            if m.file_type().is_symlink() {
                return Err(FileStoreError::InvalidPath("workspace contains symlink".into()));
            }
            if m.is_dir() {
                walk(root, &p, out)?
            } else {
                check_regular(&p)?;
                out.push((
                    p.strip_prefix(root)
                        .map_err(|_| FileStoreError::InvalidPath("outside workspace".into()))?
                        .to_string_lossy()
                        .into_owned(),
                    hash(&p)?,
                ));
            }
        }
        Ok(())
    }
    let mut o = vec![];
    walk(root, root, &mut o)?;
    o.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(o)
}
fn reservation_exists(c: &Connection, a: &str, k: &str) -> Result<bool, rusqlite::Error> {
    c.query_row(
        "SELECT EXISTS(SELECT 1 FROM reservation WHERE actor=?1 AND call_key=?2)",
        params![a, k],
        |r| r.get(0),
    )
}
type StoredVersion = (i64, String, String, String, Option<i64>, Option<i64>, Option<String>);

fn decode_version(r: &rusqlite::Row<'_>) -> rusqlite::Result<StoredVersion> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
    ))
}
fn version(v: StoredVersion) -> Result<FileVersion, FileStoreError> {
    Ok(FileVersion {
        id: v.0,
        path: v.1,
        digest: v.2,
        label: serde_json::from_str(&v.3)?,
        previous: v.4,
        edit_dependency: v.5,
        dispatch: v.6,
    })
}
fn decode_versions<I>(rows: I) -> Result<Vec<FileVersion>, FileStoreError>
where
    I: Iterator<Item = rusqlite::Result<StoredVersion>>,
{
    rows.map(|r| version(r?)).collect()
}
fn current_tx(tx: &Transaction<'_>, p: &str) -> Result<Option<FileVersion>, FileStoreError> {
    let x=tx.query_row("SELECT id,path,digest,label,previous,edit_dependency,dispatch FROM versions WHERE path=?1 ORDER BY id DESC LIMIT 1",[p],decode_version).optional()?;
    x.map(version).transpose()
}
fn current_connection(c: &Connection, p: &str) -> Result<Option<FileVersion>, FileStoreError> {
    let x=c.query_row("SELECT id,path,digest,label,previous,edit_dependency,dispatch FROM versions WHERE path=?1 ORDER BY id DESC LIMIT 1",[p],decode_version).optional()?;
    x.map(version).transpose()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tempfile::TempDir;

    struct Fixture {
        _root: TempDir,
        workspace: PathBuf,
        db: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let workspace = root.path().join("workspace");
            fs::create_dir(&workspace).unwrap();
            fs::write(workspace.join("tracked.txt"), "old").unwrap();
            let db = root.path().join("state").join("files.sqlite");
            fs::create_dir(db.parent().unwrap()).unwrap();
            Self {
                _root: root,
                workspace,
                db,
            }
        }

        fn initialize(&self, label: &Label) -> FileStore {
            FileStore::initialize(&self.db, &self.workspace, "policy-a", label).unwrap()
        }
    }

    #[test]
    fn reopening_preserves_labels_and_excludes_other_connections() {
        let fixture = Fixture::new();
        let initial = Label::top();
        let first = fixture.initialize(&initial);
        first.prepare("a", "one", FileOperation::Read, "tracked.txt").unwrap();

        let second = FileStore::open(&fixture.db, &fixture.workspace, "policy-a", &initial).unwrap();
        assert!(matches!(
            second.prepare("b", "two", FileOperation::Read, "tracked.txt"),
            Err(FileStoreError::Pending)
        ));
        drop(first);
        assert_eq!(second.current("tracked.txt").unwrap().unwrap().label, initial);
        assert!(matches!(
            FileStore::open(&fixture.db, &fixture.workspace, "policy-b", &Label::top()),
            Err(FileStoreError::BindingMismatch)
        ));
        let other = fixture._root.path().join("other");
        fs::create_dir(&other).unwrap();
        assert!(matches!(
            FileStore::open(&fixture.db, &other, "policy-a", &Label::top()),
            Err(FileStoreError::BindingMismatch)
        ));
    }

    #[test]
    fn replace_uses_bound_label_and_edit_records_dependency() {
        let fixture = Fixture::new();
        let store = fixture.initialize(&Label::top());
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
        assert_eq!(replaced.version.as_ref().unwrap().edit_dependency, None);
        assert_eq!(store.finish("a", "replace", true).unwrap(), replaced);

        let pin = store.prepare("a", "edit", FileOperation::Edit, "tracked.txt").unwrap();
        store.bind("a", "edit", "dispatch-2", &Label::top()).unwrap();
        fs::write(fixture.workspace.join("tracked.txt"), "edited").unwrap();
        let edited = store.finish("a", "edit", true).unwrap();
        assert_eq!(edited.version.unwrap().edit_dependency, pin.predecessor_version);
        assert_eq!(store.history("tracked.txt").unwrap().len(), 3);
    }

    #[test]
    fn mismatch_and_failed_partial_write_stay_pending() {
        let fixture = Fixture::new();
        let store = fixture.initialize(&Label::top());
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
        assert!(matches!(
            FileStore::open(&fixture.db, &fixture.workspace, "policy-a", &Label::top()),
            Err(FileStoreError::Uninitialized)
        ));
        let store = fixture.initialize(&Label::top());
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
}
