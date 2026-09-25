//! PostgreSQL storage for embedded hosts. The host installs the schema; Rust
//! retains the SQLite event encoding and policy-file validation. Each pooled
//! connection has a dedicated thread, which keeps the synchronous log API
//! usable from async hooks.
//!
//! A host that needs several statements on one connection — an outer
//! transaction around a hook dispatch, a session-level advisory lock — leases
//! one ([`PostgresStore::lease`]). Work on different leases runs concurrently.
//! A connection that returns to the pool closed, or that does not answer its
//! reset, is dropped, and a later checkout opens another in its place.
//!
//! The schema the host's migrations must provide:
//!
//! ```sql
//! CREATE TABLE openappa_events (root TEXT NOT NULL, seq BIGINT NOT NULL, payload BYTEA NOT NULL,
//!                               PRIMARY KEY (root, seq));
//! CREATE TABLE openappa_policy_files (hash TEXT PRIMARY KEY, bytes BYTEA NOT NULL);
//! CREATE TABLE openappa_host_keys (key TEXT NOT NULL, root TEXT NOT NULL, PRIMARY KEY (key, root));
//! CREATE TABLE openappa_offer_owners (
//!     organization_id TEXT NOT NULL, caller_id TEXT, session_id TEXT NOT NULL, binding TEXT NOT NULL,
//!     offer_id TEXT NOT NULL, root TEXT NOT NULL, parent_id TEXT, arguments TEXT, tool TEXT, spelling TEXT,
//!     PRIMARY KEY (organization_id, offer_id));
//! ```

use std::num::NonZeroUsize;
use std::sync::{Arc, Condvar, mpsc};
use std::time::{Duration, Instant};

use ::postgres::Client;
use postgres_native_tls::MakeTlsConnector;
use serde_json::Value;

use super::*;
use crate::receipts::{StoredOperationInput, binding_name, parse_binding, scope_matches};

pub use crate::receipts::{
    OfferOwnerKey, OfferOwnerRecord, OperationClaim, OperationKey, OperationRequest, ProcessedResultClaim,
    ProcessedResultKey, ProcessedResultRequest, ReceiptBinding, ReceiptError, ReceiptScope, ReceiptStorageError,
};

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct PostgresError(pub String);

impl From<::postgres::Error> for PostgresError {
    fn from(error: ::postgres::Error) -> Self {
        Self(error.to_string())
    }
}

impl From<PostgresError> for ReceiptStorageError {
    fn from(error: PostgresError) -> Self {
        Self(error.0)
    }
}

/// Why a store could not hand out a connection.
#[derive(Debug, thiserror::Error)]
pub enum LeaseError {
    #[error("no PostgreSQL connection became free within {0:?}")]
    Exhausted(Duration),
    #[error("PostgreSQL connection failed: {0}")]
    Connect(#[from] PostgresError),
    #[error("only a PostgreSQL store leases connections")]
    NotPostgres,
}

impl From<LeaseError> for PostgresError {
    fn from(error: LeaseError) -> Self {
        match error {
            LeaseError::Connect(error) => error,
            other => Self(other.to_string()),
        }
    }
}

struct ConnectionState {
    client: Client,
    transaction: bool,
    /// Host SQL ran on this connection, so it may hold session-level advisory locks.
    host_sql: bool,
}

type Job = Box<dyn FnOnce(&mut ConnectionState) + Send>;

/// One connection on its own thread. The thread ends when the worker drops.
struct Worker {
    sender: mpsc::Sender<Job>,
}

impl Worker {
    /// `wait` bounds each connection attempt and the opening as a whole, unless the URL
    /// sets its own `connect_timeout`.
    fn connect(url: String, wait: Duration, stall: Option<Duration>) -> Result<Worker, PostgresError> {
        let (sender, receiver) = mpsc::channel::<Job>();
        let (ready, initialized) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("appa-postgres".into())
            .spawn(move || {
                let connect = || -> Result<Client, PostgresError> {
                    if let Some(stall) = stall {
                        std::thread::sleep(stall);
                    }
                    let tls = native_tls::TlsConnector::new().map_err(|e| PostgresError(e.to_string()))?;
                    let mut config: ::postgres::Config = url.parse()?;
                    if config.get_connect_timeout().is_none() {
                        config.connect_timeout(wait);
                    }
                    let mut client = config.connect(MakeTlsConnector::new(tls))?;
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
                            host_sql: false,
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
            .recv_timeout(wait)
            .map_err(|_| PostgresError("the PostgreSQL connection did not open in time".into()))??;
        Ok(Worker { sender })
    }

    /// Run `job` on the connection's thread. `None`: the worker is gone, or it gave no
    /// answer within `wait`.
    fn ask<T: Send + 'static>(
        &self,
        wait: Option<Duration>,
        job: impl FnOnce(&mut ConnectionState) -> T + Send + 'static,
    ) -> Option<T> {
        let (send, recv) = mpsc::sync_channel(1);
        self.sender
            .send(Box::new(move |state| {
                let _ = send.send(job(state));
            }))
            .ok()?;
        match wait {
            Some(wait) => recv.recv_timeout(wait).ok(),
            None => recv.recv().ok(),
        }
    }

    fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut ConnectionState) -> Result<T, PostgresError> + Send + 'static,
    ) -> Result<T, PostgresError> {
        self.ask(None, operation)
            .ok_or_else(|| PostgresError("connection worker stopped".into()))?
    }

    /// Leave the connection as a fresh one would be: no open transaction, no session-level
    /// advisory lock. `false` means the connection cannot be reused, and an answer that does
    /// not come within `wait` counts as one.
    fn reset(&self, wait: Duration, stall: Option<Duration>) -> bool {
        self.ask(Some(wait), move |state| {
            if let Some(stall) = stall {
                std::thread::sleep(stall);
            }
            let mut reset = || -> Result<(), ::postgres::Error> {
                if state.transaction {
                    state.transaction = false;
                    state.client.batch_execute("ROLLBACK")?;
                }
                if state.host_sql {
                    state.host_sql = false;
                    state.client.batch_execute("SELECT pg_advisory_unlock_all()")?;
                }
                Ok(())
            };
            reset().is_ok() && !state.client.is_closed()
        })
        .unwrap_or(false)
    }

    /// Whether an idle connection still serves. The client learns that the server ended
    /// it, or left it inside a failed transaction, only by using it.
    fn serves(&self, wait: Duration) -> bool {
        self.ask(Some(wait), |state| state.client.batch_execute("SELECT 1").is_ok())
            .unwrap_or(false)
    }
}

struct Pool {
    url: String,
    max_connections: NonZeroUsize,
    state: Mutex<PoolState>,
    freed: Condvar,
}

/// A connection that came back this recently is leased again without being asked whether
/// it still serves, so a busy pool pays no round-trip for the check.
const TRUSTED_IDLE: Duration = Duration::from_secs(1);

struct PoolState {
    /// Each with the moment it came back.
    idle: Vec<(Worker, Instant)>,
    /// Connections that exist: idle, leased, or being opened.
    open: usize,
    checkout_wait: Duration,
    reset_wait: Duration,
    /// Armed only by the `fault-injection` fail point.
    stall_next_reset: Option<Duration>,
    /// Armed only by the `fault-injection` fail point.
    stall_next_connect: Option<Duration>,
}

impl Pool {
    fn state(&self) -> std::sync::MutexGuard<'_, PoolState> {
        self.state
            .lock()
            .expect("the pool mutex is never poisoned: no panic runs while it is held")
    }

    fn checkout(self: &Arc<Self>) -> Result<Lease, LeaseError> {
        let mut state = self.state();
        let wait = state.checkout_wait;
        let deadline = Instant::now() + wait;
        loop {
            if let Some((worker, returned)) = state.idle.pop() {
                let wait = state.reset_wait;
                drop(state);
                if returned.elapsed() < TRUSTED_IDLE || worker.serves(wait) {
                    return Ok(Lease {
                        pool: Arc::clone(self),
                        worker: Some(worker),
                    });
                }
                drop(worker);
                self.forget();
                state = self.state();
                continue;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(LeaseError::Exhausted(wait));
            }
            if state.open < self.max_connections.get() {
                state.open += 1;
                let stall = state.stall_next_connect.take();
                drop(state);
                return match Worker::connect(self.url.clone(), remaining, stall) {
                    Ok(worker) => Ok(Lease {
                        pool: Arc::clone(self),
                        worker: Some(worker),
                    }),
                    Err(error) => {
                        self.forget();
                        Err(LeaseError::Connect(error))
                    }
                };
            }
            state = self
                .freed
                .wait_timeout(state, remaining)
                .expect("the pool mutex is never poisoned: no panic runs while it is held")
                .0;
        }
    }

    /// A connection that existed no longer does; a waiter may open its replacement.
    fn forget(&self) {
        self.state().open -= 1;
        self.freed.notify_one();
    }

    fn release(&self, worker: Worker) {
        let (wait, stall) = {
            let mut state = self.state();
            (state.reset_wait, state.stall_next_reset.take())
        };
        if worker.reset(wait, stall) {
            self.state().idle.push((worker, Instant::now()));
            self.freed.notify_one();
        } else {
            // A worker that did not answer is abandoned with its thread; the server ends
            // that connection's transaction and locks when the connection itself ends.
            self.forget();
        }
    }
}

/// One pooled connection, held by every store clone derived from the lease. The connection
/// goes back to the pool when the last of them drops, so a transaction always ends before
/// its connection is reset and reused.
struct Lease {
    pool: Arc<Pool>,
    worker: Option<Worker>,
}

impl Lease {
    fn worker(&self) -> &Worker {
        self.worker.as_ref().expect("a lease holds its worker until it drops")
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            self.pool.release(worker);
        }
    }
}

#[derive(Clone)]
pub struct PostgresStore {
    pool: Arc<Pool>,
    lease: Option<Arc<Lease>>,
}

impl PostgresStore {
    pub(super) fn open(url: String, max_connections: NonZeroUsize) -> Result<Self, PostgresError> {
        let pool = Arc::new(Pool {
            url,
            max_connections,
            state: Mutex::new(PoolState {
                idle: Vec::new(),
                open: 0,
                checkout_wait: Duration::from_secs(30),
                reset_wait: Duration::from_secs(5),
                stall_next_reset: None,
                stall_next_connect: None,
            }),
            freed: Condvar::new(),
        });
        // The first connection opens now, so a missing host migration refuses startup.
        drop(pool.checkout()?);
        Ok(Self { pool, lease: None })
    }

    /// A store pinned to one pooled connection. Every clone of it, and every transaction it
    /// begins, runs on that connection; an unleased store takes a connection per operation.
    ///
    /// A full pool blocks the calling thread until a connection returns, and refuses with
    /// [`LeaseError::Exhausted`] after the checkout wait. A host that must not block admits
    /// no more concurrent work than `max_connections` before it asks for a lease.
    pub fn lease(&self) -> Result<PostgresStore, LeaseError> {
        Ok(PostgresStore {
            pool: Arc::clone(&self.pool),
            lease: Some(Arc::new(self.pool.checkout()?)),
        })
    }

    fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut ConnectionState) -> Result<T, PostgresError> + Send + 'static,
    ) -> Result<T, PostgresError> {
        match &self.lease {
            Some(lease) => lease.worker().run(operation),
            None => self.pool.checkout()?.worker().run(operation),
        }
    }

    /// Host integration SQL (receipts, advisory locks) runs on the leased connection, the
    /// one the event log uses. It is refused without a lease: whatever it left on a
    /// per-operation connection, a session-level lock above all, would be gone the moment
    /// the call returned. Never expose this capability to clients.
    pub fn with_client<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Client) -> Result<T, PostgresError> + Send + 'static,
    ) -> Result<T, PostgresError> {
        if self.lease.is_none() {
            return Err(PostgresError("host SQL requires a leased connection".into()));
        }
        self.run(move |state| {
            state.host_sql = true;
            operation(&mut state.client)
        })
    }

    /// The library's own statements, which take no session-level lock.
    fn query<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Client) -> Result<T, PostgresError> + Send + 'static,
    ) -> Result<T, PostgresError> {
        self.run(move |state| operation(&mut state.client))
    }

    /// How long a checkout waits for a free connection, and how long a returned connection
    /// has to answer its reset.
    #[cfg(feature = "fault-injection")]
    pub fn set_waits(&self, checkout: Duration, reset: Duration) {
        let mut state = self.pool.state();
        state.checkout_wait = checkout;
        state.reset_wait = reset;
    }

    /// Arm the fail point: the next returned connection takes `stall` to answer its reset.
    #[cfg(feature = "fault-injection")]
    pub fn stall_next_reset(&self, stall: Duration) {
        self.pool.state().stall_next_reset = Some(stall);
    }

    /// Arm the fail point: the next connection the pool opens takes `stall` before connecting.
    #[cfg(feature = "fault-injection")]
    pub fn stall_next_connect(&self, stall: Duration) {
        self.pool.state().stall_next_connect = Some(stall);
    }

    /// Stores an offer owner record. Repeated writes with identical data are idempotent.
    pub fn store_offer_owner(&self, record: OfferOwnerRecord) -> Result<(), ReceiptError> {
        let lock = offer_owner_lock(&record.scope.organization_id, &record.offer_id);
        self.mutate_receipt(lock, move |client| {
            let binding = binding_name(record.scope.binding).to_owned();
            let inserted = client
                .query_opt(
                    "INSERT INTO openappa_offer_owners (organization_id, caller_id, session_id, binding, offer_id, root, parent_id, arguments, tool, spelling) \
                     VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) \
                     ON CONFLICT (organization_id, offer_id) DO NOTHING \
                     RETURNING organization_id",
                    &[
                        &record.scope.organization_id,
                        &record.scope.caller_id,
                        &record.scope.session_id,
                        &binding,
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
                Err(ReceiptMutationError::Collision)
            }
        })
        .map_err(ReceiptError::from)
    }

    /// Reads an offer owner record by key.
    pub fn offer_owner(&self, key: OfferOwnerKey) -> Result<Option<OfferOwnerRecord>, PostgresError> {
        self.query(move |client| read_offer_owner(client, &key))
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

    /// Claims an operation receipt before starting work. Completed receipts return saved decisions.
    pub fn claim_operation(&self, request: OperationRequest) -> Result<OperationClaim, ReceiptError> {
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
            let stored = StoredOperationInput::decode(row.get(4))
                .map_err(|error| ReceiptMutationError::Storage(PostgresError(error)))?;
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
        .map_err(ReceiptError::from)
    }

    /// Completes a claimed operation receipt with its final decision.
    pub fn complete_operation(&self, key: OperationKey, decision: Value) -> Result<(), ReceiptError> {
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
            let stored = StoredOperationInput::decode(row.get(3))
                .map_err(|error| ReceiptMutationError::Storage(PostgresError(error)))?;
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
        .map_err(ReceiptError::from)
    }

    /// Claims a durable processed-result receipt before result processing.
    pub fn claim_processed_result(
        &self,
        request: ProcessedResultRequest,
    ) -> Result<ProcessedResultClaim, ReceiptError> {
        let lock = result_lock(&request.key.session_id, &request.key.tool_call_id);
        self.mutate_receipt(lock, move |client| {
            let existing = client.query_opt(
                "SELECT organization_id, session_id, root, status, approved_output, decision \
                 FROM openappa_processed_results WHERE session_id=$1 AND tool_call_id=$2 FOR UPDATE",
                &[&request.key.session_id, &request.key.tool_call_id],
            )?;
            let Some(row) = existing else {
                client.execute(
                    "INSERT INTO openappa_processed_results (organization_id, caller_id, session_id, tool_call_id, root, status) \
                     VALUES ($1,$2,$3,$4,$5,'pending')",
                    &[
                        &request.key.organization_id,
                        &request.key.caller_id,
                        &request.key.session_id,
                        &request.key.tool_call_id,
                        &request.root,
                    ],
                )?;
                return Ok(ProcessedResultClaim::Claimed);
            };
            if !request.key.owns(row.get(0), row.get(1)) || row.get::<_, String>(2) != request.root {
                return Err(ReceiptMutationError::ScopeMismatch);
            }
            match row.get::<_, String>(3).as_str() {
                "complete" => Ok(ProcessedResultClaim::Complete {
                    approved_output: row
                        .get::<_, Option<String>>(4)
                        .ok_or_else(|| ReceiptMutationError::Storage(PostgresError("complete result lacks approved output".into())))?,
                    decision: row
                        .get::<_, Option<Value>>(5)
                        .ok_or_else(|| ReceiptMutationError::Storage(PostgresError("complete result lacks a decision".into())))?,
                }),
                "pending" => Err(ReceiptMutationError::Pending),
                _ => Err(ReceiptMutationError::Storage(PostgresError("processed result has an invalid status".into()))),
            }
        })
        .map_err(ReceiptError::from)
    }

    /// Completes a processed-result receipt with its approved output and decision.
    pub fn complete_processed_result(
        &self,
        key: ProcessedResultKey,
        approved_output: String,
        decision: Value,
    ) -> Result<(), ReceiptError> {
        let lock = result_lock(&key.session_id, &key.tool_call_id);
        self.mutate_receipt(lock, move |client| {
            let existing = client.query_opt(
                "SELECT organization_id, session_id, status, approved_output, decision \
                 FROM openappa_processed_results WHERE session_id=$1 AND tool_call_id=$2 FOR UPDATE",
                &[&key.session_id, &key.tool_call_id],
            )?;
            let Some(row) = existing else {
                return Err(ReceiptMutationError::NotPending);
            };
            if !key.owns(row.get(0), row.get(1)) {
                return Err(ReceiptMutationError::ScopeMismatch);
            }
            match row.get::<_, String>(2).as_str() {
                "pending" => {
                    client.execute(
                        "UPDATE openappa_processed_results SET status='complete', approved_output=$3, decision=$4 \
                         WHERE session_id=$1 AND tool_call_id=$2",
                        &[&key.session_id, &key.tool_call_id, &approved_output, &decision],
                    )?;
                    Ok(())
                }
                "complete"
                    if row.get::<_, Option<String>>(3).as_ref() == Some(&approved_output)
                        && row.get::<_, Option<Value>>(4).as_ref() == Some(&decision) =>
                {
                    Ok(())
                }
                "complete" => Err(ReceiptMutationError::CompletionMismatch),
                _ => Err(ReceiptMutationError::Storage(PostgresError(
                    "processed result has an invalid status".into(),
                ))),
            }
        })
        .map_err(ReceiptError::from)
    }

    /// Checks whether pending receipts exist for a root trajectory.
    pub fn has_pending_receipts(&self, root: String) -> Result<bool, PostgresError> {
        self.query(move |client| {
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

    /// Starts an outer transaction across hook dispatches. Only a leased store holds one:
    /// the transaction belongs to the lease's connection, and no other store can join it.
    pub fn begin(&self) -> Result<PostgresTransaction, PostgresError> {
        if self.lease.is_none() {
            return Err(PostgresError("a transaction requires a leased connection".into()));
        }
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
        self.query(move |client| {
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
        let batches = self.query(move |client| {
            let rows = client.query(
                "SELECT seq, payload FROM openappa_events WHERE root = $1 ORDER BY seq",
                &[&id],
            )?;
            contiguous(rows.into_iter().map(|row| (row.get(0), row.get(1))).collect())
                .map_err(|gap| PostgresError(gap.to_string()))
        })?;
        let Some(first) = batches.first() else {
            return Err(ReadError::UnknownRoot {
                root: root.as_str().to_owned(),
            });
        };
        let opening = decode(first)?;
        let Some(Fact::TrajectoryOpened(appa_engine::fact::TrajectoryOpening { policy_file_key, .. })) =
            opening.facts.first()
        else {
            return Err(ReadError::Undecodable("log does not begin with an opening".into()));
        };
        let hash = policy_file_key.as_str().to_owned();
        let lookup = hash.clone();
        let policy = self
            .query(move |client| {
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
        let roots = self.query(move |client| {
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
                .query_one(
                    "SELECT COALESCE(MAX(seq) + 1, 0) FROM openappa_events WHERE root = $1",
                    &[&root],
                )?
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
    Collision,
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

impl From<ReceiptMutationError> for ReceiptError {
    fn from(error: ReceiptMutationError) -> Self {
        match error {
            ReceiptMutationError::Storage(error) => Self::Storage(error.into()),
            ReceiptMutationError::Collision => Self::Collision,
            ReceiptMutationError::ScopeMismatch => Self::ScopeMismatch,
            ReceiptMutationError::InputMismatch => Self::InputMismatch,
            ReceiptMutationError::Pending => Self::Pending,
            ReceiptMutationError::NotPending => Self::NotPending,
            ReceiptMutationError::CompletionMismatch => Self::CompletionMismatch,
        }
    }
}

fn read_offer_owner(client: &mut Client, key: &OfferOwnerKey) -> Result<Option<OfferOwnerRecord>, PostgresError> {
    let row = client.query_opt(
        "SELECT caller_id, session_id, binding, root, parent_id, arguments, tool, spelling \
         FROM openappa_offer_owners WHERE organization_id=$1 AND offer_id=$2",
        &[&key.organization_id, &key.offer_id],
    )?;
    let Some(row) = row else {
        return Ok(None);
    };
    let binding = parse_binding(row.get::<_, String>(2).as_str()).map_err(PostgresError)?;
    Ok(Some(OfferOwnerRecord {
        scope: ReceiptScope {
            organization_id: key.organization_id.clone(),
            caller_id: row.get(0),
            session_id: row.get(1),
            binding,
        },
        offer_id: key.offer_id.clone(),
        root: row.get(3),
        parent_id: row.get(4),
        arguments: row.get(5),
        tool: row.get(6),
        spelling: row.get(7),
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
                // A rollback that failed leaves the flag up, so the pool retries it and
                // drops the connection rather than reusing one still inside a transaction.
                state.client.batch_execute("ROLLBACK")?;
                state.transaction = false;
                Ok(())
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The advisory lock keys are shared with every other process writing the same database,
    /// so their spelling is a wire format.
    #[test]
    fn advisory_lock_keys_are_frozen() {
        assert_eq!(offer_owner_lock("org", "offer"), "openappa-offer-owner:org:offer");
        assert_eq!(operation_lock("session", "op"), "openappa-operation:session:op");
        assert_eq!(result_lock("session", "call"), "openappa-result:session:call");
        let scope = |caller_id: Option<&str>| ReceiptScope {
            organization_id: "org".to_owned(),
            caller_id: caller_id.map(str::to_owned),
            session_id: "session".to_owned(),
            binding: ReceiptBinding::Caller,
        };
        assert_eq!(
            session_lock(&scope(Some("caller"))),
            "openappa-offer-session:org:caller:session"
        );
        assert_eq!(session_lock(&scope(None)), "openappa-offer-session:org::session");
    }
}
