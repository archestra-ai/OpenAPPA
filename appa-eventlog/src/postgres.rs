//! PostgreSQL storage for embedded hosts. The host installs the schema; Rust
//! retains the SQLite event encoding and policy-file validation. A dedicated
//! connection thread keeps the synchronous log API usable from async hooks.
//!
//! The schema the host's migrations must provide:
//!
//! ```sql
//! CREATE TABLE openappa_events (root TEXT NOT NULL, seq BIGINT NOT NULL, payload BYTEA NOT NULL,
//!                               PRIMARY KEY (root, seq));
//! CREATE TABLE openappa_policy_files (hash TEXT PRIMARY KEY, bytes BYTEA NOT NULL);
//! CREATE TABLE openappa_host_keys (key TEXT NOT NULL, root TEXT NOT NULL, PRIMARY KEY (key, root));
//! ```

use std::sync::mpsc;

use ::postgres::Client;
use postgres_native_tls::MakeTlsConnector;
use serde_json::Value;

use super::*;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct PostgresError(pub String);

impl From<::postgres::Error> for PostgresError {
    fn from(error: ::postgres::Error) -> Self {
        Self(error.to_string())
    }
}

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
pub enum OfferOwnerError {
    #[error("offer owner storage failed: {0}")]
    Storage(PostgresError),
    #[error("a different owner record already exists for this offer")]
    Collision,
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

/// A completed operation with its semantic request and host context restored.
#[derive(Debug, Clone, PartialEq)]
pub struct CompletedOperation {
    pub root: String,
    pub input: Value,
    pub context: Option<Value>,
    pub decision: Value,
}

#[derive(Debug, thiserror::Error)]
pub enum OperationReceiptError {
    #[error("operation receipt storage failed: {0}")]
    Storage(PostgresError),
    #[error("operation id belongs to another authenticated scope")]
    ScopeMismatch,
    #[error("operation id was reused with different input")]
    InputMismatch,
    #[error("operation is pending recovery and must not be replayed")]
    Pending,
    #[error("operation receipt is absent or no longer pending")]
    NotPending,
    #[error("operation was completed with a different decision")]
    CompletionMismatch,
}

/// The durable key for a processed tool result receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessedResultKey {
    pub scope: ReceiptScope,
    pub tool_call_id: String,
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

#[derive(Debug, thiserror::Error)]
pub enum ProcessedResultError {
    #[error("processed result receipt storage failed: {0}")]
    Storage(PostgresError),
    #[error("tool call id belongs to another authenticated scope")]
    ScopeMismatch,
    #[error("tool result is pending recovery and must not be replayed")]
    Pending,
    #[error("tool result receipt is absent or no longer pending")]
    NotPending,
    #[error("tool result was completed with a different receipt")]
    CompletionMismatch,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredOperationInput {
    version: u8,
    binding: ReceiptBinding,
    semantic: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context: Option<Value>,
}

impl StoredOperationInput {
    fn from_request(request: &OperationRequest) -> Self {
        Self {
            version: 1,
            binding: request.key.scope.binding,
            semantic: request.input.clone(),
            context: request.context.clone(),
        }
    }

    /// Decodes stored input, maintaining backwards compatibility with raw JSON inputs.
    fn decode(value: Value) -> Result<Self, ReceiptMutationError> {
        match serde_json::from_value::<Self>(value.clone()) {
            Ok(stored) if stored.version == 1 => Ok(stored),
            Ok(_) => Err(ReceiptMutationError::Storage(PostgresError(
                "operation receipt has an unsupported version".into(),
            ))),
            Err(_) => Ok(Self {
                version: 0,
                binding: ReceiptBinding::Session,
                semantic: value,
                context: None,
            }),
        }
    }
}

struct ConnectionState {
    client: Client,
    transaction: bool,
}

type Job = Box<dyn FnOnce(&mut ConnectionState) + Send>;

#[derive(Clone)]
pub struct PostgresStore {
    sender: mpsc::Sender<Job>,
}

impl PostgresStore {
    pub(super) fn open(url: String) -> Result<Self, PostgresError> {
        let (sender, receiver) = mpsc::channel::<Job>();
        let (ready, initialized) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("appa-postgres".into())
            .spawn(move || {
                let connect = || -> Result<Client, PostgresError> {
                    let tls = native_tls::TlsConnector::new().map_err(|e| PostgresError(e.to_string()))?;
                    let mut client = Client::connect(&url, MakeTlsConnector::new(tls))?;
                    // Deliberately no DDL: an incompatible/missing host migration refuses startup.
                    client.batch_execute(
                        "SELECT root, seq, payload FROM openappa_events LIMIT 0;
                    SELECT hash, bytes FROM openappa_policy_files LIMIT 0;
                    SELECT key, root FROM openappa_host_keys LIMIT 0;
                    SET lock_timeout = '30s'; SET statement_timeout = '60s'",
                    )?;
                    Ok(client)
                };
                match connect() {
                    Ok(client) => {
                        let _ = ready.send(Ok(()));
                        let mut state = ConnectionState {
                            client,
                            transaction: false,
                        };
                        for job in receiver {
                            job(&mut state);
                        }
                    }
                    Err(error) => {
                        let _ = ready.send(Err(error));
                    }
                }
            })
            .map_err(|e| PostgresError(e.to_string()))?;
        initialized
            .recv()
            .map_err(|_| PostgresError("connection worker stopped".into()))??;
        Ok(Self { sender })
    }

    fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut ConnectionState) -> Result<T, PostgresError> + Send + 'static,
    ) -> Result<T, PostgresError> {
        let (send, recv) = mpsc::sync_channel(1);
        self.sender
            .send(Box::new(move |state| {
                let _ = send.send(operation(state));
            }))
            .map_err(|_| PostgresError("connection worker stopped".into()))?;
        recv.recv()
            .map_err(|_| PostgresError("connection worker stopped".into()))?
    }

    /// Host integration SQL (receipts, advisory locks) runs on the exact same
    /// connection as the event log. Never expose this capability to clients.
    pub fn with_client<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Client) -> Result<T, PostgresError> + Send + 'static,
    ) -> Result<T, PostgresError> {
        self.run(move |state| operation(&mut state.client))
    }

    /// Stores an offer owner record. Repeated writes with identical data are idempotent.
    pub fn store_offer_owner(&self, record: OfferOwnerRecord) -> Result<(), OfferOwnerError> {
        let lock = offer_owner_lock(&record.scope.organization_id, &record.offer_id);
        self.mutate_receipt(lock, move |client| {
            let inserted = client
                .query_opt(
                    "INSERT INTO openappa_offer_owners (organization_id, caller_id, session_id, offer_id, root, parent_id, arguments, tool, spelling) \
                     VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) \
                     ON CONFLICT (organization_id, offer_id) DO NOTHING \
                     RETURNING organization_id",
                    &[
                        &record.scope.organization_id,
                        &record.scope.caller_id,
                        &record.scope.session_id,
                        &record.offer_id,
                        &record.root,
                        &record.parent_id,
                        &record.arguments,
                        &record.tool,
                        &record.spelling,
                    ],
                )?
                .is_some();
            if inserted {
                return Ok(());
            }
            let existing = read_offer_owner(client, &OfferOwnerKey {
                organization_id: record.scope.organization_id.clone(),
                offer_id: record.offer_id.clone(),
            })?
            .ok_or_else(|| PostgresError("offer owner disappeared after a conflict".into()))?;
            if existing == record {
                Ok(())
            } else {
                Err(ReceiptMutationError::OwnerCollision)
            }
        })
        .map_err(OfferOwnerError::from)
    }

    /// Reads an offer owner record by key.
    pub fn offer_owner(&self, key: OfferOwnerKey) -> Result<Option<OfferOwnerRecord>, PostgresError> {
        self.with_client(move |client| read_offer_owner(client, &key))
    }

    /// Deletes stored offer owner records for a session scope.
    pub fn expire_offer_owners(&self, scope: ReceiptScope) -> Result<u64, PostgresError> {
        let lock = session_lock(&scope);
        self.mutate_receipt(lock, move |client| {
            Ok(client.execute(
                "DELETE FROM openappa_offer_owners WHERE organization_id=$1 AND caller_id IS NOT DISTINCT FROM $2 AND session_id=$3",
                &[&scope.organization_id, &scope.caller_id, &scope.session_id],
            )?)
        })
        .map_err(|error| match error {
            ReceiptMutationError::Storage(error) => error,
            other => PostgresError(other.to_string()),
        })
    }

    /// Deletes stored offer owner records for a specific root trajectory.
    pub fn expire_offer_owners_for_root(&self, organization_id: &str, root: &str) -> Result<u64, PostgresError> {
        let lock = format!("openappa-offer-root:{organization_id}:{root}");
        let org = organization_id.to_owned();
        let rt = root.to_owned();
        self.mutate_receipt(lock, move |client| {
            Ok(client.execute(
                "DELETE FROM openappa_offer_owners WHERE organization_id=$1 AND root=$2",
                &[&org, &rt],
            )?)
        })
        .map_err(|error| match error {
            ReceiptMutationError::Storage(error) => error,
            other => PostgresError(other.to_string()),
        })
    }

    /// Claims an operation receipt before starting work. Completed receipts return saved decisions.
    pub fn claim_operation(&self, request: OperationRequest) -> Result<OperationClaim, OperationReceiptError> {
        let lock = operation_lock(&request.key.scope.session_id, &request.key.operation_id);
        self.mutate_receipt(lock, move |client| {
            let existing = client.query_opt(
                "SELECT organization_id, caller_id, session_id, root, input, status, decision \
                 FROM openappa_operations WHERE session_id=$1 AND operation_id=$2 FOR UPDATE",
                &[&request.key.scope.session_id, &request.key.operation_id],
            )?;
            let Some(row) = existing else {
                let stored = serde_json::to_value(StoredOperationInput::from_request(&request))
                    .map_err(|error| ReceiptMutationError::Storage(PostgresError(error.to_string())))?;
                client.execute(
                    "INSERT INTO openappa_operations (organization_id, caller_id, session_id, operation_id, root, input, status) \
                     VALUES ($1,$2,$3,$4,$5,$6,'pending')",
                    &[
                        &request.key.scope.organization_id,
                        &request.key.scope.caller_id,
                        &request.key.scope.session_id,
                        &request.key.operation_id,
                        &request.root,
                        &stored,
                    ],
                )?;
                return Ok(OperationClaim::Claimed);
            };
            let stored = StoredOperationInput::decode(row.get(4))?;
            if !scope_matches(
                &ReceiptScope {
                    organization_id: row.get(0),
                    caller_id: row.get(1),
                    session_id: row.get(2),
                    binding: stored.binding,
                },
                &request.key.scope,
            ) || row.get::<_, String>(3) != request.root {
                return Err(ReceiptMutationError::ScopeMismatch);
            }
            if stored.semantic != request.input {
                return Err(ReceiptMutationError::InputMismatch);
            }
            match row.get::<_, String>(5).as_str() {
                "complete" => Ok(OperationClaim::Complete {
                    decision: row
                        .get::<_, Option<Value>>(6)
                        .ok_or_else(|| ReceiptMutationError::Storage(PostgresError("complete operation lacks a decision".into())))?,
                }),
                "pending" => Err(ReceiptMutationError::Pending),
                _ => Err(ReceiptMutationError::Storage(PostgresError("operation receipt has an invalid status".into()))),
            }
        })
        .map_err(OperationReceiptError::from)
    }

    /// Completes a claimed operation receipt with its final decision.
    pub fn complete_operation(&self, key: OperationKey, decision: Value) -> Result<(), OperationReceiptError> {
        let lock = operation_lock(&key.scope.session_id, &key.operation_id);
        self.mutate_receipt(lock, move |client| {
            let existing = client.query_opt(
                "SELECT organization_id, caller_id, session_id, input, status, decision \
                 FROM openappa_operations WHERE session_id=$1 AND operation_id=$2 FOR UPDATE",
                &[&key.scope.session_id, &key.operation_id],
            )?;
            let Some(row) = existing else {
                return Err(ReceiptMutationError::NotPending);
            };
            let stored = StoredOperationInput::decode(row.get(3))?;
            let scope = ReceiptScope {
                organization_id: row.get(0),
                caller_id: row.get(1),
                session_id: row.get(2),
                binding: stored.binding,
            };
            if !scope_matches(&scope, &key.scope) {
                return Err(ReceiptMutationError::ScopeMismatch);
            }
            match row.get::<_, String>(4).as_str() {
                "pending" => {
                    client.execute(
                        "UPDATE openappa_operations SET status='complete', decision=$3 WHERE session_id=$1 AND operation_id=$2",
                        &[&key.scope.session_id, &key.operation_id, &decision],
                    )?;
                    Ok(())
                }
                "complete" if row.get::<_, Option<Value>>(5).as_ref() == Some(&decision) => Ok(()),
                "complete" => Err(ReceiptMutationError::CompletionMismatch),
                _ => Err(ReceiptMutationError::Storage(PostgresError("operation receipt has an invalid status".into()))),
            }
        })
        .map_err(OperationReceiptError::from)
    }

    /// Reads a completed operation receipt by key.
    pub fn completed_operation(&self, key: OperationKey) -> Result<Option<CompletedOperation>, OperationReceiptError> {
        let lock = operation_lock(&key.scope.session_id, &key.operation_id);
        self.mutate_receipt(lock, move |client| {
            let row = client.query_opt(
                "SELECT organization_id, caller_id, session_id, root, input, status, decision \
                 FROM openappa_operations WHERE session_id=$1 AND operation_id=$2",
                &[&key.scope.session_id, &key.operation_id],
            )?;
            let Some(row) = row else {
                return Ok(None);
            };
            let stored = StoredOperationInput::decode(row.get(4))?;
            let saved_scope = ReceiptScope {
                organization_id: row.get(0),
                caller_id: row.get(1),
                session_id: row.get(2),
                binding: stored.binding,
            };
            if !scope_matches(&saved_scope, &key.scope) {
                return Err(ReceiptMutationError::ScopeMismatch);
            }
            match row.get::<_, String>(5).as_str() {
                "complete" => Ok(Some(CompletedOperation {
                    root: row.get(3),
                    input: stored.semantic,
                    context: stored.context,
                    decision: row.get::<_, Option<Value>>(6).ok_or_else(|| {
                        ReceiptMutationError::Storage(PostgresError("complete operation lacks a decision".into()))
                    })?,
                })),
                "pending" => Err(ReceiptMutationError::Pending),
                _ => Err(ReceiptMutationError::Storage(PostgresError(
                    "operation receipt has an invalid status".into(),
                ))),
            }
        })
        .map_err(OperationReceiptError::from)
    }

    /// Claims a durable processed-result receipt before result processing.
    pub fn claim_processed_result(
        &self,
        request: ProcessedResultRequest,
    ) -> Result<ProcessedResultClaim, ProcessedResultError> {
        let lock = result_lock(&request.key.scope.session_id, &request.key.tool_call_id);
        self.mutate_receipt(lock, move |client| {
            let existing = client.query_opt(
                "SELECT organization_id, caller_id, session_id, root, status, approved_output, decision \
                 FROM openappa_processed_results WHERE session_id=$1 AND tool_call_id=$2 FOR UPDATE",
                &[&request.key.scope.session_id, &request.key.tool_call_id],
            )?;
            let Some(row) = existing else {
                client.execute(
                    "INSERT INTO openappa_processed_results (organization_id, caller_id, session_id, tool_call_id, root, status) \
                     VALUES ($1,$2,$3,$4,$5,'pending')",
                    &[
                        &request.key.scope.organization_id,
                        &request.key.scope.caller_id,
                        &request.key.scope.session_id,
                        &request.key.tool_call_id,
                        &request.root,
                    ],
                )?;
                return Ok(ProcessedResultClaim::Claimed);
            };
            let scope = ReceiptScope {
                organization_id: row.get(0),
                caller_id: row.get(1),
                session_id: row.get(2),
                binding: ReceiptBinding::Session,
            };
            if !scope_matches(&scope, &request.key.scope) || row.get::<_, String>(3) != request.root {
                return Err(ReceiptMutationError::ScopeMismatch);
            }
            match row.get::<_, String>(4).as_str() {
                "complete" => Ok(ProcessedResultClaim::Complete {
                    approved_output: row
                        .get::<_, Option<String>>(5)
                        .ok_or_else(|| ReceiptMutationError::Storage(PostgresError("complete result lacks approved output".into())))?,
                    decision: row
                        .get::<_, Option<Value>>(6)
                        .ok_or_else(|| ReceiptMutationError::Storage(PostgresError("complete result lacks a decision".into())))?,
                }),
                "pending" => Err(ReceiptMutationError::Pending),
                _ => Err(ReceiptMutationError::Storage(PostgresError("processed result has an invalid status".into()))),
            }
        })
        .map_err(ProcessedResultError::from)
    }

    /// Completes a processed-result receipt with its approved output and decision.
    pub fn complete_processed_result(
        &self,
        key: ProcessedResultKey,
        approved_output: String,
        decision: Value,
    ) -> Result<(), ProcessedResultError> {
        let lock = result_lock(&key.scope.session_id, &key.tool_call_id);
        self.mutate_receipt(lock, move |client| {
            let existing = client.query_opt(
                "SELECT organization_id, caller_id, session_id, status, approved_output, decision \
                 FROM openappa_processed_results WHERE session_id=$1 AND tool_call_id=$2 FOR UPDATE",
                &[&key.scope.session_id, &key.tool_call_id],
            )?;
            let Some(row) = existing else {
                return Err(ReceiptMutationError::NotPending);
            };
            let scope = ReceiptScope {
                organization_id: row.get(0),
                caller_id: row.get(1),
                session_id: row.get(2),
                binding: ReceiptBinding::Session,
            };
            if !scope_matches(&scope, &key.scope) {
                return Err(ReceiptMutationError::ScopeMismatch);
            }
            match row.get::<_, String>(3).as_str() {
                "pending" => {
                    client.execute(
                        "UPDATE openappa_processed_results SET status='complete', approved_output=$3, decision=$4 \
                         WHERE session_id=$1 AND tool_call_id=$2",
                        &[&key.scope.session_id, &key.tool_call_id, &approved_output, &decision],
                    )?;
                    Ok(())
                }
                "complete"
                    if row.get::<_, Option<String>>(4).as_ref() == Some(&approved_output)
                        && row.get::<_, Option<Value>>(5).as_ref() == Some(&decision) =>
                {
                    Ok(())
                }
                "complete" => Err(ReceiptMutationError::CompletionMismatch),
                _ => Err(ReceiptMutationError::Storage(PostgresError(
                    "processed result has an invalid status".into(),
                ))),
            }
        })
        .map_err(ProcessedResultError::from)
    }

    /// Checks whether pending receipts exist for a root trajectory.
    pub fn has_pending_receipts(&self, root: String) -> Result<bool, PostgresError> {
        self.with_client(move |client| {
            Ok(client
                .query_opt(
                    "SELECT 1 FROM openappa_operations WHERE root=$1 AND status='pending' \
                     UNION ALL \
                     SELECT 1 FROM openappa_processed_results WHERE root=$1 AND status='pending' \
                     LIMIT 1",
                    &[&root],
                )?
                .is_some())
        })
    }

    /// Starts an outer transaction across hook dispatches.
    pub fn begin(&self) -> Result<PostgresTransaction, PostgresError> {
        self.run(|state| {
            if state.transaction {
                return Err(PostgresError("transaction already active".into()));
            }
            state.client.batch_execute("BEGIN")?;
            state.transaction = true;
            Ok(())
        })?;
        Ok(PostgresTransaction {
            store: self.clone(),
            finished: false,
        })
    }

    /// Run `operation` in a transaction, serialized against other writers of `lock_root`
    /// where one is named. The host's own transaction is the one used when it holds one: a
    /// transaction opened inside that one would end the host's on the way out.
    fn with_tx<T: Send + 'static>(
        &self,
        lock_root: Option<String>,
        operation: impl FnOnce(&mut Client) -> Result<T, PostgresError> + Send + 'static,
    ) -> Result<T, PostgresError> {
        self.run(move |state| {
            let outer = state.transaction;
            if !outer {
                state.client.batch_execute("BEGIN")?;
            }
            let result = (|| {
                if let Some(root) = lock_root {
                    state
                        .client
                        .query_one("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))", &[&root])?;
                }
                operation(&mut state.client)
            })();
            if !outer {
                state
                    .client
                    .batch_execute(if result.is_ok() { "COMMIT" } else { "ROLLBACK" })?;
            }
            result
        })
    }

    fn mutate_receipt<T: Send + 'static>(
        &self,
        lock: String,
        operation: impl FnOnce(&mut Client) -> Result<T, ReceiptMutationError> + Send + 'static,
    ) -> Result<T, ReceiptMutationError> {
        self.run(move |state| {
            let outer = state.transaction;
            if !outer && let Err(error) = state.client.batch_execute("BEGIN") {
                return Ok(Err(ReceiptMutationError::Storage(error.into())));
            }
            let result = state
                .client
                .query_one("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))", &[&lock])
                .map_err(PostgresError::from)
                .map_err(ReceiptMutationError::Storage)
                .and_then(|_| operation(&mut state.client));
            if !outer {
                let end = if result.is_ok() { "COMMIT" } else { "ROLLBACK" };
                if let Err(error) = state.client.batch_execute(end) {
                    return Ok(Err(ReceiptMutationError::Storage(error.into())));
                }
            }
            Ok(result)
        })
        .map_err(ReceiptMutationError::Storage)?
    }

    pub(super) fn has_root(&self, root: &TrajectoryId) -> Result<bool, PostgresError> {
        let root = root.as_str().to_owned();
        self.with_client(move |client| {
            Ok(client
                .query_opt("SELECT 1 FROM openappa_events WHERE root = $1 LIMIT 1", &[&root])?
                .is_some())
        })
    }

    pub(super) fn create(
        &self,
        root: &TrajectoryId,
        key: &PolicyFileKey,
        policy: &[u8],
        bytes: Vec<u8>,
    ) -> Result<TrajectoryId, CreateError> {
        let id = root.as_str().to_owned();
        let hash = key.as_str().to_owned();
        let policy = policy.to_vec();
        let created = self.with_tx(Some(id.clone()), move |client| {
            if client
                .query_opt("SELECT 1 FROM openappa_events WHERE root = $1 LIMIT 1", &[&id])?
                .is_some()
            {
                return Ok(false);
            }
            client.execute(
                "INSERT INTO openappa_policy_files (hash, bytes) VALUES ($1, $2) ON CONFLICT DO NOTHING",
                &[&hash, &policy],
            )?;
            client.execute(
                "INSERT INTO openappa_events (root, seq, payload) VALUES ($1, 0, $2)",
                &[&id, &bytes],
            )?;
            Ok(true)
        })?;
        if !created {
            return Err(CreateError::AlreadyExists {
                root: root.as_str().to_owned(),
            });
        }
        Ok(root.clone())
    }

    pub(super) fn log(&self, root: &TrajectoryId) -> Result<Log, ReadError> {
        let id = root.as_str().to_owned();
        let batches = self.with_client(move |client| {
            let rows = client.query(
                "SELECT seq, payload FROM openappa_events WHERE root = $1 ORDER BY seq",
                &[&id],
            )?;
            let mut batches = Vec::with_capacity(rows.len());
            for (index, row) in rows.into_iter().enumerate() {
                if row.get::<_, i64>(0) != index as i64 {
                    return Err(PostgresError("event sequence contains a gap".into()));
                }
                batches.push(row.get::<_, Vec<u8>>(1));
            }
            Ok(batches)
        })?;
        let Some(first) = batches.first() else {
            return Err(ReadError::UnknownRoot {
                root: root.as_str().to_owned(),
            });
        };
        let opening = decode(first)?;
        let Some(Fact::TrajectoryOpened { policy_file_key, .. }) = opening.facts.first() else {
            return Err(ReadError::Undecodable("log does not begin with an opening".into()));
        };
        let hash = policy_file_key.as_str().to_owned();
        let lookup = hash.clone();
        let policy = self
            .with_client(move |client| {
                Ok(client
                    .query_opt("SELECT bytes FROM openappa_policy_files WHERE hash = $1", &[&lookup])?
                    .map(|row| row.get::<_, Vec<u8>>(0)))
            })?
            .ok_or(ReadError::PolicyFileMissing { key: hash })?;
        decoded(root, batches, policy)
    }

    /// See [`LogStore::roots_mentioning`].
    pub(super) fn roots_mentioning(&self, key: &str) -> Result<Vec<TrajectoryId>, ReadError> {
        let key = key.to_owned();
        let roots = self.with_client(move |client| {
            Ok(client
                .query(
                    "SELECT root FROM openappa_host_keys WHERE key = $1 ORDER BY root",
                    &[&key],
                )?
                .into_iter()
                .map(|row| row.get::<_, String>(0))
                .collect::<Vec<_>>())
        })?;
        Ok(roots.into_iter().map(TrajectoryId::new).collect())
    }

    pub(super) fn append(
        &self,
        root: &TrajectoryId,
        basis: u64,
        bytes: Vec<u8>,
        key: Option<&str>,
    ) -> Result<(), AppendError> {
        let root = root.as_str().to_owned();
        let key = key.map(str::to_owned);
        let conflict = self.with_tx(Some(root.clone()), move |client| {
            let current = client
                .query_one("SELECT count(*) FROM openappa_events WHERE root = $1", &[&root])?
                .get::<_, i64>(0) as u64;
            if current != basis {
                return Ok(Some(current));
            }
            client.execute(
                "INSERT INTO openappa_events (root, seq, payload) VALUES ($1, $2, $3)",
                &[&root, &(current as i64), &bytes],
            )?;
            if let Some(key) = key {
                client.execute(
                    "INSERT INTO openappa_host_keys (key, root) VALUES ($1, $2) ON CONFLICT DO NOTHING",
                    &[&key, &root],
                )?;
            }
            Ok(None)
        })?;
        if let Some(current) = conflict {
            return Err(AppendError::Conflict { current });
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
enum ReceiptMutationError {
    #[error("receipt storage failed: {0}")]
    Storage(#[from] PostgresError),
    #[error("a different owner record already exists for this offer")]
    OwnerCollision,
    #[error("receipt belongs to another authenticated scope")]
    ScopeMismatch,
    #[error("receipt key was reused with different input")]
    InputMismatch,
    #[error("receipt is pending recovery")]
    Pending,
    #[error("receipt is absent or no longer pending")]
    NotPending,
    #[error("receipt was completed with a different value")]
    CompletionMismatch,
}

impl From<::postgres::Error> for ReceiptMutationError {
    fn from(error: ::postgres::Error) -> Self {
        Self::Storage(error.into())
    }
}

impl From<ReceiptMutationError> for OfferOwnerError {
    fn from(error: ReceiptMutationError) -> Self {
        match error {
            ReceiptMutationError::OwnerCollision => Self::Collision,
            ReceiptMutationError::Storage(error) => Self::Storage(error),
            other => Self::Storage(PostgresError(other.to_string())),
        }
    }
}

impl From<ReceiptMutationError> for OperationReceiptError {
    fn from(error: ReceiptMutationError) -> Self {
        match error {
            ReceiptMutationError::Storage(error) => Self::Storage(error),
            ReceiptMutationError::ScopeMismatch => Self::ScopeMismatch,
            ReceiptMutationError::InputMismatch => Self::InputMismatch,
            ReceiptMutationError::Pending => Self::Pending,
            ReceiptMutationError::NotPending => Self::NotPending,
            ReceiptMutationError::CompletionMismatch => Self::CompletionMismatch,
            ReceiptMutationError::OwnerCollision => Self::Storage(PostgresError(error.to_string())),
        }
    }
}

impl From<ReceiptMutationError> for ProcessedResultError {
    fn from(error: ReceiptMutationError) -> Self {
        match error {
            ReceiptMutationError::Storage(error) => Self::Storage(error),
            ReceiptMutationError::ScopeMismatch => Self::ScopeMismatch,
            ReceiptMutationError::Pending => Self::Pending,
            ReceiptMutationError::NotPending => Self::NotPending,
            ReceiptMutationError::CompletionMismatch => Self::CompletionMismatch,
            ReceiptMutationError::OwnerCollision | ReceiptMutationError::InputMismatch => {
                Self::Storage(PostgresError(error.to_string()))
            }
        }
    }
}

fn read_offer_owner(client: &mut Client, key: &OfferOwnerKey) -> Result<Option<OfferOwnerRecord>, PostgresError> {
    Ok(client
        .query_opt(
            "SELECT caller_id, session_id, root, parent_id, arguments, tool, spelling \
             FROM openappa_offer_owners WHERE organization_id=$1 AND offer_id=$2",
            &[&key.organization_id, &key.offer_id],
        )?
        .map(|row| OfferOwnerRecord {
            scope: ReceiptScope {
                organization_id: key.organization_id.clone(),
                caller_id: row.get(0),
                session_id: row.get(1),
                binding: ReceiptBinding::Caller,
            },
            offer_id: key.offer_id.clone(),
            root: row.get(2),
            parent_id: row.get(3),
            arguments: row.get(4),
            tool: row.get(5),
            spelling: row.get(6),
        }))
}

fn offer_owner_lock(organization_id: &str, offer_id: &str) -> String {
    format!("openappa-offer-owner:{organization_id}:{offer_id}")
}

fn operation_lock(session_id: &str, operation_id: &str) -> String {
    format!("openappa-operation:{session_id}:{operation_id}")
}

fn result_lock(session_id: &str, tool_call_id: &str) -> String {
    format!("openappa-result:{session_id}:{tool_call_id}")
}

fn session_lock(scope: &ReceiptScope) -> String {
    format!(
        "openappa-offer-session:{}:{}:{}",
        scope.organization_id,
        scope.caller_id.as_deref().unwrap_or(""),
        scope.session_id
    )
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

pub struct PostgresTransaction {
    store: PostgresStore,
    finished: bool,
}

impl PostgresTransaction {
    pub fn commit(mut self) -> Result<(), PostgresError> {
        self.store.run(|state| {
            state.client.batch_execute("COMMIT")?;
            state.transaction = false;
            Ok(())
        })?;
        self.finished = true;
        Ok(())
    }
}

impl Drop for PostgresTransaction {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.store.run(|state| {
                let result = state.client.batch_execute("ROLLBACK");
                state.transaction = false;
                result.map_err(Into::into)
            });
        }
    }
}
