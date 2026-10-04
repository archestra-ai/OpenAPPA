//! PostgreSQL storage for embedded hosts. The host installs the schema; Rust
//! retains the SQLite event encoding and policy-file validation. Each pooled
//! connection has a dedicated thread, which keeps the synchronous log API
//! usable from async hooks.
//!
//! A host that needs several statements on one connection — a session-level
//! advisory lock around a hook dispatch — leases one ([`LogStore::lease`]) and
//! runs its own SQL there through [`LeasedPostgres::with_client`]. Work on
//! different leases runs concurrently. A connection that returns to the pool
//! closed, or that does not answer its reset, is dropped, and a later checkout
//! opens another in its place.
//!
//! The host's migrations provide the schema: `openappa_events`, `openappa_policy_files`,
//! `openappa_host_keys`, the receipt tables `openappa_operations` and
//! `openappa_processed_results`, and `openappa_held_peer_messages`.
//! `tests/fixtures/host_schema.sql` holds the full DDL.

use std::num::NonZeroUsize;
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime};

use ::postgres::Client;
use postgres_native_tls::MakeTlsConnector;
use serde_json::Value;

use super::*;
use crate::embedded::{
    DirectTake, EmbeddedClaim, EmbeddedError, EmbeddedRow, EmbeddedStatus, NewEmbedded, ReadClaim, ReadTake,
    StoredEmbedded, TICKET_BOUND, TICKET_OPENING, classify_read, read_ticket, same_payload, ticket_kind,
};
use crate::encoding::{contiguous, decoded, opening_key};
use crate::held::{NewHeld, StoredNotice, millis, quota_limit};
use crate::receipts::{
    Completion, SessionScope, StoredOperation, StoredOperationInput, StoredResult, resolve_operation_claim,
    resolve_operation_completion, resolve_result_claim, resolve_result_completion,
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

    /// Leave the connection as a fresh one would be: no session-level advisory lock. `false` means the connection cannot be reused, and an answer that does
    /// not come within `wait` counts as one.
    fn reset(&self, wait: Duration, stall: Option<Duration>) -> bool {
        self.ask(Some(wait), move |state| {
            if let Some(stall) = stall {
                std::thread::sleep(stall);
            }
            let mut reset = || -> Result<(), ::postgres::Error> {
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
}

/// The connection a leased store runs on, for the host's own SQL. Only a leased store hands
/// one out ([`LogStore::postgres`]).
pub struct LeasedPostgres {
    lease: Lease,
}

impl LeasedPostgres {
    /// Host integration SQL (its own tables, advisory locks) runs on the leased connection, the
    /// one the event log and receipts use. Never expose this capability to clients.
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
        Ok(Self { pool, lease: None })
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

    /// Claims an operation receipt before starting work. Completed receipts return saved decisions.
    pub(crate) fn claim_operation(&self, request: OperationRequest) -> Result<OperationClaim, ReceiptError> {
        let lock = operation_lock(&request.key);
        self.serialized(lock, move |client| {
            let claim = resolve_operation_claim(read_operation(client, &request.key)?, &request)?;
            if claim == OperationClaim::Claimed {
                let stored = serde_json::to_value(StoredOperationInput::from_request(&request))
                    .map_err(|error| ReceiptError::storage(error.to_string()))?;
                client.execute(
                    "INSERT INTO openappa_operations (organization_id, caller_id, session_id, operation_id, root, input, status) \
                     VALUES ($1,$2,$3,$4,$5,$6,'pending')",
                    &[
                        &request.key.session.organization_id,
                        &request.key.binding.caller_id(),
                        &request.key.session.session_id,
                        &request.key.operation_id,
                        &request.root.as_str(),
                        &stored,
                    ],
                )?;
            }
            Ok(claim)
        })
    }

    /// Completes a claimed operation receipt with its final decision.
    pub(crate) fn complete_operation(&self, key: OperationKey, decision: Value) -> Result<(), ReceiptError> {
        let lock = operation_lock(&key);
        self.serialized(lock, move |client| {
            let existing = read_operation(client, &key)?;
            if let Completion::Write = resolve_operation_completion(existing, &key, &decision)? {
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

    /// Claims a durable processed-result receipt before result processing.
    pub(crate) fn claim_processed_result(
        &self,
        request: ProcessedResultRequest,
    ) -> Result<ProcessedResultClaim, ReceiptError> {
        let lock = result_lock(&request.key);
        self.serialized(lock, move |client| {
            let claim = resolve_result_claim(read_result(client, &request.key)?, &request)?;
            if claim == ProcessedResultClaim::Claimed {
                client.execute(
                    "INSERT INTO openappa_processed_results (organization_id, caller_id, session_id, tool_call_id, root, status) \
                     VALUES ($1,$2,$3,$4,$5,'pending')",
                    &[
                        &request.key.session.organization_id,
                        &request.key.caller_id,
                        &request.key.session.session_id,
                        &request.key.tool_call_id,
                        &request.root.as_str(),
                    ],
                )?;
            }
            Ok(claim)
        })
    }

    /// Completes a processed-result receipt with its approved output and decision.
    pub(crate) fn complete_processed_result(
        &self,
        key: ProcessedResultKey,
        approved_output: String,
        decision: Value,
    ) -> Result<(), ReceiptError> {
        let lock = result_lock(&key);
        self.serialized(lock, move |client| {
            let existing = read_result(client, &key)?;
            if let Completion::Write = resolve_result_completion(existing, &key, &approved_output, &decision)? {
                client.execute(
                    "UPDATE openappa_processed_results SET status='complete', approved_output=$4, decision=$5 \
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
        let receiver = receiver.as_str().to_owned();
        let (id, digest, label, body) = (
            held.id.as_str().to_owned(),
            digest.to_string(),
            held.label.clone(),
            body.to_owned(),
        );
        let (expires_at, now, quota) = (held.expires_at, millis(now), quota_limit(quota));
        self.serialized(held_lock(&receiver), move |client| {
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
        self.serialized(held_lock(&receiver), move |client| {
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
        self.serialized(held_lock(&receiver), move |client| {
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
        let fresh = fresh.clone();
        self.serialized(embedded_lock(&fresh.root), move |client| {
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
        self.serialized(embedded_lock(&root), move |client| {
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
        let (root, id, recipient, call_id, arguments) = (
            root.to_owned(),
            id.to_owned(),
            recipient.to_owned(),
            call_id.to_owned(),
            arguments.to_owned(),
        );
        self.serialized(embedded_lock(&root), move |client| {
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
        let (root, id, call_id) = (root.to_owned(), id.to_owned(), call_id.to_owned());
        let decision = serde_json::from_str::<serde_json::Value>(decision)
            .map_err(|error| EmbeddedError::Storage(format!("invalid read receipt JSON: {error}")))?;
        self.serialized(embedded_lock(&root), move |client| {
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
        self.serialized(embedded_lock(&root), move |client| {
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

    /// Run `operation` in a transaction, serialized against other writers of `lock`.
    fn serialized<T, E>(
        &self,
        lock: String,
        operation: impl FnOnce(&mut Client) -> Result<T, E> + Send + 'static,
    ) -> Result<T, E>
    where
        T: Send + 'static,
        E: From<PostgresError> + Send + 'static,
    {
        self.query(move |client| {
            client.batch_execute("BEGIN")?;
            let result = match client.query_one("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))", &[&lock]) {
                Ok(_) => operation(client),
                Err(error) => Err(PostgresError::from(error).into()),
            };
            client.batch_execute(if result.is_ok() { "COMMIT" } else { "ROLLBACK" })?;
            Ok(result)
        })?
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
        let created = self.serialized::<_, PostgresError>(id.clone(), move |client| {
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
        let hash = opening_key(root, &batches)?.as_str().to_owned();
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

    pub(super) fn workspace_batches(&self, workspace: &str) -> Result<Vec<Vec<u8>>, crate::files::FileStoreError> {
        let workspace = workspace.to_owned();
        self.query(move |client| {
            let rows = client.query(
                "SELECT seq,payload FROM openappa_file_events WHERE workspace=$1 ORDER BY seq",
                &[&workspace],
            )?;
            contiguous(rows.into_iter().map(|row| (row.get(0), row.get(1))).collect())
                .map_err(|error| PostgresError(error.to_string()))
        })
        .map_err(Into::into)
    }

    pub(super) fn create_workspace(
        &self,
        workspace: &str,
        payload: Vec<u8>,
    ) -> Result<(), crate::files::FileStoreError> {
        let workspace = workspace.to_owned();
        self.serialized(format!("openappa-file-workspace:{workspace}"), move |client| {
            client.execute(
                "INSERT INTO openappa_file_events(workspace,seq,payload) VALUES ($1,0,$2) ON CONFLICT DO NOTHING",
                &[&workspace, &payload],
            )?;
            Ok(())
        })
    }

    pub(super) fn append_workspace(
        &self,
        workspace: &str,
        basis: u64,
        payload: Vec<u8>,
        root: Option<TrajectoryId>,
    ) -> Result<(), crate::files::FileStoreError> {
        let workspace = workspace.to_owned();
        self.serialized(format!("openappa-file-workspace:{workspace}"), move |client| {
            let current = client
                .query_one(
                    "SELECT COALESCE(MAX(seq)+1,0) FROM openappa_file_events WHERE workspace=$1",
                    &[&workspace],
                )?
                .get::<_, i64>(0) as u64;
            if current != basis {
                return Err(crate::files::FileStoreError::Conflict {
                    expected: basis,
                    actual: current,
                });
            }
            if let Some(root) = root {
                let root = root.as_str().to_owned();
                client.query_one(
                    "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
                    &[&format!("openappa-file-root:{root}")],
                )?;
                if let Some(row) =
                    client.query_opt("SELECT workspace FROM openappa_file_roots WHERE root=$1", &[&root])?
                {
                    let existing: String = row.get(0);
                    if existing != workspace {
                        return Err(crate::files::FileStoreError::Configuration(
                            "a root cannot change its tracked workspace".into(),
                        ));
                    }
                } else {
                    client.execute(
                        "INSERT INTO openappa_file_roots(root,workspace,seq) VALUES ($1,$2,$3)",
                        &[&root, &workspace, &(current as i64)],
                    )?;
                }
            }
            client.execute(
                "INSERT INTO openappa_file_events(workspace,seq,payload) VALUES ($1,$2,$3)",
                &[&workspace, &(current as i64), &payload],
            )?;
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
        let root = root.as_str().to_owned();
        let key = key.map(str::to_owned);
        let conflict = self.serialized::<_, PostgresError>(root.clone(), move |client| {
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

    pub(super) fn append_holding_read(
        &self,
        root: &TrajectoryId,
        basis: u64,
        bytes: Vec<u8>,
        key: Option<&str>,
        hold: &crate::embedded::EmbeddedAppendHold<'_>,
    ) -> Result<(), AppendError> {
        let root_lock = root.as_str().to_owned();
        let root = root.as_str().to_owned();
        let key = key.map(str::to_owned);
        let id = hold.id.to_owned();
        let call_id = hold.call_id.to_owned();
        let generation = hold.generation;
        let bind = hold.bind;
        let bound = serde_json::json!({"generation": generation, "kind": TICKET_BOUND});
        self.serialized::<_, PostgresError>(root_lock, move |client| {
            let current = client
                .query_one(
                    "SELECT COALESCE(MAX(seq) + 1, 0) FROM openappa_events WHERE root = $1",
                    &[&root],
                )?
                .get::<_, i64>(0) as u64;
            if current != basis {
                return Ok(Err(AppendError::Conflict { current }));
            }
            let changed = client.execute(
                "UPDATE openappa_embedded_peer_messages
                 SET decision = CASE WHEN $6 THEN $5 ELSE decision END
                 WHERE root = $1 AND id = $2 AND read_call_id = $3 AND status = 'read'
                   AND decision->>'kind' IN ('opening', 'bound')
                   AND (decision->>'generation')::bigint = $4",
                &[&root, &id, &call_id, &generation, &bound, &bind],
            )?;
            if changed != 1 {
                return Ok(Err(AppendError::ReadClaimLost));
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
            Ok(Ok(()))
        })?
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

fn read_operation(client: &mut Client, key: &OperationKey) -> Result<Option<StoredOperation>, ReceiptError> {
    client
        .query_opt(
            "SELECT organization_id, caller_id, session_id, root, input, status, decision \
             FROM openappa_operations WHERE organization_id=$1 AND session_id=$2 AND operation_id=$3 FOR UPDATE",
            &[&key.session.organization_id, &key.session.session_id, &key.operation_id],
        )?
        .map(|row| {
            let input = StoredOperationInput::decode(row.get(4))?;
            Ok(StoredOperation {
                session: SessionScope {
                    organization_id: row.get(0),
                    session_id: row.get(2),
                },
                binding: input.binding(row.get(1))?,
                root: row.get(3),
                semantic: input.semantic,
                status: row.get(5),
                decision: row.get::<_, Option<Value>>(6).map(Ok),
            })
        })
        .transpose()
}

fn read_result(client: &mut Client, key: &ProcessedResultKey) -> Result<Option<StoredResult>, ReceiptError> {
    Ok(client
        .query_opt(
            "SELECT organization_id, session_id, root, status, approved_output, decision \
             FROM openappa_processed_results WHERE organization_id=$1 AND session_id=$2 AND tool_call_id=$3 FOR UPDATE",
            &[&key.session.organization_id, &key.session.session_id, &key.tool_call_id],
        )?
        .map(|row| StoredResult {
            session: SessionScope {
                organization_id: row.get(0),
                session_id: row.get(1),
            },
            root: row.get(2),
            status: row.get(3),
            approved_output: row.get(4),
            decision: row.get::<_, Option<Value>>(5).map(Ok),
        }))
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
    let (kind, generation) = ticket_kind(row.decision.as_deref()).unwrap_or_else(|| (TICKET_OPENING.to_string(), 0));
    let next = generation.saturating_add(1);
    let ticket = serde_json::from_str::<serde_json::Value>(&read_ticket(&kind, next)).expect("the read ticket is json");
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

fn embedded_lock(root: &str) -> String {
    format!("openappa-embedded-peer:{root}")
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

fn held_lock(receiver: &str) -> String {
    format!("openappa-held:{receiver}")
}

fn operation_lock(key: &OperationKey) -> String {
    format!(
        "openappa-operation:{}:{}:{}",
        key.session.organization_id, key.session.session_id, key.operation_id
    )
}

fn result_lock(key: &ProcessedResultKey) -> String {
    format!(
        "openappa-result:{}:{}:{}",
        key.session.organization_id, key.session.session_id, key.tool_call_id
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The advisory lock keys are shared with every other process writing the same database,
    /// so their spelling is a wire format.
    #[test]
    fn advisory_lock_keys_are_frozen() {
        let session = SessionScope {
            organization_id: "org".to_owned(),
            session_id: "session".to_owned(),
        };
        let operation = OperationKey {
            session: session.clone(),
            binding: ReceiptBinding::Caller {
                caller_id: "caller".to_owned(),
            },
            operation_id: "op".to_owned(),
        };
        assert_eq!(operation_lock(&operation), "openappa-operation:org:session:op");
        let result = ProcessedResultKey {
            session,
            caller_id: Some("caller".to_owned()),
            tool_call_id: "call".to_owned(),
        };
        assert_eq!(result_lock(&result), "openappa-result:org:session:call");
        assert_eq!(held_lock("cc:root"), "openappa-held:cc:root");
    }
}
