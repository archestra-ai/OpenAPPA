//! The durable fact log: SQLite, append-only batches with a
//! revision per family, plus runtime records.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::api::{DispatchId, OfferId, TrajectoryId};

/// The count of writes to one family log. The store's own
/// numbering; the api layer converts it to the boundary's revision
/// type, so this module never names the engine boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Revision(pub u64);

/// One fact batch to append: opaque bytes plus the revision the
/// decision was based on.
#[derive(Debug, Clone, PartialEq)]
pub struct BatchAppend {
    pub bytes: Vec<u8>,
    pub based_on: Revision,
}

/// A runtime record written in the same transaction as the event's
/// batch, so the log and the runtime's own state can never disagree.
#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeRecord {
    Request { trajectory: TrajectoryId, text: String },
    OpenChild { id: TrajectoryId, parent: TrajectoryId },
    End { id: TrajectoryId },
    OpenDispatch {
        id: DispatchId,
        trajectory: TrajectoryId,
        tool: String,
        bytes: Vec<u8>,
        state: DispatchState,
    },
    PromoteDispatch { id: DispatchId },
    CloseDispatch { id: DispatchId },
    SurfaceOffer { id: OfferId, trajectory: TrajectoryId },
}

/// Where a dispatch stands. `Awaiting`: authorized, waiting for the
/// model to re-propose the call. `Executing`: released to the harness,
/// waiting for its outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchState {
    Awaiting,
    Executing,
}

impl DispatchState {
    fn as_sql(self) -> &'static str {
        match self {
            DispatchState::Awaiting => "awaiting",
            DispatchState::Executing => "executing",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DispatchRow {
    pub id: DispatchId,
    pub tool: String,
    pub bytes: Vec<u8>,
    pub state: DispatchState,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct EventWrite {
    pub batch: Option<BatchAppend>,
    pub records: Vec<RuntimeRecord>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TrajectoryRow {
    pub id: TrajectoryId,
    pub family: TrajectoryId,
    pub parent: Option<TrajectoryId>,
    pub ended: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("the database at {path} is damaged: {detail}")]
    Damaged { path: String, detail: String },
    #[error("no trajectory family {family} exists")]
    UnknownFamily { family: String },
    #[error("the database belongs to policy digest {stored}, not {supplied}")]
    PolicyMismatch { stored: String, supplied: String },
    #[error("storage failure: {0}")]
    Storage(#[from] rusqlite::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    #[error("trajectory id already exists")]
    AlreadyExists,
    #[error("storage failure: {0}")]
    Storage(#[from] rusqlite::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum CommitError {
    #[error("stale revision: the log is at {current:?}")]
    Conflict { current: Revision },
    #[error("invalid runtime record: {detail}")]
    InvalidRecord { detail: String },
    #[error("the trajectory already has an open dispatch")]
    DispatchAlreadyOpen,
    #[error("a trajectory with this id already exists")]
    TrajectoryExists,
    #[error("storage failure: {0}")]
    Storage(#[from] rusqlite::Error),
    #[cfg(test)]
    #[error("injected failure before commit")]
    Injected,
}

/// The store. One SQLite file per process; WAL with `synchronous=FULL`
/// so a committed event survives a crash (spec §14.1 accepts the
/// window between a tool invocation and its write).
pub struct Store {
    conn: Mutex<Connection>,
    #[cfg(test)]
    fail_before_commit: std::sync::atomic::AtomicBool,
}

impl Store {
    pub fn open(path: &Path) -> Result<Store, StoreError> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let check: String = conn.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        if check != "ok" {
            return Err(StoreError::Damaged {
                path: path.display().to_string(),
                detail: check,
            });
        }
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS batches (
                 family TEXT NOT NULL,
                 seq    INTEGER NOT NULL,
                 bytes  BLOB NOT NULL,
                 PRIMARY KEY (family, seq)
             );
             CREATE TABLE IF NOT EXISTS trajectories (
                 id     TEXT PRIMARY KEY,
                 family TEXT NOT NULL,
                 parent TEXT,
                 ended  INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS requests (
                 family     TEXT NOT NULL,
                 trajectory TEXT NOT NULL,
                 text       TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS dispatches (
                 id         TEXT PRIMARY KEY,
                 trajectory TEXT NOT NULL,
                 tool       TEXT NOT NULL,
                 bytes      BLOB NOT NULL,
                 state      TEXT NOT NULL
                     CHECK (state IN ('awaiting', 'executing', 'closed'))
             );
             CREATE TABLE IF NOT EXISTS offers (
                 id         TEXT PRIMARY KEY,
                 trajectory TEXT NOT NULL
             );
             CREATE UNIQUE INDEX IF NOT EXISTS one_open_dispatch
                 ON dispatches (trajectory) WHERE state != 'closed';
             CREATE TABLE IF NOT EXISTS deployment (
                 key   TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );",
        )?;
        Ok(Store {
            conn: Mutex::new(conn),
            #[cfg(test)]
            fail_before_commit: std::sync::atomic::AtomicBool::new(false),
        })
    }

    pub fn create_root(&self, id: &TrajectoryId) -> Result<(), CreateError> {
        let conn = self.lock();
        insert_trajectory(&conn, id, id, None)
    }

    /// The whole family log in append order, with its revision. The
    /// engine rebuilds its view from this; the store never interprets
    /// the bytes.
    pub fn load_log(&self, family: &TrajectoryId) -> Result<(Vec<Vec<u8>>, Revision), StoreError> {
        let conn = self.lock();
        let root: Option<String> = conn
            .query_row("SELECT id FROM trajectories WHERE id = ?1", params![family.0], |row| {
                row.get(0)
            })
            .optional()?;
        if root.is_none() {
            return Err(StoreError::UnknownFamily {
                family: family.0.clone(),
            });
        }
        let mut stmt = conn.prepare("SELECT bytes FROM batches WHERE family = ?1 ORDER BY seq ASC")?;
        let batches = stmt
            .query_map(params![family.0], |row| row.get::<_, Vec<u8>>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let revision = Revision(batches.len() as u64);
        Ok((batches, revision))
    }

    pub fn trajectory(&self, id: &TrajectoryId) -> Result<Option<TrajectoryRow>, StoreError> {
        let conn = self.lock();
        let row = conn
            .query_row(
                "SELECT id, family, parent, ended FROM trajectories WHERE id = ?1",
                params![id.0],
                |row| {
                    Ok(TrajectoryRow {
                        id: TrajectoryId(row.get(0)?),
                        family: TrajectoryId(row.get(1)?),
                        parent: row.get::<_, Option<String>>(2)?.map(TrajectoryId),
                        ended: row.get::<_, i64>(3)? != 0,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// The one dispatch this trajectory has open, if any. One call in
    /// flight: a second proposal while one is open is refused.
    pub fn open_dispatch(&self, trajectory: &TrajectoryId) -> Result<Option<DispatchRow>, StoreError> {
        let conn = self.lock();
        let row = conn
            .query_row(
                "SELECT id, tool, bytes, state FROM dispatches
                 WHERE trajectory = ?1 AND state != 'closed'",
                params![trajectory.0],
                |row| {
                    let state: String = row.get(3)?;
                    Ok(DispatchRow {
                        id: DispatchId(row.get(0)?),
                        tool: row.get(1)?,
                        bytes: row.get(2)?,
                        state: if state == "awaiting" {
                            DispatchState::Awaiting
                        } else {
                            DispatchState::Executing
                        },
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Which trajectory an offer id routes to. Routing only: whether
    /// the offer still stands is the engine's judgment.
    pub fn offer_trajectory(&self, offer: &OfferId) -> Result<Option<TrajectoryId>, StoreError> {
        let conn = self.lock();
        let row = conn
            .query_row("SELECT trajectory FROM offers WHERE id = ?1", params![offer.0], |row| {
                Ok(TrajectoryId(row.get(0)?))
            })
            .optional()?;
        Ok(row)
    }

    /// Commit one event's writes in one transaction: the batch append
    /// (compare-and-swap on the revision) and every runtime record, or
    /// nothing. The caller answers the hook only after this returns.
    pub fn commit_event(&self, family: &TrajectoryId, write: EventWrite) -> Result<Revision, CommitError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let mut revision = current_revision(&tx, family)?;
        if let Some(batch) = write.batch {
            if batch.based_on != revision {
                return Err(CommitError::Conflict { current: revision });
            }
            tx.execute(
                "INSERT INTO batches (family, seq, bytes) VALUES (?1, ?2, ?3)",
                params![family.0, revision.0 as i64, batch.bytes],
            )?;
            revision = Revision(revision.0 + 1);
        }
        for record in write.records {
            match record {
                RuntimeRecord::Request { trajectory, text } => {
                    require_member(&tx, family, &trajectory)?;
                    tx.execute(
                        "INSERT INTO requests (family, trajectory, text) VALUES (?1, ?2, ?3)",
                        params![family.0, trajectory.0, text],
                    )?;
                }
                RuntimeRecord::OpenChild { id, parent } => {
                    require_member(&tx, family, &parent)?;
                    match insert_trajectory_tx(&tx, &id, family, Some(&parent)) {
                        Ok(()) => {}
                        Err(rusqlite::Error::SqliteFailure(e, _))
                            if e.extended_code == SQLITE_CONSTRAINT_PRIMARYKEY
                                || e.extended_code == SQLITE_CONSTRAINT_UNIQUE =>
                        {
                            return Err(CommitError::TrajectoryExists);
                        }
                        Err(e) => return Err(CommitError::Storage(e)),
                    }
                }
                RuntimeRecord::End { id } => {
                    require_member(&tx, family, &id)?;
                    let changed = tx.execute("UPDATE trajectories SET ended = 1 WHERE id = ?1", params![id.0])?;
                    require_one_row(changed, "End", &id.0)?;
                }
                RuntimeRecord::OpenDispatch {
                    id,
                    trajectory,
                    tool,
                    bytes,
                    state,
                } => {
                    require_member(&tx, family, &trajectory)?;
                    match tx.execute(
                        "INSERT INTO dispatches (id, trajectory, tool, bytes, state)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![id.0, trajectory.0, tool, bytes, state.as_sql()],
                    ) {
                        Ok(_) => {}
                        Err(rusqlite::Error::SqliteFailure(e, _))
                            if e.extended_code == SQLITE_CONSTRAINT_UNIQUE
                                || e.extended_code == SQLITE_CONSTRAINT_PRIMARYKEY =>
                        {
                            return Err(CommitError::DispatchAlreadyOpen);
                        }
                        Err(e) => return Err(CommitError::Storage(e)),
                    }
                }
                RuntimeRecord::PromoteDispatch { id } => {
                    require_dispatch_member(&tx, family, &id)?;
                    let changed = tx.execute(
                        "UPDATE dispatches SET state = 'executing'
                         WHERE id = ?1 AND state = 'awaiting'",
                        params![id.0],
                    )?;
                    if changed != 1 {
                        return Err(CommitError::DispatchAlreadyOpen);
                    }
                }
                RuntimeRecord::CloseDispatch { id } => {
                    require_dispatch_member(&tx, family, &id)?;
                    let changed = tx.execute(
                        "UPDATE dispatches SET state = 'closed'
                         WHERE id = ?1 AND state != 'closed'",
                        params![id.0],
                    )?;
                    require_one_row(changed, "CloseDispatch", &id.0)?;
                }
                RuntimeRecord::SurfaceOffer { id, trajectory } => {
                    require_member(&tx, family, &trajectory)?;
                    tx.execute(
                        "INSERT INTO offers (id, trajectory) VALUES (?1, ?2)",
                        params![id.0, trajectory.0],
                    )?;
                }
            }
        }

        #[cfg(test)]
        if self.fail_before_commit.swap(false, std::sync::atomic::Ordering::SeqCst) {
            return Err(CommitError::Injected);
        }

        tx.commit()?;
        Ok(revision)
    }

    /// Record the deployment's policy digest on first open; on every
    /// later open, refuse a database that was opened under a different
    /// policy. A policy change opens a new deployment, never continues
    /// an old database.
    pub fn bind_policy_digest(&self, digest: &str) -> Result<(), StoreError> {
        let conn = self.lock();
        let stored: Option<String> = conn
            .query_row("SELECT value FROM deployment WHERE key = 'policy_digest'", [], |row| {
                row.get(0)
            })
            .optional()?;
        match stored {
            None => {
                conn.execute(
                    "INSERT INTO deployment (key, value) VALUES ('policy_digest', ?1)",
                    params![digest],
                )?;
                Ok(())
            }
            Some(stored) if stored == digest => Ok(()),
            Some(stored) => Err(StoreError::PolicyMismatch {
                stored,
                supplied: digest.to_string(),
            }),
        }
    }

    #[cfg(test)]
    pub fn request_texts(&self, family: &TrajectoryId) -> Vec<String> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT text FROM requests WHERE family = ?1 ORDER BY rowid ASC")
            .expect("the requests query prepares");
        stmt.query_map(params![family.0], |row| row.get::<_, String>(0))
            .expect("the requests query runs")
            .collect::<Result<Vec<_>, _>>()
            .expect("the request rows read")
    }

    /// Arm the fail point: the next `commit_event` rolls back in place
    /// of committing, as a process kill inside the transaction would.
    #[cfg(test)]
    pub fn fail_next_commit(&self) {
        self.fail_before_commit.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .expect("the store mutex is never poisoned: no panics under the lock")
    }
}

fn current_revision(conn: &Connection, family: &TrajectoryId) -> Result<Revision, rusqlite::Error> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM batches WHERE family = ?1",
        params![family.0],
        |row| row.get(0),
    )?;
    Ok(Revision(count as u64))
}

fn require_member(
    tx: &rusqlite::Transaction<'_>,
    family: &TrajectoryId,
    trajectory: &TrajectoryId,
) -> Result<(), CommitError> {
    let found: Option<String> = tx
        .query_row(
            "SELECT family FROM trajectories WHERE id = ?1",
            params![trajectory.0],
            |row| row.get(0),
        )
        .optional()
        .map_err(CommitError::Storage)?;
    match found {
        Some(f) if f == family.0 => Ok(()),
        Some(f) => Err(CommitError::InvalidRecord {
            detail: format!("trajectory {} belongs to family {f}, not {}", trajectory.0, family.0),
        }),
        None => Err(CommitError::InvalidRecord {
            detail: format!("trajectory {} does not exist", trajectory.0),
        }),
    }
}

fn require_dispatch_member(
    tx: &rusqlite::Transaction<'_>,
    family: &TrajectoryId,
    dispatch: &DispatchId,
) -> Result<(), CommitError> {
    let owner: Option<String> = tx
        .query_row(
            "SELECT trajectory FROM dispatches WHERE id = ?1",
            params![dispatch.0],
            |row| row.get(0),
        )
        .optional()
        .map_err(CommitError::Storage)?;
    match owner {
        Some(owner) => require_member(tx, family, &TrajectoryId(owner)),
        None => Err(CommitError::InvalidRecord {
            detail: format!("dispatch {} does not exist", dispatch.0),
        }),
    }
}

fn require_one_row(changed: usize, record: &str, id: &str) -> Result<(), CommitError> {
    if changed == 1 {
        Ok(())
    } else {
        Err(CommitError::InvalidRecord {
            detail: format!("{record} {id} matched {changed} rows, expected exactly one"),
        })
    }
}

const SQLITE_CONSTRAINT_PRIMARYKEY: i32 = 1555;
const SQLITE_CONSTRAINT_UNIQUE: i32 = 2067;

fn insert_trajectory(
    conn: &Connection,
    id: &TrajectoryId,
    family: &TrajectoryId,
    parent: Option<&TrajectoryId>,
) -> Result<(), CreateError> {
    match conn.execute(
        "INSERT INTO trajectories (id, family, parent) VALUES (?1, ?2, ?3)",
        params![id.0, family.0, parent.map(|p| p.0.clone())],
    ) {
        Ok(_) => Ok(()),
        Err(rusqlite::Error::SqliteFailure(e, _))
            if e.extended_code == SQLITE_CONSTRAINT_PRIMARYKEY || e.extended_code == SQLITE_CONSTRAINT_UNIQUE =>
        {
            Err(CreateError::AlreadyExists)
        }
        Err(e) => Err(CreateError::Storage(e)),
    }
}

fn insert_trajectory_tx(
    tx: &rusqlite::Transaction<'_>,
    id: &TrajectoryId,
    family: &TrajectoryId,
    parent: Option<&TrajectoryId>,
) -> Result<(), rusqlite::Error> {
    tx.execute(
        "INSERT INTO trajectories (id, family, parent) VALUES (?1, ?2, ?3)",
        params![id.0, family.0, parent.map(|p| p.0.clone())],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_temp() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let store = Store::open(&dir.path().join("appa.db")).expect("a fresh store opens");
        (dir, store)
    }

    fn family() -> TrajectoryId {
        TrajectoryId("cc:root".to_string())
    }

    #[test]
    fn appends_advance_the_revision_and_reload_in_order() {
        let (_dir, store) = open_temp();
        let f = family();
        store.create_root(&f).expect("a fresh root is creatable");

        let r1 = store
            .commit_event(
                &f,
                EventWrite {
                    batch: Some(BatchAppend {
                        bytes: b"b0".to_vec(),
                        based_on: Revision(0),
                    }),
                    records: vec![],
                },
            )
            .expect("an append at the current revision commits");
        assert_eq!(r1, Revision(1));

        let r2 = store
            .commit_event(
                &f,
                EventWrite {
                    batch: Some(BatchAppend {
                        bytes: b"b1".to_vec(),
                        based_on: Revision(1),
                    }),
                    records: vec![],
                },
            )
            .expect("a second append at the advanced revision commits");
        assert_eq!(r2, Revision(2));

        let (log, revision) = store.load_log(&f).expect("the log loads");
        assert_eq!(log, vec![b"b0".to_vec(), b"b1".to_vec()]);
        assert_eq!(revision, Revision(2));
    }

    #[test]
    fn a_stale_basis_conflicts_and_writes_nothing() {
        let (_dir, store) = open_temp();
        let f = family();
        store.create_root(&f).expect("a fresh root is creatable");
        store
            .commit_event(
                &f,
                EventWrite {
                    batch: Some(BatchAppend {
                        bytes: b"b0".to_vec(),
                        based_on: Revision(0),
                    }),
                    records: vec![],
                },
            )
            .expect("the first append commits");

        let stale = store.commit_event(
            &f,
            EventWrite {
                batch: Some(BatchAppend {
                    bytes: b"stale".to_vec(),
                    based_on: Revision(0),
                }),
                records: vec![RuntimeRecord::Request {
                    trajectory: f.clone(),
                    text: "prompt".to_string(),
                }],
            },
        );
        match stale {
            Err(CommitError::Conflict { current }) => assert_eq!(current, Revision(1)),
            other => panic!("expected a stale-revision conflict, got {other:?}"),
        }

        let (log, revision) = store.load_log(&f).expect("the log loads");
        assert_eq!(log.len(), 1);
        assert_eq!(revision, Revision(1));
        assert!(store.request_texts(&f).is_empty());
    }

    #[test]
    fn a_reused_trajectory_id_is_refused() {
        let (_dir, store) = open_temp();
        let f = family();
        store.create_root(&f).expect("a fresh root is creatable");
        match store.create_root(&f) {
            Err(CreateError::AlreadyExists) => {}
            other => panic!("expected AlreadyExists, got {other:?}"),
        }
    }

    #[test]
    fn a_child_opens_in_the_parents_family_and_ends_atomically() {
        let (_dir, store) = open_temp();
        let f = family();
        let child = TrajectoryId("cc:child".to_string());
        store.create_root(&f).expect("a fresh root is creatable");

        store
            .commit_event(
                &f,
                EventWrite {
                    batch: None,
                    records: vec![RuntimeRecord::OpenChild {
                        id: child.clone(),
                        parent: f.clone(),
                    }],
                },
            )
            .expect("opening a child commits");
        let row = store
            .trajectory(&child)
            .expect("the row loads")
            .expect("the child row exists");
        assert_eq!(row.family, f);
        assert_eq!(row.parent, Some(f.clone()));
        assert!(!row.ended);

        store
            .commit_event(
                &f,
                EventWrite {
                    batch: Some(BatchAppend {
                        bytes: b"return".to_vec(),
                        based_on: Revision(0),
                    }),
                    records: vec![RuntimeRecord::End { id: child.clone() }],
                },
            )
            .expect("the child's return commits");
        let row = store
            .trajectory(&child)
            .expect("the row loads")
            .expect("the child row exists");
        assert!(row.ended);
    }

    #[test]
    fn the_fail_point_rolls_back_batch_and_records_together() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.db");
        let f = family();
        {
            let store = Store::open(&path).expect("a fresh store opens");
            store.create_root(&f).expect("a fresh root is creatable");
            store.fail_next_commit();
            let killed = store.commit_event(
                &f,
                EventWrite {
                    batch: Some(BatchAppend {
                        bytes: b"lost".to_vec(),
                        based_on: Revision(0),
                    }),
                    records: vec![RuntimeRecord::Request {
                        trajectory: f.clone(),
                        text: "lost".to_string(),
                    }],
                },
            );
            assert!(matches!(killed, Err(CommitError::Injected)));
        }

        let store = Store::open(&path).expect("the store reopens");
        let (log, revision) = store.load_log(&f).expect("the log loads");
        assert!(log.is_empty());
        assert_eq!(revision, Revision(0));
        assert!(store.request_texts(&f).is_empty());
        store
            .commit_event(
                &f,
                EventWrite {
                    batch: Some(BatchAppend {
                        bytes: b"b0".to_vec(),
                        based_on: Revision(0),
                    }),
                    records: vec![],
                },
            )
            .expect("the replayed event commits from revision 0");
    }

    #[test]
    fn committed_state_survives_reopen() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.db");
        let f = family();
        {
            let store = Store::open(&path).expect("a fresh store opens");
            store.create_root(&f).expect("a fresh root is creatable");
            store
                .commit_event(
                    &f,
                    EventWrite {
                        batch: Some(BatchAppend {
                            bytes: b"b0".to_vec(),
                            based_on: Revision(0),
                        }),
                        records: vec![],
                    },
                )
                .expect("the append commits");
        }
        let store = Store::open(&path).expect("the store reopens");
        let (log, revision) = store.load_log(&f).expect("the log loads");
        assert_eq!(log, vec![b"b0".to_vec()]);
        assert_eq!(revision, Revision(1));
        assert!(
            store
                .trajectory(&f)
                .expect("the row loads")
                .is_some_and(|row| !row.ended)
        );
    }

    #[test]
    fn a_record_naming_another_family_is_refused_and_writes_nothing() {
        let (_dir, store) = open_temp();
        let a = TrajectoryId("cc:family-a".to_string());
        let b = TrajectoryId("cc:family-b".to_string());
        store.create_root(&a).expect("family a opens");
        store.create_root(&b).expect("family b opens");

        let crossed = store.commit_event(
            &a,
            EventWrite {
                batch: Some(BatchAppend {
                    bytes: b"facts".to_vec(),
                    based_on: Revision(0),
                }),
                records: vec![RuntimeRecord::End { id: b.clone() }],
            },
        );
        assert!(matches!(crossed, Err(CommitError::InvalidRecord { .. })));
        let (log, _) = store.load_log(&a).expect("the log loads");
        assert!(log.is_empty(), "the batch must roll back with the refused record");
        assert!(
            store
                .trajectory(&b)
                .expect("the row loads")
                .is_some_and(|row| !row.ended)
        );

        let ghost = store.commit_event(
            &a,
            EventWrite {
                batch: None,
                records: vec![RuntimeRecord::Request {
                    trajectory: TrajectoryId("cc:ghost".to_string()),
                    text: "lost".to_string(),
                }],
            },
        );
        assert!(matches!(ghost, Err(CommitError::InvalidRecord { .. })));
    }

    #[test]
    fn an_unknown_family_does_not_rebuild_as_fresh_state() {
        let (_dir, store) = open_temp();
        assert!(matches!(
            store.load_log(&TrajectoryId("cc:ghost".to_string())),
            Err(StoreError::UnknownFamily { .. }),
        ));
    }

    #[test]
    fn concurrent_appends_conflict_across_connections() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.db");
        let f = family();
        let first = Store::open(&path).expect("the first connection opens");
        first.create_root(&f).expect("a fresh root is creatable");
        let second = Store::open(&path).expect("the second connection opens");

        first
            .commit_event(
                &f,
                EventWrite {
                    batch: Some(BatchAppend {
                        bytes: b"first".to_vec(),
                        based_on: Revision(0),
                    }),
                    records: vec![],
                },
            )
            .expect("the first append commits");
        let stale = second.commit_event(
            &f,
            EventWrite {
                batch: Some(BatchAppend {
                    bytes: b"second".to_vec(),
                    based_on: Revision(0),
                }),
                records: vec![],
            },
        );
        match stale {
            Err(CommitError::Conflict { current }) => assert_eq!(current, Revision(1)),
            other => panic!("expected a stale-revision conflict, got {other:?}"),
        }
        second
            .commit_event(
                &f,
                EventWrite {
                    batch: Some(BatchAppend {
                        bytes: b"second".to_vec(),
                        based_on: Revision(1),
                    }),
                    records: vec![],
                },
            )
            .expect("the replayed append commits");
        let (log, _) = first.load_log(&f).expect("the log loads");
        assert_eq!(log, vec![b"first".to_vec(), b"second".to_vec()]);
    }

    #[test]
    fn a_second_open_dispatch_for_one_trajectory_is_refused() {
        let (_dir, store) = open_temp();
        let f = family();
        store.create_root(&f).expect("a fresh root is creatable");
        let open_record = |id: &str| RuntimeRecord::OpenDispatch {
            id: DispatchId(id.to_string()),
            trajectory: f.clone(),
            tool: "Bash".to_string(),
            bytes: b"call".to_vec(),
            state: DispatchState::Executing,
        };
        store
            .commit_event(
                &f,
                EventWrite {
                    batch: None,
                    records: vec![open_record("d1")],
                },
            )
            .expect("the first dispatch opens");
        assert!(matches!(
            store.commit_event(
                &f,
                EventWrite {
                    batch: None,
                    records: vec![open_record("d2")]
                }
            ),
            Err(CommitError::DispatchAlreadyOpen),
        ));
    }

    #[test]
    fn promote_and_close_refuse_a_foreign_familys_dispatch() {
        let (_dir, store) = open_temp();
        let a = TrajectoryId("cc:family-a".to_string());
        let b = TrajectoryId("cc:family-b".to_string());
        store.create_root(&a).expect("family a opens");
        store.create_root(&b).expect("family b opens");
        store
            .commit_event(
                &b,
                EventWrite {
                    batch: None,
                    records: vec![RuntimeRecord::OpenDispatch {
                        id: DispatchId("d-b".to_string()),
                        trajectory: b.clone(),
                        tool: "Bash".to_string(),
                        bytes: b"call".to_vec(),
                        state: DispatchState::Awaiting,
                    }],
                },
            )
            .expect("family b's dispatch opens");
        assert!(matches!(
            store.commit_event(
                &a,
                EventWrite {
                    batch: None,
                    records: vec![RuntimeRecord::PromoteDispatch {
                        id: DispatchId("d-b".to_string())
                    }],
                },
            ),
            Err(CommitError::InvalidRecord { .. }),
        ));
        assert!(matches!(
            store.commit_event(
                &a,
                EventWrite {
                    batch: None,
                    records: vec![RuntimeRecord::CloseDispatch {
                        id: DispatchId("d-b".to_string())
                    }],
                },
            ),
            Err(CommitError::InvalidRecord { .. }),
        ));
    }

    #[test]
    fn a_changed_policy_digest_is_refused() {
        let (_dir, store) = open_temp();
        store.bind_policy_digest("digest-one").expect("the first open binds");
        store.bind_policy_digest("digest-one").expect("the same policy reopens");
        assert!(matches!(
            store.bind_policy_digest("digest-two"),
            Err(StoreError::PolicyMismatch { .. }),
        ));
    }

    #[test]
    fn a_damaged_file_is_refused() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.db");
        std::fs::write(&path, b"not a sqlite database at all").expect("the file writes");
        assert!(Store::open(&path).is_err(), "a damaged database must be refused");
    }
}
