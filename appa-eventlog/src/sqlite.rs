//! SQLite storage: the standalone daemon's database file, and the private `:memory:` database
//! of [`Backend::Memory`](crate::Backend::Memory). This module owns the schema, its version
//! stamp, and every statement either runs.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use appa_engine::profile::PolicyFileKey;
use appa_engine::value::TrajectoryId;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::Value;

#[cfg(feature = "fault-injection")]
use crate::HostObservation;
#[cfg(feature = "fault-injection")]
use crate::encoding::encode;
use crate::encoding::{contiguous, decoded, opening_key};
use crate::receipts::{
    Completion, OfferOwnerKey, OfferOwnerRecord, OperationClaim, OperationKey, OperationRequest, ProcessedResultClaim,
    ProcessedResultKey, ProcessedResultRequest, ReceiptError, ReceiptScope, ReceiptStorageError, StoredJson,
    StoredOperation, StoredOperationInput, StoredResult, binding_name, parse_binding, resolve_offer_owner,
    resolve_operation_claim, resolve_operation_completion, resolve_result_claim, resolve_result_completion,
};
use crate::{AppendError, CreateError, Log, OpenError, ReadError};

const SCHEMA_VERSION: i64 = 3;

const SCHEMA: &str = "CREATE TABLE logs (
                         root  TEXT NOT NULL,
                         seq   INTEGER NOT NULL,
                         facts BLOB NOT NULL,
                         PRIMARY KEY (root, seq)
                     );
                     CREATE TABLE policy_files (
                         key   TEXT PRIMARY KEY,
                         bytes BLOB NOT NULL
                     );
                     CREATE TABLE host_keys (
                         key  TEXT NOT NULL,
                         root TEXT NOT NULL,
                         PRIMARY KEY (key, root)
                     );
                     CREATE TABLE offer_owners (
                         organization_id TEXT NOT NULL,
                         caller_id TEXT,
                         session_id TEXT NOT NULL,
                         binding TEXT NOT NULL,
                         offer_id TEXT NOT NULL,
                         root TEXT NOT NULL,
                         parent_id TEXT,
                         arguments TEXT,
                         tool TEXT,
                         spelling TEXT,
                         PRIMARY KEY (organization_id, offer_id)
                     );
                     CREATE TABLE operations (
                         organization_id TEXT NOT NULL,
                         caller_id TEXT,
                         session_id TEXT NOT NULL,
                         operation_id TEXT NOT NULL,
                         root TEXT NOT NULL,
                         input TEXT NOT NULL,
                         status TEXT NOT NULL,
                         decision TEXT,
                         PRIMARY KEY (session_id, operation_id)
                     );
                     CREATE TABLE processed_results (
                         organization_id TEXT NOT NULL,
                         caller_id TEXT,
                         session_id TEXT NOT NULL,
                         tool_call_id TEXT NOT NULL,
                         root TEXT NOT NULL,
                         status TEXT NOT NULL,
                         approved_output TEXT,
                         decision TEXT,
                         PRIMARY KEY (session_id, tool_call_id)
                     );";

pub(crate) struct Sqlite(Mutex<Connection>);

impl Sqlite {
    /// Open the file, refusing one that is damaged, then install or check the schema.
    pub(crate) fn open(path: &Path) -> Result<Self, OpenError> {
        let connection = Connection::open(path)?;
        let path = path.display().to_string();
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
        Self::install(connection, path)
    }

    pub(crate) fn memory() -> Result<Self, OpenError> {
        Self::install(Connection::open_in_memory()?, ":memory:".to_string())
    }

    /// Give a fresh database the schema and its version stamp, or check an existing one.
    fn install(mut connection: Connection, path: String) -> Result<Self, OpenError> {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: i64 = transaction.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        // Only an empty file is initialized. A database that holds tables
        // but carries no stamp was written by something else — an earlier
        // store, another tool — and creating this schema beside its data
        // would leave its histories present and invisible.
        if version == 0 && is_empty(&transaction)? {
            transaction.execute_batch(SCHEMA)?;
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
        Ok(Self(Mutex::new(connection)))
    }

    pub(crate) fn connection(&self) -> MutexGuard<'_, Connection> {
        self.0
            .lock()
            .expect("the log store mutex is never poisoned: no panics under the lock")
    }

    /// Store a root's opening batch and the policy file it opens under, in one transaction.
    /// `before_commit` runs last inside it, and an error from it rolls the opening back.
    pub(crate) fn create(
        &self,
        root: &TrajectoryId,
        key: &PolicyFileKey,
        policy_file: &[u8],
        bytes: &[u8],
        before_commit: impl FnOnce() -> Result<(), CreateError>,
    ) -> Result<(), CreateError> {
        immediate(&mut self.connection(), |transaction| {
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
            before_commit()
        })
    }

    /// The connection an append runs on, held until the append is done.
    pub(crate) fn appender(&self) -> Appender<'_> {
        Appender(self.connection())
    }

    pub(crate) fn has_root(&self, root: &TrajectoryId) -> Result<bool, ReadError> {
        let found: Option<i64> = self
            .connection()
            .query_row(
                "SELECT 1 FROM logs WHERE root = ?1 LIMIT 1",
                params![root.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }

    pub(crate) fn log(&self, root: &TrajectoryId) -> Result<Log, ReadError> {
        let (batches, policy_file) = {
            let connection = self.connection();
            let mut statement = connection.prepare("SELECT seq, facts FROM logs WHERE root = ?1 ORDER BY seq ASC")?;
            let rows = statement
                .query_map(params![root.as_str()], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            let batches = contiguous(rows).map_err(|gap| {
                ReadError::Storage(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
                    Some(gap.to_string()),
                ))
            })?;
            let key = opening_key(root, &batches)?;
            let policy_file = connection
                .query_row(
                    "SELECT bytes FROM policy_files WHERE key = ?1",
                    params![key.as_str()],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or_else(|| ReadError::PolicyFileMissing {
                    key: key.as_str().to_string(),
                })?;
            (batches, policy_file)
        };
        decoded(root, batches, policy_file)
    }

    pub(crate) fn roots_mentioning(&self, key: &str) -> Result<Vec<TrajectoryId>, ReadError> {
        let connection = self.connection();
        let mut statement = connection.prepare("SELECT root FROM host_keys WHERE key = ?1 ORDER BY root ASC")?;
        let roots = statement
            .query_map(params![key], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(roots.into_iter().map(TrajectoryId::new).collect())
    }

    pub(crate) fn store_offer_owner(&self, record: &OfferOwnerRecord) -> Result<(), ReceiptError> {
        immediate(&mut self.connection(), |connection| {
            let inserted = connection.execute(
                "INSERT INTO offer_owners (organization_id, caller_id, session_id, binding, offer_id, root, parent_id, arguments, tool, spelling)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
                 ON CONFLICT (organization_id, offer_id) DO NOTHING",
                params![
                    record.scope.organization_id,
                    record.scope.caller_id,
                    record.scope.session_id,
                    binding_name(record.scope.binding),
                    record.offer_id,
                    record.root,
                    record.parent_id,
                    record.arguments,
                    record.tool,
                    record.spelling,
                ],
            )?;
            if inserted == 1 {
                return Ok(());
            }
            let key = OfferOwnerKey {
                organization_id: record.scope.organization_id.clone(),
                offer_id: record.offer_id.clone(),
            };
            resolve_offer_owner(read_offer_owner(connection, &key)?, record)
        })
    }

    pub(crate) fn offer_owner(&self, key: &OfferOwnerKey) -> Result<Option<OfferOwnerRecord>, ReceiptStorageError> {
        read_offer_owner(&self.connection(), key)
    }

    pub(crate) fn expire_offer_owners(&self, scope: &ReceiptScope) -> Result<u64, ReceiptStorageError> {
        immediate(&mut self.connection(), |connection| {
            Ok(connection.execute(
                "DELETE FROM offer_owners WHERE organization_id=?1 AND caller_id IS ?2 AND session_id=?3",
                params![scope.organization_id, scope.caller_id, scope.session_id],
            )? as u64)
        })
    }

    pub(crate) fn claim_operation(&self, request: &OperationRequest) -> Result<OperationClaim, ReceiptError> {
        immediate(&mut self.connection(), |connection| {
            let claim = resolve_operation_claim(read_operation(connection, &request.key)?, request)?;
            if claim == OperationClaim::Claimed {
                let stored = serde_json::to_string(&StoredOperationInput::from_request(request))
                    .map_err(|error| ReceiptError::storage(error.to_string()))?;
                connection.execute(
                    "INSERT INTO operations (organization_id, caller_id, session_id, operation_id, root, input, status)
                     VALUES (?1,?2,?3,?4,?5,?6,'pending')",
                    params![
                        request.key.scope.organization_id,
                        request.key.scope.caller_id,
                        request.key.scope.session_id,
                        request.key.operation_id,
                        request.root,
                        stored,
                    ],
                )?;
            }
            Ok(claim)
        })
    }

    pub(crate) fn complete_operation(&self, key: &OperationKey, decision: &Value) -> Result<(), ReceiptError> {
        immediate(&mut self.connection(), |connection| {
            let existing = read_operation(connection, key)?;
            if let Completion::Write = resolve_operation_completion(existing, key, decision)? {
                let encoded =
                    serde_json::to_string(decision).map_err(|error| ReceiptError::storage(error.to_string()))?;
                connection.execute(
                    "UPDATE operations SET status='complete', decision=?3 WHERE session_id=?1 AND operation_id=?2",
                    params![key.scope.session_id, key.operation_id, encoded],
                )?;
            }
            Ok(())
        })
    }

    pub(crate) fn claim_processed_result(
        &self,
        request: &ProcessedResultRequest,
    ) -> Result<ProcessedResultClaim, ReceiptError> {
        immediate(&mut self.connection(), |connection| {
            let claim = resolve_result_claim(read_result(connection, &request.key)?, request)?;
            if claim == ProcessedResultClaim::Claimed {
                connection.execute(
                    "INSERT INTO processed_results (organization_id, caller_id, session_id, tool_call_id, root, status)
                     VALUES (?1,?2,?3,?4,?5,'pending')",
                    params![
                        request.key.organization_id,
                        request.key.caller_id,
                        request.key.session_id,
                        request.key.tool_call_id,
                        request.root,
                    ],
                )?;
            }
            Ok(claim)
        })
    }

    pub(crate) fn complete_processed_result(
        &self,
        key: &ProcessedResultKey,
        approved_output: &str,
        decision: &Value,
    ) -> Result<(), ReceiptError> {
        immediate(&mut self.connection(), |connection| {
            let existing = read_result(connection, key)?;
            if let Completion::Write = resolve_result_completion(existing, key, approved_output, decision)? {
                let encoded =
                    serde_json::to_string(decision).map_err(|error| ReceiptError::storage(error.to_string()))?;
                connection.execute(
                    "UPDATE processed_results SET status='complete', approved_output=?3, decision=?4
                     WHERE session_id=?1 AND tool_call_id=?2",
                    params![key.session_id, key.tool_call_id, approved_output, encoded],
                )?;
            }
            Ok(())
        })
    }

    pub(crate) fn has_pending_receipts(&self, root: &str) -> Result<bool, ReceiptStorageError> {
        let found: Option<i64> = self
            .connection()
            .query_row(
                "SELECT 1 FROM operations WHERE root=?1 AND status='pending'
                 UNION ALL
                 SELECT 1 FROM processed_results WHERE root=?1 AND status='pending'
                 LIMIT 1",
                params![root],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }
}

/// A held connection that one append runs on.
pub(crate) struct Appender<'a>(MutexGuard<'a, Connection>);

impl Appender<'_> {
    /// Append one batch at `basis`, or refuse because the log has moved past it. `before_commit`
    /// runs last inside the transaction, and an error from it writes nothing.
    pub(crate) fn append(
        mut self,
        root: &TrajectoryId,
        basis: u64,
        bytes: &[u8],
        key: Option<&str>,
        before_commit: impl FnOnce() -> Result<(), AppendError>,
    ) -> Result<(), AppendError> {
        immediate(&mut self.0, |transaction| {
            let current = position(transaction, root)?;
            if current != basis {
                return Err(AppendError::Conflict { current });
            }
            insert_batch(transaction, root, current, bytes, key)?;
            before_commit()
        })
    }

    /// Commit `records` at the head of their logs in one transaction of their own, as another
    /// writer racing this append would. `None` records nothing and only takes the position.
    #[cfg(feature = "fault-injection")]
    pub(crate) fn foreign(
        &mut self,
        records: &[(&TrajectoryId, Option<&HostObservation>)],
    ) -> Result<(), rusqlite::Error> {
        immediate(&mut self.0, |transaction| {
            for (root, observation) in records {
                let at = position(transaction, root)?;
                let key = observation.and_then(HostObservation::key);
                insert_batch(transaction, root, at, &encode(&[], *observation), key)?;
            }
            Ok(())
        })
    }
}

/// Damage stated in the log's own vocabulary, for callers that pin how a read refuses it.
#[cfg(feature = "fault-injection")]
impl Sqlite {
    pub(crate) fn forget_policy_files(&self) {
        self.connection()
            .execute("DELETE FROM policy_files", [])
            .expect("the damage lands");
    }

    pub(crate) fn corrupt_policy_files(&self, bytes: &[u8]) {
        self.connection()
            .execute("UPDATE policy_files SET bytes = ?1", params![bytes])
            .expect("the damage lands");
    }

    /// How many batches now hold `bytes`: one where the batch exists.
    pub(crate) fn corrupt_batch(&self, root: &TrajectoryId, seq: u64, bytes: &[u8]) -> usize {
        self.connection()
            .execute(
                "UPDATE logs SET facts = ?3 WHERE root = ?1 AND seq = ?2",
                params![root.as_str(), seq as i64, bytes],
            )
            .expect("the damage lands")
    }
}

impl From<rusqlite::Error> for ReceiptStorageError {
    fn from(error: rusqlite::Error) -> Self {
        Self(error.to_string())
    }
}

impl From<rusqlite::Error> for ReceiptError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Storage(error.into())
    }
}

/// Run `operation` in an immediate transaction, committed only when it succeeds.
fn immediate<T, E: From<rusqlite::Error>>(
    connection: &mut Connection,
    operation: impl FnOnce(&Transaction<'_>) -> Result<T, E>,
) -> Result<T, E> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let result = operation(&transaction)?;
    transaction.commit()?;
    Ok(result)
}

fn insert_batch(
    connection: &Connection,
    root: &TrajectoryId,
    at: u64,
    bytes: &[u8],
    key: Option<&str>,
) -> Result<(), rusqlite::Error> {
    connection.execute(
        "INSERT INTO logs (root, seq, facts) VALUES (?1, ?2, ?3)",
        params![root.as_str(), at as i64, bytes],
    )?;
    if let Some(key) = key {
        connection.execute(
            "INSERT OR IGNORE INTO host_keys (key, root) VALUES (?1, ?2)",
            params![key, root.as_str()],
        )?;
    }
    Ok(())
}

fn position(connection: &Connection, root: &TrajectoryId) -> Result<u64, rusqlite::Error> {
    let next: i64 = connection.query_row(
        "SELECT COALESCE(MAX(seq) + 1, 0) FROM logs WHERE root = ?1",
        params![root.as_str()],
        |row| row.get(0),
    )?;
    Ok(next as u64)
}

fn is_taken(error: &rusqlite::Error) -> bool {
    const PRIMARY_KEY: i32 = 1555;
    const UNIQUE: i32 = 2067;
    matches!(
        error,
        rusqlite::Error::SqliteFailure(e, _) if e.extended_code == PRIMARY_KEY || e.extended_code == UNIQUE
    )
}

fn has_schema(connection: &Connection) -> Result<bool, rusqlite::Error> {
    let found: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('logs', 'policy_files', 'host_keys', 'offer_owners', 'operations', 'processed_results')",
        [],
        |row| row.get(0),
    )?;
    Ok(found == 6)
}

fn is_empty(connection: &Connection) -> Result<bool, rusqlite::Error> {
    let tables: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get(0),
    )?;
    Ok(tables == 0)
}

fn read_offer_owner(
    connection: &Connection,
    key: &OfferOwnerKey,
) -> Result<Option<OfferOwnerRecord>, ReceiptStorageError> {
    let row = connection
        .query_row(
            "SELECT caller_id, session_id, binding, root, parent_id, arguments, tool, spelling
             FROM offer_owners WHERE organization_id=?1 AND offer_id=?2",
            params![key.organization_id, key.offer_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get::<_, String>(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        )
        .optional()?;
    let Some((caller_id, session_id, binding, root, parent_id, arguments, tool, spelling)) = row else {
        return Ok(None);
    };
    Ok(Some(OfferOwnerRecord {
        scope: ReceiptScope {
            organization_id: key.organization_id.clone(),
            caller_id,
            session_id,
            binding: parse_binding(&binding).map_err(ReceiptStorageError)?,
        },
        offer_id: key.offer_id.clone(),
        root,
        parent_id,
        arguments,
        tool,
        spelling,
    }))
}

fn read_operation(connection: &Connection, key: &OperationKey) -> Result<Option<StoredOperation>, ReceiptError> {
    connection
        .query_row(
            "SELECT organization_id, caller_id, session_id, root, input, status, decision
             FROM operations WHERE session_id=?1 AND operation_id=?2",
            params![key.scope.session_id, key.operation_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get::<_, String>(4)?,
                    row.get(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            },
        )
        .optional()?
        .map(
            |(organization_id, caller_id, session_id, root, input, status, decision)| {
                Ok(StoredOperation {
                    organization_id,
                    caller_id,
                    session_id,
                    root,
                    input: StoredOperationInput::decode(json(&input)?).map_err(ReceiptStorageError)?,
                    status,
                    decision: decision.as_deref().map(json),
                })
            },
        )
        .transpose()
}

fn read_result(connection: &Connection, key: &ProcessedResultKey) -> Result<Option<StoredResult>, ReceiptError> {
    Ok(connection
        .query_row(
            "SELECT organization_id, session_id, root, status, approved_output, decision
             FROM processed_results WHERE session_id=?1 AND tool_call_id=?2",
            params![key.session_id, key.tool_call_id],
            |row| {
                Ok(StoredResult {
                    organization_id: row.get(0)?,
                    session_id: row.get(1)?,
                    root: row.get(2)?,
                    status: row.get(3)?,
                    approved_output: row.get(4)?,
                    decision: row.get::<_, Option<String>>(5)?.as_deref().map(json),
                })
            },
        )
        .optional()?)
}

fn json(raw: &str) -> StoredJson {
    serde_json::from_str(raw).map_err(|error| ReceiptStorageError(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Backend, LogStore, OpenError};

    /// The SQLite schema is a persisted format: its version stamp and the DDL of each table,
    /// compared token for token.
    #[test]
    fn a_fresh_sqlite_store_has_the_frozen_schema() {
        let normalize = |sql: &str| sql.split_whitespace().collect::<Vec<_>>().join(" ");
        let expected: Vec<(String, String, Option<String>)> = [
            ("host_keys", Some("CREATE TABLE host_keys ( key TEXT NOT NULL, root TEXT NOT NULL, PRIMARY KEY (key, root) )")),
            ("logs", Some("CREATE TABLE logs ( root TEXT NOT NULL, seq INTEGER NOT NULL, facts BLOB NOT NULL, PRIMARY KEY (root, seq) )")),
            ("offer_owners", Some("CREATE TABLE offer_owners ( organization_id TEXT NOT NULL, caller_id TEXT, session_id TEXT NOT NULL, binding TEXT NOT NULL, offer_id TEXT NOT NULL, root TEXT NOT NULL, parent_id TEXT, arguments TEXT, tool TEXT, spelling TEXT, PRIMARY KEY (organization_id, offer_id) )")),
            ("operations", Some("CREATE TABLE operations ( organization_id TEXT NOT NULL, caller_id TEXT, session_id TEXT NOT NULL, operation_id TEXT NOT NULL, root TEXT NOT NULL, input TEXT NOT NULL, status TEXT NOT NULL, decision TEXT, PRIMARY KEY (session_id, operation_id) )")),
            ("policy_files", Some("CREATE TABLE policy_files ( key TEXT PRIMARY KEY, bytes BLOB NOT NULL )")),
            ("processed_results", Some("CREATE TABLE processed_results ( organization_id TEXT NOT NULL, caller_id TEXT, session_id TEXT NOT NULL, tool_call_id TEXT NOT NULL, root TEXT NOT NULL, status TEXT NOT NULL, approved_output TEXT, decision TEXT, PRIMARY KEY (session_id, tool_call_id) )")),
            ("sqlite_autoindex_host_keys_1", None),
            ("sqlite_autoindex_logs_1", None),
            ("sqlite_autoindex_offer_owners_1", None),
            ("sqlite_autoindex_operations_1", None),
            ("sqlite_autoindex_policy_files_1", None),
            ("sqlite_autoindex_processed_results_1", None),
        ]
        .into_iter()
        .map(|(name, sql)| {
            let kind = if sql.is_some() { "table" } else { "index" };
            (kind.to_owned(), name.to_owned(), sql.map(str::to_owned))
        })
        .collect();

        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let backends = [
            Backend::Sqlite {
                path: dir.path().join("appa.db"),
            },
            Backend::Memory,
        ];
        for backend in backends {
            let store = LogStore::open(backend).expect("a fresh store opens");
            let connection = store.lock();
            let version: i64 = connection
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .expect("the version reads");
            assert_eq!(version, 3);
            let mut statement = connection
                .prepare("SELECT type, name, sql FROM sqlite_master ORDER BY name")
                .expect("the schema query prepares");
            let found = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?.map(|sql| normalize(&sql)),
                    ))
                })
                .expect("the schema reads")
                .collect::<Result<Vec<_>, _>>()
                .expect("every schema row reads");
            assert_eq!(found, expected);
        }
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
}
