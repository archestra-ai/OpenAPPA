//! # appa-eventlog — the trajectory log, and where it is kept
//!
//! A root trajectory and its branches append to one shared log. That log holds
//! every lasting fact of the system; the stored policy files are the only other durable state, and
//! everything else — a branch's parent, whether it has ended, which dispatch is open, whether an
//! offer still stands — is read back from the log by the engine's projection.
//!
//! This crate is where the log is written and read. The record encoding, the database, and the
//! conditional append are private to it: a caller hands it [`Fact`]s and gets [`Log`]s back, and
//! never names SQL, a row, or a byte. Where the log is kept is the closed [`Backend`] enum,
//! dispatched by `match` — no trait, because two SQLite connection modes are not two
//! implementations.
//!
//! Two tables, and no derived state:
//!
//! - the log itself, one row per appended batch, keyed by the root trajectory;
//! - the stored policy files, content addressed by the SHA-256 of their exact bytes, write-once
//!   and shared by every root that opened under them.
//!
//! There is no index from a branch to its root. Every caller already knows the root: a harness
//! event names it, and a surfaced offer's identity carries it. An index would be a third place
//! for the truth to live, and this crate has none.
//!
//! ## The compare-and-swap is a value, not a number
//!
//! An append is accepted only if the log still stands where the decision was computed. Here
//! that position is not a number a caller supplies but the [`Log`] it read: [`LogStore::append`]
//! takes the very value the decision was made against. [`Log`] has no public constructor, so a
//! basis cannot be forged, and appending against a position that was never read cannot be
//! written down.
//!
//! ## What this crate does not do
//!
//! It never judges. It stores what the engine produced and returns what it stored. A log whose
//! records do not form a legal history is refused by the engine's transition validator when the
//! log is next read, not here: serialization removes the in-process seal, and
//! re-validation on read is the gate.

use std::path::PathBuf;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

pub use appa_engine::fact::Fact;
use appa_engine::profile::PolicyFileKey;
use appa_engine::value::TrajectoryId;

const SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    Sqlite {
        path: PathBuf,
    },
    /// Private to one [`LogStore`] and gone when it drops. An in-memory adapter sits
    /// beside the durable one deliberately: the decision core cannot tell them apart.
    Memory,
}

pub struct LogStore {
    connection: Mutex<Connection>,
    #[cfg(feature = "fault-injection")]
    commits_until_failure: std::sync::atomic::AtomicU64,
    #[cfg(feature = "fault-injection")]
    contended_appends: std::sync::atomic::AtomicU64,
}

/// The records of one read, and the position they were read at.
#[derive(Debug, Clone, PartialEq)]
pub struct Log {
    root: TrajectoryId,
    facts: Vec<Fact>,
    basis: u64,
    policy_file: Vec<u8>,
}

impl Log {
    pub fn root(&self) -> &TrajectoryId {
        &self.root
    }

    pub fn facts(&self) -> &[Fact] {
        &self.facts
    }

    /// The count of accepted batches this read stands at — the compare-and-swap position,
    /// never a count of facts.
    pub fn basis(&self) -> u64 {
        self.basis
    }

    pub fn policy_file(&self) -> &[u8] {
        &self.policy_file
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("the database at {path} is damaged: {detail}")]
    Damaged { path: String, detail: String },
    #[error("the database at {path} is at schema version {found}, and this build writes {expected}")]
    ForeignSchema { path: String, found: i64, expected: i64 },
    #[error("storage failure: {0}")]
    Storage(#[from] rusqlite::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    #[error("a log for root {root} already exists")]
    AlreadyExists { root: String },
    #[error("the opening batch is not usable as one: {detail}")]
    Malformed { detail: String },
    /// The supplied file is not the one the opening record names. The opening carries the
    /// exact-bytes key, so storing other bytes beside it would leave a root bound to a file it
    /// never opened under.
    #[error("the supplied policy file does not hash to the key the opening names")]
    PolicyFileMismatch,
    #[error("storage failure: {0}")]
    Storage(#[from] rusqlite::Error),
    #[cfg(feature = "fault-injection")]
    #[error("injected failure before commit")]
    Injected,
}

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("no log for root {root} exists")]
    UnknownRoot { root: String },
    #[error("the stored policy file {key} is missing")]
    PolicyFileMissing { key: String },
    #[error("a stored batch does not decode: {0}")]
    Undecodable(String),
    #[error("storage failure: {0}")]
    Storage(#[from] rusqlite::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum AppendError {
    #[error("the log is at {current}, not the position this decision was read at")]
    Conflict { current: u64 },
    #[error("storage failure: {0}")]
    Storage(#[from] rusqlite::Error),
    #[cfg(feature = "fault-injection")]
    #[error("injected failure before commit")]
    Injected,
}

impl LogStore {
    /// Open the log. A fresh database gets the schema and its version stamp; an existing one is
    /// checked for damage and for a version this build understands, and refused otherwise.
    pub fn open(backend: Backend) -> Result<LogStore, OpenError> {
        let (connection, path) = match &backend {
            Backend::Sqlite { path } => (Connection::open(path)?, path.display().to_string()),
            Backend::Memory => (Connection::open_in_memory()?, ":memory:".to_string()),
        };
        if matches!(backend, Backend::Sqlite { .. }) {
            let probe = || -> Result<String, rusqlite::Error> {
                connection.busy_timeout(std::time::Duration::from_secs(5))?;
                connection.pragma_update(None, "journal_mode", "WAL")?;
                connection.pragma_update(None, "synchronous", "FULL")?;
                connection.query_row("PRAGMA quick_check", [], |row| row.get(0))
            };
            let check = probe().map_err(|error| OpenError::Damaged {
                path: path.clone(),
                detail: error.to_string(),
            })?;
            if check != "ok" {
                return Err(OpenError::Damaged { path, detail: check });
            }
        }
        connection.pragma_update(None, "foreign_keys", "ON")?;

        let mut connection = connection;
        {
            let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let version: i64 = transaction.query_row("PRAGMA user_version", [], |row| row.get(0))?;
            // Only an empty file is initialized. A database that holds tables
            // but carries no stamp was written by something else — an earlier
            // store, another tool — and creating this schema beside its data
            // would leave its histories present and invisible.
            if version == 0 && is_empty(&transaction)? {
                transaction.execute_batch(
                    "CREATE TABLE logs (
                         root  TEXT NOT NULL,
                         seq   INTEGER NOT NULL,
                         facts BLOB NOT NULL,
                         PRIMARY KEY (root, seq)
                     );
                     CREATE TABLE policy_files (
                         key   TEXT PRIMARY KEY,
                         bytes BLOB NOT NULL
                     );",
                )?;
                transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            } else if version != SCHEMA_VERSION || !has_schema(&transaction)? {
                return Err(OpenError::ForeignSchema {
                    path,
                    found: version,
                    expected: SCHEMA_VERSION,
                });
            }
            transaction.commit()?;
        }
        Ok(LogStore {
            connection: Mutex::new(connection),
            #[cfg(feature = "fault-injection")]
            commits_until_failure: std::sync::atomic::AtomicU64::new(0),
            #[cfg(feature = "fault-injection")]
            contended_appends: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// Open a root's log with the opening batch the engine sealed, and store the policy file it
    /// opens under. One transaction, so the opening is durable before any other record of that
    /// root or none is.
    pub fn create_root(&self, opening: Vec<Fact>, policy_file: &[u8]) -> Result<TrajectoryId, CreateError> {
        let (root, key) = opened_by(&opening)?;
        if PolicyFileKey::of(policy_file) != key {
            return Err(CreateError::PolicyFileMismatch);
        }
        let bytes = encode(&opening);
        let mut connection = self.lock();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO policy_files (key, bytes) VALUES (?1, ?2) ON CONFLICT (key) DO NOTHING",
            params![key.as_str(), policy_file],
        )?;
        match transaction.execute(
            "INSERT INTO logs (root, seq, facts) VALUES (?1, 0, ?2)",
            params![root.as_str(), bytes],
        ) {
            Ok(_) => {}
            Err(error) if is_taken(&error) => {
                return Err(CreateError::AlreadyExists {
                    root: root.as_str().to_string(),
                });
            }
            Err(error) => return Err(CreateError::Storage(error)),
        }
        #[cfg(feature = "fault-injection")]
        if self.failure_fires() {
            // Dropping the transaction rolls it back, exactly as a process kill before the
            // commit would leave the file.
            return Err(CreateError::Injected);
        }
        transaction.commit()?;
        Ok(root)
    }

    /// Whether this root has a log at all. The cheap question a caller asks before it decides
    /// to open one — reading the whole log to learn only this would cost the caller a second
    /// read on the path that then goes on to read it properly.
    pub fn has_root(&self, root: &TrajectoryId) -> Result<bool, ReadError> {
        let connection = self.lock();
        let found: Option<i64> = connection
            .query_row(
                "SELECT 1 FROM logs WHERE root = ?1 LIMIT 1",
                params![root.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }

    /// Read one root's whole log, with the position it stands at and the policy file it opened
    /// under.
    pub fn log(&self, root: &TrajectoryId) -> Result<Log, ReadError> {
        let (batches, policy_file) = {
            let connection = self.lock();
            stored(&connection, root)?
        };
        decoded(root, batches, policy_file)
    }

    /// Append records to the log `based_on` was read from, only if it still stands where that
    /// read left it. A conflict writes nothing; the caller reads again and replays.
    pub fn append(&self, based_on: &Log, facts: &[Fact]) -> Result<(), AppendError> {
        let bytes = encode(facts);
        let mut connection = self.lock();
        #[cfg(feature = "fault-injection")]
        if self.contention_fires() {
            // A foreign writer wins the race in its own committed transaction, exactly as a
            // second process would. It takes the position and records nothing, so this caller's
            // append conflicts on position and replays, and an assertion reads whose write landed
            // from the position rather than from records a later read would have to accept.
            let foreign = encode(&foreign_batch());
            let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let at = position(&transaction, &based_on.root)?;
            transaction.execute(
                "INSERT INTO logs (root, seq, facts) VALUES (?1, ?2, ?3)",
                params![based_on.root.as_str(), at as i64, foreign],
            )?;
            transaction.commit()?;
        }
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = position(&transaction, &based_on.root)?;
        if current != based_on.basis {
            return Err(AppendError::Conflict { current });
        }
        transaction.execute(
            "INSERT INTO logs (root, seq, facts) VALUES (?1, ?2, ?3)",
            params![based_on.root.as_str(), current as i64, bytes],
        )?;
        #[cfg(feature = "fault-injection")]
        if self.failure_fires() {
            return Err(AppendError::Injected);
        }
        transaction.commit()?;
        Ok(())
    }

    /// Arm the fail point: `skip` commits land normally and the one after them rolls back, as a
    /// process kill inside the transaction would.
    #[cfg(feature = "fault-injection")]
    pub fn fail_commit_after(&self, skip: u64) {
        self.commits_until_failure
            .store(skip + 1, std::sync::atomic::Ordering::SeqCst);
    }

    /// Arm the contention point: the next `count` appends are raced by a foreign writer that
    /// wins, so each loses the compare-and-swap and its caller replays.
    #[cfg(feature = "fault-injection")]
    pub fn contend_next_appends(&self, count: u64) {
        self.contended_appends.store(count, std::sync::atomic::Ordering::SeqCst);
    }

    /// Forget every stored policy file, leaving each root's opening naming a
    /// file this database no longer holds. Damage stated in this
    /// crate's own vocabulary, so a caller can pin how it refuses without
    /// learning the schema.
    #[cfg(feature = "fault-injection")]
    pub fn forget_policy_files(&self) {
        self.lock()
            .execute("DELETE FROM policy_files", [])
            .expect("the deletion runs");
    }

    /// Replace the bytes of every stored policy file, so each stops hashing to
    /// the key its roots' openings name.
    #[cfg(feature = "fault-injection")]
    pub fn corrupt_policy_files(&self, bytes: &[u8]) {
        self.lock()
            .execute("UPDATE policy_files SET bytes = ?1", params![bytes])
            .expect("the update runs");
    }

    /// Replace what one batch of a root's log holds. The bytes are stored as
    /// given, so a caller can leave records that do not decode, or records
    /// that decode but are not the history they claim to be.
    #[cfg(feature = "fault-injection")]
    pub fn corrupt_batch(&self, root: &TrajectoryId, seq: u64, bytes: &[u8]) {
        let changed = self
            .lock()
            .execute(
                "UPDATE logs SET facts = ?3 WHERE root = ?1 AND seq = ?2",
                params![root.as_str(), seq as i64, bytes],
            )
            .expect("the update runs");
        assert_eq!(changed, 1, "the batch to corrupt exists");
    }

    #[cfg(feature = "fault-injection")]
    fn failure_fires(&self) -> bool {
        consume(&self.commits_until_failure) == Some(1)
    }

    #[cfg(feature = "fault-injection")]
    fn contention_fires(&self) -> bool {
        consume(&self.contended_appends).is_some()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.connection
            .lock()
            .expect("the log store mutex is never poisoned: no panics under the lock")
    }
}

/// What a raced append's foreign writer records: nothing. A batch takes a log position whether or
/// not it holds facts, so the race is won — the caller's append conflicts and replays — without
/// minting a record every later read would then have to accept.
#[cfg(feature = "fault-injection")]
fn foreign_batch() -> Vec<Fact> {
    Vec::new()
}

#[cfg(feature = "fault-injection")]
fn consume(counter: &std::sync::atomic::AtomicU64) -> Option<u64> {
    use std::sync::atomic::Ordering::SeqCst;
    counter
        .fetch_update(SeqCst, SeqCst, |remaining| match remaining {
            0 => None,
            remaining => Some(remaining - 1),
        })
        .ok()
}

fn has_schema(connection: &Connection) -> Result<bool, rusqlite::Error> {
    let found: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('logs', 'policy_files')",
        [],
        |row| row.get(0),
    )?;
    Ok(found == 2)
}

fn is_empty(connection: &Connection) -> Result<bool, rusqlite::Error> {
    let tables: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get(0),
    )?;
    Ok(tables == 0)
}

fn opened_by(opening: &[Fact]) -> Result<(TrajectoryId, PolicyFileKey), CreateError> {
    match opening.first() {
        Some(Fact::TrajectoryOpened {
            trajectory,
            policy_file_key,
            ..
        }) => Ok((trajectory.clone(), policy_file_key.clone())),
        Some(_) => Err(CreateError::Malformed {
            detail: "the first record is not a TrajectoryOpened".to_string(),
        }),
        None => Err(CreateError::Malformed {
            detail: "the batch is empty".to_string(),
        }),
    }
}

fn position(connection: &Connection, root: &TrajectoryId) -> Result<u64, rusqlite::Error> {
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM logs WHERE root = ?1",
        params![root.as_str()],
        |row| row.get(0),
    )?;
    Ok(count as u64)
}

fn stored(connection: &Connection, root: &TrajectoryId) -> Result<(Vec<Vec<u8>>, Vec<u8>), ReadError> {
    let mut statement = connection.prepare("SELECT facts FROM logs WHERE root = ?1 ORDER BY seq ASC")?;
    let batches = statement
        .query_map(params![root.as_str()], |row| row.get::<_, Vec<u8>>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let Some(first) = batches.first() else {
        return Err(ReadError::UnknownRoot {
            root: root.as_str().to_string(),
        });
    };
    let key = match decode(first)?.first() {
        Some(Fact::TrajectoryOpened { policy_file_key, .. }) => policy_file_key.clone(),
        _ => {
            return Err(ReadError::Undecodable(
                "the log does not open with a TrajectoryOpened record".to_string(),
            ));
        }
    };
    let policy_file: Option<Vec<u8>> = connection
        .query_row(
            "SELECT bytes FROM policy_files WHERE key = ?1",
            params![key.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    let Some(policy_file) = policy_file else {
        return Err(ReadError::PolicyFileMissing {
            key: key.as_str().to_string(),
        });
    };
    Ok((batches, policy_file))
}

fn decoded(root: &TrajectoryId, batches: Vec<Vec<u8>>, policy_file: Vec<u8>) -> Result<Log, ReadError> {
    let basis = batches.len() as u64;
    let mut facts = Vec::new();
    for batch in &batches {
        facts.extend(decode(batch)?);
    }
    Ok(Log {
        root: root.clone(),
        facts,
        basis,
        policy_file,
    })
}

fn encode(facts: &[Fact]) -> Vec<u8> {
    serde_json::to_vec(facts).expect("engine records serialize: every field is a serde type with no float or map key")
}

fn decode(bytes: &[u8]) -> Result<Vec<Fact>, ReadError> {
    serde_json::from_slice(bytes).map_err(|error| ReadError::Undecodable(error.to_string()))
}

fn is_taken(error: &rusqlite::Error) -> bool {
    const PRIMARY_KEY: i32 = 1555;
    const UNIQUE: i32 = 2067;
    matches!(
        error,
        rusqlite::Error::SqliteFailure(e, _) if e.extended_code == PRIMARY_KEY || e.extended_code == UNIQUE
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const POLICY: &str = r#"
        version = 1
    "#;

    fn engine() -> appa_engine::engine::Engine {
        appa_policy::Config::from_toml_str(POLICY)
            .expect("the minimal policy compiles")
            .engine()
            .clone()
    }

    fn root() -> TrajectoryId {
        TrajectoryId::new("cc:root")
    }

    fn opening(id: &TrajectoryId) -> Vec<Fact> {
        engine()
            .open_trajectory(id, PolicyFileKey::of(POLICY.as_bytes()))
            .expect("the opening seals")
            .into_unsealed()
    }

    fn punctuation() -> Vec<Fact> {
        vec![Fact::Boundary {
            trajectory: root(),
            kind: appa_engine::fact::BoundaryKind::VoidReturn,
        }]
    }

    fn memory() -> LogStore {
        LogStore::open(Backend::Memory).expect("an in-memory store opens")
    }

    fn opened() -> LogStore {
        let store = memory();
        store
            .create_root(opening(&root()), POLICY.as_bytes())
            .expect("a fresh root opens");
        store
    }

    #[test]
    fn an_opened_root_reads_back_with_its_records_and_its_policy_file() {
        let store = opened();
        let log = store.log(&root()).expect("the log reads");
        assert_eq!(log.root(), &root());
        assert_eq!(log.basis(), 1, "the opening batch is the log's first position");
        assert!(matches!(log.facts(), [Fact::TrajectoryOpened { .. }]));
        assert_eq!(log.policy_file(), POLICY.as_bytes());
    }

    #[test]
    fn a_second_root_under_one_id_is_refused() {
        let store = opened();
        assert!(matches!(
            store.create_root(opening(&root()), POLICY.as_bytes()),
            Err(CreateError::AlreadyExists { .. }),
        ));
    }

    #[test]
    fn an_opening_that_does_not_lead_with_its_record_is_refused() {
        let store = memory();
        assert!(matches!(
            store.create_root(punctuation(), POLICY.as_bytes()),
            Err(CreateError::Malformed { .. }),
        ));
        assert!(matches!(
            store.create_root(Vec::new(), POLICY.as_bytes()),
            Err(CreateError::Malformed { .. }),
        ));
    }

    #[test]
    fn a_policy_file_the_opening_does_not_name_is_refused() {
        let store = memory();
        assert!(matches!(
            store.create_root(opening(&root()), b"other bytes"),
            Err(CreateError::PolicyFileMismatch),
        ));
        assert!(matches!(store.log(&root()), Err(ReadError::UnknownRoot { .. })));
    }

    #[test]
    fn appends_advance_the_position_and_read_back_in_order() {
        let store = opened();
        let log = store.log(&root()).expect("the log reads");
        store.append(&log, &punctuation()).expect("the append lands");
        let log = store.log(&root()).expect("the log reads");
        assert_eq!(log.basis(), 2);
        store.append(&log, &punctuation()).expect("the second append lands");

        let log = store.log(&root()).expect("the log reads");
        assert_eq!(log.basis(), 3);
        assert!(matches!(
            log.facts(),
            [
                Fact::TrajectoryOpened { .. },
                Fact::Boundary { .. },
                Fact::Boundary { .. },
            ],
        ));
    }

    #[test]
    fn an_append_on_a_stale_read_conflicts_and_writes_nothing() {
        let store = opened();
        let stale = store.log(&root()).expect("the log reads");
        store.append(&stale, &punctuation()).expect("the first append lands");

        match store.append(&stale, &punctuation()) {
            Err(AppendError::Conflict { current }) => assert_eq!(current, 2),
            other => panic!("expected a conflict, got {other:?}"),
        }
        assert_eq!(store.log(&root()).expect("the log reads").basis(), 2);
    }

    #[test]
    fn an_unknown_root_does_not_read_as_empty_history() {
        let store = opened();
        assert!(matches!(
            store.log(&TrajectoryId::new("cc:ghost")),
            Err(ReadError::UnknownRoot { .. }),
        ));
        assert!(!store.has_root(&TrajectoryId::new("cc:ghost")).expect("the check runs"));
        assert!(store.has_root(&root()).expect("the check runs"));
    }

    #[test]
    fn two_roots_under_one_policy_file_share_the_stored_row() {
        let store = opened();
        let second = TrajectoryId::new("cc:second");
        store
            .create_root(opening(&second), POLICY.as_bytes())
            .expect("a second root opens under the same file");

        assert_eq!(
            store.log(&second).expect("the log reads").policy_file(),
            POLICY.as_bytes()
        );
        let rows: i64 = store
            .lock()
            .query_row("SELECT COUNT(*) FROM policy_files", [], |row| row.get(0))
            .expect("the count runs");
        assert_eq!(rows, 1, "the file is stored once, not once per root");
    }

    #[test]
    fn a_missing_stored_policy_file_refuses_the_read() {
        let store = opened();
        store
            .lock()
            .execute("DELETE FROM policy_files", [])
            .expect("the deletion lands");
        assert!(matches!(store.log(&root()), Err(ReadError::PolicyFileMissing { .. }),));
    }

    #[test]
    fn committed_state_survives_a_reopen() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.db");
        {
            let store = LogStore::open(Backend::Sqlite { path: path.clone() }).expect("a fresh store opens");
            store
                .create_root(opening(&root()), POLICY.as_bytes())
                .expect("a fresh root opens");
            let log = store.log(&root()).expect("the log reads");
            store.append(&log, &punctuation()).expect("the append lands");
        }
        let store = LogStore::open(Backend::Sqlite { path }).expect("the store reopens");
        assert_eq!(store.log(&root()).expect("the log reads").basis(), 2);
    }

    #[test]
    fn two_connections_serialize_through_the_conflict() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.db");
        let first = LogStore::open(Backend::Sqlite { path: path.clone() }).expect("the first connection opens");
        first
            .create_root(opening(&root()), POLICY.as_bytes())
            .expect("a fresh root opens");
        let second = LogStore::open(Backend::Sqlite { path }).expect("the second connection opens");

        let seen_by_first = first.log(&root()).expect("the log reads");
        let seen_by_second = second.log(&root()).expect("the log reads");
        first.append(&seen_by_first, &punctuation()).expect("the winner lands");
        assert!(matches!(
            second.append(&seen_by_second, &punctuation()),
            Err(AppendError::Conflict { current: 2 }),
        ));

        let replayed = second.log(&root()).expect("the log reads");
        second.append(&replayed, &punctuation()).expect("the replay lands");
        assert_eq!(first.log(&root()).expect("the log reads").basis(), 3);
    }

    #[test]
    fn a_database_at_another_schema_version_is_refused() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.db");
        drop(LogStore::open(Backend::Sqlite { path: path.clone() }).expect("a fresh store opens"));
        Connection::open(&path)
            .expect("the file reopens")
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .expect("the version moves");

        match LogStore::open(Backend::Sqlite { path }).err() {
            Some(OpenError::ForeignSchema { found, expected, .. }) => {
                assert_eq!((found, expected), (SCHEMA_VERSION + 1, SCHEMA_VERSION));
            }
            other => panic!("expected a schema refusal, got {other:?}"),
        }
    }

    #[test]
    fn an_unstamped_database_that_already_holds_tables_is_refused() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.db");
        Connection::open(&path)
            .expect("the file opens")
            .execute_batch("CREATE TABLE batches (family TEXT, seq INTEGER, bytes BLOB);")
            .expect("the older schema lands");

        match LogStore::open(Backend::Sqlite { path: path.clone() }).err() {
            Some(OpenError::ForeignSchema { found, expected, .. }) => {
                assert_eq!((found, expected), (0, SCHEMA_VERSION));
            }
            other => panic!("expected a schema refusal, got {other:?}"),
        }
        let tables: i64 = Connection::open(&path)
            .expect("the file reopens")
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('logs', 'policy_files')",
                [],
                |row| row.get(0),
            )
            .expect("the count runs");
        assert_eq!(tables, 0, "the refusal wrote nothing");
    }

    #[test]
    fn a_damaged_file_is_refused() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.db");
        std::fs::write(&path, b"not a sqlite database at all").expect("the file writes");
        assert!(LogStore::open(Backend::Sqlite { path }).is_err());
    }

    #[test]
    fn the_memory_backend_is_private_to_its_store() {
        let first = opened();
        assert!(first.log(&root()).is_ok());
        assert!(matches!(memory().log(&root()), Err(ReadError::UnknownRoot { .. })));
    }
}
