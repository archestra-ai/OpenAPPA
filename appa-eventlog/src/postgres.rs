//! PostgreSQL storage for embedded hosts. The host installs the schema; Rust
//! retains the SQLite event encoding and policy-file validation. A dedicated
//! connection thread keeps the synchronous log API usable from async hooks.

use std::sync::mpsc;

use ::postgres::Client;
use postgres_native_tls::MakeTlsConnector;

use super::*;

/// Additional host migration required for proxy receipts and checkpoints.
pub const PROXY_SCHEMA_SQL: &str = include_str!("postgres-proxy.sql");

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct PostgresError(pub String);

impl From<::postgres::Error> for PostgresError {
    fn from(error: ::postgres::Error) -> Self {
        Self(error.to_string())
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

    /// Hold an outer transaction across asynchronous hook dispatch. The host
    /// must serialize access to this store until commit or rollback.
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

    fn mutate<T: Send + 'static>(
        &self,
        root: String,
        operation: impl FnOnce(&mut Client) -> Result<T, PostgresError> + Send + 'static,
    ) -> Result<T, PostgresError> {
        self.run(move |state| {
            let outer = state.transaction;
            if !outer {
                state.client.batch_execute("BEGIN")?;
            }
            let result = (|| {
                state
                    .client
                    .query_one("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))", &[&root])?;
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
        let created = self.mutate(id.clone(), move |client| {
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
        let opening = match decode(first)? {
            Batch::Facts(facts) => facts,
            Batch::Inventory(_) => Vec::new(),
        };
        let Some(Fact::TrajectoryOpened { policy_file_key, .. }) = opening.first() else {
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

    pub(super) fn append(&self, based_on: &Log, bytes: Vec<u8>) -> Result<(), AppendError> {
        let root = based_on.root.as_str().to_owned();
        let basis = based_on.basis;
        let conflict = self.mutate(root.clone(), move |client| {
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
            Ok(None)
        })?;
        if let Some(current) = conflict {
            return Err(AppendError::Conflict { current });
        }
        Ok(())
    }

    pub(super) fn checkpoint(
        &self,
        based_on: &Log,
        id: CheckpointId,
        snapshot: CheckpointSnapshot,
    ) -> Result<Checkpoint, CheckpointError> {
        enum Admission {
            Written,
            Conflict(u64),
            IdConflict,
        }
        let root = based_on.root.as_str().to_owned();
        let basis = based_on.basis;
        let id_text = id.as_str().to_owned();
        let policy_key = PolicyFileKey::of(&based_on.policy_file).as_str().to_owned();
        let snapshot_bytes = serde_json::to_vec(&snapshot).expect("checkpoint snapshots serialize");
        let admission = self
            .mutate(root.clone(), move |client| {
                let current = client
                    .query_one("SELECT count(*) FROM openappa_events WHERE root = $1", &[&root])?
                    .get::<_, i64>(0) as u64;
                if current != basis {
                    return Ok(Admission::Conflict(current));
                }
                let existing = client.query_opt(
                    "SELECT source_root, snapshot FROM checkpoints WHERE id = $1",
                    &[&id_text],
                )?;
                if let Some(existing) = existing {
                    let source_root: String = existing.get(0);
                    let stored: Vec<u8> = existing.get(1);
                    return Ok(if source_root == root && stored == snapshot_bytes {
                        Admission::Written
                    } else {
                        Admission::IdConflict
                    });
                }
                client.execute(
                    "INSERT INTO checkpoints (id, source_root, basis, policy_key, snapshot) VALUES ($1, $2, $3, $4, $5)",
                    &[&id_text, &root, &(basis as i64), &policy_key, &snapshot_bytes],
                )?;
                Ok(Admission::Written)
            })
            .map_err(CheckpointError::from)?;
        match admission {
            Admission::Written => Ok(Checkpoint {
                id,
                root: based_on.root.clone(),
                snapshot,
            }),
            Admission::Conflict(current) => Err(CheckpointError::Conflict { current }),
            Admission::IdConflict => Err(CheckpointError::IdConflict {
                id: id.as_str().to_owned(),
            }),
        }
    }

    pub(super) fn checkpoint_by_id(&self, id: &CheckpointId) -> Result<Checkpoint, CheckpointLookupError> {
        let id_text = id.as_str().to_owned();
        let row = self
            .with_client(move |client| {
                Ok(client
                    .query_opt(
                        "SELECT source_root, snapshot FROM checkpoints WHERE id = $1",
                        &[&id_text],
                    )?
                    .map(|row| (row.get::<_, String>(0), row.get::<_, Vec<u8>>(1))))
            })
            .map_err(CheckpointLookupError::from)?;
        let Some((root, snapshot)) = row else {
            return Err(CheckpointLookupError::Unknown);
        };
        let snapshot =
            serde_json::from_slice(&snapshot).map_err(|error| CheckpointLookupError::Undecodable(error.to_string()))?;
        Ok(Checkpoint {
            id: id.clone(),
            root: TrajectoryId::new(root),
            snapshot,
        })
    }

    pub(super) fn fork_checkpoint(
        &self,
        root: TrajectoryId,
        key: PolicyFileKey,
        checkpoint: CheckpointOpening,
        policy_file: Vec<u8>,
        snapshot_bytes: Vec<u8>,
        bytes: Vec<u8>,
    ) -> Result<TrajectoryId, ForkError> {
        enum Admission {
            Opened,
            Existing(Vec<u8>),
            Unknown,
            PolicyMismatch,
            TargetExists,
            SnapshotMismatch,
        }
        let target = root.as_str().to_owned();
        let checkpoint_id = checkpoint.id.as_str().to_owned();
        let checkpoint_lookup = checkpoint_id.clone();
        let policy_key = key.as_str().to_owned();
        let admission = self
            .mutate(target.clone(), move |client| {
                let durable = client.query_opt(
                    "SELECT source_root, policy_key, snapshot FROM checkpoints WHERE id = $1",
                    &[&checkpoint_lookup],
                )?;
                let Some(durable) = durable else {
                    return Ok(Admission::Unknown);
                };
                let source_root: String = durable.get(0);
                let stored_key: String = durable.get(1);
                let stored_snapshot: Vec<u8> = durable.get(2);
                if stored_key != policy_key {
                    return Ok(Admission::PolicyMismatch);
                }
                if source_root == target {
                    return Ok(Admission::TargetExists);
                }
                if stored_snapshot != snapshot_bytes {
                    return Ok(Admission::SnapshotMismatch);
                }
                if let Some(existing) = client.query_opt(
                    "SELECT payload FROM openappa_events WHERE root = $1 AND seq = 0",
                    &[&target],
                )? {
                    return Ok(Admission::Existing(existing.get(0)));
                }
                client.execute(
                    "INSERT INTO openappa_policy_files (hash, bytes) VALUES ($1, $2) ON CONFLICT DO NOTHING",
                    &[&policy_key, &policy_file],
                )?;
                client.execute(
                    "INSERT INTO openappa_events (root, seq, payload) VALUES ($1, 0, $2)",
                    &[&target, &bytes],
                )?;
                Ok(Admission::Opened)
            })
            .map_err(ForkError::from)?;
        match admission {
            Admission::Opened => Ok(root),
            Admission::Existing(existing) => {
                let existing = decode(&existing).map_err(|error| ForkError::Malformed {
                    detail: error.to_string(),
                })?;
                if matches!(existing, Batch::Facts(facts) if checkpoint_opening(&facts).is_some_and(|found| found == &checkpoint))
                {
                    Ok(root)
                } else {
                    Err(ForkError::TargetExists {
                        root: root.as_str().to_owned(),
                    })
                }
            }
            Admission::Unknown => Err(ForkError::UnknownCheckpoint { id: checkpoint_id }),
            Admission::PolicyMismatch => Err(ForkError::PolicyMismatch),
            Admission::TargetExists => Err(ForkError::TargetExists {
                root: root.as_str().to_owned(),
            }),
            Admission::SnapshotMismatch => Err(ForkError::CheckpointMismatch),
        }
    }

    pub(super) fn begin_proxy_event(
        &self,
        root_id: &str,
        event_id: &str,
        body_digest: &str,
        boot_owner: &str,
    ) -> Result<ProxyEventAdmission, ProxyStoreError> {
        let root_id = root_id.to_owned();
        let event_id = event_id.to_owned();
        let body_digest = body_digest.to_owned();
        let boot_owner = boot_owner.to_owned();
        self.mutate(root_id.clone(), move |client| {
            let existing = client.query_opt(
                "SELECT body_digest, state, boot_owner, response FROM proxy_events WHERE root_id = $1 AND event_id = $2",
                &[&root_id, &event_id],
            )?;
            if let Some(existing) = existing {
                let digest: String = existing.get(0);
                let state: String = existing.get(1);
                let owner: String = existing.get(2);
                let response: Option<Vec<u8>> = existing.get(3);
                return Ok(if digest != body_digest {
                    ProxyEventAdmission::Conflict
                } else if state == "completed" {
                    response.map(ProxyEventAdmission::Replay).unwrap_or(ProxyEventAdmission::Uncertain)
                } else if state == "pending" && owner == boot_owner {
                    ProxyEventAdmission::InProgress
                } else {
                    ProxyEventAdmission::Uncertain
                });
            }
            if client
                .query_opt(
                    "SELECT 1 FROM proxy_events WHERE root_id = $1 AND state = 'pending' LIMIT 1",
                    &[&root_id],
                )?
                .is_some()
            {
                return Ok(ProxyEventAdmission::RootPending);
            }
            let count = client
                .query_one("SELECT COUNT(*) FROM proxy_events WHERE root_id = $1", &[&root_id])?
                .get::<_, i64>(0);
            if count >= MAX_PROXY_EVENTS_PER_ROOT {
                return Ok(ProxyEventAdmission::BudgetExceeded);
            }
            client.execute(
                "INSERT INTO proxy_events (root_id, event_id, body_digest, state, boot_owner) VALUES ($1, $2, $3, 'pending', $4)",
                &[&root_id, &event_id, &body_digest, &boot_owner],
            )?;
            Ok(ProxyEventAdmission::Started)
        })
        .map_err(ProxyStoreError::from)
    }

    pub(super) fn complete_proxy_event(&self, completion: &ProxyEventCompletion<'_>) -> Result<(), ProxyStoreError> {
        let root_id = completion.root_id.to_owned();
        let event_id = completion.event_id.to_owned();
        let body_digest = completion.body_digest.to_owned();
        let response = completion.response.to_vec();
        let bindings = completion.bindings.to_vec();
        let dispatch_bindings = completion.dispatch_bindings.to_vec();
        let approval_id = completion.approval_id.map(str::to_owned);
        const LOST_INTENT: &str = "proxy event completion lost its pending intent";
        self.mutate(root_id.clone(), move |client| {
            let changed = client.execute(
                "UPDATE proxy_events SET state = 'completed', response = $4 WHERE root_id = $1 AND event_id = $2 AND body_digest = $3 AND state = 'pending'",
                &[&root_id, &event_id, &body_digest, &response],
            )?;
            if changed != 1 {
                return Err(PostgresError(LOST_INTENT.to_owned()));
            }
            for binding in bindings {
                client.execute(
                    "INSERT INTO proxy_offer_bindings (offer_id, root_id, tool, arguments_sha256, kind, deployment_fingerprint, batch_id, position) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                    &[&binding.offer_id, &binding.root_id, &binding.tool, &binding.arguments_sha256, &binding.kind, &binding.deployment_fingerprint, &binding.batch_id, &binding.position.map(i64::from)],
                )?;
            }
            for binding in dispatch_bindings {
                client.execute(
                    "INSERT INTO proxy_dispatch_bindings (root_id, lane_id, call_id, tool, arguments_sha256, dispatch, spawn_binding, deployment_fingerprint, batch_id, position) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
                    &[&binding.root_id, &binding.lane_id, &binding.call_id, &binding.tool, &binding.arguments_sha256, &binding.dispatch, &binding.spawn_binding, &binding.deployment_fingerprint, &binding.batch_id, &binding.position.map(i64::from)],
                )?;
            }
            if let Some(approval_id) = approval_id {
                let changed = client.execute(
                    "UPDATE proxy_approval_grants SET state = 'completed' WHERE approval_id = $1 AND root_id = $2 AND event_id = $3 AND body_digest = $4 AND state = 'pending'",
                    &[&approval_id, &root_id, &event_id, &body_digest],
                )?;
                if changed != 1 {
                    return Err(PostgresError(LOST_INTENT.to_owned()));
                }
            }
            Ok(())
        })
        .map_err(|error| {
            if error.0 == LOST_INTENT {
                ProxyStoreError::LostIntent
            } else {
                ProxyStoreError::Postgres(error)
            }
        })
    }

    pub(super) fn proxy_offer_binding(&self, offer_id: &str) -> Result<Option<ProxyOfferBinding>, ProxyStoreError> {
        let offer_id = offer_id.to_owned();
        self.with_client(move |client| {
            Ok(client
                .query_opt(
                    "SELECT offer_id, root_id, tool, arguments_sha256, kind, deployment_fingerprint, batch_id, position FROM proxy_offer_bindings WHERE offer_id = $1",
                    &[&offer_id],
                )?
                .map(|row| ProxyOfferBinding {
                    offer_id: row.get(0),
                    root_id: row.get(1),
                    tool: row.get(2),
                    arguments_sha256: row.get(3),
                    kind: row.get(4),
                    deployment_fingerprint: row.get(5),
                    batch_id: row.get(6),
                    position: row.get::<_, Option<i64>>(7).map(|position| position as u32),
                }))
        })
        .map_err(ProxyStoreError::from)
    }

    pub(super) fn proxy_dispatch_binding(
        &self,
        root_id: &str,
        lane_id: &str,
        call_id: &str,
    ) -> Result<Option<ProxyDispatchBinding>, ProxyStoreError> {
        let root_id = root_id.to_owned();
        let lane_id = lane_id.to_owned();
        let call_id = call_id.to_owned();
        self.with_client(move |client| {
            Ok(client
                .query_opt(
                    "SELECT root_id, lane_id, call_id, tool, arguments_sha256, dispatch, spawn_binding, deployment_fingerprint, batch_id, position FROM proxy_dispatch_bindings WHERE root_id = $1 AND lane_id = $2 AND call_id = $3",
                    &[&root_id, &lane_id, &call_id],
                )?
                .map(|row| ProxyDispatchBinding {
                    root_id: row.get(0),
                    lane_id: row.get(1),
                    call_id: row.get(2),
                    tool: row.get(3),
                    arguments_sha256: row.get(4),
                    dispatch: row.get(5),
                    spawn_binding: row.get(6),
                    deployment_fingerprint: row.get(7),
                    batch_id: row.get(8),
                    position: row.get::<_, Option<i64>>(9).map(|position| position as u32),
                }))
        })
        .map_err(ProxyStoreError::from)
    }

    pub(super) fn create_proxy_batch(
        &self,
        batch: &ProxyBatchBinding,
        positions: &[ProxyBatchPosition],
    ) -> Result<bool, ProxyStoreError> {
        let batch = batch.clone();
        let positions = positions.to_vec();
        self.mutate(batch.root_id.clone(), move |client| {
            let existing = client.query_opt(
                "SELECT root_id, lane_id, core_batch_id, positions, basis, deployment_fingerprint FROM proxy_batches WHERE batch_id = $1",
                &[&batch.batch_id],
            )?;
            if let Some(existing) = existing {
                return Ok(
                    existing.get::<_, String>(0) == batch.root_id
                        && existing.get::<_, String>(1) == batch.lane_id
                        && existing.get::<_, String>(2) == batch.core_batch_id
                        && existing.get::<_, i32>(3) == batch.positions as i32
                        && existing.get::<_, i64>(4) == batch.basis as i64
                        && existing.get::<_, String>(5) == batch.deployment_fingerprint,
                );
            }
            if positions.len() != batch.positions as usize
                || positions.iter().enumerate().any(|(index, position)| {
                    position.batch_id != batch.batch_id || position.position != index as u32
                })
            {
                return Ok(false);
            }
            client.execute(
                "INSERT INTO proxy_batches (batch_id, root_id, lane_id, core_batch_id, positions, basis, deployment_fingerprint) VALUES ($1, $2, $3, $4, $5, $6, $7)",
                &[&batch.batch_id, &batch.root_id, &batch.lane_id, &batch.core_batch_id, &(batch.positions as i32), &(batch.basis as i64), &batch.deployment_fingerprint],
            )?;
            for position in positions {
                client.execute(
                    "INSERT INTO proxy_batch_positions (batch_id, position, call_id, tool, arguments_sha256, arguments, effective_tool, effective_arguments_sha256, effective_arguments, dispatch, spawn, spawn_binding, authorized) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
                    &[&position.batch_id, &(position.position as i32), &position.call_id, &position.tool, &position.arguments_sha256, &position.arguments, &position.effective_tool, &position.effective_arguments_sha256, &position.effective_arguments, &position.dispatch, &position.spawn, &position.spawn_binding, &position.authorized],
                )?;
            }
            Ok(true)
        })
        .map_err(ProxyStoreError::from)
    }

    pub(super) fn proxy_batch(&self, batch_id: &str) -> Result<Option<ProxyBatchBinding>, ProxyStoreError> {
        let batch_id = batch_id.to_owned();
        self.with_client(move |client| {
            Ok(client
                .query_opt(
                    "SELECT batch_id, root_id, lane_id, core_batch_id, positions, basis, deployment_fingerprint FROM proxy_batches WHERE batch_id = $1",
                    &[&batch_id],
                )?
                .map(|row| ProxyBatchBinding {
                    batch_id: row.get(0),
                    root_id: row.get(1),
                    lane_id: row.get(2),
                    core_batch_id: row.get(3),
                    positions: row.get::<_, i32>(4) as u32,
                    basis: row.get::<_, i64>(5) as u64,
                    deployment_fingerprint: row.get(6),
                }))
        })
        .map_err(ProxyStoreError::from)
    }

    pub(super) fn advance_proxy_batch_basis(
        &self,
        batch_id: &str,
        expected: u64,
        next: u64,
    ) -> Result<bool, ProxyStoreError> {
        let batch_id = batch_id.to_owned();
        self.with_client(move |client| {
            Ok(client.execute(
                "UPDATE proxy_batches SET basis = $3 WHERE batch_id = $1 AND basis = $2",
                &[&batch_id, &(expected as i64), &(next as i64)],
            )? == 1)
        })
        .map_err(ProxyStoreError::from)
    }

    pub(super) fn proxy_batch_positions(&self, batch_id: &str) -> Result<Vec<ProxyBatchPosition>, ProxyStoreError> {
        let batch_id = batch_id.to_owned();
        self.with_client(move |client| {
            let rows = client.query(
                "SELECT batch_id, position, call_id, tool, arguments_sha256, arguments, effective_tool, effective_arguments_sha256, effective_arguments, dispatch, spawn, spawn_binding, authorized FROM proxy_batch_positions WHERE batch_id = $1 ORDER BY position",
                &[&batch_id],
            )?;
            Ok(rows
                .into_iter()
                .map(|row| ProxyBatchPosition {
                    batch_id: row.get(0),
                    position: row.get::<_, i32>(1) as u32,
                    call_id: row.get(2),
                    tool: row.get(3),
                    arguments_sha256: row.get(4),
                    arguments: row.get(5),
                    effective_tool: row.get(6),
                    effective_arguments_sha256: row.get(7),
                    effective_arguments: row.get(8),
                    dispatch: row.get(9),
                    spawn: row.get(10),
                    spawn_binding: row.get(11),
                    authorized: row.get(12),
                })
                .collect())
        })
        .map_err(ProxyStoreError::from)
    }

    pub(super) fn update_proxy_batch_position(&self, position: &ProxyBatchPosition) -> Result<(), ProxyStoreError> {
        let position = position.clone();
        let changed = self
            .with_client(move |client| {
                Ok(client.execute(
                    "UPDATE proxy_batch_positions SET effective_tool = $3, effective_arguments_sha256 = $4, effective_arguments = $5, dispatch = $6, spawn_binding = $7, authorized = $8 WHERE batch_id = $1 AND position = $2",
                    &[&position.batch_id, &(position.position as i32), &position.effective_tool, &position.effective_arguments_sha256, &position.effective_arguments, &position.dispatch, &position.spawn_binding, &position.authorized],
                )?)
            })
            .map_err(ProxyStoreError::from)?;
        if changed == 1 {
            Ok(())
        } else {
            Err(ProxyStoreError::LostIntent)
        }
    }

    pub(super) fn quarantine_proxy_batch(&self, batch_id: &str, reason: &str) -> Result<(), ProxyStoreError> {
        let batch_id = batch_id.to_owned();
        let reason = reason.to_owned();
        self.with_client(move |client| {
            client.execute(
                "INSERT INTO proxy_batch_quarantines (batch_id, reason) VALUES ($1, $2) ON CONFLICT DO NOTHING",
                &[&batch_id, &reason],
            )?;
            Ok(())
        })
        .map_err(ProxyStoreError::from)
    }

    pub(super) fn proxy_batch_quarantined(&self, batch_id: &str) -> Result<bool, ProxyStoreError> {
        let batch_id = batch_id.to_owned();
        self.with_client(move |client| {
            Ok(client
                .query_opt(
                    "SELECT 1 FROM proxy_batch_quarantines WHERE batch_id = $1",
                    &[&batch_id],
                )?
                .is_some())
        })
        .map_err(ProxyStoreError::from)
    }

    pub(super) fn begin_proxy_approval_grant(
        &self,
        approval_id: &str,
        root_id: &str,
        event_id: &str,
        body_digest: &str,
    ) -> Result<ProxyApprovalAdmission, ProxyStoreError> {
        let approval_id = approval_id.to_owned();
        let root_id = root_id.to_owned();
        let event_id = event_id.to_owned();
        let body_digest = body_digest.to_owned();
        self.mutate(root_id.clone(), move |client| {
            if client
                .query_opt("SELECT 1 FROM proxy_approval_grants WHERE approval_id = $1", &[&approval_id])?
                .is_some()
            {
                return Ok(ProxyApprovalAdmission::Consumed);
            }
            client.execute(
                "INSERT INTO proxy_approval_grants (approval_id, root_id, event_id, body_digest, state) VALUES ($1, $2, $3, $4, 'pending')",
                &[&approval_id, &root_id, &event_id, &body_digest],
            )?;
            Ok(ProxyApprovalAdmission::Started)
        })
        .map_err(ProxyStoreError::from)
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
