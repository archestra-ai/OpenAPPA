//! Typed offer-owner routing and idempotent operation receipts.
//!
//! These records are not engine facts. They are host integration state: which authenticated
//! scope minted an offer, and whether an operation or processed result is claimed, pending, or
//! complete. Offer validity still rehydrates from the log. SQLite and Memory keep the rows in
//! the same database as the log; PostgreSQL hosts install the equivalent `openappa_*` tables
//! through their own migrations.

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

use super::{LogStore, Store, immediate, lock};

/// Scope binding for a durable receipt: conversation session or authenticated caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptBinding {
    Session,
    Caller,
}

/// Authenticated scope associated with a durable integration receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptScope {
    pub organization_id: String,
    pub caller_id: Option<String>,
    pub session_id: String,
    pub binding: ReceiptBinding,
}

/// Routing metadata for a typed remedy offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfferOwnerRecord {
    pub scope: ReceiptScope,
    pub offer_id: String,
    pub root: String,
    pub parent_id: Option<String>,
    pub arguments: Option<String>,
    pub tool: Option<String>,
    pub spelling: Option<String>,
}

/// The stable lookup key for a durable offer owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfferOwnerKey {
    pub organization_id: String,
    pub offer_id: String,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ReceiptStorageError(pub String);

impl From<rusqlite::Error> for ReceiptStorageError {
    fn from(error: rusqlite::Error) -> Self {
        Self(error.to_string())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReceiptError {
    #[error("receipt storage failed: {0}")]
    Storage(ReceiptStorageError),
    #[error("a different owner record already exists for this offer")]
    Collision,
    #[error("receipt belongs to another authenticated scope")]
    ScopeMismatch,
    #[error("receipt key was reused with different input")]
    InputMismatch,
    #[error("receipt is pending recovery and must not be replayed")]
    Pending,
    #[error("receipt is absent or no longer pending")]
    NotPending,
    #[error("receipt was completed with a different value")]
    CompletionMismatch,
}

/// The durable request key for an operation receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationKey {
    pub scope: ReceiptScope,
    pub operation_id: String,
}

/// Immutable request recorded before executing work to ensure idempotent replay.
#[derive(Debug, Clone, PartialEq)]
pub struct OperationRequest {
    pub key: OperationKey,
    pub root: String,
    /// The stable part used to detect a conflicting retry.
    pub input: Value,
    /// Optional non-semantic host context retained for a later result hook.
    /// Changes here never alter the idempotency contract.
    pub context: Option<Value>,
}

/// The result of claiming one operation receipt.
#[derive(Debug, Clone, PartialEq)]
pub enum OperationClaim {
    Claimed,
    Complete { decision: Value },
}

/// The durable key for a processed tool result receipt. It is always session-bound: the
/// caller is recorded but never compared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessedResultKey {
    pub organization_id: String,
    pub caller_id: Option<String>,
    pub session_id: String,
    pub tool_call_id: String,
}

impl ProcessedResultKey {
    pub(crate) fn owns(&self, organization_id: &str, session_id: &str) -> bool {
        self.organization_id == organization_id && self.session_id == session_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessedResultRequest {
    pub key: ProcessedResultKey,
    pub root: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProcessedResultClaim {
    Claimed,
    Complete { approved_output: String, decision: Value },
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredOperationInput {
    pub(crate) version: u8,
    pub(crate) binding: ReceiptBinding,
    pub(crate) semantic: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) context: Option<Value>,
}

impl StoredOperationInput {
    pub(crate) fn from_request(request: &OperationRequest) -> Self {
        Self {
            version: 1,
            binding: request.key.scope.binding,
            semantic: request.input.clone(),
            context: request.context.clone(),
        }
    }

    pub(crate) fn decode(value: Value) -> Result<Self, ReceiptStorageError> {
        match serde_json::from_value::<Self>(value) {
            Ok(stored) if stored.version == 1 => Ok(stored),
            Ok(stored) => Err(ReceiptStorageError(format!(
                "operation receipt has unsupported version {}",
                stored.version
            ))),
            Err(error) => Err(ReceiptStorageError(format!(
                "operation receipt input is not a v1 envelope: {error}"
            ))),
        }
    }
}

/// A stored JSON column, decoded when read but refused only where a check reaches it.
pub(crate) type StoredJson = Result<Value, ReceiptStorageError>;

/// One operation receipt row, its input envelope decoded.
pub(crate) struct StoredOperation {
    pub(crate) organization_id: String,
    pub(crate) caller_id: Option<String>,
    pub(crate) session_id: String,
    pub(crate) root: String,
    pub(crate) input: StoredOperationInput,
    pub(crate) status: String,
    pub(crate) decision: Option<StoredJson>,
}

impl StoredOperation {
    fn owned_by(&self, requested: &ReceiptScope) -> bool {
        scope_matches(
            &ReceiptScope {
                organization_id: self.organization_id.clone(),
                caller_id: self.caller_id.clone(),
                session_id: self.session_id.clone(),
                binding: self.input.binding,
            },
            requested,
        )
    }
}

/// One processed-result receipt row.
pub(crate) struct StoredResult {
    pub(crate) organization_id: String,
    pub(crate) session_id: String,
    pub(crate) root: String,
    pub(crate) status: String,
    pub(crate) approved_output: Option<String>,
    pub(crate) decision: Option<StoredJson>,
}

/// What a completion does to the row it found.
pub(crate) enum Completion {
    /// The receipt was pending: record the completion.
    Write,
    /// The receipt already holds this very completion.
    Unchanged,
}

pub(crate) fn binding_name(binding: ReceiptBinding) -> &'static str {
    match binding {
        ReceiptBinding::Session => "session",
        ReceiptBinding::Caller => "caller",
    }
}

pub(crate) fn parse_binding(value: &str) -> Result<ReceiptBinding, String> {
    match value {
        "session" => Ok(ReceiptBinding::Session),
        "caller" => Ok(ReceiptBinding::Caller),
        other => Err(format!("offer owner has an invalid binding {other}")),
    }
}

fn scope_matches(saved: &ReceiptScope, requested: &ReceiptScope) -> bool {
    if saved.organization_id != requested.organization_id
        || saved.session_id != requested.session_id
        || saved.binding != requested.binding
    {
        return false;
    }
    match saved.binding {
        ReceiptBinding::Session => true,
        ReceiptBinding::Caller => saved.caller_id == requested.caller_id,
    }
}

/// An owner write that met a stored row: an identical record is a replay, any other collides.
pub(crate) fn resolve_offer_owner(
    existing: Option<OfferOwnerRecord>,
    record: &OfferOwnerRecord,
) -> Result<(), ReceiptError> {
    match existing {
        None => Err(ReceiptError::storage("offer owner disappeared after a conflict")),
        Some(existing) if existing == *record => Ok(()),
        Some(_) => Err(ReceiptError::Collision),
    }
}

/// A claim on an absent receipt takes it: [`OperationClaim::Claimed`] tells the backend to
/// write the pending row. A completed one replays its decision.
pub(crate) fn resolve_operation_claim(
    existing: Option<StoredOperation>,
    request: &OperationRequest,
) -> Result<OperationClaim, ReceiptError> {
    let Some(stored) = existing else {
        return Ok(OperationClaim::Claimed);
    };
    if !stored.owned_by(&request.key.scope) || stored.root != request.root {
        return Err(ReceiptError::ScopeMismatch);
    }
    if stored.input.semantic != request.input {
        return Err(ReceiptError::InputMismatch);
    }
    match stored.status.as_str() {
        "complete" => Ok(OperationClaim::Complete {
            decision: stored
                .decision
                .ok_or_else(|| ReceiptError::storage("complete operation lacks a decision"))??,
        }),
        "pending" => Err(ReceiptError::Pending),
        _ => Err(ReceiptError::storage("operation receipt has an invalid status")),
    }
}

pub(crate) fn resolve_operation_completion(
    existing: Option<StoredOperation>,
    key: &OperationKey,
    decision: &Value,
) -> Result<Completion, ReceiptError> {
    let Some(stored) = existing else {
        return Err(ReceiptError::NotPending);
    };
    if !stored.owned_by(&key.scope) {
        return Err(ReceiptError::ScopeMismatch);
    }
    match stored.status.as_str() {
        "pending" => Ok(Completion::Write),
        "complete" => match stored.decision.transpose()?.as_ref() == Some(decision) {
            true => Ok(Completion::Unchanged),
            false => Err(ReceiptError::CompletionMismatch),
        },
        _ => Err(ReceiptError::storage("operation receipt has an invalid status")),
    }
}

/// A claim on an absent result takes it, as [`resolve_operation_claim`] does.
pub(crate) fn resolve_result_claim(
    existing: Option<StoredResult>,
    request: &ProcessedResultRequest,
) -> Result<ProcessedResultClaim, ReceiptError> {
    let Some(stored) = existing else {
        return Ok(ProcessedResultClaim::Claimed);
    };
    if !request.key.owns(&stored.organization_id, &stored.session_id) || stored.root != request.root {
        return Err(ReceiptError::ScopeMismatch);
    }
    match stored.status.as_str() {
        "complete" => Ok(ProcessedResultClaim::Complete {
            approved_output: stored
                .approved_output
                .ok_or_else(|| ReceiptError::storage("complete result lacks approved output"))?,
            decision: stored
                .decision
                .ok_or_else(|| ReceiptError::storage("complete result lacks a decision"))??,
        }),
        "pending" => Err(ReceiptError::Pending),
        _ => Err(ReceiptError::storage("processed result has an invalid status")),
    }
}

pub(crate) fn resolve_result_completion(
    existing: Option<StoredResult>,
    key: &ProcessedResultKey,
    approved_output: &str,
    decision: &Value,
) -> Result<Completion, ReceiptError> {
    let Some(stored) = existing else {
        return Err(ReceiptError::NotPending);
    };
    if !key.owns(&stored.organization_id, &stored.session_id) {
        return Err(ReceiptError::ScopeMismatch);
    }
    match stored.status.as_str() {
        "pending" => Ok(Completion::Write),
        "complete" => match stored.approved_output.as_deref() == Some(approved_output)
            && stored.decision.transpose()?.as_ref() == Some(decision)
        {
            true => Ok(Completion::Unchanged),
            false => Err(ReceiptError::CompletionMismatch),
        },
        _ => Err(ReceiptError::storage("processed result has an invalid status")),
    }
}

impl ReceiptError {
    pub(crate) fn storage(detail: impl Into<String>) -> Self {
        Self::Storage(ReceiptStorageError(detail.into()))
    }
}

impl From<ReceiptStorageError> for ReceiptError {
    fn from(error: ReceiptStorageError) -> Self {
        Self::Storage(error)
    }
}

impl From<rusqlite::Error> for ReceiptError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Storage(error.into())
    }
}

impl LogStore {
    /// Stores an offer owner record. Repeated writes with identical data are idempotent.
    pub fn store_offer_owner(&self, record: OfferOwnerRecord) -> Result<(), ReceiptError> {
        match &self.store {
            Store::Sqlite(connection) => immediate(&mut lock(connection), |connection| {
                sqlite_store_offer_owner(connection, &record)
            }),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.store_offer_owner(record),
        }
    }

    /// Reads an offer owner record by key.
    pub fn offer_owner(&self, key: OfferOwnerKey) -> Result<Option<OfferOwnerRecord>, ReceiptStorageError> {
        match &self.store {
            Store::Sqlite(connection) => sqlite_read_offer_owner(&lock(connection), &key),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.offer_owner(key).map_err(Into::into),
        }
    }

    /// Deletes stored offer owner records for a session scope.
    pub fn expire_offer_owners(&self, scope: ReceiptScope) -> Result<u64, ReceiptStorageError> {
        match &self.store {
            Store::Sqlite(connection) => immediate(&mut lock(connection), |connection| {
                Ok(connection.execute(
                    "DELETE FROM offer_owners WHERE organization_id=?1 AND caller_id IS ?2 AND session_id=?3",
                    params![scope.organization_id, scope.caller_id, scope.session_id],
                )? as u64)
            }),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.expire_offer_owners(scope).map_err(Into::into),
        }
    }

    /// Claims an operation receipt before starting work. Completed receipts return saved decisions.
    pub fn claim_operation(&self, request: OperationRequest) -> Result<OperationClaim, ReceiptError> {
        match &self.store {
            Store::Sqlite(connection) => immediate(&mut lock(connection), |connection| {
                sqlite_claim_operation(connection, &request)
            }),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.claim_operation(request),
        }
    }

    /// Completes a claimed operation receipt with its final decision.
    pub fn complete_operation(&self, key: OperationKey, decision: Value) -> Result<(), ReceiptError> {
        match &self.store {
            Store::Sqlite(connection) => immediate(&mut lock(connection), |connection| {
                sqlite_complete_operation(connection, &key, &decision)
            }),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.complete_operation(key, decision),
        }
    }

    /// Claims a durable processed-result receipt before result processing.
    pub fn claim_processed_result(
        &self,
        request: ProcessedResultRequest,
    ) -> Result<ProcessedResultClaim, ReceiptError> {
        match &self.store {
            Store::Sqlite(connection) => immediate(&mut lock(connection), |connection| {
                sqlite_claim_processed_result(connection, &request)
            }),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.claim_processed_result(request),
        }
    }

    /// Completes a processed-result receipt with its approved output and decision.
    pub fn complete_processed_result(
        &self,
        key: ProcessedResultKey,
        approved_output: String,
        decision: Value,
    ) -> Result<(), ReceiptError> {
        match &self.store {
            Store::Sqlite(connection) => immediate(&mut lock(connection), |connection| {
                sqlite_complete_processed_result(connection, &key, &approved_output, &decision)
            }),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.complete_processed_result(key, approved_output, decision),
        }
    }

    /// Checks whether pending receipts exist for a root trajectory.
    pub fn has_pending_receipts(&self, root: String) -> Result<bool, ReceiptStorageError> {
        match &self.store {
            Store::Sqlite(connection) => {
                let found: Option<i64> = lock(connection)
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
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.has_pending_receipts(root).map_err(Into::into),
        }
    }
}

fn sqlite_store_offer_owner(connection: &Connection, record: &OfferOwnerRecord) -> Result<(), ReceiptError> {
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
    let existing = sqlite_read_offer_owner(
        connection,
        &OfferOwnerKey {
            organization_id: record.scope.organization_id.clone(),
            offer_id: record.offer_id.clone(),
        },
    )?;
    resolve_offer_owner(existing, record)
}

fn sqlite_read_offer_owner(
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
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
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

fn sqlite_read_operation(connection: &Connection, key: &OperationKey) -> Result<Option<StoredOperation>, ReceiptError> {
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
                    input: StoredOperationInput::decode(json(&input)?)?,
                    status,
                    decision: decision.as_deref().map(json),
                })
            },
        )
        .transpose()
}

fn sqlite_claim_operation(connection: &Connection, request: &OperationRequest) -> Result<OperationClaim, ReceiptError> {
    let claim = resolve_operation_claim(sqlite_read_operation(connection, &request.key)?, request)?;
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
}

fn sqlite_complete_operation(
    connection: &Connection,
    key: &OperationKey,
    decision: &Value,
) -> Result<(), ReceiptError> {
    let existing = sqlite_read_operation(connection, key)?;
    if let Completion::Write = resolve_operation_completion(existing, key, decision)? {
        let encoded = serde_json::to_string(decision).map_err(|error| ReceiptError::storage(error.to_string()))?;
        connection.execute(
            "UPDATE operations SET status='complete', decision=?3 WHERE session_id=?1 AND operation_id=?2",
            params![key.scope.session_id, key.operation_id, encoded],
        )?;
    }
    Ok(())
}

fn sqlite_read_result(connection: &Connection, key: &ProcessedResultKey) -> Result<Option<StoredResult>, ReceiptError> {
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

fn sqlite_claim_processed_result(
    connection: &Connection,
    request: &ProcessedResultRequest,
) -> Result<ProcessedResultClaim, ReceiptError> {
    let claim = resolve_result_claim(sqlite_read_result(connection, &request.key)?, request)?;
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
}

fn sqlite_complete_processed_result(
    connection: &Connection,
    key: &ProcessedResultKey,
    approved_output: &str,
    decision: &Value,
) -> Result<(), ReceiptError> {
    let existing = sqlite_read_result(connection, key)?;
    if let Completion::Write = resolve_result_completion(existing, key, approved_output, decision)? {
        let encoded = serde_json::to_string(decision).map_err(|error| ReceiptError::storage(error.to_string()))?;
        connection.execute(
            "UPDATE processed_results SET status='complete', approved_output=?3, decision=?4
             WHERE session_id=?1 AND tool_call_id=?2",
            params![key.session_id, key.tool_call_id, approved_output, encoded],
        )?;
    }
    Ok(())
}

fn json(raw: &str) -> StoredJson {
    serde_json::from_str(raw).map_err(|error| ReceiptStorageError(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Backend;

    /// Each test runs on both SQLite-backed stores: the file and the in-memory one.
    fn stores() -> Vec<(LogStore, Option<tempfile::TempDir>)> {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let file = LogStore::open(Backend::Sqlite {
            path: dir.path().join("appa.db"),
        })
        .expect("a fresh file store opens");
        let memory = LogStore::open(Backend::Memory).expect("an in-memory store opens");
        vec![(file, Some(dir)), (memory, None)]
    }

    fn scope(binding: ReceiptBinding, caller: &str) -> ReceiptScope {
        ReceiptScope {
            organization_id: "org".to_owned(),
            caller_id: Some(caller.to_owned()),
            session_id: "session".to_owned(),
            binding,
        }
    }

    fn operation(scope: ReceiptScope) -> OperationRequest {
        OperationRequest {
            key: OperationKey {
                scope,
                operation_id: "op-1".to_owned(),
            },
            root: "root".to_owned(),
            input: serde_json::json!({"offer_id": "0123456789abcdef"}),
            context: None,
        }
    }

    fn result(caller: &str) -> ProcessedResultRequest {
        ProcessedResultRequest {
            key: ProcessedResultKey {
                organization_id: "org".to_owned(),
                caller_id: Some(caller.to_owned()),
                session_id: "session".to_owned(),
                tool_call_id: "call-1".to_owned(),
            },
            root: "root".to_owned(),
        }
    }

    fn operation_row(store: &LogStore) -> (String, String, Option<String>) {
        store
            .lock()
            .query_row(
                "SELECT input, status, decision FROM operations WHERE session_id='session' AND operation_id='op-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("the operation row reads")
    }

    fn execute(store: &LogStore, sql: &str) {
        store.lock().execute(sql, []).expect("the fixture statement runs");
    }

    #[test]
    fn operation_receipts_store_the_frozen_v1_input_envelope() {
        let envelope =
            r#"{"version":1,"binding":"caller","semantic":{"offer_id":"0123456789abcdef"},"context":{"hook":"pre"}}"#;
        for (store, _dir) in stores() {
            let mut request = operation(scope(ReceiptBinding::Caller, "caller"));
            request.context = Some(serde_json::json!({"hook": "pre"}));
            store.claim_operation(request.clone()).expect("the operation claims");
            assert_eq!(operation_row(&store), (envelope.to_owned(), "pending".to_owned(), None));
            store
                .complete_operation(request.key.clone(), serde_json::json!({"decision": "allow_call"}))
                .expect("the operation completes");
            assert_eq!(
                operation_row(&store),
                (
                    envelope.to_owned(),
                    "complete".to_owned(),
                    Some(r#"{"decision":"allow_call"}"#.to_owned())
                )
            );

            execute(&store, "DELETE FROM operations");
            store
                .claim_operation(operation(scope(ReceiptBinding::Session, "caller")))
                .expect("the operation claims");
            assert_eq!(
                operation_row(&store).0,
                r#"{"version":1,"binding":"session","semantic":{"offer_id":"0123456789abcdef"}}"#
            );
        }
    }

    /// Only the v1 envelope decodes: raw semantic input, a misspelled envelope, or another
    /// version is a storage failure.
    #[test]
    fn operation_input_outside_the_v1_envelope_is_a_storage_failure() {
        for input in [
            r#"{"offer_id":"0123456789abcdef"}"#,
            r#"{"version":1,"bindng":"session","semantic":{"offer_id":"0123456789abcdef"}}"#,
            r#"{"version":1,"binding":"session","semantic":{"offer_id":"0123456789abcdef"},"extra":1}"#,
            r#"{"version":2,"binding":"session","semantic":{"offer_id":"0123456789abcdef"}}"#,
        ] {
            for (store, _dir) in stores() {
                store
                    .lock()
                    .execute(
                        "INSERT INTO operations (organization_id, caller_id, session_id, operation_id, root, input, status)
                         VALUES ('org', 'caller', 'session', 'op-1', 'root', ?1, 'pending')",
                        [input],
                    )
                    .expect("the fixture row inserts");
                let request = operation(scope(ReceiptBinding::Session, "caller"));
                assert!(
                    matches!(store.claim_operation(request.clone()), Err(ReceiptError::Storage(_))),
                    "{input}"
                );
                assert!(
                    matches!(
                        store.complete_operation(request.key, serde_json::json!({"decision": "allow_call"})),
                        Err(ReceiptError::Storage(_))
                    ),
                    "{input}"
                );
            }
        }
    }

    #[test]
    fn operation_receipts_refuse_another_scope_without_writing() {
        for (store, _dir) in stores() {
            let owned = operation(scope(ReceiptBinding::Caller, "caller"));
            store.claim_operation(owned.clone()).expect("the operation claims");

            let other_caller = operation(scope(ReceiptBinding::Caller, "other"));
            let session_bound = operation(scope(ReceiptBinding::Session, "caller"));
            let mut other_organization = owned.clone();
            other_organization.key.scope.organization_id = "other-org".to_owned();
            let mut other_root = owned.clone();
            other_root.root = "other-root".to_owned();
            for request in [&other_caller, &session_bound, &other_organization, &other_root] {
                assert!(matches!(
                    store.claim_operation(request.clone()),
                    Err(ReceiptError::ScopeMismatch)
                ));
            }
            for key in [&other_caller.key, &session_bound.key, &other_organization.key] {
                assert!(matches!(
                    store.complete_operation(key.clone(), serde_json::json!({"decision": "deny"})),
                    Err(ReceiptError::ScopeMismatch)
                ));
            }
            assert_eq!(operation_row(&store).1, "pending");
            store
                .complete_operation(owned.key.clone(), serde_json::json!({"decision": "allow_call"}))
                .expect("the owning scope completes");
        }
    }

    /// A session-bound receipt does not compare caller identities.
    #[test]
    fn a_session_bound_operation_ignores_the_caller() {
        for (store, _dir) in stores() {
            store
                .claim_operation(operation(scope(ReceiptBinding::Session, "caller")))
                .expect("the operation claims");
            assert!(matches!(
                store.claim_operation(operation(scope(ReceiptBinding::Session, "other"))),
                Err(ReceiptError::Pending)
            ));
            store
                .complete_operation(
                    operation(scope(ReceiptBinding::Session, "other")).key,
                    serde_json::json!({"decision": "allow_call"}),
                )
                .expect("another caller in the session completes");
        }
    }

    #[test]
    fn processed_results_refuse_another_scope_without_writing() {
        for (store, _dir) in stores() {
            let owned = result("caller");
            store.claim_processed_result(owned.clone()).expect("the result claims");

            let mut other_organization = owned.clone();
            other_organization.key.organization_id = "other-org".to_owned();
            let mut other_root = owned.clone();
            other_root.root = "other-root".to_owned();
            for request in [&other_organization, &other_root] {
                assert!(matches!(
                    store.claim_processed_result(request.clone()),
                    Err(ReceiptError::ScopeMismatch)
                ));
            }
            assert!(matches!(
                store.complete_processed_result(
                    other_organization.key.clone(),
                    "approved".to_owned(),
                    serde_json::json!({"decision": "deny"})
                ),
                Err(ReceiptError::ScopeMismatch)
            ));
            assert!(matches!(
                store.claim_processed_result(result("other")),
                Err(ReceiptError::Pending)
            ));
            store
                .complete_processed_result(
                    owned.key.clone(),
                    "approved".to_owned(),
                    serde_json::json!({"decision": "allow"}),
                )
                .expect("the owning scope completes");
        }
    }

    #[test]
    fn a_completed_processed_result_replays() {
        for (store, _dir) in stores() {
            let request = result("caller");
            let decision = serde_json::json!({"decision": "allow"});
            assert_eq!(
                store
                    .claim_processed_result(request.clone())
                    .expect("the result claims"),
                ProcessedResultClaim::Claimed
            );
            store
                .complete_processed_result(request.key.clone(), "approved".to_owned(), decision.clone())
                .expect("the result completes");
            assert_eq!(
                store
                    .claim_processed_result(request)
                    .expect("the completed result replays"),
                ProcessedResultClaim::Complete {
                    approved_output: "approved".to_owned(),
                    decision,
                }
            );
        }
    }

    #[test]
    fn an_offer_owner_with_other_data_collides() {
        for (store, _dir) in stores() {
            let record = OfferOwnerRecord {
                scope: scope(ReceiptBinding::Caller, "caller"),
                offer_id: "0123456789abcdef".to_owned(),
                root: "root".to_owned(),
                parent_id: None,
                arguments: None,
                tool: None,
                spelling: None,
            };
            store.store_offer_owner(record.clone()).expect("the owner stores");
            let mut other_root = record.clone();
            other_root.root = "other-root".to_owned();
            let mut other_caller = record.clone();
            other_caller.scope.caller_id = Some("other".to_owned());
            let mut other_tool = record.clone();
            other_tool.tool = Some("wire".to_owned());
            for colliding in [other_root, other_caller, other_tool] {
                assert!(matches!(
                    store.store_offer_owner(colliding),
                    Err(ReceiptError::Collision)
                ));
            }
            store
                .store_offer_owner(record)
                .expect("the stored owner is unchanged, so its replay is idempotent");
        }
    }

    #[test]
    fn a_claimed_receipt_refuses_a_second_claim_and_other_input() {
        for (store, _dir) in stores() {
            let request = operation(scope(ReceiptBinding::Caller, "caller"));
            store.claim_operation(request.clone()).expect("the operation claims");
            let mut changed = request.clone();
            changed.input = serde_json::json!({"offer_id": "other"});
            assert!(matches!(
                store.claim_operation(changed),
                Err(ReceiptError::InputMismatch)
            ));
            let mut recontextualized = request.clone();
            recontextualized.context = Some(serde_json::json!({"hook": "post"}));
            assert!(matches!(
                store.claim_operation(recontextualized),
                Err(ReceiptError::Pending)
            ));
            assert!(matches!(store.claim_operation(request), Err(ReceiptError::Pending)));

            let request = result("caller");
            store
                .claim_processed_result(request.clone())
                .expect("the result claims");
            assert!(matches!(
                store.claim_processed_result(request),
                Err(ReceiptError::Pending)
            ));
        }
    }

    #[test]
    fn completing_an_absent_receipt_is_not_pending() {
        for (store, _dir) in stores() {
            assert!(matches!(
                store.complete_operation(
                    operation(scope(ReceiptBinding::Session, "caller")).key,
                    serde_json::json!({})
                ),
                Err(ReceiptError::NotPending)
            ));
            assert!(matches!(
                store.complete_processed_result(result("caller").key, "approved".to_owned(), serde_json::json!({})),
                Err(ReceiptError::NotPending)
            ));
        }
    }

    #[test]
    fn a_completed_receipt_accepts_only_its_own_completion_again() {
        for (store, _dir) in stores() {
            let decision = serde_json::json!({"decision": "allow_call"});
            let other = serde_json::json!({"decision": "deny"});

            let request = operation(scope(ReceiptBinding::Caller, "caller"));
            store.claim_operation(request.clone()).expect("the operation claims");
            store
                .complete_operation(request.key.clone(), decision.clone())
                .expect("the operation completes");
            store
                .complete_operation(request.key.clone(), decision.clone())
                .expect("the same completion is idempotent");
            assert!(matches!(
                store.complete_operation(request.key, other.clone()),
                Err(ReceiptError::CompletionMismatch)
            ));

            let request = result("caller");
            store
                .claim_processed_result(request.clone())
                .expect("the result claims");
            store
                .complete_processed_result(request.key.clone(), "approved".to_owned(), decision.clone())
                .expect("the result completes");
            store
                .complete_processed_result(request.key.clone(), "approved".to_owned(), decision.clone())
                .expect("the same completion is idempotent");
            assert!(matches!(
                store.complete_processed_result(request.key.clone(), "changed".to_owned(), decision.clone()),
                Err(ReceiptError::CompletionMismatch)
            ));
            assert!(matches!(
                store.complete_processed_result(request.key, "approved".to_owned(), other),
                Err(ReceiptError::CompletionMismatch)
            ));
        }
    }

    #[test]
    fn a_corrupt_operation_row_is_a_storage_failure() {
        let request = || operation(scope(ReceiptBinding::Session, "caller"));
        let decision = serde_json::json!({"decision": "allow_call"});
        for corruption in [
            "UPDATE operations SET status='bogus'",
            "UPDATE operations SET input='not json'",
            "UPDATE operations SET status='complete', decision='not json'",
        ] {
            for (store, _dir) in stores() {
                store.claim_operation(request()).expect("the operation claims");
                execute(&store, corruption);
                assert!(
                    matches!(store.claim_operation(request()), Err(ReceiptError::Storage(_))),
                    "{corruption}"
                );
                assert!(
                    matches!(
                        store.complete_operation(request().key, decision.clone()),
                        Err(ReceiptError::Storage(_))
                    ),
                    "{corruption}"
                );
            }
        }
        for (store, _dir) in stores() {
            store.claim_operation(request()).expect("the operation claims");
            execute(&store, "UPDATE operations SET status='complete'");
            assert!(matches!(
                store.claim_operation(request()),
                Err(ReceiptError::Storage(_))
            ));
        }
    }

    #[test]
    fn a_corrupt_processed_result_row_is_a_storage_failure() {
        let request = || result("caller");
        let decision = serde_json::json!({"decision": "allow"});
        for corruption in [
            "UPDATE processed_results SET status='bogus'",
            "UPDATE processed_results SET status='complete', approved_output='approved', decision='not json'",
        ] {
            for (store, _dir) in stores() {
                store.claim_processed_result(request()).expect("the result claims");
                execute(&store, corruption);
                assert!(
                    matches!(store.claim_processed_result(request()), Err(ReceiptError::Storage(_))),
                    "{corruption}"
                );
                assert!(
                    matches!(
                        store.complete_processed_result(request().key, "approved".to_owned(), decision.clone()),
                        Err(ReceiptError::Storage(_))
                    ),
                    "{corruption}"
                );
            }
        }
        for corruption in [
            "UPDATE processed_results SET status='complete', decision='{}'",
            "UPDATE processed_results SET status='complete', approved_output='approved'",
        ] {
            for (store, _dir) in stores() {
                store.claim_processed_result(request()).expect("the result claims");
                execute(&store, corruption);
                assert!(
                    matches!(store.claim_processed_result(request()), Err(ReceiptError::Storage(_))),
                    "{corruption}"
                );
            }
        }
    }

    #[test]
    fn a_corrupt_offer_owner_binding_is_a_storage_failure() {
        for (store, _dir) in stores() {
            let record = OfferOwnerRecord {
                scope: scope(ReceiptBinding::Caller, "caller"),
                offer_id: "0123456789abcdef".to_owned(),
                root: "root".to_owned(),
                parent_id: None,
                arguments: None,
                tool: None,
                spelling: None,
            };
            store.store_offer_owner(record.clone()).expect("the owner stores");
            let stored: String = store
                .lock()
                .query_row("SELECT binding FROM offer_owners", [], |row| row.get(0))
                .expect("the binding reads");
            assert_eq!(stored, "caller");
            execute(&store, "UPDATE offer_owners SET binding='bogus'");
            assert!(
                store
                    .offer_owner(OfferOwnerKey {
                        organization_id: "org".to_owned(),
                        offer_id: record.offer_id.clone(),
                    })
                    .is_err()
            );
            assert!(matches!(store.store_offer_owner(record), Err(ReceiptError::Storage(_))));
        }
    }
}
