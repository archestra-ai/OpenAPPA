//! # appa-eventlog — the trajectory log, and where it is kept
//!
//! A root trajectory and its branches append to one shared log. That log holds two streams at
//! one position: the engine's lasting facts, and the host's own observations — what a harness
//! saw of its inventory, its calls, its turns and the standing it recorded for them. The stored
//! policy files are the only other durable state, and
//! everything else — a branch's parent, whether it has ended, which dispatch is open, whether an
//! offer still stands — is read back from the log by the engine's projection.
//!
//! This crate is where the log is written and read. The record encoding, the database, and the
//! conditional append are private to it: a caller hands it [`Fact`]s and gets [`Log`]s back, and
//! never names SQL, a row, or a byte. Where the log is kept is the closed [`Backend`] enum,
//! dispatched by `match`: SQLite for standalone use and optional PostgreSQL for
//! embedded hosts that install the schema through their own migrations.
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
use std::time::SystemTime;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

pub use appa_engine::fact::Fact;
use appa_engine::profile::PolicyFileKey;
use appa_engine::value::{DispatchId, TrajectoryId};
use appa_runtime_api::{AdapterName, Ruling, inventory::ToolInventory};

pub mod files;
#[cfg(feature = "postgres")]
pub mod postgres;

const SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    Sqlite {
        path: PathBuf,
    },
    /// Private to one [`LogStore`] and gone when it drops. An in-memory adapter sits
    /// beside the durable one deliberately: the decision core cannot tell them apart.
    Memory,
    /// Schema is installed by the embedding application's migrations.
    #[cfg(feature = "postgres")]
    Postgres {
        url: String,
    },
}

pub struct LogStore {
    connection: Option<Mutex<Connection>>,
    #[cfg(feature = "postgres")]
    postgres: Option<postgres::PostgresStore>,
    #[cfg(feature = "fault-injection")]
    commits_until_failure: std::sync::atomic::AtomicU64,
    #[cfg(feature = "fault-injection")]
    contended_appends: std::sync::atomic::AtomicU64,
    #[cfg(feature = "fault-injection")]
    failing_reads: std::sync::atomic::AtomicU64,
    /// What the next foreign writer records rather than nothing, so a caller's re-derivation
    /// meets a changed state and not only a moved position.
    #[cfg(feature = "fault-injection")]
    contending_record: Mutex<Option<(TrajectoryId, TrajectoryId, HostObservation)>>,
}

/// The records of one read, and the position they were read at.
#[derive(Debug, Clone, PartialEq)]
pub struct Log {
    root: TrajectoryId,
    facts: Vec<Fact>,
    basis: u64,
    policy_file: Vec<u8>,
    host: Vec<HostRecord>,
}

/// One thing a harness observed or did, recorded beside the engine's facts.
///
/// This is neither an engine fact nor a policy update: no observation here changes what the
/// engine decided. It is written at the same compare-and-swap position as facts, so admission
/// cannot race past an uncommitted observation, and it survives a restart, so a runtime holds
/// no actor state of its own between calls.
///
/// A closed enum, and one wire spelling per variant: a reader that meets a shape this build
/// does not know refuses the log rather than dropping the record.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HostObservation {
    /// Identity evidence from one actor's host: the tools it reports, in that actor's own
    /// scope.
    Inventory {
        actor: TrajectoryId,
        adapter: AdapterName,
        inventory: ToolInventory,
    },
    /// The host's opaque identity for one call, bound to the dispatch the engine opened for
    /// it. Written in the same batch as the opening facts, so a restart cannot leave an open
    /// dispatch whose result can no longer name it.
    CallBound {
        trajectory: TrajectoryId,
        call_id: String,
        dispatch: DispatchId,
    },
    /// This actor stands behind this key, with the ruling its harness attached where it
    /// reviewed through a channel of its own.
    Vouched {
        actor: HostActor,
        key: String,
        ruling: Option<Ruling>,
    },
    /// This actor is executing what the key names, until at least `until`. The bound is the
    /// hard-crash backstop: an execution that ends writes [`HostObservation::Released`].
    Claimed {
        actor: HostActor,
        key: String,
        until: SystemTime,
    },
    /// This actor's standing behind the key is spent or given up.
    Released { actor: HostActor, key: String },
    /// A prompt reached this actor, so its previous turn is over however it ended.
    PromptSeen { actor: HostActor },
    /// What the prompt left open is settled, and the actor's standing survives it.
    PromptSettled { actor: HostActor },
    /// This actor's turn ended: its prompt mark and every vouch it still held are over.
    TurnEnded { actor: HostActor },
}

impl HostObservation {
    /// The text a stored row contains exactly when its observation names this key.
    ///
    /// Derived from the encoding rather than spelled beside it, so a key whose wire form
    /// changes moves the query that finds it. Pass the result to
    /// [`LogStore::roots_mentioning`].
    pub fn names_key(key: &str) -> String {
        let quoted = serde_json::to_string(key).expect("a string serializes");
        format!("\"key\":{quoted}")
    }
}

/// Whose observation this is: the family's root, and the child where the harness named one.
/// The exact actor, so a parent's turn end never settles what its subagent left standing.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostActor {
    pub root: TrajectoryId,
    pub child: Option<TrajectoryId>,
}

/// One host observation and the batch position it was appended at, so a reader can order it
/// against the facts of the same read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRecord {
    pub seq: u64,
    pub observation: HostObservation,
}

/// One recorded call identity, as [`Log::call_bindings`] reads it back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallBinding<'a> {
    pub trajectory: &'a TrajectoryId,
    pub call_id: &'a str,
    pub dispatch: &'a DispatchId,
}

/// One stored batch. Both streams share a position, so an engine decision and the host
/// observation it belongs with are durable together or not at all.
///
/// The encoding is the shape: a batch carrying no host observation is the bare JSON array of
/// its facts, and one carrying an observation is an object with both fields. Nothing sniffs
/// between unrelated payloads — the first token settles which of the two a stored row is.
struct Record {
    facts: Vec<Fact>,
    host: Option<HostObservation>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HostRow {
    #[serde(default)]
    facts: Vec<Fact>,
    host: HostObservation,
}

#[derive(serde::Serialize)]
struct HostRowRef<'a> {
    facts: &'a [Fact],
    host: &'a HostObservation,
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

    /// Host observations in append order. Consumers validate them under the
    /// opening configuration; no later observation changes earlier engine facts.
    pub fn host_records(&self) -> &[HostRecord] {
        &self.host
    }

    /// Host call identities in append order. A binding remains after its
    /// dispatch closes so reuse of one host id can be refused after restart.
    pub fn call_bindings(&self) -> impl Iterator<Item = CallBinding<'_>> {
        self.host.iter().filter_map(|record| match &record.observation {
            HostObservation::CallBound {
                trajectory,
                call_id,
                dispatch,
            } => Some(CallBinding {
                trajectory,
                call_id,
                dispatch,
            }),
            _ => None,
        })
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
            #[cfg(feature = "postgres")]
            CreateError::Postgres(_) => StoreErrorClass::Storage,
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
            #[cfg(feature = "postgres")]
            ReadError::Postgres(_) => StoreErrorClass::Storage,
            #[cfg(feature = "fault-injection")]
            ReadError::Injected => StoreErrorClass::Storage,
        }
    }
}

impl From<&AppendError> for StoreErrorClass {
    fn from(error: &AppendError) -> Self {
        match error {
            AppendError::Conflict { .. } => StoreErrorClass::Conflict,
            AppendError::Storage(_) => StoreErrorClass::Storage,
            #[cfg(feature = "postgres")]
            AppendError::Postgres(_) => StoreErrorClass::Storage,
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
    #[cfg(feature = "postgres")]
    #[error("PostgreSQL storage failure: {0}")]
    Postgres(#[from] postgres::PostgresError),
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
    #[cfg(feature = "postgres")]
    #[error("PostgreSQL storage failure: {0}")]
    Postgres(#[from] postgres::PostgresError),
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
    #[cfg(feature = "postgres")]
    #[error("PostgreSQL storage failure: {0}")]
    Postgres(#[from] postgres::PostgresError),
    #[cfg(feature = "fault-injection")]
    #[error("injected read failure")]
    Injected,
}

#[derive(Debug, thiserror::Error)]
pub enum AppendError {
    #[error("the log is at {current}, not the position this decision was read at")]
    Conflict { current: u64 },
    #[error("storage failure: {0}")]
    Storage(#[from] rusqlite::Error),
    #[cfg(feature = "postgres")]
    #[error("PostgreSQL storage failure: {0}")]
    Postgres(#[from] postgres::PostgresError),
    #[cfg(feature = "fault-injection")]
    #[error("injected failure before commit")]
    Injected,
}

impl LogStore {
    /// Coordinate host-owned receipts with the connection that writes event batches.
    /// The embedding host must serialize operations during an outer transaction.
    #[cfg(feature = "postgres")]
    pub fn postgres(&self) -> Option<&postgres::PostgresStore> {
        self.postgres.as_ref()
    }

    /// Open the log. A fresh database gets the schema and its version stamp; an existing one is
    /// checked for damage and for a version this build understands, and refused otherwise.
    pub fn open(backend: Backend) -> Result<LogStore, OpenError> {
        #[cfg(feature = "postgres")]
        if let Backend::Postgres { url } = &backend {
            return Ok(LogStore {
                connection: None,
                postgres: Some(postgres::PostgresStore::open(url.clone())?),
                #[cfg(feature = "fault-injection")]
                commits_until_failure: std::sync::atomic::AtomicU64::new(0),
                #[cfg(feature = "fault-injection")]
                contended_appends: std::sync::atomic::AtomicU64::new(0),
                #[cfg(feature = "fault-injection")]
                failing_reads: std::sync::atomic::AtomicU64::new(0),
                #[cfg(feature = "fault-injection")]
                contending_record: Mutex::new(None),
            });
        }
        let (mut connection, path) = match &backend {
            Backend::Sqlite { path } => (Connection::open(path)?, path.display().to_string()),
            Backend::Memory => (Connection::open_in_memory()?, ":memory:".to_string()),
            #[cfg(feature = "postgres")]
            Backend::Postgres { .. } => unreachable!("handled above"),
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
                     );",
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
            connection: Some(Mutex::new(connection)),
            #[cfg(feature = "postgres")]
            postgres: None,
            #[cfg(feature = "fault-injection")]
            commits_until_failure: std::sync::atomic::AtomicU64::new(0),
            #[cfg(feature = "fault-injection")]
            contended_appends: std::sync::atomic::AtomicU64::new(0),
            #[cfg(feature = "fault-injection")]
            failing_reads: std::sync::atomic::AtomicU64::new(0),
            #[cfg(feature = "fault-injection")]
            contending_record: Mutex::new(None),
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
        let bytes = encode(&opening, None);
        #[cfg(feature = "postgres")]
        if let Some(pg) = &self.postgres {
            return pg.create(&root, &key, policy_file, bytes);
        }
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
        #[cfg(feature = "postgres")]
        if let Some(pg) = &self.postgres {
            return pg.has_root(root).map_err(Into::into);
        }
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
        #[cfg(feature = "fault-injection")]
        self.read_refused()?;
        #[cfg(feature = "postgres")]
        if let Some(pg) = &self.postgres {
            return pg.log(root);
        }
        let (batches, policy_file) = {
            let connection = self.lock();
            stored(&connection, root)?
        };
        decoded(root, batches, policy_file)
    }

    /// Append records to the log `based_on` was read from, only if it still stands where that
    /// read left it. A conflict writes nothing; the caller reads again and replays.
    pub fn append(&self, based_on: &Log, facts: &[Fact]) -> Result<(), AppendError> {
        self.append_bytes(based_on, encode(facts, None))
    }

    /// Append one host observation, and the engine facts it belongs with. The observation is
    /// durable exactly when those facts are, and a stale read writes nothing, just as append.
    pub fn append_host(
        &self,
        based_on: &Log,
        facts: &[Fact],
        observation: &HostObservation,
    ) -> Result<(), AppendError> {
        self.append_bytes(based_on, encode(facts, Some(observation)))
    }

    /// Every root whose host records contain `needle`, and nothing of what they recorded.
    ///
    /// The store answers "which families may have recorded this" without the caller naming
    /// them and without decoding a single row: the caller reads the families it gets back.
    /// The answer stays small by what a needle is for — a root is here only if it once
    /// recorded this exact key, which is at most one root for an offer, and one root per
    /// session that quoted an identical ticket. Build the needle with
    /// [`HostObservation::names_key`] so the query and the encoding can never disagree about
    /// how a key is spelled.
    ///
    /// The scan itself is every family's host records: there is no index over what
    /// a row holds, because this store owns no DDL a deployment's PostgreSQL would have to
    /// be given. So a key that is spelled right and stands for nothing still costs one pass,
    /// and a caller that can be asked about keys it never minted checks the spelling before
    /// it asks here.
    pub fn roots_mentioning(&self, needle: &str) -> Result<Vec<TrajectoryId>, ReadError> {
        #[cfg(feature = "fault-injection")]
        self.read_refused()?;
        #[cfg(feature = "postgres")]
        if let Some(pg) = &self.postgres {
            return pg.roots_mentioning(needle);
        }
        let connection = self.lock();
        let mut statement = connection.prepare(
            "SELECT DISTINCT root FROM logs \
             WHERE substr(facts, 1, 1) = x'7b' AND instr(facts, ?1) > 0 ORDER BY root ASC",
        )?;
        let roots = statement
            .query_map(params![needle.as_bytes()], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(roots.into_iter().map(TrajectoryId::new).collect())
    }

    fn append_bytes(&self, based_on: &Log, bytes: Vec<u8>) -> Result<(), AppendError> {
        self.append_at(&based_on.root, based_on.basis, bytes)
    }

    fn append_at(&self, root: &TrajectoryId, basis: u64, bytes: Vec<u8>) -> Result<(), AppendError> {
        #[cfg(feature = "postgres")]
        if let Some(pg) = &self.postgres {
            return pg.append(root, basis, bytes);
        }
        let mut connection = self.lock();
        #[cfg(feature = "fault-injection")]
        if self.contention_fires() {
            // A foreign writer wins the race in its own committed transaction, exactly as a
            // second process would. It takes the position and records nothing, so this caller's
            // append conflicts on position and replays, and an assertion reads whose write landed
            // from the position rather than from records a later read would have to accept.
            //
            // Where the injection names an observation, the winner records that instead, and
            // where it names another family it records there and still takes this one's
            // position: a foreign writer that changed a sibling's log is the race a reader of
            // several families has to survive.
            let armed = self
                .contending_record
                .lock()
                .expect("the injection mutex is never poisoned")
                .take()
                .filter(|(racing, _, _)| racing == root);
            let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let write = |into: &TrajectoryId, bytes: Vec<u8>| -> Result<(), rusqlite::Error> {
                let at = position(&transaction, into)?;
                transaction.execute(
                    "INSERT INTO logs (root, seq, facts) VALUES (?1, ?2, ?3)",
                    params![into.as_str(), at as i64, bytes],
                )?;
                Ok(())
            };
            match &armed {
                Some((_, recorded_in, observation)) => {
                    write(recorded_in, encode(&[], Some(observation)))?;
                    if recorded_in != root {
                        write(root, encode(&[], None))?;
                    }
                }
                None => write(root, encode(&[], None))?,
            }
            transaction.commit()?;
        }
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = position(&transaction, root)?;
        if current != basis {
            return Err(AppendError::Conflict { current });
        }
        transaction.execute(
            "INSERT INTO logs (root, seq, facts) VALUES (?1, ?2, ?3)",
            params![root.as_str(), current as i64, bytes],
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

    /// Arm the read fail point: the next `count` reads answer with a failure instead of the
    /// store's rows. A caller that refuses without asking the store leaves the arming where
    /// it was, so the read that comes after it still meets the failure.
    #[cfg(feature = "fault-injection")]
    pub fn fail_next_reads(&self, count: u64) {
        self.failing_reads.store(count, std::sync::atomic::Ordering::SeqCst);
    }

    /// Arm the contention point: the next `count` appends are raced by a foreign writer that
    /// wins, so each loses the compare-and-swap and its caller replays.
    #[cfg(feature = "fault-injection")]
    pub fn contend_next_appends(&self, count: u64) {
        self.contended_appends.store(count, std::sync::atomic::Ordering::SeqCst);
    }

    /// Arm the contention point once, with what the winner records. The next append to `root`
    /// loses to a writer that put `observation` in the log, so the caller's next derivation
    /// answers to a state another writer changed rather than to a position it only moved.
    #[cfg(feature = "fault-injection")]
    pub fn contend_next_append_with(
        &self,
        racing: &TrajectoryId,
        recorded_in: &TrajectoryId,
        observation: &HostObservation,
    ) {
        *self
            .contending_record
            .lock()
            .expect("the injection mutex is never poisoned") =
            Some((racing.clone(), recorded_in.clone(), observation.clone()));
        self.contend_next_appends(1);
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

    #[cfg(feature = "fault-injection")]
    fn read_refused(&self) -> Result<(), ReadError> {
        match consume(&self.failing_reads) {
            Some(_) => Err(ReadError::Injected),
            None => Ok(()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.connection
            .as_ref()
            .expect("SQLite-only operation")
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
    let opening = decode(first)?;
    let Some(Fact::TrajectoryOpened {
        policy_file_key: key, ..
    }) = opening.facts.first()
    else {
        return Err(ReadError::Undecodable(
            "the log does not open with a TrajectoryOpened record".to_string(),
        ));
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
    let mut host = Vec::new();
    for (seq, batch) in batches.iter().enumerate() {
        let record = decode(batch)?;
        facts.extend(record.facts);
        if let Some(observation) = record.host {
            host.push(HostRecord {
                seq: seq as u64,
                observation,
            });
        }
    }
    Ok(Log {
        root: root.clone(),
        facts,
        basis,
        policy_file,
        host,
    })
}

fn encode(facts: &[Fact], host: Option<&HostObservation>) -> Vec<u8> {
    let expectation = "records serialize: every field is a serde type with no float or map key";
    match host {
        None => serde_json::to_vec(facts).expect(expectation),
        Some(host) => serde_json::to_vec(&HostRowRef { facts, host }).expect(expectation),
    }
}

/// Which of the two shapes a stored row is, from its first token. A row that is neither —
/// an older encoding, or bytes this build cannot read — refuses the whole log.
fn decode(bytes: &[u8]) -> Result<Record, ReadError> {
    let undecodable = |error: serde_json::Error| ReadError::Undecodable(error.to_string());
    match bytes.iter().find(|byte| !byte.is_ascii_whitespace()) {
        Some(b'[') => serde_json::from_slice(bytes)
            .map(|facts| Record { facts, host: None })
            .map_err(undecodable),
        Some(b'{') => serde_json::from_slice::<HostRow>(bytes)
            .map(|row| Record {
                facts: row.facts,
                host: Some(row.host),
            })
            .map_err(undecodable),
        _ => Err(ReadError::Undecodable(
            "a stored batch is neither an engine batch nor a host record".to_string(),
        )),
    }
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

    fn observed(actor: &str, server: &str) -> HostObservation {
        HostObservation::Inventory {
            actor: TrajectoryId::new(actor),
            adapter: AdapterName::Kagent,
            inventory: ToolInventory {
                tools: vec![appa_runtime_api::inventory::ObservedTool {
                    name: "read".into(),
                    tool: format!("mcp:{server}/read"),
                }],
                sources: Vec::new(),
            },
        }
    }

    fn observations(log: &Log) -> Vec<HostObservation> {
        log.host_records()
            .iter()
            .map(|record| record.observation.clone())
            .collect()
    }

    #[test]
    fn host_and_fact_appends_share_one_compare_and_swap() {
        let store = opened();
        let stale = store.log(&root()).unwrap();
        let observation = observed(root().as_str(), "demo");
        store.append_host(&stale, &[], &observation).unwrap();
        assert!(matches!(
            store.append(&stale, &punctuation()),
            Err(AppendError::Conflict { .. })
        ));
        assert!(matches!(
            store.append_host(&stale, &[], &observation),
            Err(AppendError::Conflict { .. })
        ));
        let seen = store.log(&root()).unwrap();
        assert_eq!(observations(&seen), vec![observation]);
        assert_eq!(
            seen.host_records()[0].seq,
            1,
            "the record names the position it landed at"
        );
        assert_eq!(seen.facts(), stale.facts());
        assert_eq!(seen.policy_file(), stale.policy_file());
        store.append(&seen, &punctuation()).unwrap();
        assert!(matches!(
            store.append_host(&seen, &[], &observed("child", "other")),
            Err(AppendError::Conflict { .. })
        ));
        assert_eq!(store.log(&root()).unwrap().host_records().len(), 1);
    }

    /// An engine batch and a host observation can be one act: a binding is durable exactly
    /// when the facts that opened the dispatch are.
    #[test]
    fn facts_and_an_observation_land_in_one_batch() {
        let store = opened();
        let log = store.log(&root()).unwrap();
        let resolved = appa_policy::Config::from_toml_str("version = 2\n[[tool]]\nname = \"read\"\n")
            .expect("the fixture policy compiles")
            .engine()
            .resolve_call(appa_engine::value::ToolName::new("read"), b"{}")
            .expect("the fixture call resolves through the engine");
        let dispatch = DispatchId::new(root(), resolved.digest(), 0);
        let bound = HostObservation::CallBound {
            trajectory: root(),
            call_id: "toolu_1".to_string(),
            dispatch: dispatch.clone(),
        };
        store.append_host(&log, &punctuation(), &bound).unwrap();

        let seen = store.log(&root()).unwrap();
        assert_eq!(seen.basis(), 2, "both streams took one position");
        assert!(matches!(
            seen.facts(),
            [Fact::TrajectoryOpened { .. }, Fact::Boundary { .. }]
        ));
        let bindings: Vec<_> = seen.call_bindings().collect();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].call_id, "toolu_1");
        assert_eq!(bindings[0].dispatch, &dispatch);
        assert_eq!(bindings[0].trajectory, &root());
    }

    /// The encoding is the shape, and a batch with no observation is byte-for-byte what an
    /// engine-only store wrote: nothing sniffs between two unrelated payloads.
    #[test]
    fn a_batch_without_an_observation_is_the_bare_array_of_its_facts() {
        assert_eq!(
            encode(&punctuation(), None),
            serde_json::to_vec(&punctuation()).unwrap()
        );
        let host = observed(root().as_str(), "demo");
        let object: serde_json::Value = serde_json::from_slice(&encode(&[], Some(&host))).unwrap();
        assert_eq!(object["facts"], serde_json::json!([]));
        assert_eq!(object["host"]["kind"], "inventory");
    }

    /// An object that is not this build's host record refuses the read rather than being
    /// dropped: a log this build cannot fully read is not one to decide under.
    #[test]
    fn an_object_that_is_not_a_host_record_refuses_the_read() {
        for row in [
            br#"{"actor":"cc:root","adapter":"kagent","inventory":{}}"#.as_slice(),
            br#"{"facts":[],"host":{"kind":"from_a_later_build"}}"#.as_slice(),
            b"not json at all",
        ] {
            let store = opened();
            store
                .lock()
                .execute(
                    "INSERT INTO logs (root, seq, facts) VALUES (?1, 1, ?2)",
                    params![root().as_str(), row],
                )
                .expect("the row lands");
            assert!(
                matches!(store.log(&root()), Err(ReadError::Undecodable(_))),
                "{}",
                String::from_utf8_lossy(row)
            );
        }
    }

    #[test]
    fn host_observations_survive_reopen_without_changing_the_policy_or_engine_facts() {
        let dir = tempfile::tempdir().unwrap();
        let backend = Backend::Sqlite {
            path: dir.path().join("inventory.db"),
        };
        let parent = observed(root().as_str(), "demo");
        let child = observed("child", "other");
        {
            let store = LogStore::open(backend.clone()).unwrap();
            store.create_root(opening(&root()), POLICY.as_bytes()).unwrap();
            store.append_host(&store.log(&root()).unwrap(), &[], &parent).unwrap();
            store.append_host(&store.log(&root()).unwrap(), &[], &child).unwrap();
        }
        let store = LogStore::open(backend).unwrap();
        let log = store.log(&root()).unwrap();
        assert_eq!(observations(&log), vec![parent, child]);
        assert_eq!(log.basis(), 3);
        assert_eq!(log.facts(), opening(&root()));
        assert_eq!(log.policy_file(), POLICY.as_bytes());
    }

    /// The query a reader uses when it knows the key but not the family that recorded it:
    /// the roots that named the key, and no root that named another.
    #[test]
    fn the_needle_finds_the_roots_that_name_a_key_and_no_others() {
        let store = opened();
        let second = TrajectoryId::new("cc:second");
        store.create_root(opening(&second), POLICY.as_bytes()).unwrap();
        let vouched = |root: &TrajectoryId, key: &str| HostObservation::Vouched {
            actor: HostActor {
                root: root.clone(),
                child: None,
            },
            key: key.to_string(),
            ruling: None,
        };
        store.append(&store.log(&root()).unwrap(), &punctuation()).unwrap();
        store
            .append_host(&store.log(&root()).unwrap(), &[], &vouched(&root(), "offer:one"))
            .unwrap();
        store
            .append_host(&store.log(&second).unwrap(), &[], &vouched(&second, "offer:one"))
            .unwrap();
        store
            .append_host(&store.log(&second).unwrap(), &[], &vouched(&second, "offer:two"))
            .unwrap();
        // A second row naming the same key, so a root that recorded twice is named once.
        store
            .append_host(&store.log(&second).unwrap(), &[], &vouched(&second, "offer:one"))
            .unwrap();
        store.append(&store.log(&second).unwrap(), &punctuation()).unwrap();

        assert_eq!(
            store
                .roots_mentioning(&HostObservation::names_key("offer:one"))
                .unwrap(),
            vec![root(), second.clone()],
            "one entry per root, however many of its rows name the key"
        );
        assert_eq!(
            store
                .roots_mentioning(&HostObservation::names_key("offer:two"))
                .unwrap(),
            vec![second.clone()],
            "a key nothing else names answers with the one root that does"
        );
        assert_eq!(
            store.roots_mentioning(&HostObservation::names_key("offer:on")).unwrap(),
            Vec::new(),
            "the needle carries the closing quote, so one key is never a prefix of another"
        );
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

    /// Run against an Archestra-migrated, disposable PostgreSQL database:
    /// OPENAPPA_TEST_DATABASE_URL=... cargo test -p appa-eventlog --features postgres -- --ignored
    #[cfg(feature = "postgres")]
    #[test]
    #[ignore = "requires OPENAPPA_TEST_DATABASE_URL and host migrations"]
    fn postgres_preserves_encoding_cas_and_outer_transaction_atomicity() {
        let url = std::env::var("OPENAPPA_TEST_DATABASE_URL").expect("test database URL");
        let first = LogStore::open(Backend::Postgres { url: url.clone() }).unwrap();
        let second = LogStore::open(Backend::Postgres { url }).unwrap();
        let unique = tempfile::tempdir().unwrap();
        let id = TrajectoryId::new(format!("pg-test:{}", unique.path().display()));
        let facts = vec![Fact::Boundary {
            trajectory: id.clone(),
            kind: appa_engine::fact::BoundaryKind::VoidReturn,
        }];
        let sqlite = memory();
        first.create_root(opening(&id), POLICY.as_bytes()).unwrap();
        sqlite.create_root(opening(&id), POLICY.as_bytes()).unwrap();
        assert_eq!(first.log(&id).unwrap(), sqlite.log(&id).unwrap());
        assert!(matches!(
            second.create_root(opening(&id), POLICY.as_bytes()),
            Err(CreateError::AlreadyExists { .. })
        ));
        assert!(matches!(
            first.log(&TrajectoryId::new("pg-test:ghost")),
            Err(ReadError::UnknownRoot { .. }),
        ));

        let before = first.log(&id).unwrap();
        let tx = first.postgres().unwrap().begin().unwrap();
        first.append(&before, &facts).unwrap();
        first.append(&first.log(&id).unwrap(), &facts).unwrap();
        assert_eq!(first.log(&id).unwrap().basis(), 3);
        assert_eq!(
            second.log(&id).unwrap(),
            before,
            "uncommitted hook writes are invisible"
        );
        drop(tx);
        assert_eq!(first.log(&id).unwrap(), before, "all hook writes roll back together");

        let tx = first.postgres().unwrap().begin().unwrap();
        first.append(&before, &facts).unwrap();
        tx.commit().unwrap();
        assert!(matches!(
            second.append(&before, &facts),
            Err(AppendError::Conflict { current: 2 })
        ));
        sqlite.append(&sqlite.log(&id).unwrap(), &facts).unwrap();
        assert_eq!(second.log(&id).unwrap(), sqlite.log(&id).unwrap());

        let observation = observed(id.as_str(), "demo");
        first.append_host(&first.log(&id).unwrap(), &[], &observation).unwrap();
        sqlite
            .append_host(&sqlite.log(&id).unwrap(), &[], &observation)
            .unwrap();
        assert_eq!(second.log(&id).unwrap(), sqlite.log(&id).unwrap());
        assert_eq!(
            first.log(&id).unwrap().host_records(),
            sqlite.log(&id).unwrap().host_records(),
            "one root's host records read the same on both backends"
        );

        let vouched = HostObservation::Vouched {
            actor: HostActor {
                root: id.clone(),
                child: None,
            },
            key: "offer:one".to_string(),
            ruling: None,
        };
        let before_vouch = first.log(&id).unwrap();
        first.append_host(&before_vouch, &[], &vouched).unwrap();
        assert!(
            matches!(
                first.append_host(&before_vouch, &[], &vouched),
                Err(AppendError::Conflict { .. }),
            ),
            "a host observation is appended against its read position on this backend too"
        );
        assert!(
            second
                .roots_mentioning(&HostObservation::names_key("offer:one"))
                .unwrap()
                .contains(&id),
            "the needle names the root that recorded the key"
        );
        assert_eq!(
            second
                .roots_mentioning(&HostObservation::names_key("offer:two"))
                .unwrap(),
            Vec::new(),
            "and nothing for a key no row names"
        );
        assert_eq!(
            second
                .log(&id)
                .unwrap()
                .host_records()
                .iter()
                .rev()
                .find(|record| matches!(&record.observation, HostObservation::Vouched { .. })),
            Some(&HostRecord {
                seq: 4,
                observation: vouched,
            }),
            "and the record reads the same through the connection thread"
        );

        let stale = first.log(&id).unwrap();
        let barrier = std::sync::Barrier::new(2);
        let outcomes = std::thread::scope(|scope| {
            let a = scope.spawn(|| {
                barrier.wait();
                first.append(&stale, &facts)
            });
            let b = scope.spawn(|| {
                barrier.wait();
                second.append(&stale, &facts)
            });
            [a.join().unwrap(), b.join().unwrap()]
        });
        assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            outcomes
                .iter()
                .filter(|result| matches!(result, Err(AppendError::Conflict { current: 4 })))
                .count(),
            1
        );
        assert_eq!(second.log(&id).unwrap().basis(), 4);

        let bound = HostObservation::CallBound {
            trajectory: id.clone(),
            call_id: "host-call-1".into(),
            dispatch: DispatchId::new(
                id.clone(),
                serde_json::from_value(serde_json::json!("00".repeat(32))).unwrap(),
                0,
            ),
        };
        let bindings = |log: &Log| {
            log.call_bindings()
                .map(|binding| (binding.call_id.to_string(), binding.dispatch.clone()))
                .collect::<Vec<_>>()
        };
        let HostObservation::CallBound { call_id, dispatch, .. } = &bound else {
            unreachable!("built above");
        };
        let expected = vec![(call_id.clone(), dispatch.clone())];
        let before = first.log(&id).unwrap();
        let tx = first.postgres().unwrap().begin().unwrap();
        first.append_host(&before, &facts, &bound).unwrap();
        assert_eq!(bindings(&first.log(&id).unwrap()), expected);
        assert_eq!(second.log(&id).unwrap(), before);
        drop(tx);
        assert_eq!(first.log(&id).unwrap(), before, "binding and facts roll back together");

        first.append_host(&before, &facts, &bound).unwrap();
        let restored = second.log(&id).unwrap();
        assert_eq!(bindings(&restored), expected);
        assert_eq!(restored.facts().len(), before.facts().len() + facts.len());
        assert!(matches!(
            second.append_host(&before, &facts, &bound),
            Err(AppendError::Conflict { current: 5 })
        ));

        first
            .postgres()
            .unwrap()
            .with_client(move |client| {
                client.execute("DELETE FROM openappa_events WHERE root=$1", &[&id.as_str()])?;
                Ok(())
            })
            .unwrap();
    }
}
