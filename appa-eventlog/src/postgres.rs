//! PostgreSQL storage for embedded hosts. The host installs the schema; Rust
//! retains the SQLite event encoding and policy-file validation. Each pooled
//! connection has a dedicated thread, which keeps the synchronous log API
//! usable from async hooks.
//!
//! A host that needs several statements on one connection — its own rows
//! written beside a hook dispatch — leases one ([`LogStore::lease`]) and runs
//! its own SQL there through [`LeasedPostgres::with_client`]. Work on
//! different leases runs concurrently. A returned connection has any
//! transaction the host left open rolled back. One that returns closed, or
//! that does not answer that reset, is dropped, and a later checkout opens
//! another in its place.
//!
//! Receipts, held peer messages, embedded peer messages and file workspaces
//! coordinate through rows alone. A primary key admits one row per identity
//! (`INSERT … ON CONFLICT DO NOTHING`), and writers that must not interleave
//! lock a row they share (`SELECT … FOR UPDATE`) for one transaction: the
//! receipt row itself, the receiver's `openappa_peer_inboxes` row, or the
//! family's `openappa_roots` row. Whether a pending receipt's owner still
//! drives it is the lease on its row, judged by the database clock.
//!
//! The host's migrations provide the schema: `openappa_events`, `openappa_policy_files`,
//! `openappa_host_keys`, the receipt tables `openappa_operations` and
//! `openappa_processed_results`, `openappa_held_peer_messages` with
//! `openappa_peer_inboxes`, and `openappa_roots`.
//! `tests/fixtures/host_schema.sql` holds the full DDL.

use std::num::NonZeroUsize;
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime};

use crate::encryption::{StorageCipher, field_context};
use ::postgres::{Client, GenericClient};
use postgres_native_tls::MakeTlsConnector;
use serde_json::Value;

use super::*;
use crate::embedded::{
    DirectTake, EmbeddedClaim, EmbeddedError, EmbeddedRow, EmbeddedStatus, NewEmbedded, ReadClaim, ReadTake,
    StoredEmbedded, TICKET_BOUND, TICKET_OPENING, classify_read, next_read_ticket, read_ticket, same_payload,
};
use crate::encoding::{contiguous, contiguous_from, decoded, extend, opening_key};
use crate::held::{NewHeld, StoredNotice, millis, quota_limit};
use crate::receipts::{
    Completion, SessionScope, StoredLease, StoredOperation, StoredOperationInput, StoredResult,
    resolve_operation_claim, resolve_operation_completion, resolve_result_claim, resolve_result_completion,
};

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct PostgresError(pub String);

impl From<::postgres::Error> for PostgresError {
    fn from(error: ::postgres::Error) -> Self {
        Self(error.to_string())
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
    /// Host SQL ran on this connection, so it may have left a transaction open.
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
                    // Deliberately no DDL. Core host migrations are required at startup;
                    // optional file-event migrations are checked only when file tracking uses them.
                    client.batch_execute(
                        "SELECT root, seq, payload FROM openappa_events LIMIT 0;
                    SELECT hash, bytes FROM openappa_policy_files LIMIT 0;
                    SELECT key, root FROM openappa_host_keys LIMIT 0;
                    SELECT seq, id, receiver, digest, label, body, expires_at, notified FROM openappa_held_peer_messages LIMIT 0;
                    SELECT seq, id, root, sender, recipient, pending_spawn, dispatch, digest, label, body, status, read_call_id, decision, expires_at, read_arguments, created_at FROM openappa_embedded_peer_messages LIMIT 0;
                    SET lock_timeout = '30s'; SET statement_timeout = '60s'",
                    )?;
                    check_receipt_keys(&mut client)?;
                    Ok(client)
                };
                match connect() {
                    Ok(client) => {
                        let _ = ready.send(Ok(()));
                        let mut state = ConnectionState {
                            client,
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

    /// Leave the connection as a fresh one would be: no open transaction, and so no row lock
    /// it held. `false` means the connection cannot be reused, and an answer that does not come
    /// within `wait` counts as one.
    fn reset(&self, wait: Duration, stall: Option<Duration>) -> bool {
        self.ask(Some(wait), move |state| {
            if let Some(stall) = stall {
                std::thread::sleep(stall);
            }
            let mut reset = || -> Result<(), ::postgres::Error> {
                if state.host_sql {
                    state.host_sql = false;
                    state.client.batch_execute("ROLLBACK")?;
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

/// One pooled connection, held by a leased store. It goes back to the pool when the store
/// drops.
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

pub(crate) struct PostgresStore {
    pool: Arc<Pool>,
    lease: Option<LeasedPostgres>,
    /// Shared with every lease of this store.
    logs: Arc<LogCache>,
    pub(crate) cipher: StorageCipher,
}

/// How many facts the cached logs of one store hold at most, together. A log read past the
/// bound is still read, from its first batch.
const CACHED_FACTS: usize = 250_000;

/// The logs this process read last, so a later read of the same root decodes only the batches
/// accepted since. Batches are append-only, so a cached log stays a true prefix; a read still
/// fetches the cached log's last batch again and drops the entry when the stored bytes differ,
/// which catches a log that was removed or rewritten behind the cache.
#[derive(Default)]
struct LogCache {
    entries: Mutex<CachedLogs>,
}

#[derive(Default)]
struct CachedLogs {
    logs: std::collections::HashMap<String, CachedLog>,
    /// Roots in the order they were last used, oldest first.
    order: std::collections::VecDeque<String>,
    facts: usize,
}

#[derive(Clone)]
struct CachedLog {
    log: Log,
    last_batch: Vec<u8>,
}

impl LogCache {
    fn entries(&self) -> std::sync::MutexGuard<'_, CachedLogs> {
        self.entries
            .lock()
            .expect("the log cache mutex is never poisoned: no panic runs while it is held")
    }

    fn get(&self, root: &str) -> Option<CachedLog> {
        self.entries().logs.get(root).cloned()
    }

    fn forget(&self, root: &str) {
        let mut entries = self.entries();
        if let Some(gone) = entries.logs.remove(root) {
            entries.facts -= gone.log.facts().len();
            entries.order.retain(|cached| cached != root);
        }
    }

    /// Keep `log` unless the cache already holds a later read of its root.
    fn keep(&self, log: &Log, last_batch: &[u8]) {
        let root = log.root().as_str();
        let size = log.facts().len();
        let mut entries = self.entries();
        if size > CACHED_FACTS {
            return;
        }
        if entries
            .logs
            .get(root)
            .is_some_and(|cached| cached.log.basis() >= log.basis())
        {
            // A read the cache already answered still counts as a use.
            entries.order.retain(|cached| cached != root);
            entries.order.push_back(root.to_owned());
            return;
        }
        if let Some(older) = entries.logs.remove(root) {
            entries.facts -= older.log.facts().len();
            entries.order.retain(|cached| cached != root);
        }
        while entries.facts + size > CACHED_FACTS {
            let Some(oldest) = entries.order.pop_front() else {
                break;
            };
            let gone = entries.logs.remove(&oldest);
            debug_assert!(gone.is_some(), "every root in the order has a cached log");
            if let Some(gone) = gone {
                entries.facts -= gone.log.facts().len();
            }
        }
        entries.order.push_back(root.to_owned());
        entries.facts += size;
        entries.logs.insert(
            root.to_owned(),
            CachedLog {
                log: log.clone(),
                last_batch: last_batch.to_vec(),
            },
        );
    }
}

/// One read-decide-append step on one root's log: a transaction on one connection that holds
/// the root's row lock from the read to the append, so writers of a family take turns instead
/// of losing a compare-and-swap to each other. The lock and the connection are held for this
/// step only; dropping the step without an append rolls the transaction back.
pub(crate) struct PostgresStep<'a> {
    store: &'a PostgresStore,
    /// The connection checked out for this step when the store is not leased.
    own: Option<Lease>,
    root: TrajectoryId,
    open: bool,
}

/// The connection a leased store runs on, for the host's own SQL. Only a leased store hands
/// one out ([`LogStore::postgres`]).
pub struct LeasedPostgres {
    lease: Lease,
}

impl LeasedPostgres {
    /// Host integration SQL (its own tables and transactions) runs on the leased connection, the
    /// one the event log and receipts use. A transaction it leaves open is rolled back when the
    /// connection returns to the pool. Never expose this capability to clients.
    pub fn with_client<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Client) -> Result<T, PostgresError> + Send + 'static,
    ) -> Result<T, PostgresError> {
        self.lease.worker().run(move |state| {
            state.host_sql = true;
            operation(&mut state.client)
        })
    }
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
        Ok(Self {
            pool,
            lease: None,
            logs: Arc::default(),
            cipher: StorageCipher::Plain,
        })
    }

    pub(crate) fn encrypted_view(&self, cipher: StorageCipher) -> Self {
        Self {
            pool: Arc::clone(&self.pool),
            lease: None,
            logs: Arc::default(),
            cipher,
        }
    }

    /// A store pinned to one pooled connection; an unleased store takes a connection per
    /// operation.
    ///
    /// A full pool blocks the calling thread until a connection returns, and refuses with
    /// [`LeaseError::Exhausted`] after the checkout wait. A host that must not block admits
    /// no more concurrent work than `max_connections` before it asks for a lease.
    pub(crate) fn lease(&self) -> Result<PostgresStore, LeaseError> {
        Ok(PostgresStore {
            pool: Arc::clone(&self.pool),
            lease: Some(LeasedPostgres {
                lease: self.pool.checkout()?,
            }),
            logs: Arc::clone(&self.logs),
            cipher: self.cipher.clone(),
        })
    }

    pub(crate) fn leased(&self) -> Option<&LeasedPostgres> {
        self.lease.as_ref()
    }

    fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut ConnectionState) -> Result<T, PostgresError> + Send + 'static,
    ) -> Result<T, PostgresError> {
        match &self.lease {
            Some(leased) => leased.lease.worker().run(operation),
            None => self.pool.checkout()?.worker().run(operation),
        }
    }

    /// The library's own statements, which take no session-level lock.
    fn query<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Client) -> Result<T, PostgresError> + Send + 'static,
    ) -> Result<T, PostgresError> {
        self.run(move |state| operation(&mut state.client))
    }

    /// Run `operation` in one transaction, committed when it succeeds and rolled back when it
    /// fails. Writers that must not interleave lock a row they share inside it.
    fn transaction<T, E>(&self, operation: impl FnOnce(&mut Client) -> Result<T, E> + Send + 'static) -> Result<T, E>
    where
        T: Send + 'static,
        E: From<PostgresError> + Send + 'static,
    {
        self.query(move |client| {
            client.batch_execute("BEGIN")?;
            let result = operation(client);
            match result {
                Ok(_) => client.batch_execute("COMMIT")?,
                // The operation's own error is the one to report. A connection that cannot
                // roll back is dropped by the pool's reset, and the server ends the
                // transaction with it.
                Err(_) => {
                    let _ = client.batch_execute("ROLLBACK");
                }
            }
            Ok(result)
        })?
    }

    /// How long a checkout waits for a free connection, and how long a returned connection
    /// has to answer its reset.
    #[cfg(all(test, feature = "fault-injection"))]
    pub(crate) fn set_waits(&self, checkout: Duration, reset: Duration) {
        let mut state = self.pool.state();
        state.checkout_wait = checkout;
        state.reset_wait = reset;
    }

    /// Arm the fail point: the next returned connection takes `stall` to answer its reset.
    #[cfg(all(test, feature = "fault-injection"))]
    pub(crate) fn stall_next_reset(&self, stall: Duration) {
        self.pool.state().stall_next_reset = Some(stall);
    }

    /// Arm the fail point: the next connection the pool opens takes `stall` before connecting.
    #[cfg(all(test, feature = "fault-injection"))]
    pub(crate) fn stall_next_connect(&self, stall: Duration) {
        self.pool.state().stall_next_connect = Some(stall);
    }

    /// Claims an operation receipt under `lease` before starting work. The primary key admits
    /// one pending row; a claim that finds one already there locks it and resolves it by its
    /// status and lease.
    pub(crate) fn claim_operation(
        &self,
        request: OperationRequest,
        lease: ReceiptLease,
    ) -> Result<OperationClaim, ReceiptError> {
        let input = serde_json::to_value(StoredOperationInput::from_request(&request))
            .map_err(|error| ReceiptError::storage(error.to_string()))?;
        let cipher = self.cipher.clone();
        let input = cipher.seal_json(&operation_field(&request.key, "input"), &input)?;
        self.transaction(move |client| {
            let key = &request.key;
            check_root_cipher(client, request.root.as_str(), &cipher)?;
            for _ in 0..CLAIM_ATTEMPTS {
                let inserted = client.execute(
                    "INSERT INTO openappa_operations \
                         (organization_id, caller_id, session_id, operation_id, root, input, status, lease_owner, lease_expires_at) \
                     VALUES ($1,$2,$3,$4,$5,$6,'pending',$7, now() + $8::bigint * interval '1 microsecond') \
                     ON CONFLICT DO NOTHING",
                    &[
                        &key.session.organization_id,
                        &key.binding.caller_id(),
                        &key.session.session_id,
                        &key.operation_id,
                        &request.root.as_str(),
                        &input,
                        &lease.owner().as_str(),
                        &lease.ttl_micros(),
                    ],
                )?;
                if inserted == 1 {
                    return Ok(OperationClaim::Claimed);
                }
                // Gone again when the host deleted it after the insert met it.
                let Some(stored) = read_operation(client, key, &cipher)? else {
                    continue;
                };
                let claim = resolve_operation_claim(stored, &request, lease.owner())?;
                if claim.holds() {
                    client.execute(
                        "UPDATE openappa_operations \
                         SET lease_owner=$4, lease_expires_at = now() + $5::bigint * interval '1 microsecond' \
                         WHERE organization_id=$1 AND session_id=$2 AND operation_id=$3",
                        &[
                            &key.session.organization_id,
                            &key.session.session_id,
                            &key.operation_id,
                            &lease.owner().as_str(),
                            &lease.ttl_micros(),
                        ],
                    )?;
                }
                return Ok(claim);
            }
            Err(ReceiptError::storage("the operation receipt changed under every claim attempt"))
        })
    }

    /// Completes a claimed operation receipt with its final decision, only while `owner` holds
    /// its lease.
    pub(crate) fn complete_operation(
        &self,
        key: OperationKey,
        decision: Value,
        owner: LeaseOwner,
    ) -> Result<(), ReceiptError> {
        let cipher = self.cipher.clone();
        self.transaction(move |client| {
            let existing = read_operation(client, &key, &cipher)?;
            if let Completion::Write = resolve_operation_completion(existing, &key, &decision, &owner)? {
                let decision = cipher.seal_json(&operation_field(&key, "decision"), &decision)?;
                client.execute(
                    "UPDATE openappa_operations SET status='complete', decision=$4 \
                     WHERE organization_id=$1 AND session_id=$2 AND operation_id=$3",
                    &[
                        &key.session.organization_id,
                        &key.session.session_id,
                        &key.operation_id,
                        &decision,
                    ],
                )?;
            }
            Ok(())
        })
    }

    /// Claims a durable processed-result receipt under `lease`, as [`Self::claim_operation`]
    /// claims an operation.
    pub(crate) fn claim_processed_result(
        &self,
        request: ProcessedResultRequest,
        lease: ReceiptLease,
    ) -> Result<ProcessedResultClaim, ReceiptError> {
        let cipher = self.cipher.clone();
        self.transaction(move |client| {
            let key = &request.key;
            check_root_cipher(client, request.root.as_str(), &cipher)?;
            for _ in 0..CLAIM_ATTEMPTS {
                let inserted = client.execute(
                    "INSERT INTO openappa_processed_results \
                         (organization_id, caller_id, session_id, tool_call_id, root, status, lease_owner, lease_expires_at) \
                     VALUES ($1,$2,$3,$4,$5,'pending',$6, now() + $7::bigint * interval '1 microsecond') \
                     ON CONFLICT DO NOTHING",
                    &[
                        &key.session.organization_id,
                        &key.caller_id,
                        &key.session.session_id,
                        &key.tool_call_id,
                        &request.root.as_str(),
                        &lease.owner().as_str(),
                        &lease.ttl_micros(),
                    ],
                )?;
                if inserted == 1 {
                    return Ok(ProcessedResultClaim::Claimed);
                }
                let Some(stored) = read_result(client, key, &cipher)? else {
                    continue;
                };
                let claim = resolve_result_claim(stored, &request, lease.owner())?;
                if claim.holds() {
                    client.execute(
                        "UPDATE openappa_processed_results \
                         SET lease_owner=$4, lease_expires_at = now() + $5::bigint * interval '1 microsecond' \
                         WHERE organization_id=$1 AND session_id=$2 AND tool_call_id=$3",
                        &[
                            &key.session.organization_id,
                            &key.session.session_id,
                            &key.tool_call_id,
                            &lease.owner().as_str(),
                            &lease.ttl_micros(),
                        ],
                    )?;
                }
                return Ok(claim);
            }
            Err(ReceiptError::storage("the processed result receipt changed under every claim attempt"))
        })
    }

    /// Completes a processed-result receipt with its approved output and decision, only while
    /// `owner` holds its lease. The completion time is the database clock's.
    pub(crate) fn complete_processed_result(
        &self,
        key: ProcessedResultKey,
        approved_output: String,
        decision: Value,
        owner: LeaseOwner,
    ) -> Result<(), ReceiptError> {
        let cipher = self.cipher.clone();
        self.transaction(move |client| {
            let existing = read_result(client, &key, &cipher)?;
            if let Completion::Write = resolve_result_completion(existing, &key, &approved_output, &decision, &owner)? {
                let approved_output = cipher.seal_text(&result_field(&key, "approved_output"), &approved_output)?;
                let decision = cipher.seal_json(&result_field(&key, "decision"), &decision)?;
                client.execute(
                    "UPDATE openappa_processed_results \
                     SET status='complete', approved_output=$4, decision=$5, completed_at=clock_timestamp() \
                     WHERE organization_id=$1 AND session_id=$2 AND tool_call_id=$3",
                    &[
                        &key.session.organization_id,
                        &key.session.session_id,
                        &key.tool_call_id,
                        &approved_output,
                        &decision,
                    ],
                )?;
            }
            Ok(())
        })
    }

    /// Extends `lease` on each listed receipt still pending under its owner, in one transaction
    /// that locks the rows in key order.
    pub(crate) fn renew_receipts(
        &self,
        lease: ReceiptLease,
        operations: Vec<OperationKey>,
        results: Vec<ProcessedResultKey>,
    ) -> Result<Renewal, ReceiptError> {
        self.transaction(move |client| {
            let mut renewal = Renewal::Kept;
            for key in operations_in_key_order(operations) {
                let (organization, session, id) =
                    (&key.session.organization_id, &key.session.session_id, &key.operation_id);
                let renewed = client.execute(
                    "UPDATE openappa_operations SET lease_expires_at = now() + $5::bigint * interval '1 microsecond' \
                     WHERE organization_id=$1 AND session_id=$2 AND operation_id=$3 \
                       AND status='pending' AND lease_owner=$4",
                    &[organization, session, id, &lease.owner().as_str(), &lease.ttl_micros()],
                )?;
                if renewed == 0
                    && client
                        .query_opt(
                            "SELECT 1 FROM openappa_operations \
                             WHERE organization_id=$1 AND session_id=$2 AND operation_id=$3 AND status='complete' \
                               AND lease_owner=$4",
                            &[organization, session, id, &lease.owner().as_str()],
                        )?
                        .is_none()
                {
                    renewal = Renewal::Lost;
                }
            }
            for key in results_in_key_order(results) {
                let (organization, session, id) =
                    (&key.session.organization_id, &key.session.session_id, &key.tool_call_id);
                let renewed = client.execute(
                    "UPDATE openappa_processed_results SET lease_expires_at = now() + $5::bigint * interval '1 microsecond' \
                     WHERE organization_id=$1 AND session_id=$2 AND tool_call_id=$3 \
                       AND status='pending' AND lease_owner=$4",
                    &[organization, session, id, &lease.owner().as_str(), &lease.ttl_micros()],
                )?;
                if renewed == 0
                    && client
                        .query_opt(
                            "SELECT 1 FROM openappa_processed_results \
                             WHERE organization_id=$1 AND session_id=$2 AND tool_call_id=$3 AND status='complete' \
                               AND lease_owner=$4",
                            &[organization, session, id, &lease.owner().as_str()],
                        )?
                        .is_none()
                {
                    renewal = Renewal::Lost;
                }
            }
            Ok(renewal)
        })
    }

    /// Ends `owner`'s lease on each listed receipt still pending under it, by the database
    /// clock, in one transaction that locks the rows in key order.
    pub(crate) fn release_receipts(
        &self,
        owner: LeaseOwner,
        operations: Vec<OperationKey>,
        results: Vec<ProcessedResultKey>,
    ) -> Result<(), ReceiptError> {
        self.transaction(move |client| {
            for key in operations_in_key_order(operations) {
                client.execute(
                    "UPDATE openappa_operations SET lease_expires_at = now() \
                     WHERE organization_id=$1 AND session_id=$2 AND operation_id=$3 \
                       AND status='pending' AND lease_owner=$4",
                    &[
                        &key.session.organization_id,
                        &key.session.session_id,
                        &key.operation_id,
                        &owner.as_str(),
                    ],
                )?;
            }
            for key in results_in_key_order(results) {
                client.execute(
                    "UPDATE openappa_processed_results SET lease_expires_at = now() \
                     WHERE organization_id=$1 AND session_id=$2 AND tool_call_id=$3 \
                       AND status='pending' AND lease_owner=$4",
                    &[
                        &key.session.organization_id,
                        &key.session.session_id,
                        &key.tool_call_id,
                        &owner.as_str(),
                    ],
                )?;
            }
            Ok(())
        })
    }

    /// Checks whether pending receipts exist for a root trajectory.
    pub(crate) fn has_pending_receipts(&self, root: &TrajectoryId) -> Result<bool, PostgresError> {
        let root = root.as_str().to_owned();
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

    pub(crate) fn hold_peer_message(
        &self,
        receiver: &TrajectoryId,
        digest: PeerDigest,
        body: &str,
        held: &NewHeld,
        now: SystemTime,
        quota: usize,
    ) -> Result<(), HeldError> {
        if self.cipher.is_encrypted() {
            return Err(HeldError::Storage(
                "peer sharing is unavailable for an encrypted storage view".into(),
            ));
        }
        let receiver = receiver.as_str().to_owned();
        let (id, digest, label, body) = (
            held.id.as_str().to_owned(),
            digest.to_string(),
            held.label.clone(),
            body.to_owned(),
        );
        let (expires_at, now, quota) = (held.expires_at, millis(now), quota_limit(quota));
        self.transaction(move |client| {
            lock_inbox(client, &receiver)?;
            client.execute(
                "INSERT INTO openappa_held_peer_messages (id, receiver, digest, label, body, expires_at, notified) \
                 VALUES ($1, $2, $3, $4, $5, $6, false)",
                &[&id, &receiver, &digest, &label, &body, &expires_at],
            )?;
            // Every receiver's expired rows go, so a session that ended unread leaves none.
            client.execute(
                "DELETE FROM openappa_held_peer_messages WHERE expires_at <= $1",
                &[&now],
            )?;
            client.execute(
                "DELETE FROM openappa_held_peer_messages WHERE receiver = $1 AND seq NOT IN ( \
                     SELECT seq FROM openappa_held_peer_messages WHERE receiver = $1 ORDER BY seq DESC LIMIT $2)",
                &[&receiver, &quota],
            )?;
            Ok(())
        })
    }

    pub(crate) fn peer_notices(&self, receiver: &TrajectoryId, now: SystemTime) -> Result<Vec<HeldNotice>, HeldError> {
        let receiver = receiver.as_str().to_owned();
        let now = millis(now);
        // Asked on every acknowledged hook: a receiver with nothing new takes no lock.
        let (probe_receiver, probe_now) = (receiver.clone(), now);
        let pending: bool = self
            .query(move |client| {
                Ok(client.query_one(
                    "SELECT EXISTS (SELECT 1 FROM openappa_held_peer_messages \
                     WHERE receiver = $1 AND NOT notified AND expires_at > $2)",
                    &[&probe_receiver, &probe_now],
                )?)
            })?
            .get(0);
        if !pending {
            return Ok(Vec::new());
        }
        self.transaction(move |client| {
            lock_inbox(client, &receiver)?;
            client
                .query(
                    "UPDATE openappa_held_peer_messages SET notified = true \
                     WHERE receiver = $1 AND NOT notified AND expires_at > $2 \
                     RETURNING seq, id, digest, label, expires_at",
                    &[&receiver, &now],
                )?
                .into_iter()
                .map(|row| (row.get::<_, i64>(0), held_row(&row, 1)))
                .collect::<std::collections::BTreeMap<_, _>>()
                .into_values()
                .map(StoredNotice::decode)
                .collect()
        })
    }

    pub(crate) fn peek_peer_message(
        &self,
        receiver: &TrajectoryId,
        id: &HeldPeerId,
        now: SystemTime,
    ) -> Result<Option<HeldNotice>, HeldError> {
        let (receiver, id, now) = (receiver.as_str().to_owned(), id.as_str().to_owned(), millis(now));
        self.query(move |client| {
            Ok(client.query_opt(
                "SELECT id, digest, label, expires_at FROM openappa_held_peer_messages \
                 WHERE receiver = $1 AND id = $2 AND expires_at > $3",
                &[&receiver, &id, &now],
            )?)
        })?
        .map(|row| held_row(&row, 0).decode())
        .transpose()
    }

    pub(crate) fn take_peer_message(
        &self,
        receiver: &TrajectoryId,
        id: &HeldPeerId,
        now: SystemTime,
    ) -> Result<Option<HeldPeerMessage>, HeldError> {
        let (receiver, id, now) = (receiver.as_str().to_owned(), id.as_str().to_owned(), millis(now));
        self.transaction(move |client| {
            lock_inbox(client, &receiver)?;
            client
                .query_opt(
                    "DELETE FROM openappa_held_peer_messages WHERE receiver = $1 AND id = $2 \
                     RETURNING id, digest, label, expires_at, body",
                    &[&receiver, &id],
                )?
                .map(|row| (held_row(&row, 0), row.get::<_, String>(4)))
                .filter(|(stored, _)| stored.expires_at > now)
                .map(|(stored, body)| {
                    Ok(HeldPeerMessage {
                        notice: stored.decode()?,
                        body,
                    })
                })
                .transpose()
        })
    }

    pub(crate) fn claim_embedded_peer(&self, fresh: &NewEmbedded) -> Result<EmbeddedClaim, EmbeddedError> {
        if self.cipher.is_encrypted() {
            return Err(EmbeddedError::Storage(
                "peer sharing is unavailable for an encrypted storage view".into(),
            ));
        }
        let fresh = fresh.clone();
        self.transaction(move |client| {
            lock_root(client, &fresh.root)?;
            claim_embedded_pg(client, &fresh)
        })
    }

    pub(crate) fn load_embedded_peer(&self, root: &str, id: &str) -> Result<Option<EmbeddedRow>, EmbeddedError> {
        let (root, id) = (root.to_owned(), id.to_owned());
        let stored = self
            .query(move |client| {
                Ok(client
                    .query_opt(&embedded_select("root = $1 AND id = $2"), &[&root, &id])?
                    .map(|row| embedded_pg_row(&row)))
            })
            .map_err(EmbeddedError::from)?;
        stored.map(StoredEmbedded::decode).transpose()
    }

    pub(crate) fn expire_embedded_inbox(&self, root: &str, now: i64) -> Result<(), EmbeddedError> {
        let root = root.to_owned();
        self.query(move |client| {
            client.execute(
                "UPDATE openappa_embedded_peer_messages SET body = NULL
                 WHERE root = $1 AND status IN ('held', 'direct') AND expires_at <= $2",
                &[&root, &now],
            )?;
            Ok(())
        })
        .map_err(EmbeddedError::from)
    }

    pub(crate) fn list_embedded_peer(
        &self,
        root: &str,
        recipient: &str,
        now: i64,
    ) -> Result<Vec<EmbeddedRow>, EmbeddedError> {
        let (root, recipient) = (root.to_owned(), recipient.to_owned());
        let stored = self
            .query(move |client| {
                client.execute(
                    "UPDATE openappa_embedded_peer_messages SET body = NULL
                     WHERE root = $1 AND recipient = $2 AND status IN ('held', 'direct') AND expires_at <= $3",
                    &[&root, &recipient, &now],
                )?;
                Ok(client
                    .query(
                        "SELECT id, sender, recipient, pending_spawn, dispatch, digest, label, body, status, read_call_id, decision, expires_at, read_arguments
                         FROM openappa_embedded_peer_messages
                         WHERE root = $1 AND recipient = $2 AND status = 'held' AND body IS NOT NULL AND expires_at > $3
                         ORDER BY seq ASC",
                        &[&root, &recipient, &now],
                    )?
                    .into_iter()
                    .map(|row| embedded_pg_row(&row))
                    .collect::<Vec<_>>())
            })
            .map_err(EmbeddedError::from)?;
        stored.into_iter().map(StoredEmbedded::decode).collect()
    }

    pub(crate) fn take_embedded_direct(
        &self,
        root: &str,
        id: &str,
        sender: &str,
        recipient: &str,
        digest: &str,
    ) -> Result<DirectTake, EmbeddedError> {
        let (root, id, sender, recipient, digest) = (
            root.to_owned(),
            id.to_owned(),
            sender.to_owned(),
            recipient.to_owned(),
            digest.to_owned(),
        );
        self.transaction(move |client| {
            lock_root(client, &root)?;
            let Some(row) = load_embedded_pg(client, &root, &id)? else {
                return Ok(DirectTake::Missing);
            };
            if row.sender != sender || row.recipient != recipient || row.digest != digest {
                return Ok(DirectTake::Missing);
            }
            match row.status {
                EmbeddedStatus::Direct => Ok(DirectTake::Already(row)),
                EmbeddedStatus::Read => Ok(DirectTake::Busy),
                EmbeddedStatus::Held => {
                    client.execute(
                        "UPDATE openappa_embedded_peer_messages SET status = 'direct'
                         WHERE root = $1 AND id = $2 AND status = 'held'",
                        &[&root, &id],
                    )?;
                    Ok(DirectTake::Taken(row))
                }
            }
        })
    }

    pub(crate) fn claim_embedded_read(
        &self,
        root: &str,
        id: &str,
        recipient: &str,
        call_id: &str,
        arguments: &str,
    ) -> Result<ReadTake, EmbeddedError> {
        if self.cipher.is_encrypted() {
            return Err(EmbeddedError::Storage(
                "peer sharing is unavailable for an encrypted storage view".into(),
            ));
        }
        let (root, id, recipient, call_id, arguments) = (
            root.to_owned(),
            id.to_owned(),
            recipient.to_owned(),
            call_id.to_owned(),
            arguments.to_owned(),
        );
        self.transaction(move |client| {
            lock_root(client, &root)?;
            let Some(row) = load_embedded_pg(client, &root, &id)? else {
                return Ok(ReadTake::Missing);
            };
            if row.recipient != recipient {
                return Ok(ReadTake::Missing);
            }
            if let Some(taken) = classify_read(&row, &call_id, &arguments) {
                return Ok(taken);
            }
            match row.status {
                EmbeddedStatus::Held => claim_held_read_pg(client, &root, &id, &recipient, &call_id, &arguments),
                EmbeddedStatus::Read => resume_embedded_read_pg(client, &root, &id, &row),
                EmbeddedStatus::Direct => Ok(ReadTake::Busy),
            }
        })
    }

    pub(crate) fn finish_embedded_read(
        &self,
        root: &str,
        id: &str,
        call_id: &str,
        generation: i64,
        decision: &str,
    ) -> Result<bool, EmbeddedError> {
        if self.cipher.is_encrypted() {
            return Err(EmbeddedError::Storage(
                "peer sharing is unavailable for an encrypted storage view".into(),
            ));
        }
        let (root, id, call_id) = (root.to_owned(), id.to_owned(), call_id.to_owned());
        let decision = serde_json::from_str::<serde_json::Value>(decision)
            .map_err(|error| EmbeddedError::Storage(format!("invalid read receipt JSON: {error}")))?;
        self.transaction(move |client| {
            lock_root(client, &root)?;
            let changed = client.execute(
                "UPDATE openappa_embedded_peer_messages SET decision = $4
                 WHERE root = $1 AND id = $2 AND read_call_id = $3 AND status = 'read'
                   AND decision->>'kind' IN ('opening', 'bound')
                   AND (decision->>'generation')::bigint = $5",
                &[&root, &id, &call_id, &decision, &generation],
            )?;
            Ok(changed == 1)
        })
    }

    pub(crate) fn release_embedded_read(
        &self,
        root: &str,
        id: &str,
        call_id: &str,
        generation: i64,
    ) -> Result<bool, EmbeddedError> {
        let (root, id, call_id) = (root.to_owned(), id.to_owned(), call_id.to_owned());
        self.transaction(move |client| {
            lock_root(client, &root)?;
            let changed = client.execute(
                "UPDATE openappa_embedded_peer_messages
                 SET status = 'held', read_call_id = NULL, read_arguments = NULL, decision = NULL
                 WHERE root = $1 AND id = $2 AND read_call_id = $3 AND status = 'read'
                   AND decision->>'kind' = 'opening' AND (decision->>'generation')::bigint = $4",
                &[&root, &id, &call_id, &generation],
            )?;
            Ok(changed == 1)
        })
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
        let bytes = self
            .cipher
            .seal_bytes(&event_field(&id, 0), &bytes)
            .map_err(PostgresError::from)?;
        let hash = key.as_str().to_owned();
        let policy = policy.to_vec();
        let created = self.query(move |client| {
            let mut transaction = client.transaction()?;
            transaction.execute(
                "INSERT INTO openappa_policy_files (hash, bytes) VALUES ($1, $2) ON CONFLICT DO NOTHING",
                &[&hash, &policy],
            )?;
            // The (root, seq) key refuses a second opening; the root's row is what its writers lock.
            let inserted = transaction.execute(
                "INSERT INTO openappa_events (root, seq, payload) VALUES ($1, 0, $2) ON CONFLICT (root, seq) DO NOTHING",
                &[&id, &bytes],
            )?;
            transaction.execute("INSERT INTO openappa_roots (root) VALUES ($1) ON CONFLICT DO NOTHING", &[&id])?;
            transaction.commit()?;
            Ok(inserted == 1)
        })?;
        if !created {
            return Err(CreateError::AlreadyExists {
                root: root.as_str().to_owned(),
            });
        }
        Ok(root.clone())
    }

    pub(super) fn log(&self, root: &TrajectoryId) -> Result<Log, ReadError> {
        match &self.lease {
            Some(leased) => self.log_on(leased.lease.worker(), root),
            None => {
                let lease = self.pool.checkout().map_err(PostgresError::from)?;
                self.log_on(lease.worker(), root)
            }
        }
    }

    /// Read `root`'s log on `worker`'s connection, decoding only what the cache lacks.
    fn log_on(&self, worker: &Worker, root: &TrajectoryId) -> Result<Log, ReadError> {
        let cached = self.logs.get(root.as_str());
        match self.read_log(worker, root, cached)? {
            Some(log) => Ok(log),
            // The cached copy no longer matches what is stored: read the whole log once.
            None => {
                self.logs.forget(root.as_str());
                Ok(self
                    .read_log(worker, root, None)?
                    .expect("a read without a cached copy has nothing to mismatch"))
            }
        }
    }

    /// Read `root`'s log, extending `cached` when it is still a prefix of what is stored.
    /// `None` when `cached` no longer matches the stored log.
    fn read_log(
        &self,
        worker: &Worker,
        root: &TrajectoryId,
        cached: Option<CachedLog>,
    ) -> Result<Option<Log>, ReadError> {
        let id = root.as_str().to_owned();
        // A cached log is read again from its last batch, which must still be the stored one.
        let from = cached.as_ref().map_or(0, |cached| cached.log.basis() - 1);
        let (prefix, rows) = worker.run({
            let id = id.clone();
            move |state| {
                let start = stored_position(from)?;
                // The batches before the cached last one are not read again, only counted, so a
                // removed row still refuses the log as a gap.
                let prefix: i64 = match from {
                    0 => 0,
                    _ => state
                        .client
                        .query_one(
                            "SELECT count(*) FROM openappa_events WHERE root = $1 AND seq < $2",
                            &[&id, &start],
                        )?
                        .get(0),
                };
                let rows = state.client.query(
                    "SELECT seq, payload FROM openappa_events WHERE root = $1 AND seq >= $2 ORDER BY seq",
                    &[&id, &start],
                )?;
                let rows = contiguous_from(from, rows.into_iter().map(|row| (row.get(0), row.get(1))).collect())
                    .map_err(|gap| PostgresError(gap.to_string()))?;
                Ok((prefix as u64, rows))
            }
        })?;
        let rows = rows
            .into_iter()
            .enumerate()
            .map(|(offset, bytes)| {
                self.cipher
                    .open_bytes(&event_field(&id, from + offset as u64), &bytes)
                    .map_err(PostgresError::from)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let (log, last_batch) = match cached {
            Some(CachedLog { mut log, last_batch }) if prefix == from && rows.first() == Some(&last_batch) => {
                let batches = rows.into_iter().skip(1).collect::<Vec<_>>();
                let last = batches.last().cloned().unwrap_or(last_batch);
                extend(&mut log, batches)?;
                (log, last)
            }
            Some(_) => {
                return Ok(None);
            }
            None => {
                let hash = opening_key(root, &rows)?.as_str().to_owned();
                let lookup = hash.clone();
                let policy = worker
                    .run(move |state| {
                        Ok(state
                            .client
                            .query_opt("SELECT bytes FROM openappa_policy_files WHERE hash = $1", &[&lookup])?
                            .map(|row| row.get::<_, Vec<u8>>(0)))
                    })?
                    .ok_or(ReadError::PolicyFileMissing { key: hash })?;
                let last = rows.last().cloned().unwrap_or_default();
                (decoded(root, rows, policy)?, last)
            }
        };
        self.logs.keep(&log, &last_batch);
        Ok(Some(log))
    }

    /// Begin one read-decide-append step on `root`: a transaction holding the root's row lock.
    /// A leased store runs the step on its own connection, so the host must not have a
    /// transaction of its own open there until the step ends.
    pub(super) fn step(&self, root: &TrajectoryId) -> Result<PostgresStep<'_>, PostgresError> {
        let own = match &self.lease {
            Some(_) => None,
            None => Some(self.pool.checkout()?),
        };
        // Open from the first statement on, so a failed lock still rolls the transaction back.
        let step = PostgresStep {
            store: self,
            own,
            root: root.clone(),
            open: true,
        };
        let key = root.as_str().to_owned();
        step.worker().run(move |state| {
            // A step dropped without an answer from its thread leaves the reset to end it.
            state.host_sql = true;
            state.client.batch_execute("BEGIN")?;
            lock_root(&mut state.client, &key)?;
            Ok(())
        })?;
        Ok(step)
    }

    pub(super) fn workspace_batches(&self, workspace: &str) -> Result<Vec<Vec<u8>>, crate::files::FileStoreError> {
        let workspace = workspace.to_owned();
        let cipher = self.cipher.clone();
        self.query(move |client| {
            let rows = client.query(
                "SELECT seq,payload FROM openappa_file_events WHERE workspace=$1 ORDER BY seq",
                &[&workspace],
            )?;
            let batches = contiguous(rows.into_iter().map(|row| (row.get(0), row.get(1))).collect())
                .map_err(|error| PostgresError(error.to_string()))?;
            batches
                .into_iter()
                .enumerate()
                .map(|(seq, bytes)| {
                    cipher
                        .open_bytes(
                            &field_context(&["openappa_file_events", &workspace, &seq.to_string()]),
                            &bytes,
                        )
                        .map_err(Into::into)
                })
                .collect()
        })
        .map_err(Into::into)
    }

    pub(super) fn create_workspace(
        &self,
        workspace: &str,
        payload: Vec<u8>,
    ) -> Result<(), crate::files::FileStoreError> {
        let workspace = workspace.to_owned();
        let payload = self
            .cipher
            .seal_bytes(
                &field_context(&["openappa_file_events", &workspace, &0.to_string()]),
                &payload,
            )
            .map_err(PostgresError::from)?;
        self.query(move |client| {
            client.execute(
                "INSERT INTO openappa_file_events(workspace,seq,payload) VALUES ($1,0,$2) ON CONFLICT DO NOTHING",
                &[&workspace, &payload],
            )?;
            Ok(())
        })
        .map_err(Into::into)
    }

    /// The compare-and-swap is the primary key: the event lands at `basis` only while no
    /// event holds that position. A root binding lands in the same transaction, and a root
    /// another workspace already bound refuses it.
    pub(super) fn append_workspace(
        &self,
        workspace: &str,
        basis: u64,
        payload: Vec<u8>,
        root: Option<TrajectoryId>,
    ) -> Result<(), crate::files::FileStoreError> {
        let workspace = workspace.to_owned();
        let payload = self
            .cipher
            .seal_bytes(
                &field_context(&["openappa_file_events", &workspace, &basis.to_string()]),
                &payload,
            )
            .map_err(PostgresError::from)?;
        self.transaction(move |client| {
            let position = stored_position(basis)?;
            let appended = client.execute(
                "INSERT INTO openappa_file_events(workspace,seq,payload) VALUES ($1,$2,$3) \
                 ON CONFLICT (workspace, seq) DO NOTHING",
                &[&workspace, &position, &payload],
            )?;
            if appended == 0 {
                let current = client
                    .query_one(
                        "SELECT COALESCE(MAX(seq)+1,0) FROM openappa_file_events WHERE workspace=$1",
                        &[&workspace],
                    )?
                    .get::<_, i64>(0) as u64;
                return Err(crate::files::FileStoreError::Conflict {
                    expected: basis,
                    actual: current,
                });
            }
            if let Some(root) = root {
                let root = root.as_str().to_owned();
                client.execute(
                    "INSERT INTO openappa_file_roots(root,workspace,seq) VALUES ($1,$2,$3) ON CONFLICT DO NOTHING",
                    &[&root, &workspace, &position],
                )?;
                let bound: String = client
                    .query_one("SELECT workspace FROM openappa_file_roots WHERE root=$1", &[&root])?
                    .get(0);
                if bound != workspace {
                    return Err(crate::files::FileStoreError::Configuration(
                        "a root cannot change its tracked workspace".into(),
                    ));
                }
            }
            Ok(())
        })
    }

    pub(super) fn workspace_for_root(
        &self,
        root: &TrajectoryId,
    ) -> Result<Option<String>, crate::files::FileStoreError> {
        let root = root.as_str().to_owned();
        self.query(move |client| {
            let installed: bool = client
                .query_one("SELECT to_regclass('openappa_file_roots') IS NOT NULL", &[])?
                .get(0);
            if !installed {
                return Ok(None);
            }
            Ok(client
                .query_opt("SELECT workspace FROM openappa_file_roots WHERE root=$1", &[&root])?
                .map(|row| row.get(0)))
        })
        .map_err(Into::into)
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
        let bytes = self
            .cipher
            .seal_bytes(&event_field(root.as_str(), basis), &bytes)
            .map_err(PostgresError::from)?;
        let batch = Batch::new(root, basis, bytes, key, None);
        let cipher = self.cipher.clone();
        self.query(move |client| {
            let mut transaction = client.transaction()?;
            let written = batch.write(&mut transaction, &cipher)?;
            if written == Written::Done {
                transaction.commit()?;
            }
            Ok(written)
        })?
        .into()
    }

    pub(super) fn append_holding_read(
        &self,
        root: &TrajectoryId,
        basis: u64,
        bytes: Vec<u8>,
        key: Option<&str>,
        hold: &crate::embedded::EmbeddedAppendHold<'_>,
    ) -> Result<(), AppendError> {
        let bytes = self
            .cipher
            .seal_bytes(&event_field(root.as_str(), basis), &bytes)
            .map_err(PostgresError::from)?;
        let batch = Batch::new(root, basis, bytes, key, Some(HeldRead::new(hold)));
        let cipher = self.cipher.clone();
        self.query(move |client| {
            let mut transaction = client.transaction()?;
            let written = batch.write(&mut transaction, &cipher)?;
            if written == Written::Done {
                transaction.commit()?;
            }
            Ok(written)
        })?
        .into()
    }
}

impl PostgresStep<'_> {
    fn worker(&self) -> &Worker {
        match (&self.own, &self.store.lease) {
            (Some(own), _) => own.worker(),
            (None, Some(leased)) => leased.lease.worker(),
            (None, None) => unreachable!("a step on an unleased store checks out its own connection"),
        }
    }

    /// The root's log as it stands while this step holds the root.
    pub(super) fn log(&self) -> Result<Log, ReadError> {
        self.store.log_on(self.worker(), &self.root)
    }

    /// Append one batch at `basis` and end the step. A conflict writes nothing.
    pub(super) fn append(
        mut self,
        basis: u64,
        bytes: Vec<u8>,
        key: Option<&str>,
        hold: Option<&crate::embedded::EmbeddedAppendHold<'_>>,
    ) -> Result<(), AppendError> {
        let bytes = self
            .store
            .cipher
            .seal_bytes(&event_field(self.root.as_str(), basis), &bytes)
            .map_err(PostgresError::from)?;
        let batch = Batch::new(&self.root, basis, bytes, key, hold.map(HeldRead::new));
        let cipher = self.store.cipher.clone();
        let written = self.worker().run(move |state| {
            let written = batch.write(&mut state.client, &cipher)?;
            if written == Written::Done {
                state.client.batch_execute("COMMIT")?;
            }
            Ok(written)
        })?;
        if written == Written::Done {
            self.open = false;
        }
        written.into()
    }
}

impl Drop for PostgresStep<'_> {
    fn drop(&mut self) {
        if self.open {
            // Should the connection not answer, the pool's reset ends the transaction instead.
            let _ = self.worker().run(|state| Ok(state.client.batch_execute("ROLLBACK")?));
        }
    }
}

/// A log position as the `bigint` column stores it. A position past `i64::MAX` cannot be
/// stored, so it is refused rather than wrapped.
fn stored_position(position: u64) -> Result<i64, PostgresError> {
    i64::try_from(position).map_err(|_| PostgresError(format!("the log position {position} cannot be stored")))
}

/// Lock `root`'s family row for the rest of the transaction, creating the row on first use.
/// Writers of one family serialize on it; the lock ends with the transaction.
fn lock_root(client: &mut Client, root: &str) -> Result<(), ::postgres::Error> {
    let lock = "SELECT 1 FROM openappa_roots WHERE root = $1 FOR UPDATE";
    if client.query_opt(lock, &[&root])?.is_some() {
        return Ok(());
    }
    client.execute(
        "INSERT INTO openappa_roots (root) VALUES ($1) ON CONFLICT DO NOTHING",
        &[&root],
    )?;
    client.query_one(lock, &[&root])?;
    Ok(())
}

/// One batch to write at one position of one root's log.
struct Batch {
    root: String,
    basis: u64,
    bytes: Vec<u8>,
    key: Option<String>,
    hold: Option<HeldRead>,
}

/// The embedded read a batch binds, which must still be this read's when the batch lands.
struct HeldRead {
    id: String,
    call_id: String,
    generation: i64,
    bind: bool,
}

impl HeldRead {
    fn new(hold: &crate::embedded::EmbeddedAppendHold<'_>) -> HeldRead {
        HeldRead {
            id: hold.id.to_owned(),
            call_id: hold.call_id.to_owned(),
            generation: hold.generation,
            bind: hold.bind,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Written {
    Done,
    /// The position is taken; the log stands at this position now.
    Conflict(u64),
    ReadClaimLost,
}

impl From<Written> for Result<(), AppendError> {
    fn from(written: Written) -> Self {
        match written {
            Written::Done => Ok(()),
            Written::Conflict(current) => Err(AppendError::Conflict { current }),
            Written::ReadClaimLost => Err(AppendError::ReadClaimLost),
        }
    }
}

impl Batch {
    fn new(root: &TrajectoryId, basis: u64, bytes: Vec<u8>, key: Option<&str>, hold: Option<HeldRead>) -> Batch {
        Batch {
            root: root.as_str().to_owned(),
            basis,
            bytes,
            key: key.map(str::to_owned),
            hold,
        }
    }

    /// Write this batch inside the caller's transaction. The `(root, seq)` key is the
    /// compare-and-swap: a position another writer took inserts nothing.
    fn write(&self, client: &mut impl GenericClient, cipher: &StorageCipher) -> Result<Written, PostgresError> {
        check_root_cipher(client, &self.root, cipher)?;
        let current: i64 = client
            .query_one(
                "SELECT COALESCE(MAX(seq) + 1, 0) FROM openappa_events WHERE root = $1",
                &[&self.root],
            )?
            .get(0);
        let current = u64::try_from(current)
            .map_err(|_| PostgresError(format!("the log ends at a negative position: {current}")))?;
        if current != self.basis {
            return Ok(Written::Conflict(current));
        }
        if let Some(hold) = &self.hold {
            let bound = serde_json::json!({"generation": hold.generation, "kind": TICKET_BOUND});
            let changed = client.execute(
                "UPDATE openappa_embedded_peer_messages
                 SET decision = CASE WHEN $6 THEN $5 ELSE decision END
                 WHERE root = $1 AND id = $2 AND read_call_id = $3 AND status = 'read'
                   AND decision->>'kind' IN ('opening', 'bound')
                   AND (decision->>'generation')::bigint = $4",
                &[
                    &self.root,
                    &hold.id,
                    &hold.call_id,
                    &hold.generation,
                    &bound,
                    &hold.bind,
                ],
            )?;
            if changed != 1 {
                return Ok(Written::ReadClaimLost);
            }
        }
        let inserted = client.execute(
            "INSERT INTO openappa_events (root, seq, payload) VALUES ($1, $2, $3) ON CONFLICT (root, seq) DO NOTHING",
            &[&self.root, &stored_position(self.basis)?, &self.bytes],
        )?;
        if inserted == 0 {
            // The log ended at `basis` a statement ago in this transaction, so the writer that
            // took the position wrote exactly one batch there.
            return Ok(Written::Conflict(self.basis + 1));
        }
        if let Some(key) = &self.key {
            client.execute(
                "INSERT INTO openappa_host_keys (key, root) VALUES ($1, $2) ON CONFLICT DO NOTHING",
                &[&key, &self.root],
            )?;
        }
        Ok(Written::Done)
    }
}

impl From<PostgresError> for ReceiptError {
    fn from(error: PostgresError) -> Self {
        Self::Storage(error.0)
    }
}

impl From<::postgres::Error> for ReceiptError {
    fn from(error: ::postgres::Error) -> Self {
        PostgresError::from(error).into()
    }
}

/// Lock one operation receipt row for the rest of the transaction and read it, its lease
/// judged by the database clock.
fn read_operation(
    client: &mut Client,
    key: &OperationKey,
    cipher: &StorageCipher,
) -> Result<Option<StoredOperation>, ReceiptError> {
    client
        .query_opt(
            "SELECT organization_id, caller_id, session_id, root, input, status, decision, \
                    lease_owner, COALESCE(lease_expires_at > now(), false) \
             FROM openappa_operations WHERE organization_id=$1 AND session_id=$2 AND operation_id=$3 FOR UPDATE",
            &[&key.session.organization_id, &key.session.session_id, &key.operation_id],
        )?
        .map(|row| {
            let input = StoredOperationInput::decode(cipher.open_json(&operation_field(key, "input"), row.get(4))?)?;
            Ok(StoredOperation {
                session: SessionScope {
                    organization_id: row.get(0),
                    session_id: row.get(2),
                },
                binding: input.binding(row.get(1))?,
                root: row.get(3),
                semantic: input.semantic,
                status: row.get(5),
                decision: row.get::<_, Option<Value>>(6).map(|value| {
                    cipher
                        .open_json(&operation_field(key, "decision"), value)
                        .map_err(ReceiptError::from)
                }),
                lease: StoredLease {
                    owner: row.get(7),
                    live: row.get(8),
                },
            })
        })
        .transpose()
}

/// Lock one processed-result receipt row and read it, as [`read_operation`] does.
fn read_result(
    client: &mut Client,
    key: &ProcessedResultKey,
    cipher: &StorageCipher,
) -> Result<Option<StoredResult>, ReceiptError> {
    client
        .query_opt(
            "SELECT organization_id, session_id, root, status, approved_output, decision, \
                    lease_owner, COALESCE(lease_expires_at > now(), false) \
             FROM openappa_processed_results WHERE organization_id=$1 AND session_id=$2 AND tool_call_id=$3 FOR UPDATE",
            &[&key.session.organization_id, &key.session.session_id, &key.tool_call_id],
        )?
        .map(|row| {
            let root: String = row.get(2);
            check_root_cipher(client, &root, cipher)?;
            Ok(StoredResult {
                session: SessionScope {
                    organization_id: row.get(0),
                    session_id: row.get(1),
                },
                root,
                status: row.get(3),
                approved_output: row
                    .get::<_, Option<String>>(4)
                    .map(|value| cipher.open_text(&result_field(key, "approved_output"), value))
                    .transpose()?,
                decision: row.get::<_, Option<Value>>(5).map(|value| {
                    cipher
                        .open_json(&result_field(key, "decision"), value)
                        .map_err(ReceiptError::from)
                }),
                lease: StoredLease {
                    owner: row.get(6),
                    live: row.get(7),
                },
            })
        })
        .transpose()
}

fn check_root_cipher(client: &mut impl GenericClient, root: &str, cipher: &StorageCipher) -> Result<(), PostgresError> {
    if let Some(row) = client.query_opt("SELECT payload FROM openappa_events WHERE root=$1 AND seq=0", &[&root])? {
        let bytes: Vec<u8> = row.get(0);
        cipher.open_bytes(&event_field(root, 0), &bytes)?;
    }
    Ok(())
}

fn operation_field(key: &OperationKey, field: &str) -> String {
    field_context(&[
        "openappa_operations",
        &key.session.organization_id,
        &key.session.session_id,
        &key.operation_id,
        field,
    ])
}

fn result_field(key: &ProcessedResultKey, field: &str) -> String {
    field_context(&[
        "openappa_processed_results",
        &key.session.organization_id,
        &key.session.session_id,
        &key.tool_call_id,
        field,
    ])
}

fn event_field(root: &str, seq: u64) -> String {
    field_context(&["openappa_events", root, &seq.to_string(), "payload"])
}

/// The primary key each receipt table must carry, column for column. A host on another key
/// would let one organization's receipt collide with another's, so its store refuses to open.
const RECEIPT_KEYS: [(&str, &[&str]); 2] = [
    (
        "openappa_operations",
        &["organization_id", "session_id", "operation_id"],
    ),
    (
        "openappa_processed_results",
        &["organization_id", "session_id", "tool_call_id"],
    ),
];

fn check_receipt_keys(client: &mut Client) -> Result<(), PostgresError> {
    for (table, expected) in RECEIPT_KEYS {
        let found: Vec<String> = client
            .query(
                "SELECT a.attname::text \
                 FROM pg_constraint c \
                 CROSS JOIN LATERAL unnest(c.conkey) WITH ORDINALITY AS k(attnum, position) \
                 JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = k.attnum \
                 WHERE c.contype = 'p' AND c.conrelid = to_regclass($1) \
                 ORDER BY k.position",
                &[&table],
            )?
            .iter()
            .map(|row| row.get(0))
            .collect();
        if found != expected {
            return Err(PostgresError(format!(
                "incompatible host migration: {table} has primary key ({}), expected ({})",
                found.join(", "),
                expected.join(", ")
            )));
        }
    }
    Ok(())
}

/// A held row's notice columns, starting at column `from`: id, digest, label, expiry.
fn held_row(row: &::postgres::Row, from: usize) -> StoredNotice {
    StoredNotice {
        id: row.get(from),
        digest: row.get(from + 1),
        label: row.get(from + 2),
        expires_at: row.get(from + 3),
    }
}

impl From<PostgresError> for HeldError {
    fn from(error: PostgresError) -> Self {
        Self::Storage(error.0)
    }
}

impl From<::postgres::Error> for HeldError {
    fn from(error: ::postgres::Error) -> Self {
        PostgresError::from(error).into()
    }
}

fn claim_held_read_pg(
    client: &mut Client,
    root: &str,
    id: &str,
    recipient: &str,
    call_id: &str,
    arguments: &str,
) -> Result<ReadTake, EmbeddedError> {
    let taken: i64 = client
        .query_one(
            "SELECT COUNT(*) FROM openappa_embedded_peer_messages
             WHERE root = $1 AND recipient = $2 AND read_call_id = $3 AND id != $4",
            &[&root, &recipient, &call_id, &id],
        )?
        .get(0);
    if taken > 0 {
        return Ok(ReadTake::Busy);
    }
    let ticket =
        serde_json::from_str::<serde_json::Value>(&read_ticket(TICKET_OPENING, 1)).expect("the opening ticket is json");
    let changed = match client.execute(
        "UPDATE openappa_embedded_peer_messages
         SET status = 'read', read_call_id = $3, read_arguments = $4, decision = $5
         WHERE root = $1 AND id = $2 AND status = 'held'",
        &[&root, &id, &call_id, &arguments, &ticket],
    ) {
        Ok(changed) => changed,
        Err(error) if error.code() == Some(&::postgres::error::SqlState::UNIQUE_VIOLATION) => {
            return Ok(ReadTake::Busy);
        }
        Err(error) => return Err(error.into()),
    };
    if changed == 0 {
        return Ok(ReadTake::Busy);
    }
    owned_read_pg(client, root, id, 1)
}

fn resume_embedded_read_pg(
    client: &mut Client,
    root: &str,
    id: &str,
    row: &EmbeddedRow,
) -> Result<ReadTake, EmbeddedError> {
    let (kind, next) = next_read_ticket(row.decision.as_deref())?;
    let generation = next - 1;
    let ticket = serde_json::json!({"generation": next, "kind": kind});
    let changed = if row.decision.is_none() {
        client.execute(
            "UPDATE openappa_embedded_peer_messages SET decision = $3
             WHERE root = $1 AND id = $2 AND status = 'read' AND decision IS NULL",
            &[&root, &id, &ticket],
        )?
    } else {
        client.execute(
            "UPDATE openappa_embedded_peer_messages SET decision = $4
             WHERE root = $1 AND id = $2 AND status = 'read'
               AND decision->>'kind' = $3 AND (decision->>'generation')::bigint = $5",
            &[&root, &id, &kind, &ticket, &generation],
        )?
    };
    if changed == 0 {
        return Ok(ReadTake::Busy);
    }
    owned_read_pg(client, root, id, next)
}

fn owned_read_pg(client: &mut Client, root: &str, id: &str, generation: i64) -> Result<ReadTake, EmbeddedError> {
    Ok(ReadTake::Ready(ReadClaim {
        row: load_embedded_pg(client, root, id)?
            .ok_or_else(|| EmbeddedError::Storage("the claimed embedded peer row is missing".to_string()))?,
        generation,
    }))
}

fn claim_embedded_pg(client: &mut Client, fresh: &NewEmbedded) -> Result<EmbeddedClaim, EmbeddedError> {
    client.execute(
        "UPDATE openappa_embedded_peer_messages SET body = NULL
         WHERE root = $1 AND status IN ('held', 'direct') AND expires_at <= $2",
        &[&fresh.root, &fresh.created_at],
    )?;
    if let Some(existing) = load_embedded_dispatch_pg(client, &fresh.root, &fresh.sender, &fresh.dispatch)? {
        return Ok(if same_payload(&existing, fresh) {
            EmbeddedClaim::Stored(existing)
        } else {
            EmbeddedClaim::Conflict
        });
    }
    let unread_count: i64 = client
        .query_one(
            "SELECT COUNT(*) FROM openappa_embedded_peer_messages
             WHERE root = $1 AND recipient = $2 AND status = 'held' AND body IS NOT NULL AND expires_at > $3",
            &[&fresh.root, &fresh.recipient, &fresh.created_at],
        )?
        .get(0);
    if unread_count >= crate::embedded::UNREAD_QUOTA as i64 {
        return Ok(EmbeddedClaim::Quota);
    }
    client.execute(
        "INSERT INTO openappa_embedded_peer_messages (
            id, root, sender, recipient, pending_spawn, dispatch, digest, label, body, status, expires_at, created_at
         ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'held', $10, $11)",
        &[
            &fresh.id,
            &fresh.root,
            &fresh.sender,
            &fresh.recipient,
            &fresh.pending_spawn,
            &fresh.dispatch,
            &fresh.digest,
            &fresh.label,
            &fresh.body,
            &fresh.expires_at,
            &fresh.created_at,
        ],
    )?;
    Ok(EmbeddedClaim::Stored(
        load_embedded_pg(client, &fresh.root, &fresh.id)?
            .ok_or_else(|| EmbeddedError::Storage("the inserted embedded peer row is missing".to_string()))?,
    ))
}

fn load_embedded_pg(client: &mut Client, root: &str, id: &str) -> Result<Option<EmbeddedRow>, EmbeddedError> {
    client
        .query_opt(
            "SELECT id, sender, recipient, pending_spawn, dispatch, digest, label, body, status, read_call_id, decision, expires_at, read_arguments
             FROM openappa_embedded_peer_messages WHERE root = $1 AND id = $2",
            &[&root, &id],
        )?
        .map(|row| StoredEmbedded::decode(embedded_pg_row(&row)))
        .transpose()
}

fn load_embedded_dispatch_pg(
    client: &mut Client,
    root: &str,
    sender: &str,
    dispatch: &str,
) -> Result<Option<EmbeddedRow>, EmbeddedError> {
    client
        .query_opt(
            "SELECT id, sender, recipient, pending_spawn, dispatch, digest, label, body, status, read_call_id, decision, expires_at, read_arguments
             FROM openappa_embedded_peer_messages WHERE root = $1 AND sender = $2 AND dispatch = $3",
            &[&root, &sender, &dispatch],
        )?
        .map(|row| StoredEmbedded::decode(embedded_pg_row(&row)))
        .transpose()
}

fn embedded_pg_row(row: &::postgres::Row) -> StoredEmbedded {
    let decision: Option<serde_json::Value> = row.get(10);
    StoredEmbedded {
        id: row.get(0),
        sender: row.get(1),
        recipient: row.get(2),
        pending_spawn: row.get(3),
        dispatch: row.get(4),
        digest: row.get(5),
        label: row.get(6),
        body: row.get(7),
        status: row.get(8),
        read_call_id: row.get(9),
        decision: decision.map(|value| value.to_string()),
        expires_at: row.get(11),
        read_arguments: row.get(12),
    }
}

fn embedded_select(predicate: &str) -> String {
    format!(
        "SELECT id, sender, recipient, pending_spawn, dispatch, digest, label, body, status, read_call_id, decision, expires_at, read_arguments
         FROM openappa_embedded_peer_messages WHERE {predicate}"
    )
}

/// Lock the receiver's row in `openappa_peer_inboxes` until the transaction ends, as
/// `lock_root` locks a family. Writers of one receiver's held messages serialize on it.
fn lock_inbox(client: &mut Client, receiver: &str) -> Result<(), ::postgres::Error> {
    client.execute(
        "INSERT INTO openappa_peer_inboxes (receiver) VALUES ($1) ON CONFLICT DO NOTHING",
        &[&receiver],
    )?;
    client.query_one(
        "SELECT 1 FROM openappa_peer_inboxes WHERE receiver = $1 FOR UPDATE",
        &[&receiver],
    )?;
    Ok(())
}

#[cfg(feature = "fault-injection")]
impl PostgresStore {
    pub(crate) fn testing_clear_embedded_decision(&self, root: &str, id: &str) -> Result<(), EmbeddedError> {
        let (root, id) = (root.to_owned(), id.to_owned());
        self.query(move |client| {
            client.execute(
                "UPDATE openappa_embedded_peer_messages SET decision = NULL WHERE root = $1 AND id = $2",
                &[&root, &id],
            )?;
            Ok(())
        })
        .map_err(EmbeddedError::from)
    }

    pub(crate) fn testing_expire_embedded(&self, root: &str, id: &str) -> Result<(), EmbeddedError> {
        let (root, id) = (root.to_owned(), id.to_owned());
        self.query(move |client| {
            client.execute(
                "UPDATE openappa_embedded_peer_messages SET expires_at = 0 WHERE root = $1 AND id = $2",
                &[&root, &id],
            )?;
            Ok(())
        })
        .map_err(EmbeddedError::from)
    }

    pub(crate) fn testing_replace_embedded_body(&self, root: &str, id: &str, body: &str) -> Result<(), EmbeddedError> {
        let (root, id, body) = (root.to_owned(), id.to_owned(), body.to_owned());
        self.query(move |client| {
            client.execute(
                "UPDATE openappa_embedded_peer_messages SET body = $3 WHERE root = $1 AND id = $2",
                &[&root, &id, &body],
            )?;
            Ok(())
        })
        .map_err(EmbeddedError::from)
    }
}

impl From<PostgresError> for EmbeddedError {
    fn from(error: PostgresError) -> Self {
        Self::Storage(error.0)
    }
}

impl From<::postgres::Error> for EmbeddedError {
    fn from(error: ::postgres::Error) -> Self {
        PostgresError::from(error).into()
    }
}

/// How many times a claim inserts again after the row its insert met was deleted before the
/// claim could lock it.
const CLAIM_ATTEMPTS: usize = 3;

/// The operation keys in primary-key order, so two writes that lock several receipt rows lock
/// them in one order and cannot deadlock.
fn operations_in_key_order(mut keys: Vec<OperationKey>) -> Vec<OperationKey> {
    keys.sort_by(|a, b| {
        (&a.session.organization_id, &a.session.session_id, &a.operation_id).cmp(&(
            &b.session.organization_id,
            &b.session.session_id,
            &b.operation_id,
        ))
    });
    keys
}

/// The result keys in primary-key order, as [`operations_in_key_order`].
fn results_in_key_order(mut keys: Vec<ProcessedResultKey>) -> Vec<ProcessedResultKey> {
    keys.sort_by(|a, b| {
        (&a.session.organization_id, &a.session.session_id, &a.tool_call_id).cmp(&(
            &b.session.organization_id,
            &b.session.session_id,
            &b.tool_call_id,
        ))
    });
    keys
}
