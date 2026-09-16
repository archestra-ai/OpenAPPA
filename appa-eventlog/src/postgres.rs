//! PostgreSQL storage for embedded hosts. The host installs the schema; Rust
//! retains the SQLite event encoding and policy-file validation. A dedicated
//! connection thread keeps the synchronous log API usable from async hooks.

use std::sync::mpsc;

use ::postgres::Client;
use postgres_native_tls::MakeTlsConnector;

use super::*;

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
