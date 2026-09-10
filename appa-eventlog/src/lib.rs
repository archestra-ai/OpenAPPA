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

const SCHEMA_VERSION: i64 = 8;
const MAX_PROXY_EVENTS_PER_ROOT: i64 = 10_000;

/// The durable result of admitting one proxy protocol event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyEventAdmission {
    Started,
    Replay(Vec<u8>),
    Conflict,
    InProgress,
    Uncertain,
    RootPending,
    BudgetExceeded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyApprovalAdmission {
    Started,
    Consumed,
}

/// The immutable association between an offer and the call that surfaced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyOfferBinding {
    pub offer_id: String,
    pub root_id: String,
    pub tool: String,
    pub arguments_sha256: String,
    pub kind: String,
    pub deployment_fingerprint: String,
    pub batch_id: Option<String>,
    pub position: Option<u32>,
}

/// The immutable association from a proxy lane-local call id to the engine dispatch it opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyDispatchBinding {
    pub root_id: String,
    pub lane_id: String,
    pub call_id: String,
    pub tool: String,
    pub arguments_sha256: String,
    pub dispatch: String,
    pub spawn_binding: Option<String>,
    pub deployment_fingerprint: String,
    pub batch_id: Option<String>,
    pub position: Option<u32>,
}

/// The completed receipt and immutable bindings produced by one proxy event.
pub struct ProxyEventCompletion<'a> {
    pub root_id: &'a str,
    pub event_id: &'a str,
    pub body_digest: &'a str,
    pub response: &'a [u8],
    pub bindings: &'a [ProxyOfferBinding],
    pub dispatch_bindings: &'a [ProxyDispatchBinding],
    pub approval_id: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyBatchBinding {
    pub batch_id: String,
    pub root_id: String,
    pub lane_id: String,
    pub core_batch_id: String,
    pub positions: u32,
    pub basis: u64,
    pub deployment_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyBatchPosition {
    pub batch_id: String,
    pub position: u32,
    pub call_id: String,
    pub tool: String,
    pub arguments_sha256: String,
    pub arguments: String,
    pub effective_tool: String,
    pub effective_arguments_sha256: String,
    pub effective_arguments: String,
    pub dispatch: Option<String>,
    pub spawn: bool,
    pub spawn_binding: Option<String>,
    pub authorized: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ProxyStoreError {
    #[error("proxy event storage failure: {0}")]
    Storage(#[from] rusqlite::Error),
    #[error("proxy event completion does not match a pending intent")]
    LostIntent,
}

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

/// Why a store operation failed, with nothing of the failure in it.
///
/// Every error this crate returns carries free text — a root id, a path, a decode detail, a
/// `rusqlite` message. That text is fine locally, where it is logged and read by the operator,
/// and unusable anywhere a report leaves the machine. This is the closed form: a caller that
/// must name a failure without repeating it matches the source variant here, once, and carries
/// the class instead of the message.
///
/// The mapping lives in this crate because this crate owns the variants. `Injected` exists only
/// under `fault-injection`, and a caller whose own dependency does not enable that feature has
/// no `cfg` to gate an arm on, so an exhaustive match written anywhere else compiles in one
/// build mode and fails in the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreErrorClass {
    /// No log for this root exists.
    UnknownRoot,
    /// A log for this root already exists.
    AlreadyExists,
    /// The opening's policy file is missing from the store.
    PolicyUnavailable,
    /// The supplied policy file is not the one the opening names.
    PolicyMismatch,
    /// A stored batch does not decode.
    Undecodable,
    /// The opening batch is not usable as one.
    Malformed,
    /// The log moved under a decision that was computed against an earlier position.
    Conflict,
    /// The database itself failed.
    Storage,
}

impl From<&CreateError> for StoreErrorClass {
    fn from(error: &CreateError) -> Self {
        match error {
            CreateError::AlreadyExists { .. } => StoreErrorClass::AlreadyExists,
            CreateError::Malformed { .. } => StoreErrorClass::Malformed,
            CreateError::PolicyFileMismatch => StoreErrorClass::PolicyMismatch,
            CreateError::Storage(_) => StoreErrorClass::Storage,
            #[cfg(feature = "fault-injection")]
            CreateError::Injected => StoreErrorClass::Storage,
        }
    }
}

impl From<&ReadError> for StoreErrorClass {
    fn from(error: &ReadError) -> Self {
        match error {
            ReadError::UnknownRoot { .. } => StoreErrorClass::UnknownRoot,
            ReadError::PolicyFileMissing { .. } => StoreErrorClass::PolicyUnavailable,
            ReadError::Undecodable(_) => StoreErrorClass::Undecodable,
            ReadError::Storage(_) => StoreErrorClass::Storage,
        }
    }
}

impl From<&AppendError> for StoreErrorClass {
    fn from(error: &AppendError) -> Self {
        match error {
            AppendError::Conflict { .. } => StoreErrorClass::Conflict,
            AppendError::Storage(_) => StoreErrorClass::Storage,
            #[cfg(feature = "fault-injection")]
            AppendError::Injected => StoreErrorClass::Storage,
        }
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
        let (mut connection, path) = match &backend {
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
                     );
                     CREATE TABLE proxy_events (
                         root_id TEXT NOT NULL,
                         event_id TEXT NOT NULL,
                         body_digest TEXT NOT NULL,
                         state TEXT NOT NULL CHECK (state IN ('pending', 'completed')),
                         boot_owner TEXT NOT NULL,
                         response BLOB,
                         PRIMARY KEY (root_id, event_id)
                     );
                     CREATE TABLE proxy_offer_bindings (
                         offer_id TEXT PRIMARY KEY,
                         root_id TEXT NOT NULL,
                         tool TEXT NOT NULL,
                          arguments_sha256 TEXT NOT NULL,
                          kind TEXT NOT NULL,
                          deployment_fingerprint TEXT NOT NULL,
                          batch_id TEXT,
                          position INTEGER
                     );
                     CREATE TABLE proxy_approval_grants (
                         approval_id TEXT PRIMARY KEY,
                         root_id TEXT NOT NULL,
                         event_id TEXT NOT NULL,
                         body_digest TEXT NOT NULL,
                          state TEXT NOT NULL CHECK (state IN ('pending', 'completed'))
                     );
                      CREATE TABLE proxy_dispatch_bindings (
                           root_id TEXT NOT NULL,
                           lane_id TEXT NOT NULL,
                           call_id TEXT NOT NULL,
                          tool TEXT NOT NULL,
                          arguments_sha256 TEXT NOT NULL,
                          dispatch TEXT NOT NULL,
                           spawn_binding TEXT,
                           deployment_fingerprint TEXT NOT NULL,
                           batch_id TEXT,
                           position INTEGER,
                            PRIMARY KEY (root_id, lane_id, call_id)
                      );
                     CREATE TABLE proxy_batches (
                           batch_id TEXT PRIMARY KEY,
                           root_id TEXT NOT NULL,
                           lane_id TEXT NOT NULL,
                           core_batch_id TEXT NOT NULL,
                           positions INTEGER NOT NULL,
                           basis INTEGER NOT NULL,
                           deployment_fingerprint TEXT NOT NULL
                     );
                     CREATE TABLE proxy_batch_positions (
                           batch_id TEXT NOT NULL,
                           position INTEGER NOT NULL,
                           call_id TEXT NOT NULL,
                           tool TEXT NOT NULL,
                           arguments_sha256 TEXT NOT NULL,
                           arguments TEXT NOT NULL,
                           effective_tool TEXT NOT NULL,
                           effective_arguments_sha256 TEXT NOT NULL,
                           effective_arguments TEXT NOT NULL,
                           dispatch TEXT,
                           spawn INTEGER NOT NULL CHECK (spawn IN (0, 1)),
                           spawn_binding TEXT,
                           authorized INTEGER NOT NULL CHECK (authorized IN (0, 1)),
                           PRIMARY KEY (batch_id, position),
                           UNIQUE (batch_id, call_id)
                     );
                     CREATE TABLE proxy_batch_quarantines (
                           batch_id TEXT PRIMARY KEY,
                           reason TEXT NOT NULL
                     );",
                )?;
                transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            } else if version == 1 {
                transaction.execute_batch(
                    "CREATE TABLE proxy_events (
                         root_id TEXT NOT NULL,
                         event_id TEXT NOT NULL,
                         body_digest TEXT NOT NULL,
                         state TEXT NOT NULL CHECK (state IN ('pending', 'completed')),
                         boot_owner TEXT NOT NULL,
                         response BLOB,
                         PRIMARY KEY (root_id, event_id)
                     );
                     CREATE TABLE proxy_offer_bindings (
                         offer_id TEXT PRIMARY KEY,
                         root_id TEXT NOT NULL,
                         tool TEXT NOT NULL,
                         arguments_sha256 TEXT NOT NULL,
                         kind TEXT NOT NULL,
                         deployment_fingerprint TEXT NOT NULL
                     );
                     CREATE TABLE proxy_approval_grants (
                         approval_id TEXT PRIMARY KEY,
                         root_id TEXT NOT NULL,
                         event_id TEXT NOT NULL,
                         body_digest TEXT NOT NULL,
                          state TEXT NOT NULL CHECK (state IN ('pending', 'completed'))
                     );
                      CREATE TABLE proxy_dispatch_bindings (
                           root_id TEXT NOT NULL,
                           lane_id TEXT NOT NULL,
                           call_id TEXT NOT NULL,
                          tool TEXT NOT NULL,
                          arguments_sha256 TEXT NOT NULL,
                          dispatch TEXT NOT NULL,
                          spawn_binding TEXT,
                          deployment_fingerprint TEXT NOT NULL,
                           PRIMARY KEY (root_id, lane_id, call_id)
                     );",
                )?;
                transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            } else if version == 2 {
                transaction.execute_batch(
                    "ALTER TABLE proxy_offer_bindings ADD COLUMN deployment_fingerprint TEXT NOT NULL DEFAULT '';
                     CREATE TABLE proxy_approval_grants (
                         approval_id TEXT PRIMARY KEY,
                         root_id TEXT NOT NULL,
                         event_id TEXT NOT NULL,
                         body_digest TEXT NOT NULL,
                          state TEXT NOT NULL CHECK (state IN ('pending', 'completed'))
                     );
                      CREATE TABLE proxy_dispatch_bindings (
                           root_id TEXT NOT NULL,
                           lane_id TEXT NOT NULL,
                           call_id TEXT NOT NULL,
                          tool TEXT NOT NULL,
                          arguments_sha256 TEXT NOT NULL,
                          dispatch TEXT NOT NULL,
                          spawn_binding TEXT,
                          deployment_fingerprint TEXT NOT NULL,
                           PRIMARY KEY (root_id, lane_id, call_id)
                     );",
                )?;
                transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            } else if version == 3 {
                transaction.execute_batch(
                    "CREATE TABLE proxy_dispatch_bindings (
                          root_id TEXT NOT NULL,
                          lane_id TEXT NOT NULL,
                          call_id TEXT NOT NULL,
                         tool TEXT NOT NULL,
                          arguments_sha256 TEXT NOT NULL,
                          dispatch TEXT NOT NULL,
                          spawn_binding TEXT,
                          deployment_fingerprint TEXT NOT NULL,
                          PRIMARY KEY (root_id, lane_id, call_id)
                     );",
                )?;
                transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            } else if version == 4 {
                transaction.execute_batch(
                    "ALTER TABLE proxy_dispatch_bindings ADD COLUMN spawn_binding TEXT;
                     ALTER TABLE proxy_dispatch_bindings RENAME TO proxy_dispatch_bindings_v4;
                     CREATE TABLE proxy_dispatch_bindings (
                         root_id TEXT NOT NULL,
                         lane_id TEXT NOT NULL,
                         call_id TEXT NOT NULL,
                         tool TEXT NOT NULL,
                         arguments_sha256 TEXT NOT NULL,
                         dispatch TEXT NOT NULL,
                         spawn_binding TEXT,
                         deployment_fingerprint TEXT NOT NULL,
                         PRIMARY KEY (root_id, lane_id, call_id)
                     );
                     INSERT INTO proxy_dispatch_bindings (root_id, lane_id, call_id, tool, arguments_sha256, dispatch, spawn_binding, deployment_fingerprint)
                     SELECT root_id, 'kagent:' || root_id, call_id, tool, arguments_sha256, dispatch, spawn_binding, deployment_fingerprint
                     FROM proxy_dispatch_bindings_v4;
                     DROP TABLE proxy_dispatch_bindings_v4;",
                )?;
                transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            } else if version == 5 {
                transaction.execute_batch(
                    "ALTER TABLE proxy_dispatch_bindings RENAME TO proxy_dispatch_bindings_v5;
                     CREATE TABLE proxy_dispatch_bindings (
                         root_id TEXT NOT NULL,
                         lane_id TEXT NOT NULL,
                         call_id TEXT NOT NULL,
                         tool TEXT NOT NULL,
                         arguments_sha256 TEXT NOT NULL,
                         dispatch TEXT NOT NULL,
                         spawn_binding TEXT,
                         deployment_fingerprint TEXT NOT NULL,
                         PRIMARY KEY (root_id, lane_id, call_id)
                     );
                     INSERT INTO proxy_dispatch_bindings (root_id, lane_id, call_id, tool, arguments_sha256, dispatch, spawn_binding, deployment_fingerprint)
                     SELECT root_id, 'kagent:' || root_id, call_id, tool, arguments_sha256, dispatch, spawn_binding, deployment_fingerprint
                     FROM proxy_dispatch_bindings_v5;
                     DROP TABLE proxy_dispatch_bindings_v5;",
                )?;
                transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            } else if version == 6 {
                transaction.execute_batch(
                    "ALTER TABLE proxy_offer_bindings ADD COLUMN batch_id TEXT;
                     ALTER TABLE proxy_offer_bindings ADD COLUMN position INTEGER;
                     ALTER TABLE proxy_dispatch_bindings ADD COLUMN batch_id TEXT;
                     ALTER TABLE proxy_dispatch_bindings ADD COLUMN position INTEGER;
                     CREATE TABLE proxy_batches (
                           batch_id TEXT PRIMARY KEY,
                           root_id TEXT NOT NULL,
                           lane_id TEXT NOT NULL,
                           core_batch_id TEXT NOT NULL,
                           positions INTEGER NOT NULL,
                           basis INTEGER NOT NULL,
                           deployment_fingerprint TEXT NOT NULL
                     );
                     CREATE TABLE proxy_batch_positions (
                           batch_id TEXT NOT NULL,
                           position INTEGER NOT NULL,
                           call_id TEXT NOT NULL,
                           tool TEXT NOT NULL,
                           arguments_sha256 TEXT NOT NULL,
                           arguments TEXT NOT NULL,
                           effective_tool TEXT NOT NULL,
                           effective_arguments_sha256 TEXT NOT NULL,
                           effective_arguments TEXT NOT NULL,
                           dispatch TEXT,
                           spawn INTEGER NOT NULL CHECK (spawn IN (0, 1)),
                           spawn_binding TEXT,
                           authorized INTEGER NOT NULL CHECK (authorized IN (0, 1)),
                           PRIMARY KEY (batch_id, position),
                           UNIQUE (batch_id, call_id)
                     );
                     CREATE TABLE proxy_batch_quarantines (
                           batch_id TEXT PRIMARY KEY,
                           reason TEXT NOT NULL
                     );",
                )?;
                transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            } else if version == 7 {
                transaction.execute_batch(
                    "ALTER TABLE proxy_batch_positions ADD COLUMN spawn INTEGER NOT NULL DEFAULT 0 CHECK (spawn IN (0, 1));
                     ALTER TABLE proxy_batch_positions ADD COLUMN spawn_binding TEXT;",
                )?;
                transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            } else if version != SCHEMA_VERSION {
                return Err(OpenError::ForeignSchema {
                    path,
                    found: version,
                    expected: SCHEMA_VERSION,
                });
            } else if !has_schema(&transaction)? {
                return Err(OpenError::Damaged {
                    path,
                    detail: "stamped at this build's schema version, but its tables are missing".to_string(),
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
            let foreign = encode(&[]);
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

    /// Write the proxy request's intent before a runtime event can change the trajectory.
    /// Pending rows deliberately have no lease: a process death leaves an uncertainty tombstone.
    pub fn begin_proxy_event(
        &self,
        root_id: &str,
        event_id: &str,
        body_digest: &str,
        boot_owner: &str,
    ) -> Result<ProxyEventAdmission, ProxyStoreError> {
        let mut connection = self.lock();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<(String, String, String, Option<Vec<u8>>)> = transaction
            .query_row(
                "SELECT body_digest, state, boot_owner, response FROM proxy_events WHERE root_id = ?1 AND event_id = ?2",
                params![root_id, event_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let admission = match existing {
            Some((digest, _, _, _)) if digest != body_digest => ProxyEventAdmission::Conflict,
            Some((_, state, _, Some(response))) if state == "completed" => ProxyEventAdmission::Replay(response),
            Some((_, state, owner, _)) if state == "pending" && owner == boot_owner => ProxyEventAdmission::InProgress,
            Some((_, state, _, _)) if state == "pending" => ProxyEventAdmission::Uncertain,
            Some(_) => ProxyEventAdmission::Uncertain,
            None => {
                let pending: Option<i64> = transaction
                    .query_row(
                        "SELECT 1 FROM proxy_events WHERE root_id = ?1 AND state = 'pending' LIMIT 1",
                        params![root_id],
                        |row| row.get(0),
                    )
                    .optional()?;
                if pending.is_some() {
                    return Ok(ProxyEventAdmission::RootPending);
                }
                let count: i64 = transaction.query_row(
                    "SELECT COUNT(*) FROM proxy_events WHERE root_id = ?1",
                    params![root_id],
                    |row| row.get(0),
                )?;
                if count >= MAX_PROXY_EVENTS_PER_ROOT {
                    ProxyEventAdmission::BudgetExceeded
                } else {
                    transaction.execute(
                        "INSERT INTO proxy_events (root_id, event_id, body_digest, state, boot_owner) VALUES (?1, ?2, ?3, 'pending', ?4)",
                        params![root_id, event_id, body_digest, boot_owner],
                    )?;
                    ProxyEventAdmission::Started
                }
            }
        };
        transaction.commit()?;
        Ok(admission)
    }

    /// Atomically cache a completed response and the offer bindings it surfaced.
    pub fn complete_proxy_event(&self, completion: &ProxyEventCompletion<'_>) -> Result<(), ProxyStoreError> {
        let mut connection = self.lock();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE proxy_events SET state = 'completed', response = ?4 WHERE root_id = ?1 AND event_id = ?2 AND body_digest = ?3 AND state = 'pending'",
            params![completion.root_id, completion.event_id, completion.body_digest, completion.response],
        )?;
        if changed != 1 {
            return Err(ProxyStoreError::LostIntent);
        }
        for binding in completion.bindings {
            transaction.execute(
                "INSERT INTO proxy_offer_bindings (offer_id, root_id, tool, arguments_sha256, kind, deployment_fingerprint, batch_id, position) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![binding.offer_id, binding.root_id, binding.tool, binding.arguments_sha256, binding.kind, binding.deployment_fingerprint, binding.batch_id, binding.position],
            )?;
        }
        for binding in completion.dispatch_bindings {
            transaction.execute(
                "INSERT INTO proxy_dispatch_bindings (root_id, lane_id, call_id, tool, arguments_sha256, dispatch, spawn_binding, deployment_fingerprint, batch_id, position) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![binding.root_id, binding.lane_id, binding.call_id, binding.tool, binding.arguments_sha256, binding.dispatch, binding.spawn_binding, binding.deployment_fingerprint, binding.batch_id, binding.position],
            )?;
        }
        if let Some(approval_id) = completion.approval_id {
            let changed = transaction.execute(
                "UPDATE proxy_approval_grants SET state = 'completed' WHERE approval_id = ?1 AND root_id = ?2 AND event_id = ?3 AND body_digest = ?4 AND state = 'pending'",
                params![approval_id, completion.root_id, completion.event_id, completion.body_digest],
            )?;
            if changed != 1 {
                return Err(ProxyStoreError::LostIntent);
            }
        }
        transaction.commit()?;
        Ok(())
    }

    /// Read the immutable binding that a later resolution must match exactly.
    pub fn proxy_offer_binding(&self, offer_id: &str) -> Result<Option<ProxyOfferBinding>, ProxyStoreError> {
        let connection = self.lock();
        connection
            .query_row(
                "SELECT offer_id, root_id, tool, arguments_sha256, kind, deployment_fingerprint, batch_id, position FROM proxy_offer_bindings WHERE offer_id = ?1",
                params![offer_id],
                |row| {
                    Ok(ProxyOfferBinding {
                        offer_id: row.get(0)?,
                        root_id: row.get(1)?,
                        tool: row.get(2)?,
                        arguments_sha256: row.get(3)?,
                        kind: row.get(4)?,
                        deployment_fingerprint: row.get(5)?,
                        batch_id: row.get(6)?,
                        position: row.get(7)?,
                    })
                },
            )
            .optional()
            .map_err(ProxyStoreError::from)
    }

    /// Read the immutable dispatch mapping a later `tool_result` must use.
    pub fn proxy_dispatch_binding(
        &self,
        root_id: &str,
        lane_id: &str,
        call_id: &str,
    ) -> Result<Option<ProxyDispatchBinding>, ProxyStoreError> {
        let connection = self.lock();
        connection
            .query_row(
                "SELECT root_id, lane_id, call_id, tool, arguments_sha256, dispatch, spawn_binding, deployment_fingerprint, batch_id, position FROM proxy_dispatch_bindings WHERE root_id = ?1 AND lane_id = ?2 AND call_id = ?3",
                params![root_id, lane_id, call_id],
                |row| {
                    Ok(ProxyDispatchBinding {
                        root_id: row.get(0)?,
                        lane_id: row.get(1)?,
                        call_id: row.get(2)?,
                        tool: row.get(3)?,
                        arguments_sha256: row.get(4)?,
                        dispatch: row.get(5)?,
                        spawn_binding: row.get(6)?,
                        deployment_fingerprint: row.get(7)?,
                        batch_id: row.get(8)?,
                        position: row.get(9)?,
                    })
                },
            )
            .optional()
            .map_err(ProxyStoreError::from)
    }

    /// Persist a held batch before any client can be told about its offers. A duplicate write is
    /// allowed only when every immutable field is identical; a reused UUID otherwise refuses.
    pub fn create_proxy_batch(
        &self,
        batch: &ProxyBatchBinding,
        positions: &[ProxyBatchPosition],
    ) -> Result<bool, ProxyStoreError> {
        let mut connection = self.lock();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<(String, String, String, i64, i64, String)> = transaction
            .query_row(
                "SELECT root_id, lane_id, core_batch_id, positions, basis, deployment_fingerprint FROM proxy_batches WHERE batch_id = ?1",
                params![batch.batch_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
            )
            .optional()?;
        if let Some(existing) = existing {
            let same = existing
                == (
                    batch.root_id.clone(),
                    batch.lane_id.clone(),
                    batch.core_batch_id.clone(),
                    i64::from(batch.positions),
                    i64::try_from(batch.basis).unwrap_or(i64::MAX),
                    batch.deployment_fingerprint.clone(),
                );
            transaction.commit()?;
            return Ok(same);
        }
        if positions.len() != batch.positions as usize
            || positions
                .iter()
                .enumerate()
                .any(|(index, position)| position.batch_id != batch.batch_id || position.position != index as u32)
        {
            return Err(ProxyStoreError::LostIntent);
        }
        transaction.execute(
            "INSERT INTO proxy_batches (batch_id, root_id, lane_id, core_batch_id, positions, basis, deployment_fingerprint) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![batch.batch_id, batch.root_id, batch.lane_id, batch.core_batch_id, batch.positions, i64::try_from(batch.basis).unwrap_or(i64::MAX), batch.deployment_fingerprint],
        )?;
        for position in positions {
            transaction.execute(
                "INSERT INTO proxy_batch_positions (batch_id, position, call_id, tool, arguments_sha256, arguments, effective_tool, effective_arguments_sha256, effective_arguments, dispatch, spawn, spawn_binding, authorized) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![position.batch_id, position.position, position.call_id, position.tool, position.arguments_sha256, position.arguments, position.effective_tool, position.effective_arguments_sha256, position.effective_arguments, position.dispatch, position.spawn, position.spawn_binding, position.authorized],
            )?;
        }
        transaction.commit()?;
        Ok(true)
    }

    pub fn proxy_batch(&self, batch_id: &str) -> Result<Option<ProxyBatchBinding>, ProxyStoreError> {
        let connection = self.lock();
        connection
            .query_row(
                "SELECT batch_id, root_id, lane_id, core_batch_id, positions, basis, deployment_fingerprint FROM proxy_batches WHERE batch_id = ?1",
                params![batch_id],
                |row| Ok(ProxyBatchBinding {
                    batch_id: row.get(0)?,
                    root_id: row.get(1)?,
                    lane_id: row.get(2)?,
                    core_batch_id: row.get(3)?,
                    positions: row.get(4)?,
                    basis: u64::try_from(row.get::<_, i64>(5)?).unwrap_or(u64::MAX),
                    deployment_fingerprint: row.get(6)?,
                }),
            )
            .optional()
            .map_err(ProxyStoreError::from)
    }

    /// Advance a held batch's expected family-log position only if no other batch has advanced
    /// it first. The runtime has already verified that `next` is the position immediately after
    /// the batch action it just admitted.
    pub fn advance_proxy_batch_basis(&self, batch_id: &str, expected: u64, next: u64) -> Result<bool, ProxyStoreError> {
        let changed = self.lock().execute(
            "UPDATE proxy_batches SET basis = ?3 WHERE batch_id = ?1 AND basis = ?2",
            params![
                batch_id,
                i64::try_from(expected).unwrap_or(i64::MAX),
                i64::try_from(next).unwrap_or(i64::MAX)
            ],
        )?;
        Ok(changed == 1)
    }

    pub fn proxy_batch_positions(&self, batch_id: &str) -> Result<Vec<ProxyBatchPosition>, ProxyStoreError> {
        let connection = self.lock();
        let mut statement = connection.prepare(
                "SELECT batch_id, position, call_id, tool, arguments_sha256, arguments, effective_tool, effective_arguments_sha256, effective_arguments, dispatch, spawn, spawn_binding, authorized FROM proxy_batch_positions WHERE batch_id = ?1 ORDER BY position",
        )?;
        let positions = statement
            .query_map(params![batch_id], |row| {
                Ok(ProxyBatchPosition {
                    batch_id: row.get(0)?,
                    position: row.get(1)?,
                    call_id: row.get(2)?,
                    tool: row.get(3)?,
                    arguments_sha256: row.get(4)?,
                    arguments: row.get(5)?,
                    effective_tool: row.get(6)?,
                    effective_arguments_sha256: row.get(7)?,
                    effective_arguments: row.get(8)?,
                    dispatch: row.get(9)?,
                    spawn: row.get(10)?,
                    spawn_binding: row.get(11)?,
                    authorized: row.get(12)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(positions)
    }

    pub fn update_proxy_batch_position(&self, position: &ProxyBatchPosition) -> Result<(), ProxyStoreError> {
        let changed = self.lock().execute(
            "UPDATE proxy_batch_positions SET effective_tool = ?3, effective_arguments_sha256 = ?4, effective_arguments = ?5, dispatch = ?6, spawn_binding = ?7, authorized = ?8 WHERE batch_id = ?1 AND position = ?2",
            params![position.batch_id, position.position, position.effective_tool, position.effective_arguments_sha256, position.effective_arguments, position.dispatch, position.spawn_binding, position.authorized],
        )?;
        if changed == 1 {
            Ok(())
        } else {
            Err(ProxyStoreError::LostIntent)
        }
    }

    /// A quarantined batch has an unknown external boundary. It stays durable so a restart
    /// cannot turn "did the authority/sanitizer act?" into permission to ask it again.
    pub fn quarantine_proxy_batch(&self, batch_id: &str, reason: &str) -> Result<(), ProxyStoreError> {
        self.lock().execute(
            "INSERT OR IGNORE INTO proxy_batch_quarantines (batch_id, reason) VALUES (?1, ?2)",
            params![batch_id, reason],
        )?;
        Ok(())
    }

    pub fn proxy_batch_quarantined(&self, batch_id: &str) -> Result<bool, ProxyStoreError> {
        let quarantined: Option<i64> = self
            .lock()
            .query_row(
                "SELECT 1 FROM proxy_batch_quarantines WHERE batch_id = ?1",
                params![batch_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(quarantined.is_some())
    }

    /// Consume a signed approval grant before the runtime can consult its HITL authority.
    pub fn begin_proxy_approval_grant(
        &self,
        approval_id: &str,
        root_id: &str,
        event_id: &str,
        body_digest: &str,
    ) -> Result<ProxyApprovalAdmission, ProxyStoreError> {
        let mut connection = self.lock();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<i64> = transaction
            .query_row(
                "SELECT 1 FROM proxy_approval_grants WHERE approval_id = ?1",
                params![approval_id],
                |row| row.get(0),
            )
            .optional()?;
        let admission = if existing.is_some() {
            ProxyApprovalAdmission::Consumed
        } else {
            transaction.execute(
                "INSERT INTO proxy_approval_grants (approval_id, root_id, event_id, body_digest, state) VALUES (?1, ?2, ?3, ?4, 'pending')",
                params![approval_id, root_id, event_id, body_digest],
            )?;
            ProxyApprovalAdmission::Started
        };
        transaction.commit()?;
        Ok(admission)
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
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('logs', 'policy_files', 'proxy_events', 'proxy_offer_bindings', 'proxy_approval_grants', 'proxy_dispatch_bindings', 'proxy_batches', 'proxy_batch_positions', 'proxy_batch_quarantines')",
        [],
        |row| row.get(0),
    )?;
    Ok(found == 9)
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
        version = 2
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
    fn a_stamped_database_without_its_tables_is_damaged() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.db");
        Connection::open(&path)
            .expect("the file opens")
            .pragma_update(None, "user_version", SCHEMA_VERSION)
            .expect("the stamp lands");

        match LogStore::open(Backend::Sqlite { path }).err() {
            Some(OpenError::Damaged { .. }) => {}
            other => panic!("expected a damage refusal, got {other:?}"),
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
        match LogStore::open(Backend::Sqlite { path }).err() {
            Some(OpenError::Damaged { .. }) => {}
            other => panic!("expected a damage refusal, got {other:?}"),
        }
    }

    #[test]
    fn the_memory_backend_is_private_to_its_store() {
        let first = opened();
        assert!(first.log(&root()).is_ok());
        assert!(matches!(memory().log(&root()), Err(ReadError::UnknownRoot { .. })));
    }

    #[test]
    fn a_version_one_database_upgrades_without_touching_its_log() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.db");
        let connection = Connection::open(&path).expect("the database opens");
        connection
            .execute_batch(
                "CREATE TABLE logs (root TEXT NOT NULL, seq INTEGER NOT NULL, facts BLOB NOT NULL, PRIMARY KEY (root, seq));
                 CREATE TABLE policy_files (key TEXT PRIMARY KEY, bytes BLOB NOT NULL);",
            )
            .expect("version one tables create");
        connection
            .pragma_update(None, "user_version", 1)
            .expect("version one stamp lands");
        drop(connection);

        let store = LogStore::open(Backend::Sqlite { path }).expect("version one upgrades");
        assert_eq!(
            store
                .begin_proxy_event("root", "event", "digest", "boot")
                .expect("proxy intent persists"),
            ProxyEventAdmission::Started
        );
    }

    #[test]
    fn proxy_events_replay_completed_rows_and_preserve_pending_tombstones() {
        let store = memory();
        assert_eq!(
            store
                .begin_proxy_event("root", "event", "a", "boot")
                .expect("intent persists"),
            ProxyEventAdmission::Started
        );
        assert_eq!(
            store
                .begin_proxy_event("root", "event", "a", "boot")
                .expect("intent reads"),
            ProxyEventAdmission::InProgress
        );
        assert_eq!(
            store
                .begin_proxy_event("root", "event", "a", "other")
                .expect("intent reads"),
            ProxyEventAdmission::Uncertain
        );
        assert_eq!(
            store
                .begin_proxy_event("root", "later", "a", "other")
                .expect("root pending reads"),
            ProxyEventAdmission::RootPending
        );
        assert_eq!(
            store
                .begin_proxy_event("root", "event", "b", "boot")
                .expect("intent reads"),
            ProxyEventAdmission::Conflict
        );
        store
            .complete_proxy_event(&ProxyEventCompletion {
                root_id: "root",
                event_id: "event",
                body_digest: "a",
                response: br#"{"decision":"ack"}"#,
                bindings: &[ProxyOfferBinding {
                    offer_id: "offer".to_string(),
                    root_id: "root".to_string(),
                    tool: "tool".to_string(),
                    arguments_sha256: "hash".to_string(),
                    kind: "restriction".to_string(),
                    deployment_fingerprint: "deployment".to_string(),
                    batch_id: None,
                    position: None,
                }],
                dispatch_bindings: &[ProxyDispatchBinding {
                    root_id: "root".to_string(),
                    lane_id: "kagent:root".to_string(),
                    call_id: "call-1".to_string(),
                    tool: "tool".to_string(),
                    arguments_sha256: "hash".to_string(),
                    dispatch: "dispatch".to_string(),
                    spawn_binding: Some("spawn".to_string()),
                    deployment_fingerprint: "deployment".to_string(),
                    batch_id: None,
                    position: None,
                }],
                approval_id: None,
            })
            .expect("completion persists");
        assert_eq!(
            store
                .begin_proxy_event("root", "event", "a", "later")
                .expect("completion replays"),
            ProxyEventAdmission::Replay(br#"{"decision":"ack"}"#.to_vec())
        );
        assert_eq!(
            store.proxy_offer_binding("offer").expect("binding reads"),
            Some(ProxyOfferBinding {
                offer_id: "offer".to_string(),
                root_id: "root".to_string(),
                tool: "tool".to_string(),
                arguments_sha256: "hash".to_string(),
                kind: "restriction".to_string(),
                deployment_fingerprint: "deployment".to_string(),
                batch_id: None,
                position: None,
            })
        );
        assert_eq!(
            store
                .proxy_dispatch_binding("root", "kagent:root", "call-1")
                .expect("dispatch binding reads"),
            Some(ProxyDispatchBinding {
                root_id: "root".to_string(),
                lane_id: "kagent:root".to_string(),
                call_id: "call-1".to_string(),
                tool: "tool".to_string(),
                arguments_sha256: "hash".to_string(),
                dispatch: "dispatch".to_string(),
                spawn_binding: Some("spawn".to_string()),
                deployment_fingerprint: "deployment".to_string(),
                batch_id: None,
                position: None,
            })
        );
    }

    #[test]
    fn dispatch_bindings_are_scoped_to_their_actor_lane() {
        let store = memory();
        for (event_id, lane_id, dispatch) in [
            ("parent-event", "kagent:root", "parent-dispatch"),
            ("child-event", "kagent:root:child", "child-dispatch"),
        ] {
            assert_eq!(
                store
                    .begin_proxy_event("root", event_id, event_id, "boot")
                    .expect("intent persists"),
                ProxyEventAdmission::Started
            );
            store
                .complete_proxy_event(&ProxyEventCompletion {
                    root_id: "root",
                    event_id,
                    body_digest: event_id,
                    response: br#"{"decision":"allow_calls"}"#,
                    bindings: &[],
                    dispatch_bindings: &[ProxyDispatchBinding {
                        root_id: "root".to_string(),
                        lane_id: lane_id.to_string(),
                        call_id: "call-1".to_string(),
                        tool: "fetch".to_string(),
                        arguments_sha256: "hash".to_string(),
                        dispatch: dispatch.to_string(),
                        spawn_binding: None,
                        deployment_fingerprint: "deployment".to_string(),
                        batch_id: None,
                        position: None,
                    }],
                    approval_id: None,
                })
                .expect("completion persists");
        }
        assert_eq!(
            store
                .proxy_dispatch_binding("root", "kagent:root", "call-1")
                .expect("parent binding reads")
                .map(|binding| binding.dispatch),
            Some("parent-dispatch".to_string())
        );
        assert_eq!(
            store
                .proxy_dispatch_binding("root", "kagent:root:child", "call-1")
                .expect("child binding reads")
                .map(|binding| binding.dispatch),
            Some("child-dispatch".to_string())
        );
    }

    #[test]
    fn held_batch_positions_keep_identical_calls_distinct_across_a_reopen() {
        let directory = tempfile::tempdir().expect("temporary directory exists");
        let path = directory.path().join("appa.db");
        let first = LogStore::open(Backend::Sqlite { path: path.clone() }).expect("store opens");
        let batch = ProxyBatchBinding {
            batch_id: "123e4567-e89b-12d3-a456-426614174000".to_string(),
            root_id: "root".to_string(),
            lane_id: "kagent:root".to_string(),
            core_batch_id: "123e4567-e89b-12d3-a456-426614174000".to_string(),
            positions: 2,
            basis: 7,
            deployment_fingerprint: "deployment".to_string(),
        };
        let position = |index, dispatch: Option<&str>| ProxyBatchPosition {
            batch_id: batch.batch_id.clone(),
            position: index,
            call_id: format!("call-{index}"),
            tool: "same".to_string(),
            arguments_sha256: "same-arguments".to_string(),
            arguments: "{}".to_string(),
            effective_tool: "same".to_string(),
            effective_arguments_sha256: "same-arguments".to_string(),
            effective_arguments: "{}".to_string(),
            dispatch: dispatch.map(str::to_string),
            spawn: false,
            spawn_binding: None,
            authorized: true,
        };
        assert!(
            first
                .create_proxy_batch(&batch, &[position(0, Some("dispatch-0")), position(1, None)])
                .expect("mapping persists")
        );
        drop(first);

        let reopened = LogStore::open(Backend::Sqlite { path }).expect("store reopens");
        let positions = reopened.proxy_batch_positions(&batch.batch_id).expect("positions read");
        assert_eq!(positions.len(), 2);
        assert_eq!(positions[0].dispatch.as_deref(), Some("dispatch-0"));
        assert_eq!(positions[1].dispatch, None);
        assert_eq!(
            positions[0].tool, positions[1].tool,
            "identical calls retain their positions"
        );
    }

    #[test]
    fn held_batch_basis_advances_only_from_the_expected_position() {
        let store = memory();
        let batch = ProxyBatchBinding {
            batch_id: "123e4567-e89b-12d3-a456-426614174009".to_string(),
            root_id: "root".to_string(),
            lane_id: "kagent:root".to_string(),
            core_batch_id: "123e4567-e89b-12d3-a456-426614174009".to_string(),
            positions: 1,
            basis: 7,
            deployment_fingerprint: "deployment".to_string(),
        };
        let position = ProxyBatchPosition {
            batch_id: batch.batch_id.clone(),
            position: 0,
            call_id: "call".to_string(),
            tool: "tool".to_string(),
            arguments_sha256: "hash".to_string(),
            arguments: "{}".to_string(),
            effective_tool: "tool".to_string(),
            effective_arguments_sha256: "hash".to_string(),
            effective_arguments: "{}".to_string(),
            dispatch: None,
            spawn: false,
            spawn_binding: None,
            authorized: false,
        };
        assert!(store.create_proxy_batch(&batch, &[position]).expect("batch persists"));
        assert!(
            store
                .advance_proxy_batch_basis(&batch.batch_id, 7, 8)
                .expect("expected basis advances")
        );
        assert!(
            !store
                .advance_proxy_batch_basis(&batch.batch_id, 7, 9)
                .expect("stale basis is refused")
        );
        assert_eq!(
            store
                .proxy_batch(&batch.batch_id)
                .expect("batch reads")
                .expect("batch exists")
                .basis,
            8
        );
    }
}
